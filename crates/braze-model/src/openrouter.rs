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
use crate::http_error::http_error_to_model_error;
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
}

#[async_trait]
impl ModelBackend for OpenRouterBackend {
    fn name(&self) -> &str {
        "openrouter"
    }

    #[tracing::instrument(
        skip(self, req),
        fields(provider = "openrouter", model = %self.model, message_count = req.messages.len())
    )]
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let body = build_request(&req, &self.model, self.temperature, self.seed);
        tracing::info!(
            tool_count = body.tools.len(),
            "starting openrouter completion turn"
        );

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Request(format!("openrouter request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(http_error_to_model_error(response, "openrouter").await);
        }

        let byte_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map(|b| b.to_vec()));
        let ctx = StreamCtx {
            byte_stream: Box::pin(byte_stream),
            buf: Vec::new(),
            state: OpenRouterStreamState::new(),
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
    state: OpenRouterStreamState,
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
                            return Some((Err(ModelError::StreamError(message)), ctx));
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            raw = %data,
                            "openrouter stream: invalid JSON in SSE data, terminating stream"
                        );
                        ctx.finished = true;
                        return Some((
                            Err(ModelError::Decode(format!(
                                "openrouter stream: invalid JSON in SSE data: {err}"
                            ))),
                            ctx,
                        ));
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
                    return Some((
                        Err(ModelError::StreamError(format!("transport error: {err}"))),
                        ctx,
                    ));
                }
                None => {
                    ctx.finished = true;
                    if ctx.state.done {
                        // Connection closed cleanly right after [DONE] —
                        // nothing more to yield.
                        return None;
                    }
                    // Connection closed before the [DONE] sentinel ever
                    // arrived (even if we already saw a finish_reason): an
                    // incomplete stream must not be treated as a
                    // successful completion.
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
        );

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
        );

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
