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
use crate::http_error::http_error_to_model_error;

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
}

impl AnthropicBackend {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::Client::new(),
            base_url: ANTHROPIC_API_URL.to_string(),
        }
    }

    /// Test-only hook: point at a local fake server instead of the real
    /// Anthropic endpoint.
    #[cfg(test)]
    fn with_base_url(api_key: String, model: String, base_url: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::Client::new(),
            base_url,
        }
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
    ) -> Result<Pin<Box<dyn Stream<Item = CompletionEvent> + Send>>, ModelError> {
        let body = build_request(&req, &self.model);
        tracing::info!(tool_count = body.tools.len(), "starting anthropic completion turn");

        let response = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Request(format!("anthropic request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(http_error_to_model_error(response, "anthropic").await);
        }

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
}

async fn drive_stream(mut ctx: StreamCtx) -> Option<(CompletionEvent, StreamCtx)> {
    loop {
        if let Some(event) = ctx.pending.pop_front() {
            return Some((event, ctx));
        }
        if ctx.finished {
            return None;
        }

        match extract_next_sse_data(&mut ctx.buf) {
            Some(data) => match serde_json::from_str::<serde_json::Value>(&data) {
                Ok(json) => {
                    tracing::debug!(event_type = ?json.get("type"), "anthropic sse event");
                    let events = ctx.state.handle_event(&json);
                    ctx.pending.extend(events);
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
                    return None;
                }
            },
            None => match ctx.byte_stream.next().await {
                Some(Ok(bytes)) => {
                    ctx.buf.extend_from_slice(&bytes);
                }
                Some(Err(err)) => {
                    tracing::error!(error = %err, "anthropic stream: transport error, terminating stream");
                    return None;
                }
                None => {
                    // Connection closed. If we never saw message_stop, there's
                    // nothing more to yield — just end the stream.
                    return None;
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

        let addr = crate::test_support::spawn_canned_http_server(200, "text/event-stream", sse_body.as_bytes().to_vec()).await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
        );

        let mut stream = backend.complete(sample_request()).await.expect("request should succeed");

        let mut text = String::new();
        let mut saw_usage = false;
        let mut saw_done = false;
        while let Some(event) = stream.next().await {
            match event {
                CompletionEvent::TextDelta(t) => text.push_str(&t),
                CompletionEvent::Usage { input_tokens, output_tokens } => {
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

        let addr = crate::test_support::spawn_canned_http_server(200, "text/event-stream", sse_body.as_bytes().to_vec()).await;

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
            .filter_map(|e| match e {
                CompletionEvent::ToolCallRequested { id, name, arguments } => {
                    Some((id.clone(), name.clone(), arguments.clone()))
                }
                _ => None,
            })
            .collect();

        assert_eq!(tool_calls.len(), 1);
        let (id, name, arguments) = &tool_calls[0];
        assert_eq!(id, "toolu_1");
        assert_eq!(name, "get_weather");
        assert_eq!(arguments, &serde_json::json!({"location": "Santiago"}));
    }

    #[tokio::test]
    async fn complete_maps_429_to_rate_limited() {
        let body = br#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#.to_vec();
        let addr = crate::test_support::spawn_canned_http_server(429, "application/json", body).await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
        );

        let err = match backend.complete(sample_request()).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(matches!(err, ModelError::RateLimited(_)), "expected RateLimited, got {err:?}");
    }

    #[tokio::test]
    async fn complete_maps_500_to_request_error() {
        let body = br#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#.to_vec();
        let addr = crate::test_support::spawn_canned_http_server(500, "application/json", body).await;

        let backend = AnthropicBackend::with_base_url(
            "test-key".to_string(),
            "claude-opus-4-8".to_string(),
            format!("http://{addr}/v1/messages"),
        );

        let err = match backend.complete(sample_request()).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(matches!(err, ModelError::Request(_)), "expected Request, got {err:?}");
    }
}
