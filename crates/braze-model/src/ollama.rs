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

        // Retry opt-in de TRANSPORTE (pendiente de infra Nitro en CLAUDE.md
        // § "Próximos pasos", materializado tras el incidente del ancla BFCL
        // 2026-07-18: dos sweeps contaminados por ráfagas de "error sending
        // request" con la LAN degradada a RTT ~100ms). Solo reintenta el
        // fallo del `send()` — la fase donde CERO bytes del stream se han
        // consumido, así que reintentar es semánticamente inocuo. Un HTTP
        // de error o un corte a mitad de stream NUNCA se reintenta acá.
        // Off por default: `BRAZE_OLLAMA_TRANSPORT_RETRIES` (0 = off).
        let max_attempts = 1 + transport_retries_from_env();
        let mut attempt = 0u32;
        let response = loop {
            attempt += 1;
            let sent = self
                .client
                .post(url.clone())
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await;
            match sent {
                Ok(r) => break r,
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
                }
                Err(e) => {
                    return Err(ModelError::Request(format!("ollama request failed: {e}")));
                }
            }
        };

        if !response.status().is_success() {
            return Err(http_error_to_model_error(response, "ollama").await);
        }

        let byte_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map(|b| b.to_vec()));
        let ctx = StreamCtx {
            byte_stream: Box::pin(byte_stream),
            buf: Vec::new(),
            state: OllamaStreamState::new(self.num_ctx),
            pending: VecDeque::new(),
            finished: false,
        };

        let event_stream = stream::unfold(ctx, drive_stream);
        Ok(Box::pin(event_stream))
    }
}

struct StreamCtx {
    byte_stream: Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>,
    buf: Vec<u8>,
    state: OllamaStreamState,
    pending: VecDeque<CompletionEvent>,
    finished: bool,
}

async fn drive_stream(
    mut ctx: StreamCtx,
) -> Option<(Result<CompletionEvent, ModelError>, StreamCtx)> {
    loop {
        if let Some(event) = ctx.pending.pop_front() {
            return Some((Ok(event), ctx));
        }
        if ctx.finished {
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
                        return Some((Err(ModelError::StreamError(message)), ctx));
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
                    return Some((
                        Err(ModelError::Decode(format!(
                            "ollama stream: invalid JSON in NDJSON line: {err}"
                        ))),
                        ctx,
                    ));
                }
            },
            None => match ctx.byte_stream.next().await {
                Some(Ok(bytes)) => {
                    ctx.buf.extend_from_slice(&bytes);
                }
                Some(Err(err)) => {
                    tracing::error!(error = %err, "ollama stream: transport error, terminating stream");
                    ctx.finished = true;
                    return Some((
                        Err(ModelError::StreamError(format!("transport error: {err}"))),
                        ctx,
                    ));
                }
                None => {
                    ctx.finished = true;
                    if ctx.state.done {
                        return None;
                    }
                    return Some((
                        Err(ModelError::StreamError(
                            "connection closed before a terminal event was received".to_string(),
                        )),
                        ctx,
                    ));
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
