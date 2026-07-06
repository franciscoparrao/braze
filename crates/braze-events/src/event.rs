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
        /// existed still deserialize, with `key: None`. `deserialize_with`
        /// (N-40, docs/AUDITORIA-2026-07-v2.md): a `PermissionKey` variant
        /// this binary doesn't recognize (written by a newer one) falls
        /// back to `None` for just this field instead of failing to
        /// deserialize this whole event — which would otherwise abort
        /// `load()` for the entire session at that line.
        #[serde(
            default,
            deserialize_with = "braze_types::deserialize_permission_key_lossy"
        )]
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
        /// reason as `PermissionRequested::key`; `deserialize_with` for
        /// the same N-40 lossy-fallback reason.
        #[serde(
            default,
            deserialize_with = "braze_types::deserialize_permission_key_lossy"
        )]
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
    /// Catch-all for a `"type"` tag this binary's enum doesn't have a
    /// variant for (C9, docs/AUDITORIA-2026-07.md). `AgentEvent`'s serde
    /// shape is a frozen contract (PLAN.md) — a new variant is the only
    /// additive way to evolve it, and without this fallback, an older
    /// binary reading a rollout log written by a newer one (with a
    /// variant it doesn't know) fails `load` for the *entire* session at
    /// that line, not just the one it can't understand. `#[serde(other)]`
    /// on a fieldless variant is serde's own forward-compatibility escape
    /// hatch for internally-tagged enums: any unrecognized `type` value
    /// deserializes to this variant instead of erroring, discarding the
    /// rest of that line's fields (nothing useful to keep from a shape
    /// this binary has no definition for). Downstream code treats it like
    /// any other audit-only event — see
    /// `braze_session::SimpleContextCompactor::compact_tactical` and
    /// `braze_engine::history::event_to_message`.
    ///
    /// Known accepted limitation (bajo, docs/AUDITORIA-2026-07-v2.md,
    /// "AgentEvent::Unknown pierde el payload al replicarse en
    /// backtrack"): serde's `#[serde(other)]` for an internally-tagged
    /// enum only supports a unit variant — it cannot carry the original
    /// JSON's other fields alongside it, by construction of how serde
    /// resolves the tag before deserializing the rest of the object.
    /// Carrying the raw payload here would mean replacing this derive
    /// with a hand-written `Deserialize` for the whole enum — a much
    /// larger change to a frozen-contract type than this narrow case
    /// justifies. Practical effect: if `braze-tui`'s backtrack replicates
    /// a session containing an event type *this* binary doesn't
    /// recognize (written by a newer binary), the replicated copy loses
    /// that event's original fields — the untouched original session file
    /// still has them; only the new, backtracked-into session doesn't.
    #[serde(other)]
    Unknown,
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

    /// Regression test for N-40 (docs/AUDITORIA-2026-07-v2.md): a
    /// `PermissionDecided` event carrying a `key` shape this binary
    /// doesn't recognize (simulating a `PermissionKey` variant a newer
    /// binary added) must still deserialize the whole event, with
    /// `key: None` — not fail the entire line (and, previously, abort
    /// `load()` for the whole session at that point).
    #[test]
    fn permission_decided_with_an_unrecognized_key_shape_still_deserializes() {
        let json = r#"{"type":"permission_decided","action":"run `mv a b`","allowed":true,"key":{"SomeFutureVariant":{"field":"value"}}}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::PermissionDecided { key, .. } => assert_eq!(key, None),
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

    /// Regression test for C9: a `"type"` value this enum has no variant
    /// for (simulating a rollout log written by a newer binary with an
    /// event kind this one predates) must deserialize to `Unknown`
    /// instead of failing — the whole point of the forward-compat escape
    /// hatch.
    #[test]
    fn unrecognized_type_tag_deserializes_as_unknown_instead_of_erroring() {
        let json = r#"{"type":"some_future_event_kind","whatever":"fields","it":1}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        assert!(matches!(event, AgentEvent::Unknown));
    }
}
