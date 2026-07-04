use std::pin::Pin;

use async_trait::async_trait;
use braze_types::{Message, ToolStub};
use futures::Stream;

use crate::error::ModelError;

/// What the engine sends to a [`ModelBackend`] on each turn.
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    /// Names + one-line summaries only — never full JSON schemas up front.
    /// See `braze-tools-core`'s deferred-loading mechanism.
    pub tool_stubs: Vec<ToolStub>,
    pub system_prompt: String,
    pub max_tokens: u32,
}

/// One increment of a streamed completion.
#[derive(Debug, Clone)]
pub enum CompletionEvent {
    TextDelta(String),
    ToolCallRequested {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    Done,
}

/// Abstracts over LLM providers. MVP requires at least two independent
/// implementers (`AnthropicBackend`, `OllamaBackend`) to prove this isn't a
/// one-off shaped around a single vendor's API.
#[async_trait]
pub trait ModelBackend: Send + Sync {
    fn name(&self) -> &str;

    /// Streams the completion as a sequence of [`CompletionEvent`]s. Each
    /// item is a `Result`, not a bare `CompletionEvent`: a transport
    /// error, a mid-stream provider error, or the connection closing
    /// before a terminal event must surface as `Err(ModelError)` so the
    /// caller can tell that apart from a normal completion — see
    /// [`ModelError::StreamError`]. Implementations must uphold the
    /// invariant that the stream either ends with `Ok(CompletionEvent::Done)`
    /// as its last item, or yields an `Err` before ending; ending silently
    /// with neither is a bug in the implementation, not a valid outcome.
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>;
}
