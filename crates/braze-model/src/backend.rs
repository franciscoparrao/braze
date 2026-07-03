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

    /// Streams the completion as a sequence of [`CompletionEvent`]s.
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = CompletionEvent> + Send>>, ModelError>;
}
