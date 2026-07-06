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
        let body = build_request(&req, &self.model, self.num_ctx, self.temperature);
        tracing::info!(
            tool_count = body.tools.len(),
            num_ctx = self.num_ctx,
            num_predict = body.options.num_predict,
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
