//! Reconstructs the `Vec<Message>` sent to a [`braze_model::ModelBackend`]
//! from a session's event log, split into durable state + tactical window
//! by a [`braze_session::ContextCompactor`].
//!
//! **MVP simplification** (documented per PLAN.md Fase 5): this maps one
//! [`AgentEvent`] to (at most) one [`Message`]. The real Anthropic API
//! groups several consecutive `tool_use`/`tool_result` blocks produced by
//! the same role into a single `Message` with multiple `ContentBlock`s.
//! Emitting one `Message` per event instead is a coarser shape, but it is
//! still a *valid* sequence of alternating-enough roles that the wire
//! format accepts — it just doesn't pack as tightly. Revisit if/when
//! request-size or strict role-alternation becomes a real constraint.

use braze_events::AgentEvent;
use braze_session::DurableState;
use braze_types::{ContentBlock, Message, Role};

/// Builds the message history for the next model call from durable state
/// (already-settled summary, never re-derived from raw events) plus the
/// live tactical window (either raw events, or — once compacted — the
/// engine passes in the events representing that compaction instead, per
/// `Engine::run_turn`'s algorithm).
pub fn build_messages(durable: &DurableState, tactical: &[AgentEvent]) -> Vec<Message> {
    let mut messages = Vec::with_capacity(tactical.len() + 1);

    if !durable.summary.is_empty() {
        messages.push(Message::text(
            Role::User,
            format!("[Resumen de contexto previo]\n{}", durable.summary),
        ));
    }

    for event in tactical {
        if let Some(message) = event_to_message(event) {
            messages.push(message);
        }
    }

    messages
}

/// Maps a single [`AgentEvent`] to the [`Message`] it contributes to model
/// history, or `None` for events that are audit/metadata only and never
/// appear in the conversation the model sees.
fn event_to_message(event: &AgentEvent) -> Option<Message> {
    match event {
        AgentEvent::UserMessage { text } => Some(Message::text(Role::User, text.clone())),
        AgentEvent::AssistantText { text } => Some(Message::text(Role::Assistant, text.clone())),
        AgentEvent::AssistantToolCall {
            id,
            name,
            arguments,
        } => Some(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: arguments.clone(),
            }],
        }),
        AgentEvent::ToolCallCompleted { id, result } => Some(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
            }],
        }),
        // Metadata / audit-only — never part of conversational content.
        AgentEvent::ToolCallStarted { .. }
        | AgentEvent::CompactionOccurred { .. }
        | AgentEvent::PermissionRequested { .. }
        | AgentEvent::PermissionDecided { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::ToolResult;

    fn empty_durable() -> DurableState {
        DurableState::default()
    }

    #[test]
    fn empty_log_produces_no_messages() {
        let messages = build_messages(&empty_durable(), &[]);
        assert!(messages.is_empty());
    }

    #[test]
    fn nonempty_durable_summary_is_prepended_as_a_user_message() {
        let durable = DurableState {
            summary: "el usuario pidió listar archivos".to_string(),
            durable_events: Vec::new(),
        };
        let tactical = vec![AgentEvent::UserMessage {
            text: "y ahora qué".to_string(),
        }];

        let messages = build_messages(&durable, &tactical);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("[Resumen de contexto previo]"));
                assert!(text.contains("el usuario pidió listar archivos"));
            }
            other => panic!("expected a Text block, got {other:?}"),
        }
        assert_eq!(messages[1].role, Role::User);
    }

    #[test]
    fn empty_durable_summary_is_not_prepended() {
        let tactical = vec![AgentEvent::UserMessage {
            text: "hola".to_string(),
        }];
        let messages = build_messages(&empty_durable(), &tactical);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn full_tool_call_round_trip_maps_to_three_messages() {
        let tactical = vec![
            AgentEvent::UserMessage {
                text: "lee el archivo foo.txt".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "foo.txt" }),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "contenido de foo.txt".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "el archivo dice: contenido de foo.txt".to_string(),
            },
        ];

        let messages = build_messages(&empty_durable(), &tactical);

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        match &messages[1].content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "foo.txt");
            }
            other => panic!("expected a ToolUse block, got {other:?}"),
        }
        assert_eq!(messages[2].role, Role::User);
        match &messages[2].content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "call-1");
                assert_eq!(content, "contenido de foo.txt");
                assert!(!is_error);
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
        assert_eq!(messages[3].role, Role::Assistant);
    }

    #[test]
    fn audit_only_events_are_skipped() {
        let tactical = vec![
            AgentEvent::ToolCallStarted {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                background: true,
            },
            AgentEvent::CompactionOccurred {
                summary: "resumen".to_string(),
                dropped_tokens_estimate: 10,
            },
            AgentEvent::PermissionRequested {
                action: "write file /tmp/x".to_string(),
                reversible: false,
            },
            AgentEvent::PermissionDecided {
                action: "write file /tmp/x".to_string(),
                allowed: true,
            },
        ];

        let messages = build_messages(&empty_durable(), &tactical);
        assert!(messages.is_empty());
    }
}
