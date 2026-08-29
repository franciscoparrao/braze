//! [`OpenRouterBackend`] — [`ModelBackend`] implementation against
//! OpenRouter's OpenAI-compatible chat completions API
//! (`https://openrouter.ai/api/v1/chat/completions`), streaming via SSE.

use std::collections::VecDeque;
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

use crate::anthropic_wire::extract_next_sse_data;
use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;
use crate::openrouter_wire::{OPENROUTER_DEFAULT_BASE_URL, OpenRouterStreamState, build_request};

/// The literal sentinel OpenAI-compatible streaming APIs send as the final
/// SSE payload instead of a JSON object — the only reliable close signal
/// for this wire format (see `OpenRouterStreamState`'s doc comment).
const DONE_SENTINEL: &str = "[DONE]";

/// Streams completions from OpenRouter.
///
/// Holds the API key, model name (e.g. `"anthropic/claude-3.5-sonnet"`,
/// `"meta-llama/llama-3.1-70b-instruct"`), and base URL as state (per the
/// task: `CompletionRequest` has no `model` field — each concrete backend
/// owns its own).
pub struct OpenRouterBackend {
    api_key: String,
    model: String,
    client: reqwest::Client,
    base_url: String,
    /// `None` omits the field, leaving whichever underlying model
    /// OpenRouter routes to at its own default. See
    /// [`OpenRouterBackend::with_temperature`].
    temperature: Option<f32>,
    /// `None` omits the field. See [`OpenRouterBackend::with_seed`].
    seed: Option<u64>,
    /// See [`OpenRouterBackend::with_prompt_caching_enabled`]. Default
    /// `true` — only has an effect for models
    /// `openrouter_wire::model_supports_explicit_caching` recognizes
    /// (Anthropic/Qwen), so leaving it on doesn't change the request sent
    /// for any other provider.
    prompt_caching_enabled: bool,
    /// H-19: retries for the initial request on transient 429/5xx/send
    /// failures. Defaults to [`crate::retry::DEFAULT_MAX_RETRIES`]; see
    /// [`OpenRouterBackend::with_max_retries`].
    max_retries: u32,
    /// Etiqueta del proveedor en errores, trazas y clave del circuit
    /// breaker. `"openrouter"` por default; los gateways que reusan este
    /// backend con otra `base_url` (OpenCode Zen) la cambian con
    /// [`OpenRouterBackend::with_provider_label`].
    ///
    /// Existe porque sin ella un fallo contra Zen se reportaba como
    /// `openrouter HTTP 400`, que manda a diagnosticar el proveedor
    /// equivocado — encontrado en vivo el 2026-08-29.
    provider_label: &'static str,
}

impl OpenRouterBackend {
    /// Targets the real OpenRouter API.
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: crate::http_client::build_client(),
            base_url: OPENROUTER_DEFAULT_BASE_URL.to_string(),
            temperature: None,
            seed: None,
            prompt_caching_enabled: true,
            max_retries: crate::retry::DEFAULT_MAX_RETRIES,
            provider_label: "openrouter",
        }
    }

    /// Targets a custom base URL — a self-hosted OpenAI-compatible gateway,
    /// a corporate mirror, or (in tests) a local fake server.
    pub fn with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            client: crate::http_client::build_client(),
            base_url,
            temperature: None,
            seed: None,
            prompt_caching_enabled: true,
            max_retries: crate::retry::DEFAULT_MAX_RETRIES,
            provider_label: "openrouter",
        }
    }

    /// Overrides the sampling temperature sent to OpenRouter — e.g. so
    /// `braze-bench` can give every backend in a sweep the same value
    /// (N-34, docs/AUDITORIA-2026-07-v2.md). Chainable, mirrors
    /// `OllamaBackend::with_temperature`.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the standard OpenAI-compatible `seed` field — best-effort
    /// reproducibility that depends on which underlying provider
    /// OpenRouter routes the request to, but still worth setting when
    /// comparing backends run-for-run. Chainable, mirrors
    /// `OllamaBackend::with_seed`.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Overrides the H-19 retry count for the initial request — `0`
    /// restores the old single-attempt behavior (used by the HTTP
    /// mapping tests, whose canned server scripts exactly one response).
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Overrides whether requests carry explicit `cache_control` markers
    /// for models that need one (Anthropic/Qwen —
    /// `openrouter_wire::model_supports_explicit_caching`). Default
    /// `true`; see `Config.enable_prompt_caching`
    /// (`BRAZE_ENABLE_PROMPT_CACHING`) for the config-file/env-var
    /// surface. Chainable, same shape as `with_temperature`.
    pub fn with_prompt_caching_enabled(mut self, enabled: bool) -> Self {
        self.prompt_caching_enabled = enabled;
        self
    }

    /// Cambia la etiqueta de proveedor que aparece en errores, trazas y
    /// la clave del circuit breaker — ver el campo `provider_label`.
    /// La usa el provider `zen` del bench y del CLI. Chainable.
    pub fn with_provider_label(mut self, label: &'static str) -> Self {
        self.provider_label = label;
        self
    }
}

#[async_trait]
impl ModelBackend for OpenRouterBackend {
    fn name(&self) -> &str {
        self.provider_label
    }

    #[tracing::instrument(
        skip(self, req),
        fields(provider = %self.provider_label, model = %self.model, message_count = req.messages.len())
    )]
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let body = build_request(
            &req,
            &self.model,
            self.temperature,
            self.seed,
            self.prompt_caching_enabled,
        );
        tracing::info!(
            tool_count = body.tools.len(),
            "starting openrouter completion turn"
        );

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        // H-19: transient 429/5xx/send blips on the initial request are
        // retried with backoff inside the wire — the engine never sees
        // them unless they persist past the retries. Wrapped in a
        // circuit breaker keyed by destination+model (2026-07-17,
        // recalibrated per AUDITORIA-2026-07-v8 K-1) — see
        // `AnthropicBackend::complete`'s identical comment.
        let breaker_key = format!("{}:{url}:{}", self.provider_label, self.model);
        let guard = crate::circuit_breaker::acquire(&breaker_key)?;
        let send_result = crate::retry::send_with_retry(self.provider_label, self.max_retries, || {
            self.client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("content-type", "application/json")
                .json(&body)
        })
        .await;
        let response = match send_result {
            Ok(response) => response,
            Err(err) => {
                guard.observe_err(&err);
                return Err(err);
            }
        };
        log_rate_limit_headers(self.provider_label, response.headers());

        let byte_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map(|b| b.to_vec()));
        let ctx = StreamCtx {
            byte_stream: Box::pin(byte_stream),
            buf: Vec::new(),
            state: OpenRouterStreamState::new(),
            pending: VecDeque::new(),
            finished: false,
            breaker: Some(guard),
        };

        let event_stream = stream::unfold(ctx, drive_stream);
        Ok(Box::pin(event_stream))
    }
}

/// Cabeceras de rate limit que este wire sabe leer, en el orden en que
/// se registran. Cubre las tres convenciones vivas: la de OpenAI/
/// OpenRouter (`x-ratelimit-*`), la de Anthropic
/// (`anthropic-ratelimit-*`) y el `retry-after` de HTTP.
const RATE_LIMIT_HEADERS: &[&str] = &[
    "retry-after",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-limit-requests",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-reset-requests",
    "x-ratelimit-limit-tokens",
    "x-ratelimit-remaining-tokens",
    "x-ratelimit-reset-tokens",
    "anthropic-ratelimit-requests-remaining",
    "anthropic-ratelimit-tokens-remaining",
];

/// Traza a `info` las cabeceras de rate limit que el proveedor haya
/// devuelto, para poder medir sus límites sin documentación.
///
/// Silencioso cuando no viene ninguna, que es el caso **medido** de
/// OpenCode Zen al 2026-08-29: sus respuestas (200, 429 y 503) traen
/// solo `date`, `content-type`, `content-length`, `server` y las de
/// Cloudflare. La ausencia de log es entonces el dato: los límites de
/// sus modelos gratuitos hay que medirlos contando llamadas hasta el
/// 429, no leyéndolos de una cabecera.
fn log_rate_limit_headers(provider: &str, headers: &reqwest::header::HeaderMap) {
    let present: Vec<String> = RATE_LIMIT_HEADERS
        .iter()
        .filter_map(|name| {
            headers
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .map(|v| format!("{name}={v}"))
        })
        .collect();
    if !present.is_empty() {
        tracing::info!(provider, headers = %present.join(" "), "rate limit headers");
    }
}

struct StreamCtx {
    byte_stream: Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>,
    buf: Vec<u8>,
    state: OpenRouterStreamState,
    pending: VecDeque<CompletionEvent>,
    finished: bool,
    /// Circuit-breaker reporting handle: `take()`n exactly once at the
    /// stream's terminal point (clean end → `observe_ok`, error →
    /// `observe_err`). See `circuit_breaker.rs`'s module docs.
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
            // Clean termination ([DONE] seen, pending drained): report
            // end-to-end success to the circuit breaker. Error
            // terminations already took the guard below.
            if ctx.state.done
                && let Some(guard) = ctx.breaker.take()
            {
                guard.observe_ok();
            }
            return None;
        }

        match extract_next_sse_data(&mut ctx.buf) {
            Some(data) => {
                // The stream ends with a literal "[DONE]" payload, not a
                // JSON object — intercept it before attempting to parse.
                if data.trim() == DONE_SENTINEL {
                    let events = ctx.state.handle_done_sentinel();
                    ctx.pending.extend(events);
                    ctx.finished = true;
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(&data) {
                    Ok(json) => {
                        tracing::debug!("openrouter sse chunk");
                        let events = ctx.state.handle_chunk(&json);
                        ctx.pending.extend(events);
                        // A mid-stream provider error must be surfaced to
                        // the caller, not swallowed — yield it now instead
                        // of waiting for a [DONE] that may never arrive.
                        if let Some(message) = ctx.state.stream_error.take() {
                            ctx.finished = true;
                            let err = ModelError::StreamError(message);
                            if let Some(guard) = ctx.breaker.take() {
                                guard.observe_err(&err);
                            }
                            return Some((Err(err), ctx));
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            raw = %data,
                            "openrouter stream: invalid JSON in SSE data, terminating stream"
                        );
                        ctx.finished = true;
                        let err = ModelError::Decode(format!(
                            "openrouter stream: invalid JSON in SSE data: {err}"
                        ));
                        if let Some(guard) = ctx.breaker.take() {
                            guard.observe_err(&err);
                        }
                        return Some((Err(err), ctx));
                    }
                }
            }
            None => match ctx.byte_stream.next().await {
                Some(Ok(bytes)) => {
                    ctx.buf.extend_from_slice(&bytes);
                }
                Some(Err(err)) => {
                    tracing::error!(error = %err, "openrouter stream: transport error, terminating stream");
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
                        // Connection closed cleanly right after [DONE] —
                        // nothing more to yield.
                        if let Some(guard) = ctx.breaker.take() {
                            guard.observe_ok();
                        }
                        return None;
                    }
                    // Connection closed before the [DONE] sentinel ever
                    // arrived (even if we already saw a finish_reason): an
                    // incomplete stream must not be treated as a
                    // successful completion.
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

#[cfg(test)]
mod tests {
    /// El label por default no cambia: los tests y el uso existentes de
    /// OpenRouter siguen viendo "openrouter".
    #[test]
    fn provider_label_defaults_to_openrouter() {
        let b = super::OpenRouterBackend::new("k".into(), "m".into());
        assert_eq!(crate::backend::ModelBackend::name(&b), "openrouter");
        let b = super::OpenRouterBackend::with_base_url("k".into(), "m".into(), "u".into());
        assert_eq!(crate::backend::ModelBackend::name(&b), "openrouter");
    }

    /// El gateway que reusa este backend se identifica como tal — sin
    /// esto, un fallo contra Zen se reportaba como `openrouter HTTP 400`
    /// y mandaba a diagnosticar el proveedor equivocado (encontrado en
    /// vivo el 2026-08-29).
    #[test]
    fn provider_label_is_overridable() {
        let b = super::OpenRouterBackend::with_base_url("k".into(), "m".into(), "u".into())
            .with_provider_label("zen");
        assert_eq!(crate::backend::ModelBackend::name(&b), "zen");
    }

    /// Solo se registran las cabeceras presentes, y ninguna se inventa.
    /// El caso vacío importa: es el de Zen, cuyas respuestas no traen
    /// ninguna cabecera de rate limit.
    #[test]
    fn rate_limit_headers_are_collected_only_when_present() {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut h = HeaderMap::new();
        // Sin cabeceras: no hay nada que registrar y no debe romper.
        super::log_rate_limit_headers("zen", &h);
        h.insert(
            HeaderName::from_static("x-ratelimit-remaining"),
            HeaderValue::from_static("7"),
        );
        h.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        let present: Vec<&str> = super::RATE_LIMIT_HEADERS
            .iter()
            .filter(|n| h.contains_key(**n))
            .copied()
            .collect();
        assert_eq!(present, vec!["x-ratelimit-remaining"]);
        super::log_rate_limit_headers("zen", &h);
    }

    use super::*;
    use braze_types::{Message, Role};

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message::text(Role::User, "hi")],
            tool_stubs: vec![],
            system_prompt: "be terse".to_string(),
            max_tokens: 100,
        }
    }

    #[tokio::test]
    async fn complete_streams_text_deltas_from_canned_sse_response() {
        let sse_body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" there\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        );

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
                    assert_eq!(input_tokens, 10);
                    assert_eq!(output_tokens, 3);
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
    async fn complete_reassembles_tool_call_split_across_multiple_chunks() {
        let sse_body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"loc\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ation\\\": \\\"Santiago\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":8}}\n\n",
            "data: [DONE]\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        );

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
                    id,
                    name,
                    arguments,
                } => Some((id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();

        assert_eq!(tool_calls.len(), 1);
        let (id, name, arguments) = &tool_calls[0];
        assert_eq!(id, "call_1");
        assert_eq!(name, "get_weather");
        assert_eq!(arguments, &serde_json::json!({"location": "Santiago"}));
    }

    #[tokio::test]
    async fn complete_falls_back_to_zero_usage_when_no_usage_chunk_precedes_done() {
        let sse_body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        );

        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        let usage = events
            .iter()
            .find_map(|e| match e.as_ref().unwrap() {
                CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => Some((*input_tokens, *output_tokens)),
                _ => None,
            })
            .expect("expected a Usage event");
        assert_eq!(usage, (0, 0));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(CompletionEvent::Done)))
        );
    }

    /// Mirrors `anthropic.rs`/`ollama.rs`'s equivalent regression test: a
    /// mid-stream provider error must end the stream with an explicit
    /// `Err`, never a fabricated `Done`.
    #[tokio::test]
    async fn a_mid_stream_error_chunk_ends_the_stream_with_an_error_not_a_fake_done() {
        let sse_body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Voy a\"},\"finish_reason\":null}]}\n\n",
            "data: {\"error\":{\"message\":\"insufficient credits\",\"type\":\"insufficient_quota\"}}\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        );

        let events: Vec<_> = backend
            .complete(sample_request())
            .await
            .expect("request should succeed")
            .collect()
            .await;

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(CompletionEvent::Done))),
            "must never see a Done after a mid-stream provider error"
        );
        let last = events.last().expect("expected at least the error item");
        assert!(
            matches!(last, Err(ModelError::StreamError(_))),
            "expected the stream to end with a StreamError, got {last:?}"
        );
    }

    #[tokio::test]
    async fn connection_closed_before_done_is_a_stream_error() {
        // No finish_reason/usage/[DONE] — the connection simply ends here.
        let sse_body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Voy a leer\"},\"finish_reason\":null}]}\n\n";

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        );

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
        let body = br#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(429, "application/json", body).await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        )
        // H-19: single attempt — this test asserts the ERROR MAPPING of a
        // terminal status, not the retry behavior (covered in retry.rs).
        .with_max_retries(0);

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
        let body = br#"{"error":{"message":"boom","type":"api_error"}}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(500, "application/json", body).await;

        let backend = OpenRouterBackend::with_base_url(
            "test-key".to_string(),
            "openai/gpt-4o-mini".to_string(),
            format!("http://{addr}"),
        )
        // H-19: single attempt — this test asserts the ERROR MAPPING of a
        // terminal status, not the retry behavior (covered in retry.rs).
        .with_max_retries(0);

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
