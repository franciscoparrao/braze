use std::pin::Pin;

use async_trait::async_trait;
use braze_types::{Message, ToolStub};
use futures::Stream;

use crate::error::ModelError;

/// What the engine sends to a [`ModelBackend`] on each turn. `Clone` so
/// `braze-engine`'s G10 best-of-n voting (docs/AUDITORIA-2026-07.md) can
/// issue the same request to the model several times without rebuilding
/// it from scratch per attempt.
#[derive(Clone)]
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
        /// The provider's reason the round stopped (Anthropic's
        /// `stop_reason`, Ollama's `done_reason`), when reported — see
        /// `braze_events::AgentEvent::Usage`'s doc comment for why this
        /// matters (diagnosing a tool call's JSON getting cut off by
        /// `max_tokens` instead of it just silently vanishing).
        stop_reason: Option<String>,
        /// OpenRouter's `usage.prompt_tokens_details.cached_tokens` —
        /// tokens of this request's prompt that hit an existing cache
        /// entry, billed at a fraction of the normal input price.
        /// `None` for backends/responses that don't report it (Ollama,
        /// Anthropic-native today, or an OpenRouter provider that doesn't
        /// support caching at all) — never fabricated as `Some(0)`, so a
        /// caller can tell "no caching happened" apart from "this
        /// backend doesn't report caching" (docs/usability-log-2026-07-07-si2.md).
        cache_read_tokens: Option<u32>,
        /// OpenRouter's `usage.prompt_tokens_details.cache_write_tokens`
        /// — tokens newly written to cache by this request (billed at a
        /// premium over normal input price), expected non-zero on the
        /// first request establishing a cache entry and typically zero
        /// on the requests that read it back. Same `None`-means-"not
        /// reported" contract as `cache_read_tokens`.
        cache_write_tokens: Option<u32>,
        /// Set by `EscalatingBackend::complete` (H-3,
        /// docs/AUDITORIA-2026-07-v5.md) when this round is the one that
        /// *triggers* a reactive escalation to the lead model — `None` for
        /// every other round (normal worker rounds, rounds already inside
        /// an active escalation window, and every backend that isn't
        /// wrapped in `EscalatingBackend` at all). Plain `ModelBackend`
        /// implementations never set this field themselves; it exists so
        /// the decorator can signal the engine through the same `Usage`
        /// event it already emits, without either side needing a second
        /// side channel.
        escalation_trigger: Option<String>,
    },
    Done,
}

/// The permissive placeholder schema sent for a tool whose real
/// `input_schema` isn't known yet (still-deferred MCP tools — see
/// `ToolStub`'s two-tier schema policy). Shared by all three backends'
/// `build_tools` so a future change to the fallback shape can't silently
/// diverge between them.
pub(crate) fn permissive_fallback_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": true
    })
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
