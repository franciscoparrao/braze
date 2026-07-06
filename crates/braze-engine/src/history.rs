//! Reconstructs the `Vec<Message>` sent to a [`braze_model::ModelBackend`]
//! from a session's event log, split into durable state + tactical window
//! by a [`braze_session::ContextCompactor`].
//!
//! Most events map one-to-one to a `Message` (`UserMessage`/`AssistantText`
//! each get their own). `AssistantToolCall`/`ToolCallCompleted` are the
//! exception: consecutive events of the same kind are *grouped* into a
//! single `Message` carrying multiple `ContentBlock`s — matching how the
//! real Anthropic API represents one assistant turn requesting several
//! tools at once (one message, several `tool_use` blocks) and one user
//! turn answering all of them (one message, several `tool_result` blocks).
//!
//! This grouping isn't cosmetic: `dispatch_tool_calls` persists every
//! round's `AssistantToolCall`s consecutively (all of them, before
//! dispatching any), then their `ToolCallCompleted`s consecutively (in
//! whatever order the underlying tools finish). Emitting one `Message` per
//! event for a round with 2+ concurrent tool calls would put `tool_use`
//! blocks for different calls in *separate* consecutive `Assistant`
//! messages — Anthropic requires each `tool_use`'s matching `tool_result`
//! to be in the message immediately following it, so anything but the
//! last of those `tool_use` messages would have no answer in its very
//! next message and get rejected with a `400`. Grouping into one message
//! per role-run removes the ambiguity entirely instead of relying on an
//! assumption about how strictly the wire format is checked. See
//! `push_block`/`push_grouped` below and
//! docs/AUDITORIA-2026-07-v2.md (Grupo I, hallazgo N-1).

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

    // N-2 (docs/AUDITORIA-2026-07-v2.md): `durable_events` can be
    // non-empty before any `CompactionOccurred` summary has ever been
    // produced — `SimpleContextCompactor::split` moves settled events out
    // of the tactical window purely by age, independent of when
    // `Engine`'s `tactical_compaction_threshold` last triggered a real
    // compaction. Without a leading `User` message in that case, whatever
    // `durable_events` renders to first (often an `Assistant` tool_use)
    // would become the very first message in the request, and Anthropic
    // rejects any request whose first message isn't `role: user`.
    if !durable.summary.is_empty() {
        messages.push(Message::text(
            Role::User,
            format!("[Resumen de contexto previo]\n{}", durable.summary),
        ));
    } else if !durable.durable_events.is_empty() {
        messages.push(Message::text(
            Role::User,
            "[Contexto previo] Lo siguiente son eventos ya resueltos (llamadas a \
             herramientas completadas) de una parte anterior de esta sesión."
                .to_string(),
        ));
    }

    let tool_names = tool_names_by_id(&durable.durable_events);
    push_grouped(&mut messages, &durable.durable_events, |event| {
        event_to_block_cleared(event, &tool_names, never_clear)
    });
    push_grouped(&mut messages, tactical, event_to_block);

    messages
}

/// Appends every event's rendered block to `messages`, grouping
/// consecutive `ToolUse`/`ToolResult` blocks of the same kind into one
/// `Message` instead of emitting a separate `Message` per event — see the
/// module doc comment for why this specific grouping is load-bearing, not
/// cosmetic. Plain `Text` blocks (`UserMessage`/`AssistantText`) are never
/// grouped: each keeps its own `Message`, preserving the exact shape
/// existing callers already depend on.
fn push_grouped<'a>(
    messages: &mut Vec<Message>,
    events: impl IntoIterator<Item = &'a AgentEvent>,
    render: impl Fn(&AgentEvent) -> Option<(Role, ContentBlock)>,
) {
    for event in events {
        let Some((role, block)) = render(event) else {
            continue;
        };

        let same_kind_run = matches!(block, ContentBlock::ToolUse { .. })
            || matches!(block, ContentBlock::ToolResult { .. });

        if same_kind_run
            && let Some(last) = messages.last_mut()
            && last.role == role
            && last
                .content
                .last()
                .is_some_and(|prev| std::mem::discriminant(prev) == std::mem::discriminant(&block))
        {
            last.content.push(block);
            continue;
        }

        messages.push(Message {
            role,
            content: vec![block],
        });
    }
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

/// Maps a single [`AgentEvent`] to the `(Role, ContentBlock)` it
/// contributes to model history, or `None` for events that are
/// audit/metadata only and never appear in the conversation the model
/// sees. [`push_grouped`] is what turns a sequence of these into
/// `Message`s, grouping consecutive `ToolUse`/`ToolResult` blocks.
fn event_to_block(event: &AgentEvent) -> Option<(Role, ContentBlock)> {
    match event {
        AgentEvent::UserMessage { text } => {
            Some((Role::User, ContentBlock::Text { text: text.clone() }))
        }
        AgentEvent::AssistantText { text } => Some((
            Role::Assistant,
            ContentBlock::Text { text: text.clone() },
        )),
        AgentEvent::AssistantToolCall {
            id,
            name,
            arguments,
        } => Some((
            Role::Assistant,
            ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: arguments.clone(),
            },
        )),
        AgentEvent::ToolCallCompleted { id, result } => Some((
            Role::User,
            ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
            },
        )),
        // Metadata / audit-only — never part of conversational content.
        // `Unknown` (C9's forward-compat catch-all) belongs here too:
        // this binary has no definition for it, so there is nothing
        // meaningful to render into a `Message`.
        AgentEvent::ToolCallStarted { .. }
        | AgentEvent::CompactionOccurred { .. }
        | AgentEvent::PermissionRequested { .. }
        | AgentEvent::PermissionDecided { .. }
        | AgentEvent::Usage { .. }
        | AgentEvent::Unknown => None,
    }
}

/// Like [`event_to_block`], but for events already settled into
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
fn event_to_block_cleared(
    event: &AgentEvent,
    tool_names: &HashMap<&str, &str>,
    never_clear: &[&str],
) -> Option<(Role, ContentBlock)> {
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

            Some((
                Role::User,
                ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content,
                    is_error: result.is_error,
                },
            ))
        }
        // Already folded into `durable.summary` by the compactor —
        // rendering it again here would duplicate that content as a
        // separate message.
        AgentEvent::CompactionOccurred { .. } => None,
        // Every other type (including `AssistantToolCall`, which is
        // always preserved in full) behaves identically to the tactical
        // path.
        other => event_to_block(other),
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
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
                stop_reason: Some("end_turn".to_string()),
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

        // N-2 fix: a leading placeholder `User` message is now prepended
        // whenever `durable_events` is non-empty and `summary` is still
        // empty, so the real content shifts from indices [0, 1] to [1, 2].
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        match &messages[1].content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "read_file.txt");
            }
            other => panic!("expected a ToolUse block, got {other:?}"),
        }
        match &messages[2].content[0] {
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

        // N-2 fix: leading placeholder shifts real content from [0..4) to
        // [1..5).
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].role, Role::User);
        match &messages[2].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(
                    content.contains("cleared"),
                    "expected cleared, got: {content}"
                );
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
        match &messages[4].content[0] {
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

    /// Regression test for the deeper issue N-1's investigation surfaced
    /// (docs/AUDITORIA-2026-07-v2.md): `dispatch_tool_calls` persists every
    /// round's `AssistantToolCall`s consecutively (all of them, before
    /// dispatching any), then their `ToolCallCompleted`s consecutively (in
    /// completion order) — so a round with 2+ concurrent tool calls
    /// *always* produces this exact `[ATC, ATC, ATC, TCC, TCC, TCC]` shape
    /// in the raw event log, with or without any compaction/tail-cut ever
    /// happening. Before grouping, this rendered as three separate
    /// `Assistant` messages (one `tool_use` each) followed by three
    /// separate `User` messages (one `tool_result` each) — the first
    /// `tool_use`'s very next message was *another* `tool_use` message,
    /// not the `tool_result` answering it, which Anthropic rejects.
    #[test]
    fn concurrent_tool_calls_in_one_round_group_into_one_message_each_role() {
        let tactical = vec![
            AgentEvent::UserMessage {
                text: "please echo three things".to_string(),
            },
            tool_call_event("call-1", "echo"),
            AgentEvent::ToolCallStarted {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                background: false,
            },
            tool_call_event("call-2", "echo"),
            AgentEvent::ToolCallStarted {
                id: "call-2".to_string(),
                name: "echo".to_string(),
                background: false,
            },
            tool_call_event("call-3", "echo"),
            AgentEvent::ToolCallStarted {
                id: "call-3".to_string(),
                name: "echo".to_string(),
                background: false,
            },
            tool_completed_event("call-1", "ok-1"),
            tool_completed_event("call-2", "ok-2"),
            tool_completed_event("call-3", "ok-3"),
        ];

        let messages = build_messages(&empty_durable(), &tactical);

        // [user text, one Assistant message with 3 ToolUse blocks, one
        // User message with 3 ToolResult blocks] — not 7 separate messages.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].content.len(), 3);
        for (block, expected_id) in messages[1].content.iter().zip(["call-1", "call-2", "call-3"]) {
            match block {
                ContentBlock::ToolUse { id, .. } => assert_eq!(id, expected_id),
                other => panic!("expected a ToolUse block, got {other:?}"),
            }
        }
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].content.len(), 3);
        for (block, expected_id) in messages[2].content.iter().zip(["call-1", "call-2", "call-3"]) {
            match block {
                ContentBlock::ToolResult { tool_use_id, .. } => assert_eq!(tool_use_id, expected_id),
                other => panic!("expected a ToolResult block, got {other:?}"),
            }
        }

        crate::protocol_check::check_anthropic_message_protocol(&messages)
            .expect("grouped concurrent tool calls must be a protocol-valid sequence");
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

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-2.
    ///
    /// `round_trip_orders_summary_then_durable_then_tactical` above only
    /// exercises the case where `durable.summary` is already non-empty —
    /// then the prepended "[Resumen de contexto previo]" message is
    /// `Role::User` and happens to satisfy Anthropic's "first message must
    /// be user" rule regardless of what durable_events/tactical contain.
    ///
    /// But `SimpleContextCompactor::split` can populate `durable_events`
    /// (settled `AssistantToolCall`/`ToolCallCompleted` aged past the
    /// tactical window) **before any compaction has ever run** — i.e.
    /// while `summary` is still empty — purely because the log has grown
    /// past the compactor's `tactical_window`, independent of
    /// `Engine`'s `tactical_compaction_threshold`. `build_messages` then
    /// renders `durable_events` (an `Assistant` tool_use, here) before the
    /// `UserMessage` that's still sitting in `tactical`, even though that
    /// `UserMessage` chronologically preceded the tool call. Every
    /// Anthropic-backed session with tool calls crosses this exact shape
    /// once it passes ~20 events (`DEFAULT_TACTICAL_WINDOW`), well before
    /// the compaction threshold (default 40) ever triggers.
    ///
    /// Fixed: `build_messages_with_never_clear` now prepends a placeholder
    /// `User` message whenever `durable_events` is non-empty, even if
    /// `summary` is still empty — see the block right after the doc
    /// comment at the top of [`build_messages_with_never_clear`]. This
    /// doesn't reorder `durable_events` relative to `tactical` (that
    /// would need `ContextCompactor::split` — a frozen trait per
    /// PLAN.md — to preserve positional info it currently discards); it
    /// guarantees the one concrete rule this hallazgo actually violated
    /// (first message must be `role: user`) without touching that
    /// contract.
    #[test]
    fn build_messages_keeps_log_order_even_when_summary_is_still_empty() {
        // No CompactionOccurred has ever run — summary is empty — but the
        // tool call pair already aged out of the tactical window into
        // durable_events, exactly as `SimpleContextCompactor::split` does
        // once the log passes `tactical_window` events.
        let durable = DurableState {
            summary: String::new(),
            durable_events: vec![
                tool_call_event("call-1", "echo"),
                tool_completed_event("call-1", "ok"),
            ],
        };
        // The UserMessage that *caused* call-1 is still in `tactical`
        // (per the compactor's "no-silent-loss" orphan handling), even
        // though it precedes both durable events chronologically.
        let tactical = vec![
            AgentEvent::UserMessage {
                text: "please echo something".to_string(),
            },
            AgentEvent::AssistantText {
                text: "still working on it".to_string(),
            },
        ];

        let messages = build_messages(&durable, &tactical);

        crate::protocol_check::check_anthropic_message_protocol(&messages).expect(
            "build_messages must produce an Anthropic-valid sequence \
             (first message role=User) regardless of which side of the \
             durable/tactical split the log's oldest event landed on",
        );
    }
}
