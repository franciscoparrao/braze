//! Test-only validator for the message-ordering rules the real Anthropic
//! Messages API enforces (a `400` when violated). Precondition for Grupo I
//! of docs/AUDITORIA-2026-07-v2.md: several fixes in the context pipeline
//! (A1/C1, A2/C2, C4) turned out to have gaps (N-1, N-2, N-4) that silently
//! reintroduce permanently-invalid sessions, and none of them were caught
//! by existing tests because `ScriptedModel` never looks at the
//! `Vec<Message>` it's handed. This module gives tests something that does
//! — turning what would be a confusing runtime `400` (or, on backends that
//! don't validate, a silently corrupted conversation) into an immediate,
//! precisely-diagnosed test failure at the exact call site that built the
//! bad message sequence.
//!
//! Rules checked (see [`check_anthropic_message_protocol`] for the exact
//! semantics):
//! 1. The first message must have `role == Role::User`.
//! 2. Every `tool_use` id is unique across the whole request.
//! 3. Every `tool_result` references a `tool_use` id seen earlier in the
//!    request (no orphaned result).
//! 4. Every `tool_use`'s matching `tool_result` appears in the *very next*
//!    message (adjacency) — not two messages later, not never.
//!
//! Deliberately test-only (`#[cfg(test)]` in `lib.rs`): this is a
//! diagnostic harness for `braze-engine`'s own tests, not a runtime guard —
//! shipping it in the production binary would mean paying to re-derive an
//! invariant the engine should just uphold by construction.

use braze_types::{ContentBlock, Message, Role};

/// A concrete way a `Vec<Message>` violates the Anthropic message-ordering
/// protocol. Each variant carries enough detail to point straight at the
/// offending message without needing to re-read the whole sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProtocolViolation {
    FirstMessageNotUser {
        got: Role,
    },
    DuplicateToolUseId {
        id: String,
        message_index: usize,
    },
    OrphanedToolResult {
        tool_use_id: String,
        message_index: usize,
    },
    ToolResultNotAdjacent {
        tool_use_id: String,
        expected_message_index: usize,
        actual_message_index: usize,
    },
    UnansweredToolUse {
        tool_use_id: String,
    },
}

impl std::fmt::Display for ProtocolViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FirstMessageNotUser { got } => write!(
                f,
                "first message must have role=User (Anthropic 400s otherwise); got {got:?}"
            ),
            Self::DuplicateToolUseId { id, message_index } => write!(
                f,
                "duplicate tool_use id {id:?} at message #{message_index} — \
                 Anthropic rejects a request with two tool_use blocks sharing an id"
            ),
            Self::OrphanedToolResult {
                tool_use_id,
                message_index,
            } => write!(
                f,
                "tool_result for {tool_use_id:?} at message #{message_index} has no \
                 matching tool_use anywhere earlier in the request (or it was already \
                 answered once)"
            ),
            Self::ToolResultNotAdjacent {
                tool_use_id,
                expected_message_index,
                actual_message_index,
            } => write!(
                f,
                "tool_result for {tool_use_id:?} must be message #{expected_message_index} \
                 (the one immediately after its tool_use); found it at message \
                 #{actual_message_index} instead"
            ),
            Self::UnansweredToolUse { tool_use_id } => write!(
                f,
                "tool_use {tool_use_id:?} is never answered by a tool_result in the \
                 following message"
            ),
        }
    }
}

/// Checks `messages` against the ordering rules the real Anthropic
/// Messages API enforces. Returns the *first* violation found, scanning
/// messages in order — good enough for a test failure to point at the
/// right place; this is not meant to enumerate every violation at once.
///
/// Note on scope: this only checks message *shape/ordering*, not content.
/// It cannot catch bugs like a vanished context summary (N-3) or unbounded
/// compaction growth (N-6) — those aren't protocol violations, just bad
/// content inside an otherwise well-formed request.
pub(crate) fn check_anthropic_message_protocol(
    messages: &[Message],
) -> Result<(), ProtocolViolation> {
    if let Some(first) = messages.first()
        && first.role != Role::User
    {
        return Err(ProtocolViolation::FirstMessageNotUser { got: first.role });
    }

    let mut seen_tool_use_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // tool_use id -> the message index its tool_result must appear at.
    let mut pending: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (idx, message) in messages.iter().enumerate() {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    if !seen_tool_use_ids.insert(id.clone()) {
                        return Err(ProtocolViolation::DuplicateToolUseId {
                            id: id.clone(),
                            message_index: idx,
                        });
                    }
                    pending.insert(id.clone(), idx + 1);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if !seen_tool_use_ids.contains(tool_use_id) {
                        return Err(ProtocolViolation::OrphanedToolResult {
                            tool_use_id: tool_use_id.clone(),
                            message_index: idx,
                        });
                    }
                    match pending.remove(tool_use_id) {
                        Some(expected) if expected == idx => {}
                        Some(expected) => {
                            return Err(ProtocolViolation::ToolResultNotAdjacent {
                                tool_use_id: tool_use_id.clone(),
                                expected_message_index: expected,
                                actual_message_index: idx,
                            });
                        }
                        // Already removed (a second tool_result for the
                        // same id) — same observable failure as an
                        // orphan: no tool_use is left pending for it.
                        None => {
                            return Err(ProtocolViolation::OrphanedToolResult {
                                tool_use_id: tool_use_id.clone(),
                                message_index: idx,
                            });
                        }
                    }
                }
                ContentBlock::Text { .. } => {}
            }
        }
    }

    if let Some((tool_use_id, _)) = pending.into_iter().next() {
        return Err(ProtocolViolation::UnansweredToolUse { tool_use_id });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message::text(Role::User, text)
    }

    fn assistant_tool_use(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: "echo".to_string(),
                input: serde_json::json!({}),
            }],
        }
    }

    fn user_tool_result(id: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: "ok".to_string(),
                is_error: false,
            }],
        }
    }

    #[test]
    fn empty_message_list_is_valid() {
        assert_eq!(check_anthropic_message_protocol(&[]), Ok(()));
    }

    #[test]
    fn a_well_formed_sequence_is_valid() {
        let messages = vec![
            user("hi"),
            assistant_tool_use("call-1"),
            user_tool_result("call-1"),
            Message::text(Role::Assistant, "done"),
        ];
        assert_eq!(check_anthropic_message_protocol(&messages), Ok(()));
    }

    #[test]
    fn first_message_must_be_user() {
        let messages = vec![assistant_tool_use("call-1"), user_tool_result("call-1")];
        assert_eq!(
            check_anthropic_message_protocol(&messages),
            Err(ProtocolViolation::FirstMessageNotUser {
                got: Role::Assistant
            })
        );
    }

    #[test]
    fn duplicate_tool_use_ids_are_rejected() {
        let messages = vec![
            user("hi"),
            assistant_tool_use("call-1"),
            user_tool_result("call-1"),
            assistant_tool_use("call-1"),
            user_tool_result("call-1"),
        ];
        assert_eq!(
            check_anthropic_message_protocol(&messages),
            Err(ProtocolViolation::DuplicateToolUseId {
                id: "call-1".to_string(),
                message_index: 3,
            })
        );
    }

    #[test]
    fn a_tool_result_with_no_matching_tool_use_is_rejected() {
        let messages = vec![user("hi"), user_tool_result("call-1")];
        assert_eq!(
            check_anthropic_message_protocol(&messages),
            Err(ProtocolViolation::OrphanedToolResult {
                tool_use_id: "call-1".to_string(),
                message_index: 1,
            })
        );
    }

    #[test]
    fn a_tool_result_one_message_late_is_rejected() {
        let messages = vec![
            user("hi"),
            assistant_tool_use("call-1"),
            user("are you still there?"),
            user_tool_result("call-1"),
        ];
        assert_eq!(
            check_anthropic_message_protocol(&messages),
            Err(ProtocolViolation::ToolResultNotAdjacent {
                tool_use_id: "call-1".to_string(),
                expected_message_index: 2,
                actual_message_index: 3,
            })
        );
    }

    #[test]
    fn a_tool_use_with_no_result_at_all_is_rejected() {
        let messages = vec![user("hi"), assistant_tool_use("call-1")];
        assert_eq!(
            check_anthropic_message_protocol(&messages),
            Err(ProtocolViolation::UnansweredToolUse {
                tool_use_id: "call-1".to_string(),
            })
        );
    }
}
