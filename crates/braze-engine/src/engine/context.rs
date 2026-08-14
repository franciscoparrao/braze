//! Reparación de historia y presupuesto de contexto — P1.1 pasos 2 y 4
//! del split de `engine.rs` (docs/AUDITORIA-2026-07-v8.md § 3).
//! Extraído VERBATIM de `engine/mod.rs` (2026-07-18): las funciones
//! libres (paso 2) y los métodos de carga/reparación
//! (`load_and_repair`/`load_messages`/`repair_orphaned_tool_calls`,
//! paso 4 — el bloque `impl Engine` del final del archivo).
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
use crate::history::{
    MAX_FULL_OBSERVATIONS_TOTAL_CHARS, TACTICAL_FULL_OBSERVATIONS, render_durable_events,
    render_tactical_events,
};

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
        AgentEvent::VerificationFailed { output } => output.len(),
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
        | AgentEvent::EditFenceApplied { .. }
        | AgentEvent::EscalationToLead { .. }
        | AgentEvent::SummaryFallbackAttempted
        | AgentEvent::HookErrored { .. }
        | AgentEvent::SkillLoaded { .. }
        | AgentEvent::AgentsMdLoaded { .. }
        | AgentEvent::SkillLoadSkipped { .. }
        | AgentEvent::TaskCompleted { .. }
        | AgentEvent::ExplorationDelegated { .. }
        | AgentEvent::EditorDelegated { .. }
        // SC-retention: the constraint is never dropped by a compaction —
        // its verbatim copy re-renders from `DurableState::constraints`
        // on every request — so it counts 0 here (nothing is lost) and
        // is instead counted by `estimate_prompt_tokens` on the durable
        // side.
        | AgentEvent::SessionConstraintDeclared { .. }
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
    // SC-retention: the constraints block renders on every request, so
    // the budget must see it — same ~4 chars/token heuristic as the
    // summary (the framing header is small enough to ignore).
    let constraints_tokens =
        (durable.constraints.iter().map(String::len).sum::<usize>() / 4) as u32;
    let durable_events_tokens =
        estimate_message_tokens(&render_durable_events(&durable.durable_events));
    let tactical_tokens = estimate_message_tokens(&render_tactical_events(
        tactical,
        full_observations,
        full_observations_byte_budget,
    ));
    summary_tokens + constraints_tokens + durable_events_tokens + tactical_tokens
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

// P1.1 paso 4: los métodos de carga/reparación prometidos por el module
// doc de arriba — extraídos verbatim de `engine/mod.rs`.
use super::*;

impl Engine {
    /// Loads the full event log and repairs any orphaned tool_use left by
    /// a crashed/killed/power-lost previous run (see
    /// [`Engine::repair_orphaned_tool_calls`]). Called directly from
    /// `run_turn` *before* the turn's `UserMessage` is appended (N-4,
    /// docs/AUDITORIA-2026-07-v2.md) — the returned events also seed
    /// `run_turn`'s `known_tool_call_ids` (N-14, via
    /// [`Engine::existing_tool_call_ids`]) — and from
    /// [`Engine::load_messages`] (which still needs the repair for any
    /// other caller, and is idempotent if `run_turn` already ran it this
    /// turn).
    pub(super) async fn load_and_repair(
        &self,
        session: &SessionId,
        observer: &mut dyn TurnObserver,
    ) -> Result<Vec<AgentEvent>, EngineError> {
        let mut events = match self.store.load(session).await {
            Ok(events) => events,
            Err(SessionError::NotFound(_)) => Vec::new(),
            Err(err) => return Err(err.into()),
        };

        self.repair_orphaned_tool_calls(session, &mut events, observer)
            .await?;

        Ok(events)
    }

    /// Collects the id of every `AssistantToolCall` already in `events` —
    /// used to seed `run_turn`'s `known_tool_call_ids` so a
    /// freshly-generated id (whether from the model or a backend's
    /// synthetic-id fallback) that happens to collide with one already in
    /// the session's history gets renamed instead of silently entering the
    /// append-only log twice (N-14, docs/AUDITORIA-2026-07-v2.md).
    pub(super) fn existing_tool_call_ids(events: &[AgentEvent]) -> HashSet<String> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::AssistantToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Loads the full event log, splits it into durable/tactical via the
    /// compactor, and — if the tactical window has grown past
    /// `tactical_compaction_threshold` **or** the estimated prompt size has
    /// grown past `context_budget_tokens` (whichever is configured; see
    /// that field's doc comment) — folds *all* of it into a fresh
    /// `CompactionOccurred` summary (persisted; see
    /// [`SimpleContextCompactor`](braze_session::SimpleContextCompactor)'s
    /// `last_compaction_index` logic for why folding the complete backlog,
    /// not a partial prefix, is what keeps repeated compaction
    /// differential instead of re-summarizing overlapping content every
    /// round) and builds messages from durable summary + that fresh
    /// summary, **plus the last [`KEEP_RAW_TAIL`] tactical events kept
    /// verbatim** — never the empty slice. Discarding the raw tail
    /// entirely would drop the user's just-appended message for the
    /// current turn (and any tool result from the round in progress) from
    /// the very request meant to act on it. Otherwise (below both
    /// thresholds) builds messages from durable summary + the full raw
    /// tactical window, unchanged.
    ///
    /// The event-count threshold alone is a poor proxy for prompt size —
    /// a single `read_file` of a large file counts the same as a
    /// two-word "ok" — so a caller targeting a small, fixed context
    /// window (e.g. Ollama's `num_ctx`) should also set
    /// `context_budget_tokens` via [`Engine::with_context_budget`].
    pub(super) async fn load_messages(
        &self,
        session: &SessionId,
        observer: &mut dyn TurnObserver,
    ) -> Result<Vec<Message>, EngineError> {
        let events = self.load_and_repair(session, observer).await?;

        let (durable, tactical) = self.compactor.split(&events);

        // `+ablate:no-prune` (opencode ítem 2, docs/AUDITORIA-2026-07-v6.md):
        // with the collapse disabled, every observation renders full —
        // expressed as unbounded caps rather than a separate render path,
        // so the (well-tested) render pipeline stays identical and only
        // its limits move.
        let (full_observations_budget, effective_full_observations) =
            if self.observation_collapse_enabled {
                (
                    full_observations_byte_budget(self.context_budget_tokens),
                    effective_tactical_full_observations(
                        self.tactical_full_observations,
                        self.context_budget_tokens,
                    ),
                )
            } else {
                (usize::MAX, usize::MAX)
            };
        let effective_compaction_threshold = effective_tactical_compaction_threshold(
            self.tactical_compaction_threshold,
            self.context_budget_tokens,
        );
        let over_event_count_threshold = tactical.len() > effective_compaction_threshold;
        let over_token_budget = self.context_budget_tokens.is_some_and(|budget| {
            estimate_prompt_tokens(
                &durable,
                &tactical,
                effective_full_observations,
                full_observations_budget,
            ) > budget
        });
        // N-6 (docs/AUDITORIA-2026-07-v2.md): compacting only ever folds
        // `tactical` — it can't shrink `durable.summary` or
        // `durable_events`. If `tactical` is already down to (or below)
        // the raw tail every compaction keeps verbatim anyway, running
        // `compact_tactical` again can't reduce the estimate at all: it
        // would just append another near-empty `CompactionOccurred` and
        // re-trigger on every subsequent `load_messages` forever — the
        // exact "modo compactación permanente" pathology A2/C2 already
        // fixed for the event-count trigger, reintroduced here via the
        // token-budget one.
        let compaction_would_help = tactical.len() > KEEP_RAW_TAIL;

        if (over_event_count_threshold || over_token_budget)
            && compaction_would_help
            // `+ablate:no-compaction` (E1): gates BOTH triggers (event
            // count and token budget) — a long turn can then genuinely
            // blow the model's real context, which is the point of the
            // ablation: measuring what compaction is worth requires
            // letting its absence hurt.
            && self.compaction_enabled
        {
            // A9 (docs/AUDITORIA-2026-07.md): previously this branch had
            // no log statement at all — the only trace of a compaction
            // having happened was the resulting `AgentEvent::CompactionOccurred`
            // itself, silently, in the rollout log. `tactical_len` is the
            // number that actually tripped this (whichever threshold),
            // making a repeated/thrashing compaction pattern visible with
            // `RUST_LOG=debug` instead of only inferable after the fact.
            tracing::warn!(
                tactical_len = tactical.len(),
                tactical_compaction_threshold = effective_compaction_threshold,
                over_event_count_threshold,
                over_token_budget,
                "context compaction triggered"
            );

            // Bajo (docs/AUDITORIA-2026-07-v2.md, "dropped_tokens_estimate
            // cuenta como perdido el tail que se conserva"): `start` must
            // be known *before* estimating what got dropped — only
            // `tactical[..start]` is actually folded into the summary;
            // `tactical[start..]` (the live tail) survives verbatim into
            // this same request. Estimating over the whole `tactical`
            // slice counted retained events as if they were gone.
            let start = pair_aware_tail_start(&tactical, KEEP_RAW_TAIL);
            let dropped_tokens_estimate = estimate_dropped_tokens(&tactical[..start]);

            // v8 § 6 — summary-por-lead: con summarizer configurado, el
            // summary lo escribe el modelo fuerte sobre los eventos que
            // realmente se dropean; ante cualquier fallo cae al digest
            // extractivo de siempre (el path sin summarizer es
            // byte-idéntico al previo).
            let summary = match self.attempt_lead_summary(&tactical[..start]).await {
                Some(lead_summary) => lead_summary,
                None => self.compactor.compact_tactical(&tactical)?,
            };

            self.append_and_notify(
                session,
                &AgentEvent::CompactionOccurred {
                    summary: summary.clone(),
                    dropped_tokens_estimate,
                },
                observer,
            )
            .await?;

            let effective_durable = merge_summary(durable, summary);
            let live_tail = &tactical[start..];
            Ok(build_messages_with_full_observations(
                &effective_durable,
                live_tail,
                effective_full_observations,
                full_observations_budget,
            ))
        } else {
            Ok(build_messages_with_full_observations(
                &durable,
                &tactical,
                effective_full_observations,
                full_observations_budget,
            ))
        }
    }

    /// Repairs `AssistantToolCall`s left without a matching
    /// `ToolCallCompleted` (correlated by id) anywhere in the log — the
    /// process crashed, was killed, or lost power between `run_turn`
    /// persisting the tool_use (`dispatch_tool_calls` appends it *before*
    /// dispatch) and receiving the tool's result. Left unrepaired, every
    /// future request against this session is rejected by Anthropic with
    /// a permanent 400 (a `tool_use` block with no matching
    /// `tool_result`) — the session becomes permanently unresumable.
    ///
    /// Synthesizes and persists an error `ToolCallCompleted` for each
    /// orphan found, and also appends it to `events` in place so this same
    /// `load_messages` call already reflects the repair without a second
    /// round-trip to the store. Idempotent and append-only: a session with
    /// no orphans is a no-op, and a session already repaired has none left
    /// to find on a later call.
    /// v8 § 6 — la llamada del summary-por-lead: una request tools-free
    /// al `summarizer` con el transcript de los eventos dropeados,
    /// acotada por [`LEAD_SUMMARY_MAX_TOKENS`] y
    /// [`LEAD_SUMMARY_TIMEOUT`]. `None` en TODO camino de fallo (sin
    /// summarizer, dropped vacío, error de request/stream, timeout,
    /// texto vacío, stream sin `Done`) — el caller cae al digest
    /// extractivo, así que esta palanca nunca puede dejar la compactación
    /// peor que sin ella.
    ///
    /// Caveat de contabilidad (documentado, no accidental): el Usage de
    /// esta llamada NO se persiste como evento — `TaskResult::rounds`
    /// cuenta eventos Usage (uno por ronda, contrato K-33) y un Usage
    /// extra inflaría las rondas del turno. El costo queda visible vía
    /// tracing; si la palanca se promueve, su A/B debe anotar este
    /// costo fuera de `input_tokens` (misma clase de caveat que
    /// J-14/J-34 para planner/fallback fuera de hooks y breaker).
    async fn attempt_lead_summary(&self, dropped: &[AgentEvent]) -> Option<String> {
        let summarizer = self.summarizer.as_ref()?;
        if dropped.is_empty() {
            return None;
        }

        let request = CompletionRequest {
            messages: vec![Message::text(
                Role::User,
                render_events_for_lead_summary(dropped),
            )],
            tool_stubs: vec![],
            system_prompt: LEAD_SUMMARY_SYSTEM_PROMPT.to_string(),
            max_tokens: LEAD_SUMMARY_MAX_TOKENS,
        };

        let outcome = tokio::time::timeout(LEAD_SUMMARY_TIMEOUT, async {
            let mut stream = match summarizer.complete(request).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "lead summarizer request failed; falling back to the deterministic digest"
                    );
                    return None;
                }
            };
            let mut text = String::new();
            let mut saw_done = false;
            let mut usage_tokens = (0u32, 0u32);
            while let Some(event) = stream.next().await {
                match event {
                    Ok(CompletionEvent::TextDelta(delta)) => text.push_str(&delta),
                    Ok(CompletionEvent::Usage {
                        input_tokens,
                        output_tokens,
                        ..
                    }) => usage_tokens = (input_tokens, output_tokens),
                    Ok(CompletionEvent::Done) => saw_done = true,
                    // Un summarizer que intenta llamar tools está fuera de
                    // contrato — se ignora la call y se sigue leyendo texto.
                    Ok(CompletionEvent::ToolCallRequested { .. }) => {}
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "lead summarizer stream failed; falling back to the deterministic digest"
                        );
                        return None;
                    }
                }
            }
            let text = text.trim();
            if saw_done && !text.is_empty() {
                tracing::info!(
                    summary_chars = text.len(),
                    input_tokens = usage_tokens.0,
                    output_tokens = usage_tokens.1,
                    "compaction summary produced by the lead summarizer"
                );
                Some(text.to_string())
            } else {
                tracing::warn!(
                    saw_done,
                    "lead summarizer produced no usable text; falling back to the deterministic digest"
                );
                None
            }
        })
        .await;

        match outcome {
            Ok(result) => result,
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_s = LEAD_SUMMARY_TIMEOUT.as_secs(),
                    "lead summarizer timed out; falling back to the deterministic digest"
                );
                None
            }
        }
    }

    pub(super) async fn repair_orphaned_tool_calls(
        &self,
        session: &SessionId,
        events: &mut Vec<AgentEvent>,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        for id in orphaned_tool_call_ids(events) {
            tracing::warn!(
                tool_call_id = %id,
                "repairing an orphaned tool_use with no matching result \
                 (likely an interrupted process); synthesizing an error ToolCallCompleted"
            );
            let repair = build_orphan_repair(id);
            self.append_and_notify(session, &repair, observer).await?;
            events.push(repair);
        }

        Ok(())
    }
}

/// Cap de salida para el summary del lead — un summary de compactación
/// útil son ~10 líneas; más que esto ya no es compresión.
const LEAD_SUMMARY_MAX_TOKENS: u32 = 400;

/// Techo de espera por el summarizer: la compactación corre en medio de
/// un turno, y un lead colgado no puede convertir la palanca en un
/// stall. 90s cubre un lead cloud (segundos) y uno local razonable;
/// más allá, digest y seguir.
const LEAD_SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Cap del transcript de entrada (~6k tokens a 4 chars/token): los
/// eventos dropeados pueden ser arbitrariamente grandes y el summarizer
/// no necesita cada byte de cada observación para preservar decisiones.
const LEAD_SUMMARY_INPUT_CAP_CHARS: usize = 24_000;

/// Por-ítem: una observación de tool puede ser un archivo entero; para
/// el summary bastan sus primeras líneas.
const LEAD_SUMMARY_ITEM_CAP_CHARS: usize = 400;

const LEAD_SUMMARY_SYSTEM_PROMPT: &str = "You are compacting the earlier part of an \
agent session. Write a short summary (at most ~10 lines of plain prose, no headers, \
no tool-call syntax) that preserves exactly: what the user asked for, decisions made \
and why, files/paths touched and the outcome, errors hit and how they were resolved, \
and any unfinished work. Omit pleasantries and step-by-step narration. The summary \
replaces these events permanently — anything you drop is gone.";

/// El transcript compacto que ve el summarizer: una línea (capada) por
/// evento relevante, más nuevo al final. Si excede
/// [`LEAD_SUMMARY_INPUT_CAP_CHARS`] se conservan los eventos MÁS NUEVOS
/// y se antepone un marcador con cuántos se omitieron — nunca un corte
/// silencioso ("no silent caps").
fn render_events_for_lead_summary(events: &[AgentEvent]) -> String {
    fn cap(text: &str) -> String {
        if text.len() <= LEAD_SUMMARY_ITEM_CAP_CHARS {
            return text.to_string();
        }
        let mut end = LEAD_SUMMARY_ITEM_CAP_CHARS;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}[...]", &text[..end])
    }

    let lines: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::UserMessage { text } => Some(format!("user: {}", cap(text))),
            AgentEvent::AssistantText { text } => Some(format!("assistant: {}", cap(text))),
            AgentEvent::AssistantToolCall {
                name, arguments, ..
            } => Some(format!(
                "tool call: {name}({})",
                cap(&arguments.to_string())
            )),
            AgentEvent::ToolCallCompleted { result, .. } => Some(format!(
                "tool result{}: {}",
                if result.is_error { " (ERROR)" } else { "" },
                cap(&result.content)
            )),
            AgentEvent::VerificationFailed { output } => {
                Some(format!("verification (FAILED): {}", cap(output)))
            }
            AgentEvent::PlanCreated { plan } => Some(format!("plan: {}", cap(plan))),
            AgentEvent::CompactionOccurred { summary, .. } => {
                Some(format!("earlier summary: {}", cap(summary)))
            }
            _ => None,
        })
        .collect();

    let mut total = 0usize;
    let mut keep_from = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        if total + line.len() + 1 > LEAD_SUMMARY_INPUT_CAP_CHARS {
            break;
        }
        total += line.len() + 1;
        keep_from = i;
    }

    let mut out = String::new();
    if keep_from > 0 {
        out.push_str(&format!(
            "[{keep_from} earlier events omitted for length]\n"
        ));
    }
    out.push_str(&lines[keep_from..].join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 paso 6: tests de integración movidos del mod tests de
    // engine/mod.rs — fixtures compartidas en engine/test_support.rs.
    use crate::engine::Engine;
    use crate::engine::test_support::*;
    use braze_events::NoopObserver;
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_types::ToolResult;
    use braze_types::{ContentBlock, SessionId};
    use std::sync::atomic::{AtomicU32, Ordering};
    // P1.1 paso 5: tests movidos del mod tests de engine/mod.rs — usan
    // las constantes de history directamente.
    use crate::history::{MAX_FULL_OBSERVATIONS_TOTAL_CHARS, TACTICAL_FULL_OBSERVATIONS};

    /// El transcript capea cada ítem con marcador visible, nunca corta
    /// mid-char, y ante exceso total conserva los eventos MÁS NUEVOS
    /// anteponiendo cuántos omitió ("no silent caps").
    #[test]
    fn lead_summary_transcript_caps_items_and_drops_oldest_with_a_marker() {
        let long = "x".repeat(LEAD_SUMMARY_ITEM_CAP_CHARS * 2);
        let events = vec![
            AgentEvent::UserMessage {
                text: "corto".to_string(),
            },
            AgentEvent::ToolCallCompleted {
                id: "c1".to_string(),
                result: ToolResult {
                    tool_call_id: "c1".to_string(),
                    content: long.clone(),
                    is_error: false,
                },
            },
        ];
        let transcript = render_events_for_lead_summary(&events);
        assert!(transcript.contains("user: corto"));
        assert!(
            transcript.contains("[...]"),
            "ítem largo debe caparse con marcador"
        );
        assert!(
            transcript.len() < long.len(),
            "el cap por ítem debe aplicar"
        );

        // Muchos eventos → los más viejos se omiten con conteo explícito.
        let many: Vec<AgentEvent> = (0..200)
            .map(|i| AgentEvent::AssistantText {
                text: format!("evento {i}: {}", "y".repeat(300)),
            })
            .collect();
        let transcript = render_events_for_lead_summary(&many);
        assert!(transcript.len() <= LEAD_SUMMARY_INPUT_CAP_CHARS + 64);
        assert!(
            transcript.starts_with('['),
            "debe abrir con el marcador de omitidos: {}",
            &transcript[..60]
        );
        assert!(
            transcript.contains("evento 199"),
            "se conservan los MÁS NUEVOS"
        );
        assert!(
            !transcript.contains("evento 0:"),
            "los más viejos se omiten"
        );
    }

    #[test]
    fn a_configured_context_budget_keeps_the_original_protective_default() {
        assert_eq!(
            full_observations_byte_budget(Some(8192)),
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS
        );
    }

    #[test]
    fn no_configured_context_budget_gets_a_wider_default() {
        assert_eq!(
            full_observations_byte_budget(None),
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS * NO_CONTEXT_BUDGET_SCALE_MULTIPLIER
        );
        assert!(full_observations_byte_budget(None) > full_observations_byte_budget(Some(8192)));
    }

    #[test]
    fn a_configured_context_budget_keeps_the_compaction_threshold_unchanged() {
        assert_eq!(effective_tactical_compaction_threshold(40, Some(8192)), 40);
    }

    #[test]
    fn no_configured_context_budget_widens_the_compaction_threshold() {
        assert_eq!(
            effective_tactical_compaction_threshold(DEFAULT_TACTICAL_COMPACTION_THRESHOLD, None),
            DEFAULT_TACTICAL_COMPACTION_THRESHOLD * NO_CONTEXT_BUDGET_SCALE_MULTIPLIER
        );
    }

    /// Regression test for the ablation-corruption risk documented on
    /// `effective_tactical_compaction_threshold`'s own doc comment: an
    /// explicit override (e.g. `+ablate:tactical-threshold=8`) must
    /// survive verbatim even with no context budget configured — only the
    /// untouched default gets scaled.
    #[test]
    fn an_explicit_non_default_compaction_threshold_is_never_scaled() {
        assert_eq!(effective_tactical_compaction_threshold(8, None), 8);
    }

    /// The anchor the whole formula hangs on: at the historical 8K-ctx
    /// reference budget the scale is exactly 1 — byte-identical behavior
    /// for the small local models the defaults were tuned on.
    #[test]
    fn the_reference_budget_scales_by_exactly_one() {
        assert_eq!(tactical_cap_scale(Some(6_000)), 1);
        assert_eq!(tactical_cap_scale(Some(8_192)), 1);
    }

    /// A 32K-ctx local model (budget ≈ 30K tokens) gets proportionally
    /// wider caps — the exact population (qwen3.5-coder on Nitro) for
    /// which the U-17 re-read collapse was still alive under the binary
    /// Some/None logic.
    #[test]
    fn a_large_local_budget_scales_all_three_caps_proportionally() {
        let budget = Some(30_000);
        assert_eq!(tactical_cap_scale(budget), 5);
        assert_eq!(
            full_observations_byte_budget(budget),
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS * 5
        );
        assert_eq!(
            effective_tactical_compaction_threshold(DEFAULT_TACTICAL_COMPACTION_THRESHOLD, budget),
            DEFAULT_TACTICAL_COMPACTION_THRESHOLD * 5
        );
        assert_eq!(
            effective_tactical_full_observations(TACTICAL_FULL_OBSERVATIONS, budget),
            TACTICAL_FULL_OBSERVATIONS * 5
        );
    }

    /// A tiny budget never shrinks the caps below the tuned defaults —
    /// they're the protective minimum, not a starting point to scale down.
    #[test]
    fn a_tiny_budget_floors_at_the_tuned_defaults() {
        assert_eq!(tactical_cap_scale(Some(1_000)), 1);
        assert_eq!(
            full_observations_byte_budget(Some(1_000)),
            MAX_FULL_OBSERVATIONS_TOTAL_CHARS
        );
    }

    /// A huge local budget caps at the same ×10 cloud backends get — a
    /// 128K-context local model shouldn't out-scale cloud.
    #[test]
    fn a_huge_local_budget_caps_at_the_cloud_multiplier() {
        assert_eq!(
            tactical_cap_scale(Some(500_000)),
            NO_CONTEXT_BUDGET_SCALE_MULTIPLIER
        );
    }

    /// The ablation guard survives the I-2 change: an explicit override
    /// is never scaled no matter how large the budget is.
    #[test]
    fn an_explicit_override_is_never_scaled_even_with_a_large_budget() {
        assert_eq!(effective_tactical_compaction_threshold(8, Some(30_000)), 8);
        assert_eq!(effective_tactical_full_observations(2, Some(30_000)), 2);
    }

    #[test]
    fn a_configured_context_budget_keeps_full_observations_unchanged() {
        assert_eq!(
            effective_tactical_full_observations(TACTICAL_FULL_OBSERVATIONS, Some(8192)),
            TACTICAL_FULL_OBSERVATIONS
        );
    }

    #[test]
    fn no_configured_context_budget_widens_full_observations() {
        assert_eq!(
            effective_tactical_full_observations(TACTICAL_FULL_OBSERVATIONS, None),
            TACTICAL_FULL_OBSERVATIONS * NO_CONTEXT_BUDGET_SCALE_MULTIPLIER
        );
    }

    /// Same regression as `an_explicit_non_default_compaction_threshold_is_never_scaled`,
    /// for `+ablate:full-observations=N`.
    #[test]
    fn an_explicit_non_default_full_observations_is_never_scaled() {
        assert_eq!(effective_tactical_full_observations(1, None), 1);
    }

    /// Regression test for the "estimador de tokens sobre Debug repr"
    /// bajo (docs/AUDITORIA-2026-07-v2.md): the estimate must scale with
    /// the event's actual user-visible text, not its `Debug` dump —
    /// field names and enum punctuation must not count as "content".
    #[test]
    fn estimate_dropped_tokens_counts_visible_text_not_debug_repr() {
        let text = "hola".repeat(20); // 80 chars of real content
        let events = vec![AgentEvent::UserMessage { text: text.clone() }];

        let debug_repr_chars = format!("{:?}", events[0]).len();
        assert!(
            debug_repr_chars > text.len(),
            "sanity: the Debug form should be longer than the raw text itself"
        );

        let estimate = estimate_dropped_tokens(&events);
        assert_eq!(
            estimate,
            (text.len() / 4) as u32,
            "expected the estimate to scale with the raw text length, not the (longer) Debug form"
        );
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-1.
    ///
    /// `KEEP_RAW_TAIL` slices the last few tactical events verbatim into
    /// the request with no awareness of `tool_use`/`tool_result` pairing.
    /// If a round dispatches several tool calls concurrently and their
    /// completions arrive in a different order than their requests were
    /// issued (a realistic race under `TaskNotifier::spawn`), the log can
    /// end up as `[..., ATC1, ATC2, ATC3, TCC1, TCC2, TCC3]`. Once that
    /// whole span ages into the compactor's tactical window and a
    /// compaction triggers, the raw tail keeps only the last
    /// `KEEP_RAW_TAIL` (6) events — here, `[ATC3, TCC1, TCC2, TCC3]` plus
    /// two audit-only `ToolCallStarted`s that don't render — cutting
    /// `ATC1`/`ATC2` out entirely (they're not old enough to have settled
    /// into `durable_events` either, since the whole log fits inside the
    /// compactor's window). `TCC1`/`TCC2` still render as `tool_result`
    /// blocks with no matching `tool_use` anywhere in the request.
    ///
    /// Fixed by two complementary changes: `pair_aware_tail_start` (below)
    /// extends the cut backward so it never *excludes* a `tool_use` whose
    /// `tool_result` survived into the tail; and `history::push_grouped`
    /// groups consecutive `tool_use`/`tool_result` events into one
    /// `Message` each (matching how Anthropic itself represents one
    /// assistant turn requesting several tools), so a concurrent-dispatch
    /// round's naturally-non-adjacent `[ToolUse, ToolUse, ToolUse]` /
    /// `[ToolResult, ToolResult, ToolResult]` shape is never actually
    /// invalid to begin with — the tail cut alone couldn't have fixed
    /// that half on its own.
    #[tokio::test]
    async fn compaction_tail_cut_can_orphan_a_tool_result() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        fn tool_call(id: &str) -> AgentEvent {
            AgentEvent::AssistantToolCall {
                id: id.to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": id }),
            }
        }
        fn tool_started(id: &str) -> AgentEvent {
            AgentEvent::ToolCallStarted {
                id: id.to_string(),
                name: "echo".to_string(),
                background: false,
            }
        }
        fn tool_completed(id: &str) -> AgentEvent {
            AgentEvent::ToolCallCompleted {
                id: id.to_string(),
                result: ToolResult {
                    tool_call_id: id.to_string(),
                    content: "ok".to_string(),
                    is_error: false,
                },
            }
        }

        // Three concurrently-dispatched tool calls whose completions all
        // arrive after every request was issued — a realistic ordering
        // when tools run as independently-spawned background tasks.
        for event in [
            AgentEvent::UserMessage {
                text: "please echo three things".to_string(),
            },
            tool_call("call-1"),
            tool_started("call-1"),
            tool_call("call-2"),
            tool_started("call-2"),
            tool_call("call-3"),
            tool_started("call-3"),
            tool_completed("call-1"),
            tool_completed("call-2"),
            tool_completed("call-3"),
        ] {
            store.append(&session, &event).await.expect("seed event");
        }

        // A low compaction threshold forces `load_messages` to compact on
        // this very first call, exactly like a long-running session that
        // has just crossed the real (default 40) threshold would.
        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tactical_compaction_threshold(3);

        let messages = engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        crate::protocol_check::check_anthropic_message_protocol(&messages).expect(
            "load_messages must never hand back a request with an orphaned \
             tool_result, regardless of where the tactical tail happens to \
             be cut",
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// v8 § 6 — summary-por-lead: con summarizer configurado, el summary
    /// persistido en `CompactionOccurred` es el texto que escribió el
    /// lead, no el digest extractivo.
    #[tokio::test]
    async fn compaction_uses_the_lead_summarizer_when_configured() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "hola".to_string(),
                },
            )
            .await
            .expect("seed event");
        // Suficientes eventos para superar el umbral (3) Y dejar algo
        // que dropear más allá de la cola cruda (KEEP_RAW_TAIL = 6).
        for i in 0..10 {
            store
                .append(
                    &session,
                    &AgentEvent::AssistantText {
                        text: format!("texto {i}"),
                    },
                )
                .await
                .expect("seed event");
        }
        let store = Arc::new(store);

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::clone(&store) as Arc<dyn braze_session::SessionStore>,
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tactical_compaction_threshold(3)
        .with_compaction_summarizer(Box::new(ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("resumen escrito por el lead".to_string()),
            CompletionEvent::Done,
        ]])));

        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let events = store.load(&session).await.expect("load events");
        let summary = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::CompactionOccurred { summary, .. } => Some(summary.clone()),
                _ => None,
            })
            .expect("compaction must have occurred");
        assert_eq!(summary, "resumen escrito por el lead");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// v8 § 6 — el contrato "nunca peor que sin la palanca": un
    /// summarizer cuyo stream muere sin `Done` (respuesta truncada) no
    /// aporta summary y la compactación cae al digest extractivo de
    /// siempre, sin error.
    #[tokio::test]
    async fn a_truncated_summarizer_falls_back_to_the_deterministic_digest() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "hola".to_string(),
                },
            )
            .await
            .expect("seed event");
        // Suficientes eventos para superar el umbral (3) Y dejar algo
        // que dropear más allá de la cola cruda (KEEP_RAW_TAIL = 6).
        for i in 0..10 {
            store
                .append(
                    &session,
                    &AgentEvent::AssistantText {
                        text: format!("texto {i}"),
                    },
                )
                .await
                .expect("seed event");
        }
        let store = Arc::new(store);

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::clone(&store) as Arc<dyn braze_session::SessionStore>,
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tactical_compaction_threshold(3)
        // Stream que termina sin `Done`: la clase de truncamiento que un
        // lead real puede producir (conexión cortada mid-respuesta).
        .with_compaction_summarizer(Box::new(ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("truncado".to_string()),
        ]])));

        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let events = store.load(&session).await.expect("load events");
        let summary = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::CompactionOccurred { summary, .. } => Some(summary.clone()),
                _ => None,
            })
            .expect("compaction must have occurred");
        assert_ne!(summary, "truncado", "el texto truncado no debe usarse");
        assert!(
            !summary.is_empty(),
            "el fallback debe producir el digest extractivo de siempre"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Closes the coverage gap the re-audit (docs/AUDITORIA-2026-07-v2.md,
    /// "por qué los tests verdes no los atrapan") called out explicitly:
    /// every existing `ProtocolValidatingModel` test runs 1-2 short rounds
    /// that never cross `DEFAULT_TACTICAL_COMPACTION_THRESHOLD`, and every
    /// test that *does* trigger compaction (e.g.
    /// `compaction_tail_cut_can_orphan_a_tool_result` above) seeds events
    /// directly and calls `load_messages` once — bypassing `run_turn`'s
    /// real multi-turn loop entirely. Neither shape proves the two things
    /// hold *together*, organically, over a long session: concurrent tool
    /// dispatch completing out of order (N-1/N-2b's trigger) interacting
    /// with compaction firing repeatedly as the log keeps growing past the
    /// window/threshold, turn after turn.
    ///
    /// Drives many real turns through `run_turn`, each dispatching two
    /// concurrently-issued tool calls that `ReorderingEchoToolProvider`
    /// resolves in *reverse* of dispatch order (a real `tokio::spawn`/
    /// `sleep` race, not a simulated one), with a low compaction threshold
    /// so compaction triggers repeatedly across the run instead of once.
    /// `ProtocolValidatingModel` panics the instant any request built
    /// along the way would 400 against the real Anthropic API.
    #[tokio::test]
    async fn a_long_session_with_reordered_concurrent_tool_calls_stays_protocol_valid_across_repeated_compaction()
     {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        const TURNS: usize = 15;
        let mut rounds = Vec::with_capacity(TURNS * 2);
        for i in 0..TURNS {
            rounds.push(vec![
                CompletionEvent::ToolCallRequested {
                    id: format!("call-{i}-a"),
                    name: "echo".to_string(),
                    // Dispatched first but resolves last.
                    arguments: serde_json::json!({ "text": "a", "delay_ms": 30 }),
                },
                CompletionEvent::ToolCallRequested {
                    id: format!("call-{i}-b"),
                    name: "echo".to_string(),
                    // Dispatched second but resolves first.
                    arguments: serde_json::json!({ "text": "b", "delay_ms": 1 }),
                },
                CompletionEvent::Done,
            ]);
            rounds.push(vec![
                CompletionEvent::TextDelta(format!("turn {i} done")),
                CompletionEvent::Done,
            ]);
        }

        let model = ProtocolValidatingModel::new(ScriptedModel::new(rounds));
        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReorderingEchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tactical_compaction_threshold(10);

        for i in 0..TURNS {
            engine
                .run_turn(
                    &session,
                    &format!("please echo turn {i}"),
                    &mut NoopObserver,
                )
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "turn {i} failed (every prior turn passed protocol \
                         validation, so this is a genuine failure, not the \
                         validator): {err}"
                    )
                });
        }

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            (TURNS * 2) as u32,
            "every tool call across every turn must have actually dispatched"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A1/C1: when a compaction triggers,
    /// `load_messages` must never discard the live tail entirely — the
    /// user's just-appended message for the current turn (the newest
    /// event in the log) has to survive as a raw message, not be
    /// swallowed into the compaction summary with nothing concrete left
    /// for the model to act on.
    #[tokio::test]
    async fn load_messages_keeps_a_live_raw_tail_when_compaction_triggers() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Seed a backlog well past the *effective* compaction threshold
        // (hallazgo U-18: with no `context_budget_tokens` configured — the
        // case here — the real threshold `load_messages` applies is
        // `DEFAULT_TACTICAL_COMPACTION_THRESHOLD` scaled up, not the raw
        // constant) with plain, non-durable-typed events (the orphan types
        // that never leave `tactical` on their own).
        let threshold =
            effective_tactical_compaction_threshold(DEFAULT_TACTICAL_COMPACTION_THRESHOLD, None);
        for i in 0..(threshold + 10) {
            store
                .append(
                    &session,
                    &AgentEvent::UserMessage {
                        text: format!("turno {i}"),
                    },
                )
                .await
                .expect("seed backlog event");
        }
        // The newest event — exactly what `run_turn` appends right before
        // calling `load_messages` for the current turn.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "pregunta actual del usuario".to_string(),
                },
            )
            .await
            .expect("seed current turn's message");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let messages = engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        assert!(
            messages.iter().any(|m| matches!(
                &m.content[0],
                ContentBlock::Text { text } if text == "pregunta actual del usuario"
            )),
            "expected the live tail to include the just-appended user message, got: {messages:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "sanity check: a compaction should actually have been triggered"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C3: a single oversized event (e.g. a large
    /// `read_file` result) must trigger compaction via the token budget
    /// even when the raw event *count* is nowhere near
    /// `tactical_compaction_threshold` — the count alone can't tell a
    /// 200KB tool result apart from a two-word reply.
    #[tokio::test]
    async fn a_single_oversized_event_triggers_compaction_via_the_token_budget() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // The oversized event plus enough small filler events that the
        // tactical tail (`KEEP_RAW_TAIL`) doesn't fully cover it — with 8
        // tactical events total, compacting can actually exclude the huge
        // one from the kept raw tail and fold it into the digest instead
        // (see `compaction_would_help` in `load_messages`: with 2 events
        // or fewer than `KEEP_RAW_TAIL`, the tail *is* the whole tactical
        // slice, so compacting couldn't shrink anything and correctly
        // wouldn't trigger at all).
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "resume este archivo".to_string(),
                },
            )
            .await
            .expect("seed user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "x".repeat(20_000),
                },
            )
            .await
            .expect("seed oversized event");
        for i in 0..6 {
            store
                .append(
                    &session,
                    &AgentEvent::UserMessage {
                        text: format!("mensaje de relleno {i}"),
                    },
                )
                .await
                .expect("seed filler event");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_context_budget(1000); // ~4000 chars — the 20K-char event alone blows this.

        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "expected the token budget to trigger compaction despite the low event count"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Without a configured budget, a large event does NOT trigger
    /// compaction below the event-count threshold — confirms
    /// `context_budget_tokens: None` preserves the pre-C3 behavior
    /// exactly (event count is the only trigger).
    #[tokio::test]
    async fn without_a_configured_budget_only_event_count_triggers_compaction() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "x".repeat(20_000),
                },
            )
            .await
            .expect("seed oversized event");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "no budget configured: a single large event below the count threshold must not compact"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// E1 `+ablate:no-compaction` (docs/AUDITORIA-2026-07-v6.md § roadmap):
    /// with compaction disabled, NEITHER trigger fires — here the event
    /// count blows well past the threshold and still no
    /// `CompactionOccurred` lands.
    #[tokio::test]
    async fn with_compaction_disabled_even_the_event_count_trigger_does_not_fire() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Well past DEFAULT_TACTICAL_COMPACTION_THRESHOLD (40) — and past
        // its ×10 no-budget scaling too.
        for i in 0..450 {
            store
                .append(
                    &session,
                    &AgentEvent::AssistantText {
                        text: format!("evento {i}"),
                    },
                )
                .await
                .expect("seed events");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_compaction_enabled(false);

        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "compaction disabled: no amount of events may trigger it"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
