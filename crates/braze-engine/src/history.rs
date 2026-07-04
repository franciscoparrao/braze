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

use std::collections::HashMap;

use braze_events::AgentEvent;
use braze_session::DurableState;
use braze_types::{ContentBlock, Message, Role};

/// Tools whose `tool_result` must never be cleared from `durable_events`,
/// even once settled — empty by default (MVP per PLAN.md's "Grupo 3 del
/// roadmap SOTA": no tool has demonstrated a need for an exemption yet).
const NEVER_CLEAR_TOOLS: &[&str] = &[];

/// Builds the message history for the next model call from durable state
/// (already-settled summary *and* settled events, never re-derived from
/// raw events) plus the live tactical window (either raw events, or —
/// once compacted — the engine passes in the events representing that
/// compaction instead, per `Engine::run_turn`'s algorithm).
///
/// Order is: resumen (if any) -> settled `durable_events` (with old
/// `tool_result`s cleared per `event_to_message_cleared`) -> raw tactical
/// events, verbatim. This is what makes `durable.durable_events` actually
/// reach the model — previously `DurableState::durable_events` was
/// computed by `ContextCompactor::split` but silently dropped here, so
/// old events vanished from context entirely once they aged out of the
/// tactical window (unless their gist happened to survive in
/// `durable.summary`).
pub fn build_messages(durable: &DurableState, tactical: &[AgentEvent]) -> Vec<Message> {
    build_messages_with_never_clear(durable, tactical, NEVER_CLEAR_TOOLS)
}

/// Implementation behind [`build_messages`], parameterized on the
/// tool-result clearing exclusion list so tests can exercise both branches
/// without mutating the production `NEVER_CLEAR_TOOLS` constant.
fn build_messages_with_never_clear(
    durable: &DurableState,
    tactical: &[AgentEvent],
    never_clear: &[&str],
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(durable.durable_events.len() + tactical.len() + 1);

    if !durable.summary.is_empty() {
        messages.push(Message::text(
            Role::User,
            format!("[Resumen de contexto previo]\n{}", durable.summary),
        ));
    }

    let tool_names = tool_names_by_id(&durable.durable_events);
    for event in &durable.durable_events {
        if let Some(message) = event_to_message_cleared(event, &tool_names, never_clear) {
            messages.push(message);
        }
    }

    for event in tactical {
        if let Some(message) = event_to_message(event) {
            messages.push(message);
        }
    }

    messages
}

/// Resolves a `tool_use_id` to the name of the tool that issued it, built
/// once per `build_messages` call from every `AssistantToolCall` present
/// in the same durable slice — rather than re-scanned once per
/// `ToolCallCompleted`, which would be quadratic in the number of settled
/// events.
fn tool_names_by_id(events: &[AgentEvent]) -> HashMap<&str, &str> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantToolCall { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect()
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

/// Like [`event_to_message`], but for events already settled into
/// `DurableState::durable_events`: mirrors Anthropic's
/// `clear_tool_uses_20250919` by replacing an old `ToolCallCompleted`'s
/// `tool_result` content with a short placeholder, unless the tool that
/// produced it is in `never_clear`. The matching `tool_use`
/// (`AssistantToolCall`) is never touched here — only the result payload,
/// which is what tends to carry heavy content (file dumps, shell output).
///
/// `tool_names` resolves a `tool_use_id` to the name of the tool that
/// issued it (see [`tool_names_by_id`]). If the id can't be resolved —
/// not expected once `is_settled_durable` moves `AssistantToolCall` and
/// its `ToolCallCompleted` into `durable_events` together, but handled
/// defensively — the event is treated as *not* exempt and gets cleared:
/// it's safer to over-clear than to let an unbounded payload through on
/// an uncovered edge case.
fn event_to_message_cleared(
    event: &AgentEvent,
    tool_names: &HashMap<&str, &str>,
    never_clear: &[&str],
) -> Option<Message> {
    match event {
        AgentEvent::ToolCallCompleted { id, result } => {
            let is_exempt = tool_names
                .get(id.as_str())
                .copied()
                .is_some_and(|name| never_clear.contains(&name));

            let content = if is_exempt {
                result.content.clone()
            } else {
                format!(
                    "[tool result cleared: {} chars removed to keep context small; the tool call above is preserved]",
                    result.content.len()
                )
            };

            Some(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content,
                    is_error: result.is_error,
                }],
            })
        }
        // Already folded into `durable.summary` by the compactor —
        // rendering it again here would duplicate that content as a
        // separate message.
        AgentEvent::CompactionOccurred { .. } => None,
        // Every other type (including `AssistantToolCall`, which is
        // always preserved in full) behaves identically to the tactical
        // path.
        other => event_to_message(other),
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
                key: None,
            },
            AgentEvent::PermissionDecided {
                action: "write file /tmp/x".to_string(),
                allowed: true,
                key: None,
            },
        ];

        let messages = build_messages(&empty_durable(), &tactical);
        assert!(messages.is_empty());
    }

    fn tool_call_event(id: &str, name: &str) -> AgentEvent {
        AgentEvent::AssistantToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({ "path": format!("{name}.txt") }),
        }
    }

    fn tool_completed_event(id: &str, content: &str) -> AgentEvent {
        AgentEvent::ToolCallCompleted {
            id: id.to_string(),
            result: ToolResult {
                tool_call_id: id.to_string(),
                content: content.to_string(),
                is_error: false,
            },
        }
    }

    #[test]
    fn durable_tool_result_is_cleared_but_tool_use_is_preserved() {
        let long_content = "x".repeat(5_000);
        let durable = DurableState {
            summary: String::new(),
            durable_events: vec![
                tool_call_event("call-1", "read_file"),
                tool_completed_event("call-1", &long_content),
            ],
        };

        let messages = build_messages(&durable, &[]);

        assert_eq!(messages.len(), 2);
        match &messages[0].content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "read_file.txt");
            }
            other => panic!("expected a ToolUse block, got {other:?}"),
        }
        match &messages[1].content[0] {
            ContentBlock::ToolResult {
                content,
                tool_use_id,
                ..
            } => {
                assert_eq!(tool_use_id, "call-1");
                assert!(
                    !content.contains(&long_content),
                    "original content leaked: {content}"
                );
                assert!(
                    content.contains("cleared"),
                    "expected clearing placeholder, got: {content}"
                );
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
    }

    #[test]
    fn never_clear_list_exempts_only_the_named_tool() {
        // Two settled pairs for two different tools. `never_clear` is
        // passed directly rather than mutating `NEVER_CLEAR_TOOLS`, so
        // both branches (exempt / not exempt) can be exercised without
        // touching global state.
        let durable = DurableState {
            summary: String::new(),
            durable_events: vec![
                tool_call_event("call-1", "read_file"),
                tool_completed_event("call-1", "contenido de read_file"),
                tool_call_event("call-2", "keep_me"),
                tool_completed_event("call-2", "contenido que debe conservarse"),
            ],
        };

        let messages = build_messages_with_never_clear(&durable, &[], &["keep_me"]);

        assert_eq!(messages.len(), 4);
        match &messages[1].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(
                    content.contains("cleared"),
                    "expected cleared, got: {content}"
                );
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
        match &messages[3].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, "contenido que debe conservarse");
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
    }

    #[test]
    fn tactical_tool_result_is_never_cleared_regardless_of_length() {
        let long_content = "y".repeat(10_000);
        let tactical = vec![tool_completed_event("call-9", &long_content)];

        let messages = build_messages(&empty_durable(), &tactical);

        assert_eq!(messages.len(), 1);
        match &messages[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, &long_content);
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_orders_summary_then_durable_then_tactical() {
        let durable = DurableState {
            summary: "resumen previo".to_string(),
            durable_events: vec![
                AgentEvent::UserMessage {
                    text: "mensaje viejo".to_string(),
                },
                tool_call_event("call-1", "read_file"),
                tool_completed_event("call-1", "contenido viejo largo"),
            ],
        };
        let tactical = vec![
            AgentEvent::UserMessage {
                text: "mensaje reciente".to_string(),
            },
            AgentEvent::AssistantText {
                text: "respuesta reciente".to_string(),
            },
        ];

        let messages = build_messages(&durable, &tactical);

        // [resumen, mensaje viejo, tool_use viejo, tool_result viejo
        // (cleared), mensaje reciente, respuesta reciente] — durable
        // events precede tactical ones, and each side keeps its own
        // internal order.
        assert_eq!(messages.len(), 6);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => assert!(text.contains("resumen previo")),
            other => panic!("expected a Text block, got {other:?}"),
        }
        match &messages[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "mensaje viejo"),
            other => panic!("expected a Text block, got {other:?}"),
        }
        assert!(matches!(
            messages[2].content[0],
            ContentBlock::ToolUse { .. }
        ));
        match &messages[3].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(content.contains("cleared"))
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
        match &messages[4].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "mensaje reciente"),
            other => panic!("expected a Text block, got {other:?}"),
        }
        match &messages[5].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "respuesta reciente"),
            other => panic!("expected a Text block, got {other:?}"),
        }
    }
}
