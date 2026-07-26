//! [`OllamaBackend`] — [`ModelBackend`] implementation against Ollama's
//! **native** `/api/chat` endpoint (NOT the OpenAI-compatible surface),
//! streaming via NDJSON (one JSON object per line). No API key.

use std::collections::VecDeque;
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;
use crate::http_error::http_error_to_model_error;
use crate::ollama_wire::{
    OllamaSampling, OllamaStreamState, ToolTransport, build_request, extract_next_ndjson_line,
    parse_ndjson_line,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Env var del retry opt-in de transporte: número de REINTENTOS (no de
/// intentos totales) tras un fallo del `send()`. Ausente, vacía o
/// no-numérica ⇒ 0 (off) — el default histórico, sin cambio de
/// comportamiento para nadie que no lo pida.
const TRANSPORT_RETRIES_ENV: &str = "BRAZE_OLLAMA_TRANSPORT_RETRIES";

fn transport_retries_from_env() -> u32 {
    std::env::var(TRANSPORT_RETRIES_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(10)
}

/// Backoff del retry de transporte: 1s, 4s, luego 15s plano — pensado
/// para ráfagas de red degradada (segundos a decenas de segundos), no
/// para outages largos; con el cap de 10 reintentos el peor caso por
/// request es ~2.5 min, bien por debajo del timeout de tarea del bench.
/// Reintentos siempre-on del 500 "error parsing tool call" de Ollama —
/// el incidente roam (2026-07-19): un fallo de parseo POR-MUESTRA del
/// lado del server (razonamiento harmony filtrado al canal de tools),
/// donde re-muestrear es barato y casi siempre repara. Distinto del
/// retry de transporte (opt-in por env) y del dictamen anti-martilleo
/// de H-19 (que aplica a saturación, no a esto).
const TOOL_PARSE_500_RETRIES: u32 = 2;

/// ¿Es un `StreamError` de la clase "error parsing tool call"? Incidente
/// roam #17 (2026-07-20): en Ollama 0.32.1 el mismo fallo de parseo de
/// tool-call que #1 servía como HTTP 500 al enviar puede llegar en
/// cambio a mitad de un stream 200 (una línea NDJSON con `error`), y esa
/// variante NO la cubría el re-muestreo de send. Se re-muestrea con el
/// mismo criterio, pero solo si ningún evento salió aún al consumidor
/// (`produced_output == false`) — ver `complete`.
fn is_tool_parse_stream_error(err: &ModelError) -> bool {
    matches!(err, ModelError::StreamError(msg) if msg.contains("error parsing tool call"))
}

fn transport_retry_backoff(attempt: u32) -> std::time::Duration {
    match attempt {
        1 => std::time::Duration::from_secs(1),
        2 => std::time::Duration::from_secs(4),
        _ => std::time::Duration::from_secs(15),
    }
}

/// Default context window requested via `options.num_ctx`. Deliberately
/// well above Ollama's own Modelfile default (commonly 2048-4096) — an
/// agentic turn (system prompt + tool stubs + growing history) can exceed
/// that within a few tool-calling rounds, and Ollama truncates an
/// over-budget prompt from the front *silently*, with no error. 8192 is a
/// floor most locally-run 1B-8B models handle on CPU; callers can override
/// via [`OllamaBackend::with_num_ctx`].
const DEFAULT_NUM_CTX: u32 = 8192;

/// Low-but-not-zero: favors well-formed, repeatable tool calls (the
/// dominant failure mode for small local models is malformed JSON, not
/// insufficient creativity) while still letting the model recover from a
/// bad first attempt instead of repeating it identically forever.
const DEFAULT_TEMPERATURE: f32 = 0.2;

/// Streams completions from a local (or remote) Ollama server's native
/// chat API.
///
/// `base_url` defaults to `http://localhost:11434` (via [`OllamaBackend::new`])
/// but is configurable via [`OllamaBackend::with_base_url`] — documented
/// here per the task's "your choice" note: it IS configurable, not
/// hardcoded, so this backend can also target a remote/containerized
/// Ollama instance.
pub struct OllamaBackend {
    base_url: String,
    model: String,
    client: reqwest::Client,
    num_ctx: u32,
    temperature: f32,
    seed: Option<u64>,
    /// `None` (the default) omits the option so Ollama uses the model's
    /// own Modelfile value — see `OllamaOptions` in `ollama_wire.rs`.
    top_p: Option<f32>,
    top_k: Option<u32>,
    repeat_penalty: Option<f32>,
    /// Brazos B/C del A/B pre-registrado
    /// (docs/constrained-decoding-ab-design.md): `prompt_tools` renders
    /// the inventory as a system-prompt addendum instead of the `tools`
    /// field; `constrained_tools` additionally forces the envelope's JSON
    /// schema on the decoder via `format` (and implies `prompt_tools`).
    /// Both `false` (native tool-calling) in every composition root —
    /// only `braze-bench`'s `+ablate:` keys set them.
    prompt_tools: bool,
    constrained_tools: bool,
}

impl OllamaBackend {
    /// Targets `http://localhost:11434`.
    pub fn new(model: String) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            client: crate::http_client::build_client(),
            num_ctx: DEFAULT_NUM_CTX,
            temperature: DEFAULT_TEMPERATURE,
            seed: None,
            top_p: None,
            top_k: None,
            repeat_penalty: None,
            prompt_tools: false,
            constrained_tools: false,
        }
    }

    /// Targets a custom base URL (e.g. a remote Ollama instance, or a
    /// container reachable at a non-default host/port).
    pub fn with_base_url(model: String, base_url: String) -> Self {
        Self {
            base_url,
            model,
            client: crate::http_client::build_client(),
            num_ctx: DEFAULT_NUM_CTX,
            temperature: DEFAULT_TEMPERATURE,
            seed: None,
            top_p: None,
            top_k: None,
            repeat_penalty: None,
            prompt_tools: false,
            constrained_tools: false,
        }
    }

    /// Overrides the context window requested via `options.num_ctx` (see
    /// [`DEFAULT_NUM_CTX`] for why this matters). Chainable, e.g.
    /// `OllamaBackend::with_base_url(model, url).with_num_ctx(4096)`.
    pub fn with_num_ctx(mut self, num_ctx: u32) -> Self {
        self.num_ctx = num_ctx;
        self
    }

    /// Overrides the sampling temperature requested via
    /// `options.temperature` (see [`DEFAULT_TEMPERATURE`]).
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    /// Sets `options.seed` for reproducible sampling — e.g. so
    /// `braze-bench` sweeps compare backends run-for-run instead of each
    /// draw being unseeded noise (N-34, docs/AUDITORIA-2026-07-v2.md).
    /// Chainable, same shape as [`OllamaBackend::with_temperature`].
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Sets `options.top_p` (nucleus sampling). Unset by default so the
    /// model's Modelfile value applies — these three knobs exist for
    /// sampling sweeps (ítem 7 del backlog 2026-07-06: Qwen recomienda
    /// temp 0.7 / top_p 0.8 / top_k 20 / repeat_penalty 1.05).
    pub fn with_top_p(mut self, top_p: f32) -> Self {
        self.top_p = Some(top_p);
        self
    }

    /// Sets `options.top_k` — see [`OllamaBackend::with_top_p`].
    pub fn with_top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// Sets `options.repeat_penalty` — see [`OllamaBackend::with_top_p`].
    pub fn with_repeat_penalty(mut self, repeat_penalty: f32) -> Self {
        self.repeat_penalty = Some(repeat_penalty);
        self
    }

    /// Brazo B del A/B pre-registrado
    /// (docs/constrained-decoding-ab-design.md): requests advertise tools
    /// via a system-prompt addendum (envelope instructions + inventory)
    /// instead of the native `tools` field. The engine's envelope parser
    /// consumes the reply; whatever doesn't parse falls to the normal
    /// textual-rescue ladder. Chainable, same shape as
    /// [`OllamaBackend::with_temperature`].
    pub fn with_prompt_tools(mut self, enabled: bool) -> Self {
        self.prompt_tools = enabled;
        self
    }

    /// Brazo C: on top of prompt-tools mode (implied), constrains the
    /// decoder to the envelope's JSON schema via Ollama structured
    /// outputs (`format`) — syntax becomes impossible to break instead of
    /// repaired after the fact. Chainable.
    pub fn with_constrained_tools(mut self, enabled: bool) -> Self {
        self.constrained_tools = enabled;
        self
    }

    /// The [`ToolTransport`] the configured flags resolve to —
    /// `constrained_tools` implies prompt mode even if
    /// [`OllamaBackend::with_prompt_tools`] was never called.
    fn tool_transport(&self) -> ToolTransport {
        if self.prompt_tools || self.constrained_tools {
            ToolTransport::Prompt {
                constrained: self.constrained_tools,
            }
        } else {
            ToolTransport::Native
        }
    }
}

#[async_trait]
impl ModelBackend for OllamaBackend {
    fn name(&self) -> &str {
        "ollama"
    }

    #[tracing::instrument(
        skip(self, req),
        fields(provider = "ollama", model = %self.model, message_count = req.messages.len())
    )]
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let body = build_request(
            &req,
            &self.model,
            self.num_ctx,
            OllamaSampling {
                temperature: self.temperature,
                seed: self.seed,
                top_p: self.top_p,
                top_k: self.top_k,
                repeat_penalty: self.repeat_penalty,
            },
            self.tool_transport(),
        );
        tracing::info!(
            tool_count = body.tools.len(),
            prompt_tools = self.prompt_tools || self.constrained_tools,
            constrained = self.constrained_tools,
            num_ctx = self.num_ctx,
            num_predict = body.options.num_predict,
            "starting ollama completion turn"
        );

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));

        // Circuit breaker keyed by destination+model (2026-07-17,
        // recalibrated per AUDITORIA-2026-07-v8 K-1) — see
        // `AnthropicBackend::complete`'s identical comment. Ollama gets
        // no per-call HTTP-status retry (H-19's own dictamen: hammering
        // a saturated local backend doesn't help), but tracking
        // cross-call failure state is still useful — a sweep against an
        // unreachable Nitro shouldn't pay a fresh connect-timeout on
        // every subsequent task once the outage is established. The
        // guard travels into `StreamCtx`: success is only recorded once
        // the stream terminates cleanly, so a mid-generation death (the
        // documented ~2% Nitro failure mode) counts as a failure too.
        let breaker_key = format!(
            "ollama:{}:{}",
            self.base_url.trim_end_matches('/'),
            self.model
        );
        let guard = crate::circuit_breaker::acquire(&breaker_key)?;

        // Retry opt-in de TRANSPORTE (materializado tras el incidente
        // del ancla BFCL 2026-07-18: dos sweeps contaminados por ráfagas
        // de "error sending request" con la LAN degradada a RTT ~100ms).
        // Solo reintenta el fallo del `send()` — la fase donde CERO
        // bytes del stream se han consumido, así que reintentar es
        // semánticamente inocuo. Un HTTP de error o un corte a mitad de
        // stream NUNCA se reintenta acá. Off por default:
        // `BRAZE_OLLAMA_TRANSPORT_RETRIES` (0 = off). Composición con el
        // breaker (merge 2026-07-19): el retry corre POR DEBAJO del
        // guard, igual que `send_with_retry` en Anthropic/OpenRouter —
        // el breaker observa el desenlace FINAL de la llamada, no cada
        // intento transitorio.
        // Incidente roam #17: el bucle exterior re-muestrea el turno
        // cuando el stream falla con "error parsing tool call" a mitad de
        // una respuesta 200 (la variante que el 500-de-send de #1 no
        // cubre). Solo re-muestrea si NADA salió aún al consumidor, así
        // que replayar el primer evento es seguro y no hay salida a medio
        // emitir. El breaker viaja adjunto SÓLO al stream final: durante
        // el "priming" va en `None` para que un fallo re-muestreable no
        // cuente como caída del destino, igual que el 500 de send.
        let mut tool_parse_stream_attempts = 0u32;
        let event_stream: Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>> = 'resample: loop {
            let send_result = async {
                let max_attempts = 1 + transport_retries_from_env();
                let mut attempt = 0u32;
                loop {
                    attempt += 1;
                    let sent = self
                        .client
                        .post(url.clone())
                        .header("content-type", "application/json")
                        .json(&body)
                        .send()
                        .await;
                    let response = match sent {
                        Ok(r) => r,
                        Err(e) if attempt < max_attempts => {
                            let delay = transport_retry_backoff(attempt);
                            tracing::warn!(
                                attempt,
                                max_attempts,
                                delay_ms = delay.as_millis() as u64,
                                error = %e,
                                "ollama transport error on request send; retrying"
                            );
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        Err(e) => {
                            return Err(ModelError::Request(format!("ollama request failed: {e}")));
                        }
                    };

                    if response.status().is_success() {
                        return Ok(response);
                    }
                    // Incidente roam (2026-07-19, primera sesión de braze
                    // como herramienta de producción): Ollama devuelve 500
                    // "error parsing tool call" cuando el RAZONAMIENTO de un
                    // modelo harmony/thinking (gpt-oss:20b) se filtra al
                    // canal de tool-calls y su parser server-side no puede
                    // con el blob — un fallo POR-MUESTRA, no de saturación,
                    // así que el dictamen de H-19 ("no martillar un backend
                    // local saturado") no aplica: re-muestrear con el mismo
                    // request es la medicina exacta, cuesta cero bytes de
                    // stream consumidos, y convierte una muerte fatal del
                    // turno en una ronda recuperada. Acotado y siempre-on
                    // (no depende del env de transporte, que es opt-in para
                    // OTRA clase de fallo): esta clase de 500 es
                    // determinística de diagnóstico pero estocástica de
                    // ocurrencia — el próximo sample casi siempre parsea.
                    if response.status().as_u16() == 500 {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        if body.contains("error parsing tool call")
                            && attempt < 1 + TOOL_PARSE_500_RETRIES
                        {
                            tracing::warn!(
                                attempt,
                                "ollama 500 'error parsing tool call' (sample-level \
                             parse failure); re-sampling the round"
                            );
                            tokio::time::sleep(transport_retry_backoff(attempt)).await;
                            continue;
                        }
                        // Mismo formato que http_error_to_model_error, cuyo
                        // body este branch ya consumió.
                        return Err(ModelError::Request(format!("ollama HTTP {status}: {body}")));
                    }
                    return Err(http_error_to_model_error(response, "ollama").await);
                }
            }
            .await;
            let response = match send_result {
                Ok(response) => response,
                Err(err) => {
                    guard.observe_err(&err);
                    return Err(err);
                }
            };

            let byte_stream = response
                .bytes_stream()
                .map(|chunk| chunk.map(|b| b.to_vec()));
            // `breaker: None` durante el priming — se adjunta abajo solo al
            // stream que efectivamente devolvemos.
            let ctx = StreamCtx {
                byte_stream: Box::pin(byte_stream),
                buf: Vec::new(),
                state: OllamaStreamState::new(self.num_ctx),
                pending: VecDeque::new(),
                finished: false,
                breaker: None,
            };

            // Priming: un paso del stream, para saber si el primer desenlace
            // es un error de tool-parse re-muestreable ANTES de entregarle
            // nada al engine.
            match drive_stream(ctx).await {
                // El stream terminó limpio sin un solo evento: nada que
                // re-muestrear, cuenta como completación OK.
                None => {
                    guard.observe_ok();
                    break 'resample Box::pin(
                        stream::empty::<Result<CompletionEvent, ModelError>>(),
                    );
                }
                Some((item, mut primed)) => {
                    let retriable = matches!(&item, Err(e) if is_tool_parse_stream_error(e))
                        && !primed.state.produced_output
                        && tool_parse_stream_attempts < TOOL_PARSE_500_RETRIES;
                    if retriable {
                        tool_parse_stream_attempts += 1;
                        tracing::warn!(
                            attempt = tool_parse_stream_attempts,
                            "ollama mid-stream 'error parsing tool call' before any output \
                         (incident #17); re-sampling the round"
                        );
                        // `primed` (y su conexión) se descarta; el breaker
                        // nunca se le adjuntó, así que este fallo no cuenta.
                        tokio::time::sleep(transport_retry_backoff(tool_parse_stream_attempts))
                            .await;
                        continue 'resample;
                    }
                    if let Err(ref err) = item {
                        // Error no re-muestreable (tardío, de otra clase, o
                        // reintentos agotados): reportar la caída y devolver
                        // un stream que emite solo ese error.
                        guard.observe_err(err);
                        break 'resample Box::pin(stream::once(async move { item }));
                    }
                    // Primer evento real: ahora sí el breaker viaja con el
                    // stream vivo (reporta el desenlace terminal en
                    // `drive_stream`), replayamos el evento primado y seguimos.
                    primed.breaker = Some(guard);
                    break 'resample Box::pin(
                        stream::once(async move { item })
                            .chain(stream::unfold(primed, drive_stream)),
                    );
                }
            }
        };
        Ok(event_stream)
    }
}

struct StreamCtx {
    byte_stream: Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>,
    buf: Vec<u8>,
    state: OllamaStreamState,
    pending: VecDeque<CompletionEvent>,
    finished: bool,
    /// Circuit-breaker reporting handle: `take()`n exactly once at the
    /// stream's terminal point (clean end → `observe_ok`, error →
    /// `observe_err`). `None` after reporting — or from the start when
    /// the breaker is disabled-by-env, which the `Guard` handles itself.
    breaker: Option<crate::circuit_breaker::Guard>,
}

async fn drive_stream(
    mut ctx: StreamCtx,
) -> Option<(Result<CompletionEvent, ModelError>, StreamCtx)> {
    loop {
        if let Some(event) = ctx.pending.pop_front() {
            return Some((Ok(event), ctx));
        }
        if ctx.finished {
            // Clean termination (done seen, pending drained): this is
            // the point where the call is *known* to have succeeded
            // end-to-end — report it to the circuit breaker. Error
            // terminations already took the guard below, so `take()`
            // is a no-op for them.
            if ctx.state.done
                && let Some(guard) = ctx.breaker.take()
            {
                guard.observe_ok();
            }
            return None;
        }

        match extract_next_ndjson_line(&mut ctx.buf) {
            Some(line) => match parse_ndjson_line(&line) {
                Ok(json) => {
                    tracing::debug!(done = ?json.get("done"), "ollama ndjson line");
                    let events = ctx.state.handle_line(&json);
                    ctx.pending.extend(events);
                    // A line carrying a top-level "error" field must be
                    // surfaced to the caller, not swallowed.
                    if let Some(message) = ctx.state.stream_error.take() {
                        ctx.finished = true;
                        let err = ModelError::StreamError(message);
                        if let Some(guard) = ctx.breaker.take() {
                            guard.observe_err(&err);
                        }
                        return Some((Err(err), ctx));
                    }
                    if ctx.state.done {
                        ctx.finished = true;
                    }
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "ollama stream: invalid JSON in NDJSON line, terminating stream"
                    );
                    ctx.finished = true;
                    let err = ModelError::Decode(format!(
                        "ollama stream: invalid JSON in NDJSON line: {err}"
                    ));
                    if let Some(guard) = ctx.breaker.take() {
                        guard.observe_err(&err);
                    }
                    return Some((Err(err), ctx));
                }
            },
            None => match ctx.byte_stream.next().await {
                Some(Ok(bytes)) => {
                    ctx.buf.extend_from_slice(&bytes);
                }
                Some(Err(err)) => {
                    tracing::error!(error = %err, "ollama stream: transport error, terminating stream");
                    ctx.finished = true;
                    let err = ModelError::StreamError(format!("transport error: {err}"));
                    if let Some(guard) = ctx.breaker.take() {
                        guard.observe_err(&err);
                    }
                    return Some((Err(err), ctx));
                }
                None => {
                    ctx.finished = true;
                    if ctx.state.done {
                        if let Some(guard) = ctx.breaker.take() {
                            guard.observe_ok();
                        }
                        return None;
                    }
                    let err = ModelError::StreamError(
                        "connection closed before a terminal event was received".to_string(),
                    );
                    if let Some(guard) = ctx.breaker.take() {
                        guard.observe_err(&err);
                    }
                    return Some((Err(err), ctx));
                }
            },
        }
    }
}

/// Read timeout for [`list_ollama_models`]'s non-streaming metadata
/// request — `/api/tags` answers from local state in milliseconds on a
/// healthy server, so anything that stays silent this long is down, not
/// slow. Deliberately far below `http_client`'s generation-sized 600s
/// read timeout: this call sits on the TUI's startup path, best-effort.
const LIST_MODELS_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Lists the models installed on an Ollama server (`GET /api/tags`),
/// returning their `name` tags (e.g. `"qwen2.5:3b"`) in server order.
/// Used to populate the TUI's `/model` picker with what's actually
/// available locally — callers should treat failure as "server not
/// reachable" and degrade (e.g. to the configured model name) rather
/// than abort, since a stopped Ollama is a completely normal state for
/// someone running against Anthropic/OpenRouter.
pub async fn list_ollama_models(base_url: &str) -> Result<Vec<String>, ModelError> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let client = crate::http_client::build_client_with_timeouts(
        std::time::Duration::from_secs(10),
        LIST_MODELS_READ_TIMEOUT,
    );

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| ModelError::Request(format!("ollama request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(http_error_to_model_error(response, "ollama").await);
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| ModelError::Decode(format!("ollama /api/tags: invalid JSON: {e}")))?;

    // `{"models": [{"name": "qwen2.5:3b", ...}, ...]}` — entries missing a
    // string `name` are skipped rather than failing the whole listing (the
    // picker would rather show the models it *can* name).
    Ok(body
        .get("models")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("name").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// Looks up the content digest Ollama reports for an installed model
/// (`GET /api/tags`, matching by exact `name`) — e.g.
/// `"357c53fb659c5076de1d65ccb0b397446227b71a42be9d1603d46168015c9e4b"`.
/// Unlike a model tag (which Ollama can silently re-pull to different
/// weights over time, e.g. after `ollama pull qwen2.5:3b` picks up an
/// updated release under the same name), the digest ties a result to the
/// *exact* weights that produced it — `braze-bench`'s run metadata (E6,
/// docs/AUDITORIA-2026-07-v3.md) records this per Ollama model in a
/// sweep. `Ok(None)` when the server is reachable but lists no model by
/// that exact name (not installed, or the name doesn't match); `Err` only
/// on a genuine transport/decode failure — same reachability contract as
/// [`list_ollama_models`].
pub async fn ollama_model_digest(
    base_url: &str,
    model: &str,
) -> Result<Option<String>, ModelError> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let client = crate::http_client::build_client_with_timeouts(
        std::time::Duration::from_secs(10),
        LIST_MODELS_READ_TIMEOUT,
    );

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| ModelError::Request(format!("ollama request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(http_error_to_model_error(response, "ollama").await);
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| ModelError::Decode(format!("ollama /api/tags: invalid JSON: {e}")))?;

    Ok(body
        .get("models")
        .and_then(serde_json::Value::as_array)
        .and_then(|models| {
            models
                .iter()
                .find(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(model))
        })
        .and_then(|entry| entry.get("digest"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// The Ollama server's own version (`GET /api/version`) — the
/// serving-layer identity a sweep's metadata was missing (EMSE blind
/// review b2, Issue 3, 2026-07-19): chat-template rendering — the very
/// layer braze's planner mechanism findings live in — changes across
/// Ollama releases, so a sweep pinned to a harness commit and model
/// digests but not to a server version is not fully reproducible.
/// Same shape as [`ollama_model_digest`]: a plain result the caller
/// treats as best-effort.
pub async fn ollama_server_version(base_url: &str) -> Result<Option<String>, ModelError> {
    let url = format!("{}/api/version", base_url.trim_end_matches('/'));
    let client = crate::http_client::build_client_with_timeouts(
        std::time::Duration::from_secs(10),
        LIST_MODELS_READ_TIMEOUT,
    );

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| ModelError::Request(format!("ollama request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(http_error_to_model_error(response, "ollama").await);
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| ModelError::Decode(format!("ollama /api/version: invalid JSON: {e}")))?;

    Ok(body
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// How long a warm-up load may take before we give up on it. Loading a
/// mid-size model from disk into RAM/VRAM takes seconds to a couple of
/// minutes on slow disks — far more than `LIST_MODELS_READ_TIMEOUT`, but
/// it must not eat a meaningful slice of the sweep either.
const WARM_UP_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Preloads `model` into the Ollama server's memory without generating
/// anything: `POST /api/generate` with a `model` and no `prompt` is
/// Ollama's documented load-only request. J-6 (docs/AUDITORIA-2026-07-v7.md):
/// without this, the first task of each sweep arm paid the model's
/// cold-load time — inflating its wall-time (and risking a spurious
/// `[Timeout]` on CPU) on always the same task, while later arms under
/// `--no-ollama-stop` started warm from the previous arm's resident
/// model. Best-effort by design: a failure here means the first real
/// request pays the load instead, which is exactly the pre-J-6 behavior
/// — so callers log and continue rather than abort.
pub async fn warm_up_ollama_model(base_url: &str, model: &str) -> Result<(), ModelError> {
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let client = crate::http_client::build_client_with_timeouts(
        std::time::Duration::from_secs(10),
        WARM_UP_READ_TIMEOUT,
    );

    let response = client
        .post(url)
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| ModelError::Request(format!("ollama warm-up failed: {e}")))?;

    if !response.status().is_success() {
        return Err(http_error_to_model_error(response, "ollama").await);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::{Message, Role};

    /// EMSE blind b2, Issue 3 (2026-07-19): the serving-layer version
    /// must round-trip from `/api/version` so sweep metadata can pin it.
    #[tokio::test]
    async fn ollama_server_version_parses_the_version_field() {
        let body = br#"{"version":"0.30.7"}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(200, "application/json", body).await;

        let version = ollama_server_version(&format!("http://{addr}"))
            .await
            .expect("version lookup should succeed");
        assert_eq!(version.as_deref(), Some("0.30.7"));
    }

    #[tokio::test]
    async fn ollama_server_version_is_none_when_the_field_is_missing() {
        let body = br#"{}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(200, "application/json", body).await;

        let version = ollama_server_version(&format!("http://{addr}"))
            .await
            .expect("a well-formed but versionless body is not an error");
        assert_eq!(version, None);
    }

    #[tokio::test]
    async fn ollama_server_version_errors_on_an_unreachable_server() {
        assert!(ollama_server_version("http://127.0.0.1:1").await.is_err());
    }

    #[tokio::test]
    async fn list_ollama_models_returns_the_installed_model_names_in_order() {
        let body =
            br#"{"models":[{"name":"qwen2.5:3b","size":1},{"name":"llama3.2:1b","size":2}]}"#
                .to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(200, "application/json", body).await;

        let models = list_ollama_models(&format!("http://{addr}"))
            .await
            .expect("listing should succeed");
        assert_eq!(models, vec!["qwen2.5:3b", "llama3.2:1b"]);
    }

    #[tokio::test]
    async fn list_ollama_models_skips_entries_without_a_string_name() {
        let body = br#"{"models":[{"size":1},{"name":"qwen2.5:3b"},{"name":42}]}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(200, "application/json", body).await;

        let models = list_ollama_models(&format!("http://{addr}"))
            .await
            .expect("listing should succeed");
        assert_eq!(models, vec!["qwen2.5:3b"]);
    }

    #[tokio::test]
    async fn list_ollama_models_maps_an_http_error_instead_of_panicking() {
        let body = br#"{"error":"boom"}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(500, "application/json", body).await;

        let err = list_ollama_models(&format!("http://{addr}"))
            .await
            .expect_err("a 500 must surface as an error");
        // The exact variant is `http_error_to_model_error`'s business —
        // this test only pins that failure is an `Err`, not a panic or
        // an empty Ok.
        let _ = err;
    }

    #[tokio::test]
    async fn list_ollama_models_against_an_unreachable_server_is_a_request_error() {
        // Port 1 is essentially never listening; connect fails fast.
        let err = list_ollama_models("http://127.0.0.1:1")
            .await
            .expect_err("connection refused must surface as an error");
        assert!(matches!(err, ModelError::Request(_)));
    }

    // --- ollama_model_digest (E6, docs/AUDITORIA-2026-07-v3.md) ---

    #[tokio::test]
    async fn ollama_model_digest_returns_the_matching_entrys_digest() {
        let body = br#"{"models":[
            {"name":"qwen2.5:3b","digest":"abc123"},
            {"name":"qwen3.5-coder:latest","digest":"def456"}
        ]}"#
        .to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(200, "application/json", body).await;

        let digest = ollama_model_digest(&format!("http://{addr}"), "qwen3.5-coder:latest")
            .await
            .expect("lookup should succeed");
        assert_eq!(digest.as_deref(), Some("def456"));
    }

    #[tokio::test]
    async fn ollama_model_digest_is_none_when_no_entry_matches_the_name() {
        let body = br#"{"models":[{"name":"qwen2.5:3b","digest":"abc123"}]}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(200, "application/json", body).await;

        let digest = ollama_model_digest(&format!("http://{addr}"), "not-installed:latest")
            .await
            .expect("lookup should succeed even with no match");
        assert_eq!(digest, None);
    }

    #[tokio::test]
    async fn ollama_model_digest_maps_an_http_error_instead_of_panicking() {
        let body = br#"{"error":"boom"}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(500, "application/json", body).await;

        let err = ollama_model_digest(&format!("http://{addr}"), "qwen2.5:3b")
            .await
            .expect_err("a 500 must surface as an error");
        let _ = err;
    }

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message::text(Role::User, "hi")],
            tool_stubs: vec![],
            system_prompt: "be terse".to_string(),
            max_tokens: 100,
        }
    }

    #[tokio::test]
    async fn complete_streams_text_deltas_from_canned_ndjson_response() {
        let ndjson_body = concat!(
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"Hi\"},\"done\":false}\n",
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\" there\"},\"done\":false}\n",
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":7,\"eval_count\":2}\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "application/x-ndjson",
            ndjson_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let mut stream = backend
            .complete(sample_request())
            .await
            .expect("request should succeed");

        let mut text = String::new();
        let mut saw_usage = false;
        let mut saw_done = false;
        while let Some(event) = stream.next().await {
            match event.expect("no stream error expected") {
                CompletionEvent::TextDelta(t) => text.push_str(&t),
                CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    assert_eq!(input_tokens, 7);
                    assert_eq!(output_tokens, 2);
                    saw_usage = true;
                }
                CompletionEvent::Done => saw_done = true,
                CompletionEvent::ToolCallRequested { .. } => panic!("unexpected tool call"),
            }
        }

        assert_eq!(text, "Hi there");
        assert!(saw_usage);
        assert!(saw_done);
    }

    #[tokio::test]
    async fn complete_emits_tool_call_from_canned_response() {
        let ndjson_body = concat!(
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":",
            "[{\"function\":{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Santiago\"}}}]},",
            "\"done\":true,\"prompt_eval_count\":9,\"eval_count\":3}\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "application/x-ndjson",
            ndjson_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        let tool_calls: Vec<_> = events
            .iter()
            .filter_map(|e| match e.as_ref().expect("no stream error expected") {
                CompletionEvent::ToolCallRequested {
                    name, arguments, ..
                } => Some((name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].0, "get_weather");
        assert_eq!(tool_calls[0].1, serde_json::json!({"city": "Santiago"}));
    }

    /// Regression test for A3/B4: a line carrying a top-level `"error"`
    /// field (Ollama's shape for a failed generation, often without
    /// `"done": true`) must end the stream with an explicit `Err`, never
    /// silently — otherwise partial text delivered before the failure
    /// would be persisted as if it were a complete response.
    #[tokio::test]
    async fn a_mid_stream_error_line_ends_the_stream_with_an_error_not_silently() {
        let ndjson_body = concat!(
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"Voy a\"},\"done\":false}\n",
            "{\"error\":\"model runner has crashed\"}\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "application/x-ndjson",
            ndjson_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(CompletionEvent::Done)))
        );
        let last = events.last().expect("expected at least the error item");
        assert!(
            matches!(last, Err(ModelError::StreamError(_))),
            "expected the stream to end with a StreamError, got {last:?}"
        );
    }

    /// Incidente roam #17 (2026-07-20): un "error parsing tool call" a
    /// mitad de un stream 200, ANTES de emitir nada, se re-muestrea —
    /// misma clase que el 500 de send de #1, pero por el camino
    /// mid-stream que aquél no cubría. El primer response falla; el
    /// segundo (el re-sample) produce texto limpio que debe llegar.
    #[tokio::test]
    async fn a_mid_stream_tool_parse_error_before_output_is_resampled() {
        let bad = "{\"error\":\"error parsing tool call: raw='{\\\"path\\\":\\\"a.rs\\\"}', err=invalid character ',' after top-level value\"}\n";
        let good = concat!(
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"recovered\"},\"done\":false}\n",
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true}\n",
        );
        let addr = crate::test_support::spawn_sequenced_http_server(vec![
            (200, "application/x-ndjson", bad.as_bytes().to_vec()),
            (200, "application/x-ndjson", good.as_bytes().to_vec()),
        ])
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                Ok(CompletionEvent::TextDelta(t)) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text, "recovered",
            "the resampled round's output must reach the caller, got {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Err(ModelError::StreamError(_)))),
            "the early tool-parse error must have been resampled away, got {events:?}"
        );
    }

    /// Contrapeso de #17: si el tool-parse error llega DESPUÉS de haber
    /// emitido texto (`produced_output`), re-muestrear descartaría salida
    /// ya entregada — así que NO se re-muestrea y el error se surfacea,
    /// como antes. Un solo response en el server: un re-sample indebido
    /// se quedaría sin segunda respuesta.
    #[tokio::test]
    async fn a_tool_parse_error_after_output_is_not_resampled() {
        let body = concat!(
            "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"partial\"},\"done\":false}\n",
            "{\"error\":\"error parsing tool call: raw='...', err=invalid character ','\"}\n",
        );
        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "application/x-ndjson",
            body.as_bytes().to_vec(),
        )
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                Ok(CompletionEvent::TextDelta(t)) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text, "partial",
            "delivered output must survive, got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Err(ModelError::StreamError(_)))),
            "a tool-parse error after output must still surface, got {events:?}"
        );
    }

    /// Contrapeso de #17: un error mid-stream temprano que NO es de
    /// tool-parse (p.ej. "model runner has crashed") NO se re-muestrea —
    /// el guard es específico de la clase de #1/#17.
    #[tokio::test]
    async fn an_early_non_tool_parse_stream_error_is_not_resampled() {
        let body = "{\"error\":\"model runner has crashed\"}\n";
        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "application/x-ndjson",
            body.as_bytes().to_vec(),
        )
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        let last = events.last().expect("expected at least the error item");
        assert!(
            matches!(last, Err(ModelError::StreamError(msg)) if msg.contains("model runner has crashed")),
            "a non-tool-parse early error must surface unchanged (no resample), got {last:?}"
        );
    }

    /// Regression test for A3/B4: the connection closing before any line
    /// with `"done": true` ever arrived must also be a stream error, not a
    /// silent, clean end treated as a successful completion.
    #[tokio::test]
    async fn connection_closed_before_done_is_a_stream_error() {
        let ndjson_body = "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"Voy a leer\"},\"done\":false}\n";

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "application/x-ndjson",
            ndjson_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(CompletionEvent::Done)))
        );
        let last = events.last().expect("expected at least the error item");
        assert!(
            matches!(last, Err(ModelError::StreamError(_))),
            "expected the stream to end with a StreamError, got {last:?}"
        );
    }

    #[tokio::test]
    async fn complete_maps_429_to_rate_limited() {
        let body = br#"{"error":"too many requests"}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(429, "application/json", body).await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let err = match backend.complete(sample_request()).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ModelError::RateLimited(_)),
            "expected RateLimited, got {err:?}"
        );
    }

    #[tokio::test]
    async fn complete_maps_500_to_request_error() {
        let body = br#"{"error":"internal error"}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(500, "application/json", body).await;

        let backend = OllamaBackend::with_base_url("llama3".to_string(), format!("http://{addr}"));
        let err = match backend.complete(sample_request()).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ModelError::Request(_)),
            "expected Request, got {err:?}"
        );
    }
}

#[cfg(test)]
mod transport_retry_tests {
    use super::*;

    #[test]
    fn backoff_escalates_then_plateaus() {
        assert_eq!(transport_retry_backoff(1).as_secs(), 1);
        assert_eq!(transport_retry_backoff(2).as_secs(), 4);
        assert_eq!(transport_retry_backoff(3).as_secs(), 15);
        assert_eq!(transport_retry_backoff(9).as_secs(), 15);
    }

    #[test]
    fn env_absent_or_garbage_means_off_and_values_are_capped() {
        // Sin tocar el env global del proceso de tests: se valida la
        // cadena parse/cap con la misma lógica sobre valores simulados.
        let parse = |v: Option<&str>| -> u32 {
            v.and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0)
                .min(10)
        };
        assert_eq!(parse(None), 0);
        assert_eq!(parse(Some("")), 0);
        assert_eq!(parse(Some("abc")), 0);
        assert_eq!(parse(Some("4")), 4);
        assert_eq!(parse(Some("99")), 10);
    }
}
