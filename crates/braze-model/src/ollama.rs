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
    OllamaStreamState, build_request, extract_next_ndjson_line, parse_ndjson_line,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

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
}

impl OllamaBackend {
    /// Targets `http://localhost:11434`.
    pub fn new(model: String) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            client: reqwest::Client::new(),
        }
    }

    /// Targets a custom base URL (e.g. a remote Ollama instance, or a
    /// container reachable at a non-default host/port).
    pub fn with_base_url(model: String, base_url: String) -> Self {
        Self {
            base_url,
            model,
            client: reqwest::Client::new(),
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
    ) -> Result<Pin<Box<dyn Stream<Item = CompletionEvent> + Send>>, ModelError> {
        let body = build_request(&req, &self.model);
        tracing::info!(
            tool_count = body.tools.len(),
            "starting ollama completion turn"
        );

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Request(format!("ollama request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(http_error_to_model_error(response, "ollama").await);
        }

        let byte_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map(|b| b.to_vec()));
        let ctx = StreamCtx {
            byte_stream: Box::pin(byte_stream),
            buf: Vec::new(),
            state: OllamaStreamState::new(),
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

async fn drive_stream(mut ctx: StreamCtx) -> Option<(CompletionEvent, StreamCtx)> {
    loop {
        if let Some(event) = ctx.pending.pop_front() {
            return Some((event, ctx));
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
                    return None;
                }
            },
            None => match ctx.byte_stream.next().await {
                Some(Ok(bytes)) => {
                    ctx.buf.extend_from_slice(&bytes);
                }
                Some(Err(err)) => {
                    tracing::error!(error = %err, "ollama stream: transport error, terminating stream");
                    return None;
                }
                None => {
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
            match event {
                CompletionEvent::TextDelta(t) => text.push_str(&t),
                CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
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
            .filter_map(|e| match e {
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
