//! [`AnthropicBackend`] — [`ModelBackend`] implementation against the real
//! Anthropic Messages API (`https://api.anthropic.com/v1/messages`),
//! streaming via SSE (`"stream": true`).

use std::collections::VecDeque;
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

use crate::anthropic_wire::{
    ANTHROPIC_API_URL, ANTHROPIC_VERSION, AnthropicStreamState, build_request,
    extract_next_sse_data,
};
use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;

/// Streams completions from the real Anthropic API.
///
/// Holds the API key and model name as state (per the task: `CompletionRequest`
/// has no `model` field — each concrete backend owns its own).
pub struct AnthropicBackend {
    api_key: String,
    model: String,
    client: reqwest::Client,
    /// Always `ANTHROPIC_API_URL` in production; overridable only via the
    /// test-only constructor below so tests can point at a local fake
    /// server without touching the wire format.
    base_url: String,
    /// `None` (the default) leaves Anthropic's own provider default
    /// (~1.0) in effect. See [`AnthropicBackend::with_temperature`].
    temperature: Option<f32>,
    /// H-19: retries for the initial request on transient 429/5xx/send
    /// failures. Defaults to [`crate::retry::DEFAULT_MAX_RETRIES`]; see
    /// [`AnthropicBackend::with_max_retries`].
    max_retries: u32,
    /// See [`AnthropicBackend::with_prompt_caching_enabled`]. Default
    /// `true` — same posture as `OpenRouterBackend`.
    prompt_caching_enabled: bool,
}

impl AnthropicBackend {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: crate::http_client::build_client(),
            base_url: ANTHROPIC_API_URL.to_string(),
            temperature: None,
            max_retries: crate::retry::DEFAULT_MAX_RETRIES,
            prompt_caching_enabled: true,
        }
    }

    /// Test-only hook: point at a local fake server instead of the real
    /// Anthropic endpoint.
    #[cfg(test)]
    fn with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            client: crate::http_client::build_client(),
            base_url,
            temperature: None,
            max_retries: crate::retry::DEFAULT_MAX_RETRIES,
            prompt_caching_enabled: true,
        }
    }

    /// Enables/disables the `cache_control` breakpoints on the request
    /// (`anthropic_wire::apply_cache_breakpoints` — v8 § 5): the direct
    /// API twin of [`crate::OpenRouterBackend::with_prompt_caching_enabled`],
    /// honoring `Config::enable_prompt_caching` and the bench's
    /// `+ablate:no-caching` row. Default `true`. Chainable.
    pub fn with_prompt_caching_enabled(mut self, enabled: bool) -> Self {
        self.prompt_caching_enabled = enabled;
        self
    }

    /// Overrides the sampling temperature sent to Anthropic — e.g. so
    /// `braze-bench` can give every backend in a sweep the same value
    /// instead of comparing Anthropic at its provider default against
    /// Ollama pinned to a fixed low temperature (N-34,
    /// docs/AUDITORIA-2026-07-v2.md). Chainable, mirrors
    /// `OllamaBackend::with_temperature`.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Overrides the H-19 retry count for the initial request — `0`
    /// restores the old single-attempt behavior (used by the HTTP
    /// mapping tests, whose canned server scripts exactly one response).
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }
}

#[async_trait]
impl ModelBackend for AnthropicBackend {
    fn name(&self) -> &str {
        "anthropic"
    }

    #[tracing::instrument(
        skip(self, req),
        fields(provider = "anthropic", model = %self.model, message_count = req.messages.len())
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
            self.prompt_caching_enabled,
        );
        tracing::info!(
            tool_count = body.tools.len(),
            "starting anthropic completion turn"
        );

        // H-19: transient 429/5xx/send blips on the initial request are
        // retried with backoff inside the wire — the engine never sees
        // them unless they persist past the retries. Wrapped in a
        // circuit breaker keyed by destination+model (2026-07-17,
        // recalibrated per AUDITORIA-2026-07-v8 K-1): once enough
        // consecutive calls to this same destination have failed at the
        // transport level, later calls fail fast without a network
        // round-trip at all, instead of every one separately paying the
        // retry cost to rediscover a sustained outage. Deterministic
        // 4xx don't count (`circuit_breaker::classify`), and success is
        // only reported once the stream terminates cleanly — the guard
        // travels into `StreamCtx` for that.
        let breaker_key = format!(
            "anthropic:{}:{}",
            self.base_url.trim_end_matches('/'),
            self.model
        );
        let guard = crate::circuit_breaker::acquire(&breaker_key)?;
        let send_result = crate::retry::send_with_retry("anthropic", self.max_retries, || {
            self.client
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
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

        // Convert to `Vec<u8>` at the boundary rather than naming
        // `bytes::Bytes` in a type signature — `bytes` is only a transitive
        // dependency here (pulled in by reqwest), not a direct one.
        let byte_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map(|b| b.to_vec()));
        let ctx = StreamCtx {
            byte_stream: Box::pin(byte_stream),
            buf: Vec::new(),
            state: AnthropicStreamState::new(),
            pending: VecDeque::new(),
            finished: false,
            breaker: Some(guard),
        };

        let event_stream = stream::unfold(ctx, drive_stream);
        Ok(Box::pin(event_stream))
    }
}

struct StreamCtx {
    byte_stream: Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>,
    buf: Vec<u8>,
    state: AnthropicStreamState,
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
            // Clean termination (message_stop seen, pending drained):
            // report end-to-end success to the circuit breaker. Error
            // terminations already took the guard below.
            if ctx.state.done
                && let Some(guard) = ctx.breaker.take()
            {
                guard.observe_ok();
            }
            return None;
        }

        match extract_next_sse_data(&mut ctx.buf) {
            Some(data) => match serde_json::from_str::<serde_json::Value>(&data) {
                Ok(json) => {
                    tracing::debug!(event_type = ?json.get("type"), "anthropic sse event");
                    let events = ctx.state.handle_event(&json);
                    ctx.pending.extend(events);
                    // A mid-stream provider error (e.g. overloaded_error)
                    // must be surfaced to the caller, not swallowed —
                    // yield it now instead of whatever (empty) events
                    // `handle_event` produced for it.
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
                        raw = %data,
                        "anthropic stream: invalid JSON in SSE data, terminating stream"
                    );
                    ctx.finished = true;
                    let err = ModelError::Decode(format!(
                        "anthropic stream: invalid JSON in SSE data: {err}"
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
                    tracing::error!(error = %err, "anthropic stream: transport error, terminating stream");
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
                        // Connection closed cleanly right after a proper
                        // message_stop — nothing more to yield.
                        if let Some(guard) = ctx.breaker.take() {
                            guard.observe_ok();
                        }
                        return None;
                    }
                    // Connection closed before a terminal event ever
                    // arrived: an incomplete stream must not be treated
                    // as a successful completion (see
                    // `ModelError::StreamError`'s doc comment).
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
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" there\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
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
        // The SSE body itself is written in one shot by the fake server, but
        // this test exercises the same fragment-reassembly path exercised in
        // anthropic_wire's unit tests, end-to-end through `complete()`.
        let sse_body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"loc\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ation\\\": \\\"Santiago\\\"}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":8}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
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
        assert_eq!(id, "toolu_1");
        assert_eq!(name, "get_weather");
        assert_eq!(arguments, &serde_json::json!({"location": "Santiago"}));
    }

    /// Regression test for A3/B4: a mid-stream provider error (e.g.
    /// Anthropic's `overloaded_error`) must end the stream with an
    /// explicit `Err`, never a fabricated `Done` — otherwise a caller like
    /// `Engine::run_turn` would persist whatever partial text arrived
    /// ("Voy a") as if it were the model's complete, converged response.
    #[tokio::test]
    async fn a_mid_stream_error_event_ends_the_stream_with_an_error_not_a_fake_done() {
        let sse_body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Voy a\"}}\n\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
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

    /// Regression test for A3/B4: the connection closing before any
    /// terminal event (`message_stop` or `error`) ever arrived — e.g. the
    /// server process dying mid-response — must also be a stream error,
    /// not a silent, clean end treated as a successful completion.
    #[tokio::test]
    async fn connection_closed_before_a_terminal_event_is_a_stream_error() {
        let sse_body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Voy a leer el archi\"}}\n\n",
            // No content_block_stop/message_delta/message_stop — the
            // connection simply ends here.
        );

        let addr = crate::test_support::spawn_canned_http_server(
            200,
            "text/event-stream",
            sse_body.as_bytes().to_vec(),
        )
        .await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
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
        let body = br#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#
            .to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(429, "application/json", body).await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
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
        let body = br#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#.to_vec();
        let addr =
            crate::test_support::spawn_canned_http_server(500, "application/json", body).await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
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
