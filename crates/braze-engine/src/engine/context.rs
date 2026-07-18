//! Reparación de historia y presupuesto de contexto — P1.1 paso 2 del
//! split de `engine.rs` (docs/AUDITORIA-2026-07-v8.md § 3). Extraído
//! VERBATIM de `engine/mod.rs` (2026-07-18): funciones libres, cero
//! `&self` — los métodos de carga (`load_messages`/`load_and_repair`)
//! siguen en `mod.rs` y migran aquí cuando el split toque los `impl`.
//!
//! Dos familias:
//! - **Reparación de huérfanos**: un `AssistantToolCall` sin su
//!   `ToolCallCompleted` (proceso interrumpido) recibe un resultado
//!   sintético de error en el log — `synthesize_orphan_repairs` es API
//!   pública del crate (la usa braze-cli para inspección offline).
//! - **Presupuesto**: estimadores de tokens (~4 chars/token sobre el
//!   texto visible, no sobre `Debug`) y el escalado de la ventana
//!   táctica/observaciones completas según `context_budget_tokens`.

use std::collections::HashSet;

use braze_events::AgentEvent;
use braze_session::DurableState;
use braze_types::{ContentBlock, Message, ToolResult};

use super::DEFAULT_TACTICAL_COMPACTION_THRESHOLD;
use crate::history::{MAX_FULL_OBSERVATIONS_TOTAL_CHARS, TACTICAL_FULL_OBSERVATIONS, render_durable_events, render_tactical_events};

/// Ids of every `AssistantToolCall` in `events` with no matching
/// `ToolCallCompleted` anywhere in the same slice.
pub(crate) fn orphaned_tool_call_ids(events: &[AgentEvent]) -> Vec<String> {
    let completed_ids: HashSet<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallCompleted { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();

    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantToolCall { id, .. } if !completed_ids.contains(id.as_str()) => {
                Some(id.clone())
            }
            _ => None,
        })
        .collect()
}

/// The synthetic error `ToolCallCompleted` a crashed/interrupted orphaned
/// tool call is repaired with.
pub(crate) fn build_orphan_repair(id: String) -> AgentEvent {
    AgentEvent::ToolCallCompleted {
        id: id.clone(),
        result: ToolResult {
            tool_call_id: id,
            content: "tool call interrupted: the process ended before a result \
                      was received for it (crash, kill, or power loss). Retry it \
                      if it is still needed."
                .to_string(),
            is_error: true,
        },
    }
}

/// Pure: the synthetic `ToolCallCompleted` repairs for every orphaned
/// `AssistantToolCall` in `events` — see
/// `Engine::repair_orphaned_tool_calls`'s doc comment for the scenario
/// this addresses (a crashed/killed process leaving a `tool_use` with no
/// result). Exposed as a free function, not just inlined in that method,
/// so `braze-tui`'s backtrack can apply the identical repair to the
/// *replicated* prefix it writes into a new session (N-26,
/// docs/AUDITORIA-2026-07-v2.md) — without this, backtracking to a point
/// whose log prefix contains an orphaned tool_use but not its (later)
/// repair event copies the orphan into the new session with nothing to
/// fix it, poisoning it from the start.
pub fn synthesize_orphan_repairs(events: &[AgentEvent]) -> Vec<AgentEvent> {
    orphaned_tool_call_ids(events)
        .into_iter()
        .map(build_orphan_repair)
        .collect()
}

/// Returns `id` unchanged if it isn't already in `known_ids` (registering
/// it there); otherwise mints `"{id}-dup{n}"` for the smallest `n` that
/// isn't itself already known, registers *that*, and returns it — see the
/// call site in `Engine::dispatch_tool_calls` (N-14,
/// docs/AUDITORIA-2026-07-v2.md) for why a colliding id must never reach
/// the append-only session log unchanged.
pub(crate) fn ensure_unique_tool_call_id(id: String, known_ids: &mut HashSet<String>) -> String {
    if known_ids.insert(id.clone()) {
        return id;
    }

    let mut suffix = 1u32;
    loop {
        let candidate = format!("{id}-dup{suffix}");
        if known_ids.insert(candidate.clone()) {
            tracing::warn!(
                original_id = %id,
                renamed_id = %candidate,
                "tool_use id collided with one already used in this session; \
                 renaming to keep ids unique"
            );
            return candidate;
        }
        suffix += 1;
    }
}

/// Finds the earliest index into `tactical` that keeps at least
/// `min_keep` trailing events *and* never splits an `AssistantToolCall`
/// from its matching `ToolCallCompleted` — the tail cut used for the raw
/// live window kept verbatim after a compaction (N-1,
/// docs/AUDITORIA-2026-07-v2.md).
///
/// A blind `tactical.len() - min_keep` cut can land between a tool call
/// and its result: `AssistantToolCall`/`ToolCallCompleted` always appear
/// in that relative order (dispatch persists the former before the
/// latter), so if a `ToolCallCompleted` ends up inside the kept tail
/// while its `AssistantToolCall` falls just before the cut, the resulting
/// request has a `tool_result` with no matching `tool_use` — Anthropic
/// rejects that outright. The reverse (a `tool_use` kept without its
/// result) can't happen from this cut alone, since a result's index is
/// always *after* its call's, so keeping the call end never excludes an
/// already-included result.
///
/// Extends `start` backward, re-scanning after every extension, until no
/// `AssistantToolCall` before `start` has its `ToolCallCompleted` at or
/// after `start` — i.e. until the cut point falls on a pair boundary.
/// `tactical` here is always the small in-memory raw window between
/// compactions (bounded well below `tactical_compaction_threshold`), so
/// the worst-case quadratic re-scan is negligible in practice.
pub(crate) fn pair_aware_tail_start(tactical: &[AgentEvent], min_keep: usize) -> usize {
    let mut start = tactical.len().saturating_sub(min_keep);

    loop {
        let completed_ids_in_tail: std::collections::HashSet<&str> = tactical[start..]
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCallCompleted { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();

        let earliest_required = tactical[..start]
            .iter()
            .enumerate()
            .filter_map(|(i, event)| match event {
                AgentEvent::AssistantToolCall { id, .. }
                    if completed_ids_in_tail.contains(id.as_str()) =>
                {
                    Some(i)
                }
                _ => None,
            })
            .min();

        match earliest_required {
            Some(new_start) if new_start < start => start = new_start,
            _ => return start,
        }
    }
}

/// Folds a freshly-compacted tactical summary into `durable.summary`,
/// preferring not to introduce a stray leading separator when the durable
/// summary was empty (e.g. the very first compaction of a session).
pub(crate) fn merge_summary(mut durable: DurableState, summary: String) -> DurableState {
    if durable.summary.is_empty() {
        durable.summary = summary;
    } else {
        durable.summary = format!("{} {summary}", durable.summary);
    }
    durable
}


/// Rough token estimate (~4 chars/token) for the tactical events about to
/// be dropped from raw context by a compaction pass — mirrors the same
/// heuristic `SimpleContextCompactor::compact_tactical` already uses
/// internally, applied here to fill `AgentEvent::CompactionOccurred`'s
/// `dropped_tokens_estimate` field from the engine's side.
///
/// Bajo (docs/AUDITORIA-2026-07-v2.md, "estimador de tokens sobre Debug
/// repr, infla 30-50%"): counts only each event's user-visible text (same
/// principle as [`estimate_message_tokens`]'s doc comment), not a
/// `format!("{event:?}")` dump — the `Debug` form pads the count with
/// field names, `Some(...)`/enum-variant punctuation, and (for
/// `AssistantToolCall`) the raw `serde_json::Value` debug form instead of
/// its compact string one.
pub(crate) fn estimate_dropped_tokens(events: &[AgentEvent]) -> u32 {
    let chars: usize = events.iter().map(event_text_len).sum();
    (chars / 4) as u32
}

/// User-visible text length of one `AgentEvent`, for [`estimate_dropped_tokens`].
/// Audit-only variants (`ToolCallStarted`, `Usage`, `Unknown`) carry
/// nothing that ever reaches the model, so they count as `0`.
pub(crate) fn event_text_len(event: &AgentEvent) -> usize {
    match event {
        AgentEvent::UserMessage { text } | AgentEvent::AssistantText { text } => text.len(),
        AgentEvent::PlanCreated { plan } => plan.len(),
        AgentEvent::AssistantToolCall {
            name, arguments, ..
        } => name.len() + arguments.to_string().len(),
        AgentEvent::ToolCallCompleted { result, .. } => result.content.len(),
        AgentEvent::CompactionOccurred { summary, .. } => summary.len(),
        AgentEvent::PermissionRequested { action, .. }
        | AgentEvent::PermissionDecided { action, .. } => action.len(),
        // A′.2 + J-3: what reaches the model is the ephemeral
        // request-scoped copy, not this persisted event — audit-only
        // here, like the lever events below.
        AgentEvent::HarnessNote { .. } => 0,
        AgentEvent::ToolCallStarted { .. }
        | AgentEvent::Usage { .. }
        // H-3 (docs/AUDITORIA-2026-07-v5.md) lever events: audit-only,
        // same as `Usage` above — never reached the model, nothing to
        // count as dropped.
        | AgentEvent::TextualRescueApplied { .. }
        | AgentEvent::EscalationToLead { .. }
        | AgentEvent::SummaryFallbackAttempted
        | AgentEvent::HookErrored { .. }
        | AgentEvent::SkillLoaded { .. }
        | AgentEvent::SkillLoadSkipped { .. }
        | AgentEvent::TaskCompleted { .. }
        | AgentEvent::Unknown => 0,
    }
}

/// Ceiling for how far the three tactical caps scale above their
/// literature/local-model-tuned defaults: applied outright when the
/// caller *hasn't* configured a context budget (cloud backends — see
/// [`full_observations_byte_budget`]'s doc comment for the failure mode),
/// and as the upper clamp of [`tactical_cap_scale`]'s budget-proportional
/// scaling when one IS configured — a 128K-context local model shouldn't
/// get a wider tactical window than cloud backends do.
pub(crate) const NO_CONTEXT_BUDGET_SCALE_MULTIPLIER: usize = 10;

/// The single multiplier the three tactical caps
/// ([`full_observations_byte_budget`],
/// [`effective_tactical_compaction_threshold`],
/// [`effective_tactical_full_observations`]) share, derived from the
/// session's actual context budget instead of its mere presence (I-2,
/// docs/AUDITORIA-2026-07-v6.md).
///
/// The v5-era logic was binary: `Some(_)` → defaults, `None` → ×10. That
/// fixed the U-17/U-18/U-19 re-read loops for cloud backends but left
/// them fully alive for large local models — a qwen3.5-coder with
/// `num_ctx=32768` on a LAN node got the same minimal caps (8KB of full
/// observations ≈ ONE `read_file` page under braze-tools-local's ~8KB
/// per-call cap) as a 3B model on an 8K window, for exactly the
/// population this project's thesis targets.
///
/// Anchoring: the historical 8 000-char default was tuned for
/// `num_ctx=8192`, whose budget (~6 000 tokens ≈ 24 000 chars at ~4
/// chars/token) makes it one *third* of the budget in chars. Keeping
/// that ratio: `scale = (budget_tokens×4/3) / MAX_FULL_OBSERVATIONS_TOTAL_CHARS`,
/// floored at 1 (never below the tuned defaults — they're the protective
/// minimum, not a starting point to shrink) and capped at
/// [`NO_CONTEXT_BUDGET_SCALE_MULTIPLIER`] (never above what cloud gets).
/// At the 8K reference this yields exactly 1 — byte-identical behavior
/// for the small local models the defaults were tuned on; at
/// `num_ctx=32768` it yields ×5; `None` (cloud) keeps the flat ×10.
pub(crate) fn tactical_cap_scale(context_budget_tokens: Option<u32>) -> usize {
    match context_budget_tokens {
        None => NO_CONTEXT_BUDGET_SCALE_MULTIPLIER,
        Some(budget_tokens) => {
            // ~4 chars/token, same heuristic as `estimate_message_tokens`.
            let budget_chars = budget_tokens as usize * 4;
            (budget_chars / 3 / MAX_FULL_OBSERVATIONS_TOTAL_CHARS)
                .clamp(1, NO_CONTEXT_BUDGET_SCALE_MULTIPLIER)
        }
    }
}

/// The aggregate byte cap [`crate::history::render_tactical_events`]
/// enforces across the observations it keeps full, sized to whether this
/// session actually has a small, fixed context window to protect
/// (`context_budget_tokens`, set only for Ollama today — see
/// [`Engine::with_context_budget`]).
///
/// docs/usability-log-2026-07-07-si2.md, hallazgo U-17: the flat
/// `MAX_FULL_OBSERVATIONS_TOTAL_CHARS` (8 000 chars) default was applied
/// unconditionally to *every* backend, including cloud ones with context
/// windows two orders of magnitude bigger than the small local model it
/// was tuned for. Once `read_file` pages routinely land near
/// `braze_tools_local`'s own ~8 000-byte per-call cap (as they do for any
/// real source file bigger than one default page), a *single* such
/// observation already consumes the entire budget — the newest stays
/// full unconditionally, but the very next one already blows past 8 000
/// combined and gets excluded. In practice this collapsed "the last 5
/// observations stay full" down to "the last 1 does", for exactly the
/// backends least likely to need that protection. Five different models
/// hit the resulting thrash trying to read a single ~700-line file
/// (hallazgo U-6 and its repeats) before this was diagnosed.
///
/// A backend with a real small-context concern keeps the original,
/// literature-adjacent 8 000-char default unchanged — the budget only
/// widens in proportion to how much context the session actually has
/// (I-2, docs/AUDITORIA-2026-07-v6.md — the v5-era version widened it
/// only when NO budget was configured at all, leaving the U-17 collapse
/// fully alive for large local models; see [`tactical_cap_scale`]).
pub(crate) fn full_observations_byte_budget(context_budget_tokens: Option<u32>) -> usize {
    MAX_FULL_OBSERVATIONS_TOTAL_CHARS * tactical_cap_scale(context_budget_tokens)
}

/// The event-count threshold past which [`Engine::load_messages`] folds
/// the whole tactical window into a fresh `CompactionOccurred` summary,
/// scaled the same way [`full_observations_byte_budget`] is — unchanged
/// when a small, fixed context window is actually configured
/// (`context_budget_tokens`, Ollama today), scaled up otherwise.
///
/// docs/usability-log-2026-07-07-si2.md, hallazgo U-18: U-17 (the byte
/// budget above) turned out to only be half the story. `configured`
/// defaults to [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] (40 raw events)
/// regardless of backend — and unlike the per-observation collapse U-17
/// addresses (which still keeps a short excerpt), a full compaction
/// discards *everything*: `SimpleContextCompactor::compact_tactical`'s
/// summary is a bare "Tools used: read_file(path), read_file(path), ..."
/// list with no content, no line ranges, nothing the model could use to
/// avoid re-reading. A task needing ~3 tool-call events per `read_file`
/// round trip (request/started/completed) blows through 40 events after
/// roughly a dozen reads — well short of what a multi-file real-code
/// investigation needs — forcing a full memory wipe mid-task, observed
/// live to repeat 3 times across 36 `read_file` calls in one turn against
/// `z-ai/glm-5.2` (which explicitly narrated noticing this: "the read_file
/// results keep getting cleared from context").
///
/// Only scales `configured` when it's still exactly
/// [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] — i.e. nobody set it to
/// anything else. Without this guard, the very first version of this fix
/// silently multiplied *any* value by 10, including an explicit
/// `+ablate:tactical-threshold=N` from `braze-bench` (`AblationOverrides`,
/// `crates/braze-bench/src/backend_spec.rs`) — a knob that exists
/// specifically so a sweep can study the effect of *that exact* value.
/// Corrupting a deliberately-chosen ablation value is worse than not
/// scaling at all: it makes the sweep measure a different experiment than
/// the one requested, silently. The equality check isn't perfectly
/// precise (an override that happens to equal the default still gets
/// scaled), but that's a low-stakes false positive compared to the
/// alternative.
pub(crate) fn effective_tactical_compaction_threshold(
    configured: usize,
    context_budget_tokens: Option<u32>,
) -> usize {
    if configured != DEFAULT_TACTICAL_COMPACTION_THRESHOLD {
        return configured;
    }
    configured * tactical_cap_scale(context_budget_tokens)
}

/// Same reasoning and the same corruption risk as
/// [`effective_tactical_compaction_threshold`], for
/// [`Engine::tactical_full_observations`] instead — `braze-bench`'s
/// `+ablate:full-observations=N` (`AblationOverrides`) exists to study
/// this exact value too, so this only scales the untouched
/// [`crate::history::TACTICAL_FULL_OBSERVATIONS`] default, never an
/// explicit override.
///
/// docs/usability-log-2026-07-07-si2.md, hallazgo U-19: U-17/U-18 fixed
/// the byte-budget and compaction-threshold layers, but
/// `tactical_full_observation_indices` (`crates/braze-engine/src/history.rs`)
/// has a *third*, independent cap this project hadn't touched yet — only
/// the newest `full_observations` (5 by default) observations are even
/// *candidates* to stay full, regardless of how generous the byte budget
/// is. A retry against `z-ai/glm-5.2` with U-17+U-18 both live confirmed
/// this: zero compactions fired (U-18 held), yet the model still re-read
/// the same ~700-line file 3 times over within a single 20-round-trip
/// turn — the count-based cap, not the byte budget or compaction, was the
/// binding constraint the whole time for a task needing ~10-12 reads to
/// cover its files once.
pub(crate) fn effective_tactical_full_observations(
    configured: usize,
    context_budget_tokens: Option<u32>,
) -> usize {
    if configured != TACTICAL_FULL_OBSERVATIONS {
        return configured;
    }
    configured * tactical_cap_scale(context_budget_tokens)
}

/// Rough token estimate for the *entire* durable+tactical portion of the
/// next model request — everything [`crate::history::build_messages`]
/// would turn into `Message`s, not just the tactical slice about to be
/// (maybe) compacted. Used by [`Engine::load_messages`] to decide whether
/// the prompt is approaching `context_budget_tokens`, since a raw event
/// *count* alone can't tell a two-word `AssistantText` apart from a
/// `ToolCallCompleted` carrying a 200KB file dump.
///
/// Both sides are measured through what `build_messages` would actually
/// render, not the raw event payload: `durable_events` through
/// [`render_durable_events`] (N-6, docs/AUDITORIA-2026-07-v2.md) and
/// `tactical` through [`render_tactical_events`] (B2,
/// docs/AUDITORIA-2026-07-v3.md — the ACI collapse can shrink old
/// observations to one line, so estimating the raw content overstates
/// what's actually sent and can trigger compaction that doesn't reduce
/// the *real* prompt at all). `durable_events` never shrinks once settled
/// (compaction only ever folds `tactical`), so over-counting it here
/// means the estimate can never drop back under budget no matter how many
/// times `load_messages` compacts — exactly the "modo compactación
/// permanente" pathology A2/C2 already fixed for the event-count trigger,
/// reachable again through either uncollapsed side.
pub(crate) fn estimate_prompt_tokens(
    durable: &DurableState,
    tactical: &[AgentEvent],
    full_observations: usize,
    full_observations_byte_budget: usize,
) -> u32 {
    let summary_tokens = (durable.summary.len() / 4) as u32;
    let durable_events_tokens =
        estimate_message_tokens(&render_durable_events(&durable.durable_events));
    let tactical_tokens = estimate_message_tokens(&render_tactical_events(
        tactical,
        full_observations,
        full_observations_byte_budget,
    ));
    summary_tokens + durable_events_tokens + tactical_tokens
}

/// Rough token estimate (~4 chars/token) over already-rendered `Message`s
/// — every `ContentBlock` variant's user-visible text, not a `Debug` dump
/// of the whole struct (which would double-count field names/punctuation
/// and, for `ToolUse`, the *raw* `serde_json::Value` rather than its
/// compact string form).
pub(crate) fn estimate_message_tokens(messages: &[Message]) -> u32 {
    let chars: usize = messages
        .iter()
        .flat_map(|message| &message.content)
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
        })
        .sum();
    (chars / 4) as u32
}
