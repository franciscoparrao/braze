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

/// How many of the tactical window's most recent observations
/// (`ToolCallCompleted` results) are *candidates* to be rendered in full;
/// anything older is collapsed to its first line (see
/// [`collapsed_observation_content`]). The count comes straight from
/// SWE-agent/ACI (arXiv 2405.15793, Tabla 3): collapsing old observations
/// to 1 line except the last 5 was worth +3.0 pp on SWE-bench Lite vs
/// keeping full history — old file dumps and shell output mostly
/// distract; what the model needs from them usually survives in its own
/// subsequent text. Tactical-side only: durable results are already fully
/// cleared by `event_to_block_cleared`.
///
/// "Candidate", not "guaranteed full": [`tactical_full_observation_indices`]
/// also caps the *aggregate* size of these — 5 observations each near a
/// single tool output's own cap can still add up to more tokens than a
/// small local model's entire `num_ctx` (docs/AUDITORIA-2026-07-v3.md,
/// hallazgo B1), a number this constant alone (borrowed from a paper
/// measured against a large-context model) never accounted for.
///
/// The default every production call site uses — `Engine` threads its
/// own `full_observations` field (which defaults to this same value)
/// through [`build_messages_with_full_observations`]/
/// [`render_tactical_events`] instead of reading this constant directly,
/// so `braze-bench`'s `+ablate:full-observations=N` (E1,
/// docs/AUDITORIA-2026-07-v3.md) can override it per sweep row — the
/// literature's "5" was tuned against GPT-4's large context, and this is
/// exactly the knob needed to measure whether that figure holds for a
/// small local model's tiny `num_ctx`.
pub(crate) const TACTICAL_FULL_OBSERVATIONS: usize = 5;

/// Default aggregate cap (chars) across every observation
/// [`tactical_full_observation_indices`] keeps full — without *some* cap,
/// `TACTICAL_FULL_OBSERVATIONS` full-size dumps (`braze-tools-local`'s own
/// per-output cap is ~8000 bytes) can add up to tens of thousands of
/// tokens on their own, enough to overflow a small local model's entire
/// `num_ctx` (8192 by default) before the event-count/token-budget
/// compaction trigger even runs a whole turn later.
///
/// This is a *default*, not a hard limit — `Engine::load_messages`
/// (docs/usability-log-2026-07-07-si2.md, hallazgo U-17) scales the actual
/// budget it passes down from this value only when a small, fixed context
/// window is genuinely in play (`context_budget_tokens` configured, i.e.
/// Ollama today); a cloud backend with no such budget gets a much more
/// generous one instead of inheriting a cap tuned for an 8192-token local
/// model. Every call site in this module still defaults to this constant
/// when it doesn't have (or care about) that context — see
/// [`build_messages_with_full_observations`].
pub(crate) const MAX_FULL_OBSERVATIONS_TOTAL_CHARS: usize = 8_000;

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
///
/// `full_observations` overrides how many of the tactical window's most
/// recent observations stay full instead of collapsing to one line (see
/// [`TACTICAL_FULL_OBSERVATIONS`]) — production always passes `Engine`'s
/// own field (which defaults to that same constant), while
/// `braze-bench`'s `+ablate:full-observations=N` (E1,
/// docs/AUDITORIA-2026-07-v3.md) can override it per sweep row.
///
/// `full_observations_byte_budget` is the aggregate cap on full
/// observations' combined size — see [`MAX_FULL_OBSERVATIONS_TOTAL_CHARS`]'s
/// doc comment and `Engine::load_messages` (hallazgo U-17,
/// docs/usability-log-2026-07-07-si2.md) for why callers with no small,
/// fixed context window to protect should pass a bigger one instead of
/// that constant.
pub fn build_messages_with_full_observations(
    durable: &DurableState,
    tactical: &[AgentEvent],
    full_observations: usize,
    full_observations_byte_budget: usize,
) -> Vec<Message> {
    build_messages_with_never_clear(
        durable,
        tactical,
        NEVER_CLEAR_TOOLS,
        full_observations,
        full_observations_byte_budget,
    )
}

/// Renders `durable_events` alone — no leading summary placeholder, no
/// tactical — through the exact same clearing logic
/// [`build_messages_with_full_observations`] applies to the durable side.
/// Exposed so `Engine`'s token-budget
/// estimator can size *what actually reaches the model* (an old
/// `ToolCallCompleted`'s content replaced by a short "cleared" placeholder)
/// instead of the raw, uncleared event payload (N-6,
/// docs/AUDITORIA-2026-07-v2.md) — `durable_events` never shrinks once
/// settled, so over-counting it means a budget-triggered compaction can
/// never bring the estimate back under budget, re-triggering forever.
pub(crate) fn render_durable_events(durable_events: &[AgentEvent]) -> Vec<Message> {
    let tool_names = tool_names_by_id(durable_events);
    let mut messages = Vec::with_capacity(durable_events.len());
    push_grouped(&mut messages, durable_events, |event| {
        event_to_block_cleared(event, &tool_names, NEVER_CLEAR_TOOLS)
    });
    messages
}

/// Implementation behind [`build_messages_with_full_observations`], parameterized on the
/// tool-result clearing exclusion list so tests can exercise both branches
/// without mutating the production `NEVER_CLEAR_TOOLS` constant.
fn build_messages_with_never_clear(
    durable: &DurableState,
    tactical: &[AgentEvent],
    never_clear: &[&str],
    full_observations: usize,
    full_observations_byte_budget: usize,
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

    messages.extend(render_tactical_events(
        tactical,
        full_observations,
        full_observations_byte_budget,
    ));
    messages
}

/// Renders the tactical slice alone — applying the same ACI collapse
/// [`build_messages_with_full_observations`] applies to it — so `Engine`'s token-budget estimator
/// can size *what actually reaches the model* instead of the raw,
/// uncollapsed observation payloads (mirrors [`render_durable_events`]'s
/// reasoning on the durable side; see docs/AUDITORIA-2026-07-v3.md,
/// hallazgo B2: sizing the tactical slice by its raw content could keep
/// tripping repeated compaction well past the point where the actual,
/// collapsed prompt was already back under budget).
pub(crate) fn render_tactical_events(
    tactical: &[AgentEvent],
    full_observations: usize,
    full_observations_byte_budget: usize,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(tactical.len());
    let full_indices = tactical_full_observation_indices(
        tactical,
        full_observations,
        full_observations_byte_budget,
    );
    let mut observations_seen = 0usize;
    push_grouped(&mut messages, tactical, |event| {
        if let AgentEvent::ToolCallCompleted { id, result } = event {
            let idx = observations_seen;
            observations_seen += 1;
            if !full_indices.contains(&idx) {
                return Some((
                    Role::User,
                    ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: collapsed_observation_content(&result.content, full_observations),
                        is_error: result.is_error,
                    },
                ));
            }
        }
        event_to_block(event)
    });
    messages
}

/// Decides which `ToolCallCompleted` observations (identified by their
/// 0-indexed position among all observations in `tactical`, oldest first)
/// stay full rather than collapsing — both the recency rule
/// ([`TACTICAL_FULL_OBSERVATIONS`]) and the aggregate size cap
/// ([`MAX_FULL_OBSERVATIONS_TOTAL_CHARS`], hallazgo B1). Walks the
/// newest-`TACTICAL_FULL_OBSERVATIONS` candidates from newest to oldest,
/// admitting each into the full set while the running total still fits —
/// the single newest observation is always admitted first regardless of
/// its own size, so one oversized dump can never zero out the "at least
/// the current turn's own output stays visible" guarantee.
fn tactical_full_observation_indices(
    tactical: &[AgentEvent],
    full_observations: usize,
    full_observations_byte_budget: usize,
) -> std::collections::HashSet<usize> {
    let observation_lens: Vec<usize> = tactical
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallCompleted { result, .. } => Some(result.content.len()),
            _ => None,
        })
        .collect();
    let total = observation_lens.len();

    let mut full = std::collections::HashSet::new();
    let mut running_total = 0usize;
    for (rev_i, &len) in observation_lens.iter().rev().enumerate() {
        if rev_i >= full_observations {
            break;
        }
        if running_total.saturating_add(len) > full_observations_byte_budget && !full.is_empty() {
            break;
        }
        running_total += len;
        full.insert(total - 1 - rev_i);
    }
    full
}

/// Renders an old tactical observation as its first line plus a marker
/// noting what was omitted — the ACI "collapse to 1 line" treatment (see
/// [`TACTICAL_FULL_OBSERVATIONS`]). Returns the content unchanged when
/// collapsing wouldn't actually save anything (a short, single-line
/// result), so the marker never *adds* tokens to an already-small
/// observation.
fn collapsed_observation_content(content: &str, full_observations: usize) -> String {
    /// Longest first-line excerpt kept — beyond this even the "1 line"
    /// gets cut (a minified single-line JSON dump is one line and still
    /// enormous).
    const FIRST_LINE_MAX_CHARS: usize = 160;

    /// `braze-tools-local::post_edit_check`'s feedback marker —
    /// acoplamiento por convención, same as
    /// `braze-model::escalation::POST_EDIT_CHECK_FAILURE_MARKER` (neither
    /// crate depends on the other just to share one string). The marker
    /// arrives on line 3+ of an edit's tool result ("\n\n[post-edit
    /// check] ..."), so a first-line-only collapse silently dropped it —
    /// and with it both the model's awareness that this old edit broke
    /// the build AND `EscalatingBackend`'s F3 classification, exactly in
    /// the long floundering turns where the collapse fires (I-3,
    /// docs/AUDITORIA-2026-07-v6.md).
    const POST_EDIT_CHECK_MARKER: &str = "[post-edit check]";

    let first_line = content.lines().next().unwrap_or("").trim_end();
    let excerpt: String = first_line.chars().take(FIRST_LINE_MAX_CHARS).collect();
    let preserved_marker =
        if content.contains(POST_EDIT_CHECK_MARKER) && !excerpt.contains(POST_EDIT_CHECK_MARKER) {
            // Kept compact: the classification only needs the marker's
            // presence, and the model only needs to know the regression
            // existed — the full compiler output stays omitted.
            format!(" {POST_EDIT_CHECK_MARKER} (build regression in this old edit)")
        } else {
            String::new()
        };
    let omitted = content.len().saturating_sub(excerpt.len());
    // A′.1 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.1):
    // the marker states what to DO, not just what happened — a frontier
    // model infers "I can re-run the tool"; a 3B doesn't. The recovery
    // recipe costs ~10 tokens per collapsed observation and is the whole
    // point of announcing the collapse at all.
    let collapsed = format!(
        "{excerpt}{preserved_marker} [old observation collapsed: {omitted} chars omitted; the {full_observations} most recent tool results are shown in full. Re-run the tool if you need the omitted content]"
    );
    if collapsed.len() >= content.len() {
        return content.to_string();
    }
    collapsed
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
    // `FnMut`, not `Fn`: the tactical renderer counts observations as it
    // goes (see the collapse pass in `build_messages_with_never_clear`).
    mut render: impl FnMut(&AgentEvent) -> Option<(Role, ContentBlock)>,
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
/// once per `build_messages_with_full_observations` call from every `AssistantToolCall` present
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
        AgentEvent::AssistantText { text } => {
            Some((Role::Assistant, ContentBlock::Text { text: text.clone() }))
        }
        // Iteración pre-registrada del planner (PLAN.md § "Split
        // planificador/ejecutor"; ejecutada 2026-07-10): the plan renders
        // as USER-role context, not as the assistant's own text. The
        // matrix sweep (docs/sweep-matriz-4brazos-2026-07-10.md) diagnosed
        // the old assistant-role render's dominant failure as
        // degeneration — empty responses in the round right after the
        // plan, worst on tasks whose correct output was plain text — the
        // signature of a small model treating "its own" plan message as
        // having already answered. As user-role context, the plan is
        // something to act on, not something already said.
        AgentEvent::PlanCreated { plan } => Some((
            Role::User,
            ContentBlock::Text {
                text: format!(
                    "Plan for this request (context from a planning pass — you have NOT \
                     executed any of it yet):\n{plan}"
                ),
            },
        )),
        // Verification gate (H2, docs/verification-lever-design-2026-07-22.md):
        // the model claimed the task done, but the configured verification
        // command failed. Render as USER-role context (like the plan) —
        // something the model must act on, not something it already said —
        // so the next round sees the real failure instead of the model's
        // unverified claim of success (finding #15).
        AgentEvent::VerificationFailed { output } => Some((
            Role::User,
            ContentBlock::Text {
                text: format!(
                    "The task is NOT done: your answer was accepted but the project's \
                     verification command then FAILED. This is the real output — fix what \
                     it reports, do not just repeat that it passes:\n{output}"
                ),
            },
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
        //
        // `HarnessNote` (A′.2) lives here since J-3
        // (docs/AUDITORIA-2026-07-v7.md): what the model sees is the
        // ephemeral request-scoped copy `Engine::run_turn` appends for
        // the emitting turn only — rendering the persisted event from
        // history kept stale "answer now, stop calling tools"
        // instructions alive in every later turn of the session.
        AgentEvent::HarnessNote { .. }
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::CompactionOccurred { .. }
        | AgentEvent::PermissionRequested { .. }
        | AgentEvent::PermissionDecided { .. }
        | AgentEvent::Usage { .. }
        // H-3 (docs/AUDITORIA-2026-07-v5.md) lever events: audit-only,
        // same as `Usage`/`CompactionOccurred` above.
        | AgentEvent::TextualRescueApplied { .. }
        | AgentEvent::EditFenceApplied { .. }
        | AgentEvent::EscalationToLead { .. }
        | AgentEvent::SummaryFallbackAttempted
        | AgentEvent::HookErrored { .. }
        | AgentEvent::SkillLoaded { .. }
        | AgentEvent::AgentsMdLoaded { .. }
        | AgentEvent::SkillLoadSkipped { .. }
        // Audit-only trace for downstream consumers (braze-memory's
        // ProjectMemoryHook) — never rendered back to the model.
        | AgentEvent::TaskCompleted { .. }
        // I.7: audit-only cost trace — the child's conclusion reaches
        // the model as the explore call's ToolCallCompleted, rendered
        // above like any other observation.
        | AgentEvent::ExplorationDelegated { .. }
        // SWE-Edit #17: audit-only cost trace — the child's state summary
        // reaches the model as the editor call's ToolCallCompleted,
        // rendered above like any other observation.
        | AgentEvent::EditorDelegated { .. }
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
        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &[],
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );
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

        let messages = build_messages_with_full_observations(
            &durable,
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

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
        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );
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

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

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
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ];

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );
        assert!(messages.is_empty());
    }

    /// Iteración pre-registrada del planner (2026-07-10): `PlanCreated`
    /// renders as USER-role context — the old assistant-role render was
    /// diagnosed as the degeneration artifact (the model treats "its
    /// own" plan as having already answered; see the arm in
    /// docs/sweep-matriz-4brazos-2026-07-10.md).
    #[test]
    fn plan_created_renders_as_user_context_with_a_plan_prefix() {
        let tactical = vec![
            AgentEvent::UserMessage {
                text: "haz tres cosas".to_string(),
            },
            AgentEvent::PlanCreated {
                plan: "1. leer\n2. editar\n3. verificar".to_string(),
            },
        ];

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

        // Two user messages (text events don't group like tool blocks
        // do) — the request, then the plan as user-role context.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, Role::User);
        match &messages[1].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.starts_with("Plan for this request"), "got: {text}");
                assert!(text.contains("you have NOT executed any of it yet"));
                assert!(text.contains("2. editar"));
            }
            other => panic!("expected a Text block, got {other:?}"),
        }
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

        let messages = build_messages_with_full_observations(
            &durable,
            &[],
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

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

        let messages = build_messages_with_never_clear(
            &durable,
            &[],
            &["keep_me"],
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

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

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

        assert_eq!(messages.len(), 1);
        match &messages[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, &long_content);
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
    }

    // --- colapso de observaciones viejas (ACI, ítem 4 del backlog
    // 2026-07-06 — ver TACTICAL_FULL_OBSERVATIONS) ---

    /// Extracts every ToolResult content in render order, flattening the
    /// grouped messages — collapse tests only care about the payloads.
    fn tool_result_contents(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn old_tactical_observations_collapse_but_the_last_five_stay_full() {
        // 7 observations, each long enough that collapsing saves space.
        let mut tactical = Vec::new();
        for i in 0..7 {
            let id = format!("call-{i}");
            tactical.push(tool_call_event(&id, "read_file"));
            tactical.push(tool_completed_event(
                &id,
                &format!("primera linea {i}\n{}", "x".repeat(500)),
            ));
        }

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );
        let contents = tool_result_contents(&messages);
        assert_eq!(contents.len(), 7);

        // The first 2 (7 - 5) collapse to first line + marker...
        for (i, content) in contents.iter().take(2).enumerate() {
            assert!(
                content.starts_with(&format!("primera linea {i} ")),
                "observation {i} should start with its first line, got: {content}"
            );
            assert!(
                content.contains("old observation collapsed"),
                "observation {i} should carry the collapse marker"
            );
            assert!(
                !content.contains("xxx"),
                "observation {i} leaked its collapsed body"
            );
        }
        // ...and the newest 5 stay verbatim.
        for (i, content) in contents.iter().enumerate().skip(2) {
            assert!(
                content.contains(&"x".repeat(500)),
                "observation {i} (within the last 5) must stay full"
            );
        }
    }

    #[test]
    fn with_five_or_fewer_observations_nothing_collapses() {
        let mut tactical = Vec::new();
        for i in 0..5 {
            let id = format!("call-{i}");
            tactical.push(tool_call_event(&id, "read_file"));
            tactical.push(tool_completed_event(&id, &"z".repeat(400)));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        ));
        assert!(contents.iter().all(|c| c == &"z".repeat(400)));
    }

    // --- overriding full_observations (hallazgo E1, docs/AUDITORIA-2026-07-v3.md) ---

    #[test]
    fn overriding_full_observations_to_one_collapses_everything_but_the_newest() {
        // Same 5-observation fixture that `with_five_or_fewer_observations_nothing_collapses`
        // asserts stays entirely uncollapsed under the *default* (5) — with
        // an override of 1 (as `braze-bench`'s `+ablate:full-observations=1`
        // would set), only the single newest observation should survive
        // in full.
        let mut tactical = Vec::new();
        for i in 0..5 {
            let id = format!("call-{i}");
            tactical.push(tool_call_event(&id, "read_file"));
            tactical.push(tool_completed_event(
                &id,
                &format!("linea {i}\n{}", "z".repeat(400)),
            ));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            1,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        ));

        assert_eq!(contents.len(), 5);
        for (i, content) in contents.iter().take(4).enumerate() {
            assert!(
                content.contains("old observation collapsed"),
                "observation {i} should have collapsed under full_observations=1, got: {content}"
            );
        }
        assert!(
            contents[4].contains(&"z".repeat(400)),
            "the single newest observation must still stay full"
        );
    }

    // --- aggregate cap on full observations (hallazgo B1,
    // docs/AUDITORIA-2026-07-v3.md) ---

    #[test]
    fn five_large_observations_do_not_all_stay_full_despite_being_within_the_last_five() {
        // 5 observations at 3000 chars each = 15,000 chars — within the
        // last-5 recency window, but the aggregate cap
        // (MAX_FULL_OBSERVATIONS_TOTAL_CHARS = 8_000) can't fit all 5.
        let mut tactical = Vec::new();
        for i in 0..5 {
            let id = format!("call-{i}");
            tactical.push(tool_call_event(&id, "read_file"));
            tactical.push(tool_completed_event(&id, &"x".repeat(3000)));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        ));
        assert_eq!(contents.len(), 5);

        let full_count = contents
            .iter()
            .filter(|c| c.len() == 3000 && !c.contains("collapsed"))
            .count();
        let collapsed_count = contents.iter().filter(|c| c.contains("collapsed")).count();

        assert!(
            full_count < 5,
            "the aggregate cap must prevent all 5 large observations from staying full, got {full_count} full"
        );
        assert!(
            full_count >= 1,
            "the newest observation must always stay full"
        );
        assert_eq!(full_count + collapsed_count, 5);
        // The newest one (last in iteration order) must be among the full
        // ones — the current turn's own output must stay visible.
        assert_eq!(contents.last().unwrap().len(), 3000);
    }

    /// Regression test for U-17 (docs/usability-log-2026-07-07-si2.md): the
    /// exact same 5 large observations as the test above, but with a wider
    /// budget — the shape `Engine::full_observations_byte_budget` produces
    /// when no small context window is configured. All 5 must now stay
    /// full instead of collapsing down to essentially one.
    #[test]
    fn a_wider_budget_lets_all_five_large_observations_stay_full() {
        let mut tactical = Vec::new();
        for i in 0..5 {
            let id = format!("call-{i}");
            tactical.push(tool_call_event(&id, "read_file"));
            tactical.push(tool_completed_event(&id, &"x".repeat(3000)));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS * 10,
        ));
        assert_eq!(contents.len(), 5);

        let full_count = contents
            .iter()
            .filter(|c| c.len() == 3000 && !c.contains("collapsed"))
            .count();
        assert_eq!(
            full_count, 5,
            "a wide-enough budget must keep every one of the last 5 observations full, got {full_count}"
        );
    }

    #[test]
    fn many_small_observations_within_the_aggregate_cap_all_stay_full() {
        // Same shape as `old_tactical_observations_collapse_but_the_last_five_stay_full`
        // but re-asserted here to pin that the aggregate cap doesn't
        // regress the common case where observations are small: 5 × ~500
        // chars is well under the 8,000-char cap.
        let mut tactical = Vec::new();
        for i in 0..5 {
            let id = format!("call-{i}");
            tactical.push(tool_call_event(&id, "read_file"));
            tactical.push(tool_completed_event(&id, &"y".repeat(500)));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        ));
        assert!(
            contents
                .iter()
                .all(|c| c.len() == 500 && !c.contains("collapsed")),
            "small observations under the aggregate cap must all stay full"
        );
    }

    #[test]
    fn a_collapsed_observation_preserves_its_id_and_error_flag() {
        let mut tactical = vec![AgentEvent::ToolCallCompleted {
            id: "call-old".to_string(),
            result: ToolResult {
                tool_call_id: "call-old".to_string(),
                content: format!("fallo: no existe\n{}", "detalle ".repeat(100)),
                is_error: true,
            },
        }];
        for i in 0..5 {
            tactical.push(tool_completed_event(&format!("call-{i}"), "ok"));
        }

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );
        match &messages[0].content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "call-old");
                assert!(*is_error, "the error flag must survive the collapse");
                assert!(content.starts_with("fallo: no existe"));
                assert!(content.contains("collapsed"));
            }
            other => panic!("expected a ToolResult block, got {other:?}"),
        }
    }

    #[test]
    fn a_short_old_observation_is_left_untouched_by_the_collapse() {
        // Collapsing "ok" would *add* tokens (the marker) — the no-op
        // guard in `collapsed_observation_content` must kick in.
        let mut tactical = vec![tool_completed_event("call-old", "ok")];
        for i in 0..5 {
            tactical.push(tool_completed_event(&format!("call-{i}"), &"w".repeat(300)));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        ));
        assert_eq!(contents[0], "ok");
    }

    #[test]
    fn a_huge_single_line_old_observation_still_collapses() {
        // Minified single-line JSON: one "line" but enormous — the
        // excerpt cap (not the line split) is what bounds it.
        let mut tactical = vec![tool_completed_event("call-old", &"j".repeat(5_000))];
        for i in 0..5 {
            tactical.push(tool_completed_event(&format!("call-{i}"), "ok"));
        }

        let contents = tool_result_contents(&build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        ));
        assert!(contents[0].len() < 400, "got len {}", contents[0].len());
        assert!(contents[0].contains("collapsed"));
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

        let messages = build_messages_with_full_observations(
            &empty_durable(),
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

        // [user text, one Assistant message with 3 ToolUse blocks, one
        // User message with 3 ToolResult blocks] — not 7 separate messages.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].content.len(), 3);
        for (block, expected_id) in messages[1]
            .content
            .iter()
            .zip(["call-1", "call-2", "call-3"])
        {
            match block {
                ContentBlock::ToolUse { id, .. } => assert_eq!(id, expected_id),
                other => panic!("expected a ToolUse block, got {other:?}"),
            }
        }
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].content.len(), 3);
        for (block, expected_id) in messages[2]
            .content
            .iter()
            .zip(["call-1", "call-2", "call-3"])
        {
            match block {
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, expected_id)
                }
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

        let messages = build_messages_with_full_observations(
            &durable,
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

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
    /// `Engine`'s `tactical_compaction_threshold`. `build_messages_with_full_observations` then
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

        let messages = build_messages_with_full_observations(
            &durable,
            &tactical,
            TACTICAL_FULL_OBSERVATIONS,
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS,
        );

        crate::protocol_check::check_anthropic_message_protocol(&messages).expect(
            "build_messages_with_full_observations must produce an Anthropic-valid sequence \
             (first message role=User) regardless of which side of the \
             durable/tactical split the log's oldest event landed on",
        );
    }

    // --- collapsed_observation_content marker preservation (I-3,
    // docs/AUDITORIA-2026-07-v6.md) ---

    /// The post-edit marker lives on line 3+ of an edit's result ("\n\n
    /// [post-edit check] ...") — a first-line-only collapse used to drop
    /// it, losing both the model's awareness of the old regression and
    /// `EscalatingBackend`'s F3 classification, exactly in the long
    /// floundering turns where the collapse fires.
    #[test]
    fn collapsing_preserves_the_post_edit_check_marker_from_later_lines() {
        let content = format!(
            "edited src/lib.rs (replaced 1 occurrence)\n\n[post-edit check] `cargo` (exit 101) \
             in /tmp/x after this edit (the edit itself was applied). Fix these before moving \
             on:\nerror[E0308]: mismatched types{}",
            " and much more compiler output".repeat(20)
        );
        let collapsed = collapsed_observation_content(&content, 5);
        assert!(collapsed.len() < content.len(), "must actually collapse");
        assert!(
            collapsed.contains("[post-edit check]"),
            "the F3 classification marker must survive the collapse: {collapsed}"
        );
        assert!(
            collapsed.contains("[old observation collapsed:"),
            "still marked as collapsed: {collapsed}"
        );
        // A′.1: the marker carries the recovery recipe — a 3B doesn't
        // infer "I can re-run the tool" from a bare "chars omitted".
        assert!(
            collapsed.contains("Re-run the tool"),
            "the collapse marker must tell the model how to recover: {collapsed}"
        );
    }

    /// An observation without the marker gets no marker invented for it —
    /// the preservation is conditional, not a blanket suffix.
    #[test]
    fn collapsing_adds_no_marker_when_the_content_never_had_one() {
        let content = format!("line one of a big result\n{}", "more lines\n".repeat(100));
        let collapsed = collapsed_observation_content(&content, 5);
        assert!(collapsed.len() < content.len());
        assert!(!collapsed.contains("[post-edit check]"));
    }

    /// A marker already on the first line isn't duplicated by the
    /// preservation pass.
    #[test]
    fn a_marker_already_in_the_excerpt_is_not_duplicated() {
        let content = format!(
            "[post-edit check] regression right on line one\n{}",
            "filler\n".repeat(100)
        );
        let collapsed = collapsed_observation_content(&content, 5);
        assert_eq!(
            collapsed.matches("[post-edit check]").count(),
            1,
            "got: {collapsed}"
        );
    }
}
