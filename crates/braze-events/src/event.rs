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
        /// Coarse identity of the action being requested, if the caller
        /// could derive one (see `braze_permissions::derive_permission_key`).
        /// `#[serde(default)]` so rollout logs persisted before this field
        /// existed still deserialize, with `key: None`.
        #[serde(default)]
        key: Option<braze_types::PermissionKey>,
    },
    PermissionDecided {
        action: String,
        allowed: bool,
        /// Same coarse identity as `PermissionRequested::key`. When
        /// `allowed` is `true` and this is `Some`, a resumed session
        /// replays it back into a fresh `PermissionGuard` via
        /// `PermissionGuard::seed_remembered` so the same action isn't
        /// re-confirmed. `#[serde(default)]` for the same backward-compat
        /// reason as `PermissionRequested::key`.
        #[serde(default)]
        key: Option<braze_types::PermissionKey>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates loading a rollout log line written before this field
    /// existed: the JSON has no `key` at all. `#[serde(default)]` must
    /// still let it deserialize, defaulting to `None`.
    #[test]
    fn permission_decided_without_a_key_field_deserializes_with_none() {
        let json = r#"{"type":"permission_decided","action":"run `mv a b`","allowed":true}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::PermissionDecided {
                action,
                allowed,
                key,
            } => {
                assert_eq!(action, "run `mv a b`");
                assert!(allowed);
                assert_eq!(key, None);
            }
            other => panic!("expected PermissionDecided, got {other:?}"),
        }
    }
}
