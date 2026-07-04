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
    /// Token usage reported by the model backend for one completion round.
    /// Audit-only, like `ToolCallStarted`/`CompactionOccurred` — never
    /// rendered back into model history (see
    /// `braze-engine::history::event_to_message`). Added so tooling
    /// (e.g. `braze-bench`) can read per-round usage back out of the
    /// rollout log without `braze-engine` needing to expose it any other
    /// way.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        /// The provider's reason the round stopped (Anthropic's
        /// `stop_reason`, Ollama's `done_reason`), when the backend
        /// reports one — e.g. `"end_turn"`/`"stop"` for a normal
        /// completion vs. `"max_tokens"`/`"length"` for output truncated
        /// by the `max_tokens` budget. A tool call whose JSON arguments
        /// got cut off mid-stream by `max_tokens` fails to parse and is
        /// silently dropped with no other signal of *why* — this is what
        /// lets that be diagnosed instead of just observed as "the model
        /// gave up". `#[serde(default)]` for backward compat with rollout
        /// logs written before this field existed.
        #[serde(default)]
        stop_reason: Option<String>,
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

    #[test]
    fn usage_round_trips_through_json() {
        let event = AgentEvent::Usage {
            input_tokens: 123,
            output_tokens: 45,
            stop_reason: Some("end_turn".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let round_tripped: AgentEvent = serde_json::from_str(&json).unwrap();
        match round_tripped {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
            } => {
                assert_eq!(input_tokens, 123);
                assert_eq!(output_tokens, 45);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    /// Simulates loading a rollout log line written before `stop_reason`
    /// existed: the JSON has no such field at all. `#[serde(default)]`
    /// must still let it deserialize, defaulting to `None`.
    #[test]
    fn usage_without_a_stop_reason_field_deserializes_with_none() {
        let json = r#"{"type":"usage","input_tokens":10,"output_tokens":5}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::Usage { stop_reason, .. } => assert_eq!(stop_reason, None),
            other => panic!("expected Usage, got {other:?}"),
        }
    }
}
