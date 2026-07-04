use braze_types::ToolResult;
use serde::{Deserialize, Serialize};

/// One entry in a session's event log. This is the vocabulary
/// `braze-session::SessionStore` persists and `ContextCompactor` splits
/// into durable state vs. tactical (live) conversation window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    UserMessage {
        text: String,
    },
    AssistantText {
        text: String,
    },
    /// The assistant requested a tool invocation. Captured separately from
    /// `ToolCallStarted` (which only records id/name/background) because
    /// reconstructing message history for the next model call requires the
    /// full `tool_use` block the assistant produced — the Anthropic API
    /// requires that block to appear in history before the matching
    /// `tool_result` (see `braze-engine::history`).
    AssistantToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolCallStarted {
        id: String,
        name: String,
        background: bool,
    },
    ToolCallCompleted {
        id: String,
        result: ToolResult,
    },
    CompactionOccurred {
        summary: String,
        dropped_tokens_estimate: u32,
    },
    PermissionRequested {
        action: String,
        reversible: bool,
    },
    PermissionDecided {
        action: String,
        allowed: bool,
    },
}
