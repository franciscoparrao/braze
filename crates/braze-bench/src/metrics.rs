//! Turns one task run's persisted `AgentEvent` log into a comparable
//! verdict. Pure and synchronous on purpose — no model, no I/O — so it's
//! testable with hand-built event logs the same way
//! `braze-session::simple_compactor`'s tests are.

use std::collections::HashSet;
use std::time::Duration;

use braze_events::AgentEvent;
use serde::Serialize;

use crate::task::TaskDef;

/// Why a task run failed, distinguishing "the model/tool loop itself
/// broke" from "it converged but got the wrong answer" from "the harness
/// couldn't even run it" — a single `converged: bool` collapses all of
/// these into one bit, which hides exactly the information needed to
/// tell a genuine model-capability gap apart from a harness bug or a slow
/// backend. See docs/AUDITORIA-2026-07.md hallazgo F5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCause {
    /// The task exceeded its wall-clock budget (see `runner::run_task`'s
    /// `tokio::time::timeout`) — the model may still have been "thinking"
    /// (or stuck in a loop) when the harness gave up on it.
    Timeout,
    /// `Engine::run_turn` hit `MAX_TURN_ITERATIONS` without the model
    /// producing a final text-only response.
    MaxIterationsExhausted,
    /// A `ModelBackend`'s stream ended without a terminal event (see
    /// `braze_model::ModelError::StreamError` / `EngineError::IncompleteStream`).
    IncompleteStream,
    /// The model backend itself errored (transport failure, rate limit,
    /// mid-stream provider error, ...).
    ModelBackendError,
    /// The session store failed (disk I/O, corrupt rollout log, ...).
    SessionError,
    /// The tool registry failed to resolve/dispatch (should be rare —
    /// `braze-tools-local` is always the sole provider in the bench).
    ToolRegistryError,
    /// The turn converged, but `expect_tool_call` never happened.
    AssertionToolCall,
    /// The turn converged, but `expect_text_contains` didn't match.
    AssertionText,
    /// The turn converged, but `expect_file_contains` didn't match the
    /// sandbox's actual filesystem state.
    AssertionFiles,
    /// The turn converged, but `cargo check` failed in the sandbox
    /// afterwards (`expect_cargo_check`, v8 K-9) — the model's edit
    /// doesn't compile, regardless of what the substring needles say.
    AssertionCargoCheck,
    /// The turn converged, but used more model rounds than
    /// `expect_max_rounds` allowed (v4 P0.4 — budget assertions). A
    /// config that passes the correctness checks in 14 rounds is worse
    /// than one that converges in 3, and a flat pass-rate can't tell
    /// them apart.
    AssertionMaxRounds,
    /// The turn converged, but used more total tokens
    /// (`input_tokens + output_tokens`) than `expect_max_tokens` allowed
    /// (v4 P0.4). Same budget-assertion rationale as
    /// [`FailureCause::AssertionMaxRounds`]. Cache tokens are reported
    /// separately and aren't part of this budget.
    AssertionMaxTokens,
    /// The turn converged, but its estimated cost exceeded
    /// `expect_max_cost_usd` (Paquete 3, docs/AUDITORIA-2026-07-v6.md —
    /// the enforcement `TaskDef::expect_max_cost_usd` was parsed-but-
    /// waiting-for since v4 P0.4). Only evaluable when the backend row
    /// resolved a pricing entry; see `TaskResult::estimated_cost_usd`.
    AssertionMaxCost,
    /// `Engine::run_turn` hit its cumulative per-turn token budget
    /// (`max_turn_total_tokens`, v4 P0.2) and the graceful tools-free
    /// summary attempt didn't produce usable text either.
    TurnBudgetExhausted,
    /// Something failed *outside* the model/tool loop entirely — sandbox
    /// setup, reading back the session log, etc. Not attributable to the
    /// model at all; the task should generally be re-run rather than
    /// counted as a capability gap.
    HarnessError,
}

/// What `Engine::run_turn` (wrapped in a wall-clock timeout by
/// `runner::run_task`) actually produced for one attempt — the typed
/// input to [`compute_metrics`], replacing a pre-stringified
/// `Result<(), String>` so the failure-cause classification below can
/// pattern-match on the real variant instead of guessing from error text.
#[derive(Debug)]
pub enum RunOutcome {
    Converged,
    TimedOut,
    Failed(braze_engine::EngineError),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryRunMetrics {
    pub memory_tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub backend: String,
    pub task_id: String,
    pub skill: Option<String>,
    /// Experimental Paper 2 memory condition injected for this run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_condition: Option<String>,
    /// Memory/playbook file used for this run, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_file: Option<String>,
    /// Configured memory budget for this run, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_budget_tokens: Option<usize>,
    /// Approximate tokens of the rendered memory section actually injected.
    #[serde(default)]
    pub memory_tokens: u32,
    /// Which repetition (0-based) of this (task, backend) pair this is —
    /// always 0 when `--repetitions` is left at its default of 1. See
    /// docs/AUDITORIA-2026-07.md hallazgo F3.
    pub repetition: u32,
    pub converged: bool,
    pub run_error: Option<String>,
    pub failure_cause: Option<FailureCause>,
    pub tool_calls_total: u32,
    /// Exact tool names emitted by the assistant, in event-log order.
    ///
    /// This is deliberately serialized alongside `tool_calls_total` so
    /// post-hoc sweep analysis can explain `AssertionToolCall` rows
    /// without re-running the model: a row can now distinguish "called
    /// no tool", "called the wrong tool", and "called the expected tool
    /// but failed later". Repetitions are preserved because repeated
    /// calls are diagnostically meaningful for small-model loops.
    pub tool_call_names: Vec<String>,
    pub schema_validation_failures: u32,
    pub tool_execution_failures: u32,
    pub permission_denials: u32,
    /// Number of model completion rounds this turn used, derived from the
    /// persisted `AgentEvent::Usage` count (one per round — see its doc
    /// comment). The central diagnostic for small models: converging in 2
    /// rounds vs. 14 is exactly the difference this harness exists to
    /// surface, and it's invisible in wall-time alone (which conflates it
    /// with raw inference speed). NOTE: on a planned run (`planned:
    /// true`), the planner's own `Usage` counts as one round too —
    /// deliberate, it IS a model round and its cost belongs in the
    /// comparison (PLAN.md § "Split planificador/ejecutor").
    ///
    /// CENSORING CAVEAT (J-21, docs/AUDITORIA-2026-07-v7.md): on a
    /// `Timeout` row this counts only the rounds that COMPLETED before
    /// the cutoff — the turn would have used more. Averages over rows
    /// with timeouts are a lower bound, and they flatter the weaker arm
    /// (the one that times out more). Same applies to
    /// `input_tokens`/`output_tokens` below (J-10): the in-flight
    /// round's usage is lost with the dropped future, and a stream that
    /// errors mid-round loses that round's usage too. Any between-arm
    /// token/round comparison in the paper must either exclude
    /// non-converged rows or state the censoring.
    pub rounds: u32,
    /// Whether a `PlanCreated` event was persisted during this run —
    /// distinguishes planned turns from unplanned ones in the JSON
    /// output for A/B analysis (PLAN.md § "Split planificador/ejecutor").
    /// Note this reflects what actually *happened*, not what was
    /// configured: a `+plan:` spec whose planner degraded (error/
    /// truncated/empty) yields `planned: false` for that run.
    pub planned: bool,
    pub expected_tool_called: Option<bool>,
    pub expected_text_found: Option<bool>,
    pub expected_files_found: Option<bool>,
    /// v8 K-9: whether `cargo check` exited 0 in the sandbox after the
    /// run. `None` — `expect_cargo_check` not declared on the `TaskDef`;
    /// `Some(true)`/`Some(false)` — declared and passed/failed. Same
    /// `Option<bool>` "not asserted / asserted-passed / asserted-failed"
    /// contract as [`Self::expected_files_found`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_cargo_check_passed: Option<bool>,
    /// v4 P0.4 (docs/AUDITORIA-2026-07-v4.md): whether the turn stayed
    /// within `expect_max_rounds`. `None` — no rounds budget declared on
    /// the `TaskDef` (so the budget was trivially satisfied and not
    /// reported); `Some(true)` — budget declared and met; `Some(false)` —
    /// budget declared and blown (turn used more rounds than allowed).
    /// Same `Option<bool>` "not asserted / asserted-passed / asserted-
    /// failed" contract as `expected_tool_called`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_rounds_within_budget: Option<bool>,
    /// v4 P0.4: whether the turn stayed within `expect_max_tokens`
    /// (`input_tokens + output_tokens` summed across rounds; cache
    /// read/write tokens aren't part of this budget). Same `Option<bool>`
    /// contract as [`Self::expected_rounds_within_budget`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_tokens_within_budget: Option<bool>,
    /// Whether the turn stayed within `expect_max_cost_usd` (Paquete 3,
    /// docs/AUDITORIA-2026-07-v6.md). `None` when EITHER no cost budget
    /// was declared on the `TaskDef` OR the backend row resolved no
    /// pricing (`estimated_cost_usd: None`) — a declared budget with no
    /// pricing reports "not evaluated" honestly instead of a free
    /// `Some(true)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_cost_within_budget: Option<bool>,
    /// Summed across the rounds that reported a `Usage` — see the
    /// censoring caveat on [`Self::rounds`]: on Timeout / mid-stream
    /// error rows these are a lower bound (J-10).
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Tokens of this task's prompts that hit an existing cache entry,
    /// summed across every round that reported a cache-read count — the
    /// bench's view of OpenRouter's `usage.prompt_tokens_details.cached_tokens`
    /// (docs/usability-log-2026-07-07-si2.md, prompt-caching design).
    ///
    /// `None` means *no* round reported a cache-read count (a backend
    /// that doesn't expose caching at all — Ollama, Anthropic-native today
    /// — or an OpenRouter provider that didn't serve any cache hit this
    /// task). `Some(0)` is a different signal: at least one round did
    /// report cache stats and the cached total was genuinely zero (a
    /// first request establishing a cache entry, before any hit). Keeping
    /// these apart is exactly why this field is `Option<u32>` rather than
    /// `u32` with a "0 means absent" convention — the same contract
    /// `AgentEvent::Usage::cache_read_tokens` and
    /// `CompletionEvent::Usage::cache_read_tokens` already enforce
    /// end-to-end, so the bench can't silently collapse them.
    ///
    /// The aggregation rule: a round that reports `None` (not reported)
    /// contributes 0 to the sum but does *not* flip `None`-overall to
    /// `Some`; only a round that reports `Some(N)` does that, and from
    /// then on the running sum is `Some`-typed (further `None`s add 0,
    /// further `Some`s add N). Matches `Engine::complete_with_best_of_n`'s
    /// `sum_optional_u32` so a single best-of-n candidate reporting cache
    /// tokens doesn't get zeroed-out by its siblings not reporting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    /// Tokens newly written to cache by this task's prompts (billed at a
    /// premium over normal input price by OpenRouter's underlying
    /// providers, when the model is one that needs an explicit
    /// `cache_control` marker — Anthropic/Qwen via OpenRouter). Same
    /// `None`-means-"not reported" / `Some(N)`-means-"at least one round
    /// reported, summed" contract as [`Self::cache_read_tokens`], for the
    /// same reason. See that field's doc comment for the full rationale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u32>,
    /// How many times `Engine::complete_once_with`'s textual-rescue ladder
    /// recovered a tool call the model emitted as plain text instead of a
    /// structured `tool_calls` entry — counted from
    /// `AgentEvent::TextualRescueApplied` (H-3, docs/AUDITORIA-2026-07-v5.md).
    /// Plain `u32`, not `Option<u32>`: unlike cache tokens, "0 rescues" and
    /// "not applicable" are the same fact here — every backend always has
    /// an opportunity to trigger this (or not), there's no "doesn't
    /// report it" case to distinguish.
    pub rescued_tool_calls: u32,
    /// How many times `EscalatingBackend` reactively routed a round to its
    /// lead model because the worker's trailing observations crossed the
    /// failure threshold — counted from `AgentEvent::EscalationToLead`
    /// (H-3). Counts escalation *episodes* (one per triggering round), not
    /// every round spent inside an active escalation window — see that
    /// event's doc comment.
    pub leader_escalations: u32,
    /// How many times tactical context compaction fired this task —
    /// counted from `AgentEvent::CompactionOccurred`, which already
    /// existed before H-3; this field is the part of the hallazgo that
    /// was actually missing (nobody counted it).
    pub compaction_count: u32,
    /// How many times `Engine::attempt_tools_free_summary_round` was
    /// invoked — counted from `AgentEvent::SummaryFallbackAttempted`
    /// (H-3). Counts attempts, not successes; a turn can have this > 0
    /// and still end in `EmptyModelResponse` if the fallback itself came
    /// back empty too.
    pub summary_fallbacks: u32,
    /// How many mid-turn harness notes the engine injected (A′.2,
    /// docs/harness-engineering-hooks-skills-2026-07-10.md § I.2) —
    /// counted from `AgentEvent::HarnessNote`, any kind. Paired with the
    /// `no-harness-notes` ablation, this is what attributes a pass-rate
    /// delta to the announced deadline actually having been announced.
    #[serde(default)]
    pub harness_notes: u32,
    /// Estimated USD cost of this task run, from the resolved
    /// [`crate::backend_spec::PricingRates`] over the summed token
    /// counts (Paquete 3, docs/AUDITORIA-2026-07-v6.md). `None` when the
    /// spec resolved no pricing — an unlisted model, or a
    /// `+plan:`/`+lead:` composite whose halves bill at different rates
    /// (the event log can't attribute rounds to models). `Some(0.0)` is
    /// a real answer (all-Ollama rows), distinct from `None` per the
    /// same contract the cache-token fields pin.
    ///
    /// Formula, with OpenRouter's semantics (cache read/write tokens are
    /// PART of `input_tokens`, re-billed at their own rate):
    /// `uncached = input - cache_read - cache_write` (saturating), cost =
    /// `uncached*in + cache_read*(read_rate or in) + cache_write*(write_rate
    /// or in) + output*out`, all per-Mtok. When the backend reports no
    /// cache tokens (`None`), those terms are zero and it reduces to
    /// `input*in + output*out`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    pub wall_time_ms: u128,
    pub passed: bool,
}

/// Builds a [`TaskResult`] for a task that never got to run at all — the
/// sandbox failed to set up, the session log couldn't be read back, etc.
/// Used by `main.rs` so a harness-level failure still shows up as a row
/// (with a real, inspectable cause) instead of silently vanishing from
/// the totals, which previously made pass-rate denominators drift between
/// backends with no visible explanation.
pub fn harness_error_result(
    backend: &str,
    task: &TaskDef,
    repetition: u32,
    error: &crate::error::BenchError,
) -> TaskResult {
    TaskResult {
        backend: backend.to_string(),
        task_id: task.id.clone(),
        skill: task.skill.clone(),
        memory_condition: crate::memory::resolved_memory_condition(task),
        memory_file: task
            .memory_file
            .as_ref()
            .map(|path| path.display().to_string()),
        memory_budget_tokens: task.memory_budget_tokens,
        memory_tokens: 0,
        repetition,
        converged: false,
        run_error: Some(error.to_string()),
        failure_cause: Some(FailureCause::HarnessError),
        tool_calls_total: 0,
        tool_call_names: Vec::new(),
        schema_validation_failures: 0,
        tool_execution_failures: 0,
        permission_denials: 0,
        rounds: 0,
        planned: false,
        expected_tool_called: None,
        expected_text_found: None,
        expected_files_found: None,
        expected_cargo_check_passed: None,
        // Nothing ran, so no budget assertion was evaluated either —
        // `None` (not asserted) rather than `Some(false)` (budget blown).
        // Same "not reported" semantics as the cache fields below.
        expected_rounds_within_budget: None,
        expected_tokens_within_budget: None,
        expected_cost_within_budget: None,
        input_tokens: 0,
        output_tokens: 0,
        // Nothing ran, so no provider reported cache tokens at all —
        // `None` (not reported), not `Some(0)` (genuinely zero cache
        // hits). Same distinction `TaskResult::cache_read_tokens`'s doc
        // comment calls out.
        cache_read_tokens: None,
        cache_write_tokens: None,
        // Nothing ran, so none of these levers could have fired either.
        rescued_tool_calls: 0,
        leader_escalations: 0,
        compaction_count: 0,
        summary_fallbacks: 0,
        harness_notes: 0,
        // Nothing ran — no tokens, no cost to estimate.
        estimated_cost_usd: None,
        wall_time_ms: 0,
        passed: false,
    }
}

/// `true` when `needle` appears in `haystack` bounded by non-alphanumeric
/// characters (or the string's edges) on both sides — a stricter
/// "contains" than plain substring matching (F10,
/// docs/AUDITORIA-2026-07-v2.md; confirmed as a live false positive in
/// hallazgo E4, docs/AUDITORIA-2026-07-v3.md): a bare `.contains()` lets
/// an expected digit/word embedded inside an unrelated token satisfy the
/// assertion — `expect_text_contains = "2"` was satisfiable by a model's
/// wrong answer merely because the *setup file* was named
/// `informe_final_v2.txt`, with no relation to the "2" the task actually
/// asked for. Used for both `expect_text_contains` (this module) and
/// `expect_file_contains` (`runner.rs`) so the two assertion kinds share
/// one notion of "contains".
///
/// Case-sensitive by design — callers that want case-insensitive
/// matching (as `expect_text_contains` always has) lowercase both sides
/// before calling this, same as the substring check it replaces.
pub(crate) fn contains_as_a_bounded_token(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true; // preserves `"".contains("")`-style behavior
    }
    let haystack: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    let n = needle.len();
    if n > haystack.len() {
        return false;
    }
    haystack.windows(n).enumerate().any(|(start, window)| {
        window == needle.as_slice()
            && (start == 0 || !haystack[start - 1].is_alphanumeric())
            && (start + n == haystack.len() || !haystack[start + n].is_alphanumeric())
    })
}

/// Derives a [`TaskResult`] for one (task, backend) run from its
/// persisted event log plus the [`RunOutcome`] `Engine::run_turn` itself
/// produced (kept separate from the log because a hard model/tool error
/// mid-turn can abort before every expected event lands).
#[allow(clippy::too_many_arguments)]
pub fn compute_metrics(
    backend: &str,
    task: &TaskDef,
    repetition: u32,
    events: &[AgentEvent],
    wall_time: Duration,
    run_outcome: RunOutcome,
    expected_files_found: Option<bool>,
    expected_cargo_check_passed: Option<bool>,
    memory: MemoryRunMetrics,
    pricing: Option<crate::backend_spec::PricingRates>,
) -> TaskResult {
    let started_ids: HashSet<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallStarted { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();

    let tool_call_names: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantToolCall { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    let mut schema_validation_failures = 0u32;
    let mut tool_execution_failures = 0u32;
    for event in events {
        if let AgentEvent::ToolCallCompleted { id, result } = event
            && result.is_error
        {
            if started_ids.contains(id.as_str()) {
                tool_execution_failures += 1;
            } else {
                schema_validation_failures += 1;
            }
        }
    }

    let permission_denials = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::PermissionDecided { allowed: false, .. }))
        .count() as u32;

    let (input_tokens, output_tokens) =
        events
            .iter()
            .fold((0u32, 0u32), |(inp, out), event| match event {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => (inp + input_tokens, out + output_tokens),
                _ => (inp, out),
            });

    // Cache tokens use a different aggregation rule than input/output:
    // `None` per-round means "this backend doesn't report caching for
    // this round" (Ollama, Anthropic-native today, any OpenRouter
    // provider without explicit `cache_control`), not "zero cache
    // tokens happened". Summing them as plain `u32` would conflate the
    // two and understate cost on a real cache write. Instead, stay
    // `None`-overall unless at least one round reported a value — then
    // sum every round's contribution (`None` rounds add 0, `Some(N)`
    // rounds add N) and expose the result as `Some`. Same semantics as
    // `Engine::complete_with_best_of_n`'s `sum_optional_u32`, so a
    // best-of-n turn where only one candidate reports cache tokens
    // still surfaces here rather than being zeroed by its siblings.
    let cache_read_tokens = sum_optional_u32(events.iter().map(|event| match event {
        AgentEvent::Usage {
            cache_read_tokens, ..
        } => *cache_read_tokens,
        _ => None,
    }));
    let cache_write_tokens = sum_optional_u32(events.iter().map(|event| match event {
        AgentEvent::Usage {
            cache_write_tokens, ..
        } => *cache_write_tokens,
        _ => None,
    }));

    // One `Usage` event is persisted per model completion round (see
    // `Engine::run_turn`) — a direct proxy for how many rounds this turn
    // took to converge (or to exhaust the cap).
    let rounds = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::Usage { .. }))
        .count() as u32;

    // H-3 (docs/AUDITORIA-2026-07-v5.md): the four SLM-first levers this
    // harness's whole thesis rests on, counted the same way `rounds` is —
    // these actions already existed (visible via `tracing::info!`/`warn!`
    // at their call sites in `engine.rs`), this is what makes them
    // bench-readable instead of log-only.
    let rescued_tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::TextualRescueApplied { .. }))
        .count() as u32;
    let leader_escalations = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::EscalationToLead { .. }))
        .count() as u32;
    let compaction_count = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::CompactionOccurred { .. }))
        .count() as u32;
    let summary_fallbacks = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::SummaryFallbackAttempted))
        .count() as u32;
    let harness_notes = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::HarnessNote { .. }))
        .count() as u32;

    let planned = events
        .iter()
        .any(|event| matches!(event, AgentEvent::PlanCreated { .. }));

    // J-7 (docs/AUDITORIA-2026-07-v7.md): only text AFTER the last tool
    // event counts as the answer. Small models narrate before calling
    // tools ("voy a leer las 2 primeras líneas...") and that narration is
    // persisted as `AssistantText`; concatenating the whole turn let a
    // task expecting "2" pass on the narration even when the final answer
    // was wrong — a false PASS that favored verbose models. For turns
    // with no tool activity every `AssistantText` is still counted
    // (identical to the old behavior for no_tool tasks).
    let last_tool_event_idx = events.iter().rposition(|event| {
        matches!(
            event,
            AgentEvent::AssistantToolCall { .. }
                | AgentEvent::ToolCallStarted { .. }
                | AgentEvent::ToolCallCompleted { .. }
        )
    });
    let final_text = events
        .iter()
        .enumerate()
        .filter(|(idx, _)| last_tool_event_idx.is_none_or(|tool_idx| *idx > tool_idx))
        .filter_map(|(_, event)| match event {
            AgentEvent::AssistantText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (converged, run_error, run_failure_cause) = match run_outcome {
        RunOutcome::Converged => (true, None, None),
        RunOutcome::TimedOut => (
            false,
            Some("task exceeded its time budget".to_string()),
            Some(FailureCause::Timeout),
        ),
        RunOutcome::Failed(err) => {
            let cause = match &err {
                braze_engine::EngineError::TurnDidNotConverge(_) => {
                    FailureCause::MaxIterationsExhausted
                }
                braze_engine::EngineError::TurnBudgetExhausted { .. } => {
                    FailureCause::TurnBudgetExhausted
                }
                braze_engine::EngineError::IncompleteStream => FailureCause::IncompleteStream,
                // AUDITORIA-2026-07-v8 K-1d: a tripped circuit breaker
                // is infrastructure state ("we didn't even ask the
                // model"), not model capability — route it to the
                // HarnessError bucket `report.rs` already excludes from
                // the pass-rate denominator (N-37), instead of
                // mass-charging an outage's fast-failing tail to the
                // model under test.
                braze_engine::EngineError::Model(braze_model::ModelError::CircuitOpen(_)) => {
                    FailureCause::HarnessError
                }
                braze_engine::EngineError::Model(_) => FailureCause::ModelBackendError,
                braze_engine::EngineError::Session(_) => FailureCause::SessionError,
                braze_engine::EngineError::Tool(_) => FailureCause::ToolRegistryError,
                // `EngineError` is `#[non_exhaustive]`: any future variant
                // this crate doesn't know about yet still gets a bucket
                // rather than failing to compile.
                _ => FailureCause::ModelBackendError,
            };
            (false, Some(err.to_string()), Some(cause))
        }
    };

    let expected_tool_called = task
        .expect_tool_call
        .as_deref()
        .map(|expected| tool_call_names.iter().any(|name| name == expected));

    let expected_text_found = task.expect_text_contains.as_deref().map(|expected| {
        contains_as_a_bounded_token(&final_text.to_lowercase(), &expected.to_lowercase())
    });

    // v4 P0.4 (docs/AUDITORIA-2026-07-v4.md): budget assertions. A
    // turn that converges with the right answer in 14 rounds / 50k
    // tokens is *not* as good as one that does it in 3 rounds / 4k
    // tokens — the bench must be able to say "better", not just
    // "passes". `expect_max_rounds` checks against `rounds` (one
    // `AgentEvent::Usage` per model completion round, computed above);
    // `expect_max_tokens` checks against `input_tokens + output_tokens`
    // summed across rounds (cache read/write tokens are reported
    // separately in `TaskResult` and aren't counted here — billing
    // concern, not model-efficiency concern).
    //
    // `None` always means "no budget asserted" (and thus `Some(true)`
    // — within budget — trivially), mirroring the other `Option<bool>`
    // assertion results so a report can distinguish "no budget was
    // declared" from "budget declared and blown".
    let expected_rounds_within_budget = task.expect_max_rounds.map(|max| rounds <= max);
    let total_tokens = input_tokens + output_tokens;
    let expected_tokens_within_budget = task.expect_max_tokens.map(|max| total_tokens <= max);

    // Paquete 3 (docs/AUDITORIA-2026-07-v6.md): see
    // `TaskResult::estimated_cost_usd`'s doc comment for the formula and
    // its OpenRouter assumptions.
    let estimated_cost_usd = pricing.map(|rates| {
        let cache_read = cache_read_tokens.unwrap_or(0) as f64;
        let cache_write = cache_write_tokens.unwrap_or(0) as f64;
        let uncached = (input_tokens as f64 - cache_read - cache_write).max(0.0);
        let input_cost = uncached * rates.input_usd_per_mtok
            + cache_read
                * rates
                    .cache_read_usd_per_mtok
                    .unwrap_or(rates.input_usd_per_mtok)
            + cache_write
                * rates
                    .cache_write_usd_per_mtok
                    .unwrap_or(rates.input_usd_per_mtok);
        (input_cost + output_tokens as f64 * rates.output_usd_per_mtok) / 1_000_000.0
    });
    // `None` when either half is missing: a declared budget with no
    // pricing is "not evaluated", never a free pass (see the field's doc
    // comment).
    let expected_cost_within_budget = match (task.expect_max_cost_usd, estimated_cost_usd) {
        (Some(max), Some(cost)) => Some(cost <= max),
        _ => None,
    };

    let assertions_passed = expected_tool_called.unwrap_or(true)
        && (!task.expect_no_tool_call || tool_call_names.is_empty())
        && expected_text_found.unwrap_or(true)
        && expected_files_found.unwrap_or(true)
        && expected_cargo_check_passed.unwrap_or(true)
        && expected_rounds_within_budget.unwrap_or(true)
        && expected_tokens_within_budget.unwrap_or(true)
        && expected_cost_within_budget.unwrap_or(true);

    let passed = converged && assertions_passed;

    // Only meaningful once we know the turn otherwise converged — a
    // failed run already has a cause (Timeout/MaxIterationsExhausted/...)
    // that takes priority over which specific assertion would also have
    // failed.
    let failure_cause = run_failure_cause.or_else(|| {
        if !converged || assertions_passed {
            return None;
        }
        if expected_tool_called == Some(false) {
            Some(FailureCause::AssertionToolCall)
        } else if expected_text_found == Some(false) {
            Some(FailureCause::AssertionText)
        } else if expected_files_found == Some(false) {
            Some(FailureCause::AssertionFiles)
        } else if expected_cargo_check_passed == Some(false) {
            Some(FailureCause::AssertionCargoCheck)
        } else if expected_rounds_within_budget == Some(false) {
            Some(FailureCause::AssertionMaxRounds)
        } else if expected_tokens_within_budget == Some(false) {
            Some(FailureCause::AssertionMaxTokens)
        } else if expected_cost_within_budget == Some(false) {
            Some(FailureCause::AssertionMaxCost)
        } else {
            None
        }
    });

    TaskResult {
        backend: backend.to_string(),
        task_id: task.id.clone(),
        skill: task.skill.clone(),
        memory_condition: crate::memory::resolved_memory_condition(task),
        memory_file: task
            .memory_file
            .as_ref()
            .map(|path| path.display().to_string()),
        memory_budget_tokens: task.memory_budget_tokens,
        memory_tokens: memory.memory_tokens,
        repetition,
        converged,
        run_error,
        failure_cause,
        tool_calls_total: tool_call_names.len() as u32,
        tool_call_names,
        schema_validation_failures,
        tool_execution_failures,
        permission_denials,
        rounds,
        planned,
        expected_tool_called,
        expected_text_found,
        expected_files_found,
        expected_cargo_check_passed,
        expected_rounds_within_budget,
        expected_tokens_within_budget,
        expected_cost_within_budget,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        rescued_tool_calls,
        leader_escalations,
        compaction_count,
        summary_fallbacks,
        harness_notes,
        estimated_cost_usd,
        wall_time_ms: wall_time.as_millis(),
        passed,
    }
}

/// Sums an optional-per-item `u32` (cache token counts that some
/// `AgentEvent::Usage` rounds report and others don't — Ollama never
/// does, OpenRouter does for some providers, Anthropic-native today
/// doesn't) into a single optional total. `None` only when *every* item
/// was `None` (nothing reported anything — stay silent rather than claim
/// "0 tokens cached"); `Some(sum)` once at least one item is `Some`,
/// treating any further `None` as 0. Mirrors
/// `Engine::complete_with_best_of_n`'s private `sum_optional_u32` so the
/// bench's aggregation of cache tokens across rounds matches the
/// engine's own across best-of-n candidates end-to-end.
fn sum_optional_u32(values: impl Iterator<Item = Option<u32>>) -> Option<u32> {
    let mut sum = 0u32;
    let mut any_reported = false;
    for n in values.flatten() {
        sum += n;
        any_reported = true;
    }
    any_reported.then_some(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::ToolResult;
    use std::collections::HashMap;

    fn task(
        expect_tool_call: Option<&str>,
        expect_no_tool_call: bool,
        expect_text_contains: Option<&str>,
    ) -> TaskDef {
        TaskDef {
            id: "t".to_string(),
            prompt: "irrelevant".to_string(),
            setup_files: HashMap::new(),
            expect_tool_call: expect_tool_call.map(str::to_string),
            expect_no_tool_call,
            expect_text_contains: expect_text_contains.map(str::to_string),
            expect_file_contains: HashMap::new(),
            expect_cargo_check: false,
            skill: None,
            expect_max_rounds: None,
            expect_max_tokens: None,
            expect_max_cost_usd: None,
            noise_tools: 0,
            memory_condition: None,
            memory_file: None,
            memory_budget_tokens: None,
        }
    }

    fn zero() -> Duration {
        Duration::from_millis(0)
    }

    /// Thin wrapper over `compute_metrics` fixing the args every test
    /// here doesn't vary (repetition 0, no file assertions) so each test
    /// body only names what it's actually exercising.
    fn metrics(task: &TaskDef, events: &[AgentEvent], run_outcome: RunOutcome) -> TaskResult {
        compute_metrics(
            "ollama:x",
            task,
            0,
            events,
            zero(),
            run_outcome,
            None,
            None,
            MemoryRunMetrics::default(),
            None,
        )
    }

    #[test]
    fn a_clean_text_only_turn_with_no_expectations_passes() {
        let events = vec![
            AgentEvent::UserMessage {
                text: "hola".to_string(),
            },
            AgentEvent::AssistantText {
                text: "mundo".to_string(),
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert!(result.passed);
        assert!(result.converged);
        assert_eq!(result.tool_calls_total, 0);
        assert_eq!(result.failure_cause, None);
    }

    #[test]
    fn expected_tool_call_that_happened_passes() {
        let events = vec![
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallStarted {
                id: "1".to_string(),
                name: "read_file".to_string(),
                background: true,
            },
            AgentEvent::ToolCallCompleted {
                id: "1".to_string(),
                result: ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "contenido".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "listo".to_string(),
            },
        ];
        let result = metrics(
            &task(Some("read_file"), false, None),
            &events,
            RunOutcome::Converged,
        );
        assert!(result.passed);
        assert_eq!(result.expected_tool_called, Some(true));
        assert_eq!(result.tool_calls_total, 1);
        assert_eq!(result.tool_call_names, vec!["read_file"]);
        assert_eq!(result.schema_validation_failures, 0);
        assert_eq!(result.tool_execution_failures, 0);
    }

    #[test]
    fn tool_call_names_preserve_order_and_repetitions() {
        let events = vec![
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::AssistantToolCall {
                id: "2".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::AssistantToolCall {
                id: "3".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.tool_calls_total, 3);
        assert_eq!(
            result.tool_call_names,
            vec!["read_file", "write_file", "read_file"]
        );
    }

    #[test]
    fn expected_tool_call_that_never_happened_fails() {
        let events = vec![AgentEvent::AssistantText {
            text: "no hice nada".to_string(),
        }];
        let result = metrics(
            &task(Some("read_file"), false, None),
            &events,
            RunOutcome::Converged,
        );
        assert!(!result.passed);
        assert_eq!(result.expected_tool_called, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionToolCall));
    }

    #[test]
    fn schema_rejected_call_is_counted_separately_from_execution_failure() {
        let events = vec![
            // Rejected before dispatch: no ToolCallStarted for this id.
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallCompleted {
                id: "1".to_string(),
                result: ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "schema validation failed".to_string(),
                    is_error: true,
                },
            },
            // Dispatched but failed at runtime: has a ToolCallStarted.
            AgentEvent::AssistantToolCall {
                id: "2".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"text": "hi"}),
            },
            AgentEvent::ToolCallStarted {
                id: "2".to_string(),
                name: "echo".to_string(),
                background: true,
            },
            AgentEvent::ToolCallCompleted {
                id: "2".to_string(),
                result: ToolResult {
                    tool_call_id: "2".to_string(),
                    content: "boom".to_string(),
                    is_error: true,
                },
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.schema_validation_failures, 1);
        assert_eq!(result.tool_execution_failures, 1);
    }

    #[test]
    fn permission_denials_are_counted() {
        let events = vec![
            AgentEvent::PermissionRequested {
                action: "run `dd if=/dev/zero of=/dev/sda`".to_string(),
                reversible: false,
                key: None,
            },
            AgentEvent::PermissionDecided {
                action: "run `dd if=/dev/zero of=/dev/sda`".to_string(),
                allowed: false,
                key: None,
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.permission_denials, 1);
    }

    #[test]
    fn expect_no_tool_call_fails_when_a_tool_was_called() {
        let events = vec![AgentEvent::AssistantToolCall {
            id: "1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        }];
        let result = metrics(&task(None, true, None), &events, RunOutcome::Converged);
        assert!(!result.passed);
    }

    #[test]
    fn expect_no_tool_call_passes_when_no_tool_was_called() {
        let events = vec![AgentEvent::AssistantText {
            text: "4".to_string(),
        }];
        let result = metrics(&task(None, true, Some("4")), &events, RunOutcome::Converged);
        assert!(result.passed);
    }

    #[test]
    fn expect_text_contains_is_case_insensitive() {
        let events = vec![AgentEvent::AssistantText {
            text: "La respuesta es CUATRO".to_string(),
        }];
        let result = metrics(
            &task(None, false, Some("cuatro")),
            &events,
            RunOutcome::Converged,
        );
        assert_eq!(result.expected_text_found, Some(true));
        assert!(result.passed);
    }

    /// J-7 (docs/AUDITORIA-2026-07-v7.md): pre-tool narration must not
    /// satisfy `expect_text_contains`. A model that narrates "voy a leer
    /// las 2 primeras líneas" before its tool call and then answers
    /// wrongly ("el archivo tiene 5 líneas") was a false PASS for a task
    /// expecting "2".
    #[test]
    fn expect_text_contains_ignores_narration_before_the_last_tool_event() {
        let events = vec![
            AgentEvent::AssistantText {
                text: "Voy a leer las 2 primeras líneas".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallStarted {
                id: "1".to_string(),
                name: "read_file".to_string(),
                background: true,
            },
            AgentEvent::ToolCallCompleted {
                id: "1".to_string(),
                result: ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "línea A\nlínea B\nlínea C\nlínea D\nlínea E".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "El archivo tiene 5 líneas".to_string(),
            },
        ];
        let result = metrics(
            &task(None, false, Some("2")),
            &events,
            RunOutcome::Converged,
        );
        assert_eq!(result.expected_text_found, Some(false));
        assert!(!result.passed);
    }

    /// J-7 counterpart: the answer given AFTER the last tool event still
    /// matches, and a no-tool turn keeps the old whole-turn behavior.
    #[test]
    fn expect_text_contains_matches_the_answer_after_the_last_tool_event() {
        let events = vec![
            AgentEvent::AssistantText {
                text: "Voy a contar las líneas".to_string(),
            },
            AgentEvent::ToolCallStarted {
                id: "1".to_string(),
                name: "read_file".to_string(),
                background: true,
            },
            AgentEvent::ToolCallCompleted {
                id: "1".to_string(),
                result: ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "línea A\nlínea B".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "El archivo tiene 2 líneas".to_string(),
            },
        ];
        let result = metrics(
            &task(None, false, Some("2")),
            &events,
            RunOutcome::Converged,
        );
        assert_eq!(result.expected_text_found, Some(true));
        assert!(result.passed);
    }

    // --- contains_as_a_bounded_token (hallazgo E4, docs/AUDITORIA-2026-07-v3.md) ---

    #[test]
    fn a_digit_embedded_in_a_filename_like_token_does_not_match() {
        // The exact false positive confirmed in the audit:
        // error_recovery_wrong_filename's setup file is named
        // "informe_final_v2.txt" and expects text_contains="2" — a wrong
        // answer that merely echoes the filename must not satisfy the
        // assertion just because "v2" contains a "2".
        assert!(!contains_as_a_bounded_token(
            "el archivo informe_final_v2.txt tiene 5 lineas",
            "2"
        ));
    }

    #[test]
    fn a_standalone_digit_surrounded_by_spaces_still_matches() {
        assert!(contains_as_a_bounded_token(
            "el archivo tiene 2 lineas",
            "2"
        ));
    }

    #[test]
    fn a_digit_at_the_very_start_or_end_of_the_text_still_matches() {
        assert!(contains_as_a_bounded_token("2 lineas", "2"));
        assert!(contains_as_a_bounded_token("son 2", "2"));
        assert!(contains_as_a_bounded_token("2", "2"));
    }

    #[test]
    fn a_longer_phrase_needle_still_matches_as_a_bounded_token() {
        assert!(contains_as_a_bounded_token(
            "el archivo es notas_presupuesto.txt",
            "notas_presupuesto"
        ));
    }

    #[test]
    fn a_needle_embedded_inside_a_longer_word_does_not_match() {
        // "presupuesto" must not match inside "prepresupuestoso" (a
        // synthetic larger token) even though it's a textual substring.
        assert!(!contains_as_a_bounded_token(
            "prepresupuestoso",
            "presupuesto"
        ));
    }

    #[test]
    fn punctuation_and_symbols_count_as_valid_boundaries() {
        assert!(contains_as_a_bounded_token("version=2", "2"));
        assert!(contains_as_a_bounded_token("respuesta: 4.", "4"));
    }

    #[test]
    fn an_empty_needle_always_matches() {
        assert!(contains_as_a_bounded_token("cualquier cosa", ""));
    }

    #[test]
    fn a_needle_longer_than_the_haystack_never_matches() {
        assert!(!contains_as_a_bounded_token("2", "informe"));
    }

    /// End-to-end regression for the exact false positive confirmed live
    /// in the suite (hallazgo E4): a wrong answer that just repeats the
    /// setup file's name ("informe_final_v2.txt") must FAIL
    /// `error_recovery`-shaped tasks expecting `"2"`, not pass by
    /// accident via the embedded "v2".
    #[test]
    fn a_wrong_line_count_that_echoes_the_v2_filename_no_longer_falsely_passes() {
        let t = task(None, false, Some("2"));
        let events = vec![AgentEvent::AssistantText {
            text: "El archivo informe_final_v2.txt tiene 5 líneas.".to_string(),
        }];
        let result = metrics(&t, &events, RunOutcome::Converged);
        assert_eq!(result.expected_text_found, Some(false));
        assert!(!result.passed);
    }

    #[test]
    fn a_run_error_fails_the_task_regardless_of_other_expectations() {
        let events: Vec<AgentEvent> = vec![];
        let result = metrics(
            &task(None, false, None),
            &events,
            RunOutcome::Failed(braze_engine::EngineError::TurnDidNotConverge(20)),
        );
        assert!(!result.passed);
        assert!(!result.converged);
        assert!(result.run_error.is_some());
        assert_eq!(
            result.failure_cause,
            Some(FailureCause::MaxIterationsExhausted)
        );
    }

    #[test]
    fn a_timeout_is_reported_as_its_own_failure_cause() {
        let events: Vec<AgentEvent> = vec![];
        let result = metrics(&task(None, false, None), &events, RunOutcome::TimedOut);
        assert!(!result.passed);
        assert!(!result.converged);
        assert_eq!(result.failure_cause, Some(FailureCause::Timeout));
    }

    #[test]
    fn token_usage_is_summed_across_rounds() {
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 2,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            AgentEvent::Usage {
                input_tokens: 15,
                output_tokens: 3,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.input_tokens, 25);
        assert_eq!(result.output_tokens, 5);
    }

    // --- cache token aggregation (hallazgo H-1,
    // docs/AUDITORIA-2026-07-v5.md: `TaskResult::cache_read_tokens`/
    // `cache_write_tokens` were missing from `compute_metrics`, so the
    // WIP that added cache tokens to `AgentEvent::Usage` — across
    // `CompletionEvent::Usage`, `Engine::RoundUsage`, and the event log
    // — never reached the bench's per-row JSON. These tests pin the
    // aggregation rule end-to-end: `None` per-round means "this backend
    // doesn't report caching", not "zero cache tokens happened"; only a
    // round reporting `Some(N)` flips the overall to `Some`, and further
    // `None`s add 0 rather than zeroing the running sum. Mirrors
    // `Engine::complete_with_best_of_n`'s `sum_optional_u32` so a
    // best-of-n turn where only some candidates report cache tokens
    // still surfaces in the bench's sum.) ---

    /// The basic case a paper A/B "with vs without prompt caching" needs
    /// to read off the JSON: two rounds, one writes the cache entry, the
    /// next reads it back. The bench's per-row total must be the sum,
    /// separately for cache-read and cache-write.
    #[test]
    fn cache_tokens_are_summed_across_rounds_when_any_round_reports_them() {
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10_000,
                output_tokens: 100,
                stop_reason: Some("tool_use".to_string()),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(9_500),
            },
            AgentEvent::Usage {
                input_tokens: 10_200,
                output_tokens: 50,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: Some(10_100),
                cache_write_tokens: Some(0),
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.cache_read_tokens, Some(10_100));
        assert_eq!(result.cache_write_tokens, Some(9_500));
    }

    /// `None` overall when *no* round reported cache tokens — the case
    /// for any backend that doesn't expose caching (Ollama, Anthropic-native
    /// today). Caller can tell "this backend doesn't report caching" apart
    /// from "this task genuinely had zero cache hits".
    #[test]
    fn cache_tokens_stay_none_when_no_round_reports_them() {
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10_000,
                output_tokens: 100,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            AgentEvent::Usage {
                input_tokens: 10_200,
                output_tokens: 50,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.cache_read_tokens, None);
        assert_eq!(result.cache_write_tokens, None);
    }

    /// The key distinction `Option<u32>` (not `u32`) exists to preserve:
    /// a backend that reports `Some(0)` for every round ("the cache is
    /// empty but the API DID tell us that") must surface as `Some(0)`,
    /// not collapse to `None` and become indistinguishable from a
    /// backend that never reported cache stats at all. This is the exact
    /// regression that would sneak in if a future refactor used
    /// `.unwrap_or(0)`-then-`sum` instead of the `sum_optional_u32`
    /// rule.
    #[test]
    fn cache_tokens_with_some_zero_is_distinguishable_from_not_reported() {
        let events = vec![AgentEvent::Usage {
            input_tokens: 100,
            output_tokens: 10,
            stop_reason: Some("end_turn".to_string()),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
        }];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.cache_read_tokens, Some(0));
        assert_eq!(result.cache_write_tokens, Some(0));
        assert_ne!(result.cache_read_tokens, None);
    }

    /// Mixed Some/None across rounds: a mid-turn round from a backend
    /// that doesn't report cache (e.g. a summary-fallback round that
    /// went through a different provider) must not zero out the cache
    /// total the earlier rounds DID report. Same rule `Engine::complete_with_best_of_n`
    /// applies across its candidates so a single `Some` survives.
    #[test]
    fn cache_tokens_with_mixed_some_and_none_rounds_keep_the_reported_sum() {
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10_100,
                output_tokens: 100,
                stop_reason: Some("tool_use".to_string()),
                cache_read_tokens: Some(10_000),
                cache_write_tokens: Some(0),
            },
            // Round 2 from a degraded fallback path that doesn't report
            // cache stats — `None` here must contribute 0, not reset the
            // running total back to `None`.
            AgentEvent::Usage {
                input_tokens: 500,
                output_tokens: 50,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.cache_read_tokens, Some(10_000));
        assert_eq!(result.cache_write_tokens, Some(0));
    }

    /// `harness_error_result` (sandbox setup failure, etc.) never ran a
    /// single round, so it can't have reported cache tokens either —
    /// `None`, not `Some(0)`. Same contract as `compute_metrics` above.
    #[test]
    fn harness_error_result_reports_none_for_cache_tokens() {
        let t = task(None, false, None);
        let error = crate::error::BenchError::Startup("sandbox setup failed".to_string());
        let result = harness_error_result("ollama:x", &t, 0, &error);
        assert_eq!(result.cache_read_tokens, None);
        assert_eq!(result.cache_write_tokens, None);
    }

    /// PLAN.md § "Split planificador/ejecutor", oleada 4: `planned`
    /// reflects what actually happened (a `PlanCreated` in the log), not
    /// what the spec configured — a degraded planner yields `false`.
    #[test]
    fn planned_reflects_the_presence_of_a_plan_created_event() {
        let unplanned = metrics(
            &task(None, false, None),
            &[AgentEvent::AssistantText {
                text: "hola".to_string(),
            }],
            RunOutcome::Converged,
        );
        assert!(!unplanned.planned);

        let planned = metrics(
            &task(None, false, None),
            &[
                AgentEvent::PlanCreated {
                    plan: "1. responder".to_string(),
                },
                AgentEvent::AssistantText {
                    text: "hola".to_string(),
                },
            ],
            RunOutcome::Converged,
        );
        assert!(planned.planned);
    }

    #[test]
    fn rounds_counts_one_usage_event_per_model_round() {
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 2,
                stop_reason: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::Usage {
                input_tokens: 12,
                output_tokens: 3,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.rounds, 2);
    }

    /// H-3 (docs/AUDITORIA-2026-07-v5.md): the 4 SLM-first levers, each
    /// counted independently from their own `AgentEvent` variant — this
    /// is the whole point of the hallazgo, that the bench can finally
    /// read these off the event log instead of only `tracing::info!`.
    #[test]
    fn compute_metrics_counts_each_slm_lever_independently() {
        let events = vec![
            AgentEvent::TextualRescueApplied {
                parser: "<tool_call> tagged (Qwen/Hermes)".to_string(),
            },
            AgentEvent::TextualRescueApplied {
                parser: "pythonic [func(...)] (Llama)".to_string(),
            },
            AgentEvent::EscalationToLead {
                trigger: "2 consecutive failed observations (threshold 2)".to_string(),
            },
            AgentEvent::CompactionOccurred {
                summary: "digest".to_string(),
                dropped_tokens_estimate: 500,
            },
            AgentEvent::SummaryFallbackAttempted,
        ];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.rescued_tool_calls, 2);
        assert_eq!(result.leader_escalations, 1);
        assert_eq!(result.compaction_count, 1);
        assert_eq!(result.summary_fallbacks, 1);
    }

    /// A run with none of the 4 levers firing reports all-zero counts, not
    /// `None`/absent fields — same "0 means it didn't happen, always
    /// meaningful" contract the field doc comments describe.
    #[test]
    fn compute_metrics_reports_zero_slm_levers_when_none_fired() {
        let events = vec![AgentEvent::AssistantText {
            text: "hola".to_string(),
        }];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.rescued_tool_calls, 0);
        assert_eq!(result.leader_escalations, 0);
        assert_eq!(result.compaction_count, 0);
        assert_eq!(result.summary_fallbacks, 0);
    }

    #[test]
    fn expect_file_contains_fails_the_task_when_the_file_does_not_match() {
        let events = vec![AgentEvent::AssistantText {
            text: "listo".to_string(),
        }];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            0,
            &events,
            zero(),
            RunOutcome::Converged,
            Some(false),
            None,
            MemoryRunMetrics::default(),
            None,
        );
        assert!(!result.passed);
        assert_eq!(result.expected_files_found, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionFiles));
    }

    #[test]
    fn expect_file_contains_passes_the_task_when_the_file_matches() {
        let events = vec![AgentEvent::AssistantText {
            text: "listo".to_string(),
        }];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            0,
            &events,
            zero(),
            RunOutcome::Converged,
            Some(true),
            None,
            MemoryRunMetrics::default(),
            None,
        );
        assert!(result.passed);
        assert_eq!(result.expected_files_found, Some(true));
    }

    #[test]
    fn harness_error_result_is_marked_unconverged_with_a_harness_cause() {
        let t = task(None, false, None);
        let error = crate::error::BenchError::Startup("sandbox setup failed".to_string());
        let result = harness_error_result("ollama:x", &t, 0, &error);
        assert!(!result.passed);
        assert!(!result.converged);
        assert_eq!(result.failure_cause, Some(FailureCause::HarnessError));
        assert!(result.run_error.unwrap().contains("sandbox setup failed"));
    }

    // --- v4 P0.4 budget assertions (expect_max_rounds /
    // expect_max_tokens), docs/AUDITORIA-2026-07-v4.md § P0.4. The bench
    // must be able to say a config is "better" not just "passes" — a
    // turn that converges with the right answer in 14 rounds / 50k
    // tokens is worse than one that does it in 3 rounds / 4k tokens,
    // and a flat pass-rate can't tell them apart. `expected_*_within_
    // budget` follows the same `Option<bool>` "not asserted / asserted-
    // passed / asserted-failed" contract as `expected_tool_called`. ---

    fn usage_round(input: u32, output: u32) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            stop_reason: Some("end_turn".to_string()),
            cache_read_tokens: None,
            cache_write_tokens: None,
        }
    }

    /// `None`-overall when the `TaskDef` declared no budget — the same
    /// "not asserted" semantics as `expected_tool_called == None`, so a
    /// report can tell "no budget was declared" apart from "budget was
    /// declared and blown".
    #[test]
    fn budget_assertions_stay_none_when_not_declared_on_the_task() {
        let events = vec![usage_round(10, 2), usage_round(15, 3)];
        let result = metrics(&task(None, false, None), &events, RunOutcome::Converged);
        assert_eq!(result.expected_rounds_within_budget, None);
        assert_eq!(result.expected_tokens_within_budget, None);
        assert!(result.passed);
    }

    #[test]
    fn expect_max_rounds_passes_when_the_turn_used_fewer_rounds_than_the_budget() {
        // 2 Usage events => 2 rounds; budget 8 is comfortably above.
        let events = vec![usage_round(10, 2), usage_round(15, 3)];
        let mut t = task(None, false, None);
        t.expect_max_rounds = Some(8);
        let result = metrics(&t, &events, RunOutcome::Converged);
        assert_eq!(result.rounds, 2);
        assert_eq!(result.expected_rounds_within_budget, Some(true));
        assert!(result.passed);
    }

    #[test]
    fn expect_max_rounds_equal_to_the_count_is_within_budget_inclusive() {
        // Budget == actual is within (<=), matching the "at most" wording
        // on the field's doc comment.
        let events = vec![usage_round(10, 2), usage_round(15, 3)];
        let mut t = task(None, false, None);
        t.expect_max_rounds = Some(2);
        let result = metrics(&t, &events, RunOutcome::Converged);
        assert_eq!(result.expected_rounds_within_budget, Some(true));
        assert!(result.passed);
    }

    #[test]
    fn expect_max_rounds_fails_the_task_when_the_turn_used_more_rounds() {
        let events = vec![usage_round(10, 2), usage_round(15, 3), usage_round(12, 1)];
        let mut t = task(None, false, None);
        t.expect_max_rounds = Some(2);
        let result = metrics(&t, &events, RunOutcome::Converged);
        assert!(!result.passed);
        assert_eq!(result.expected_rounds_within_budget, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionMaxRounds));
    }

    #[test]
    fn expect_max_tokens_passes_when_total_tokens_stay_under_the_budget() {
        // 10+2 + 15+3 = 30 total (input + output). Cache tokens aren't
        // counted even when reported — see the field's doc comment.
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 2,
                stop_reason: Some("tool_use".to_string()),
                cache_read_tokens: Some(8),
                cache_write_tokens: Some(4),
            },
            usage_round(15, 3),
        ];
        let mut t = task(None, false, None);
        t.expect_max_tokens = Some(40);
        let result = metrics(&t, &events, RunOutcome::Converged);
        // cache tokens surfaced separately, not folded into the budget.
        assert_eq!(result.cache_read_tokens, Some(8));
        assert_eq!(result.cache_write_tokens, Some(4));
        assert_eq!(result.input_tokens, 25);
        assert_eq!(result.output_tokens, 5);
        assert_eq!(result.expected_tokens_within_budget, Some(true));
        assert!(result.passed);
    }

    #[test]
    fn expect_max_tokens_fails_the_task_when_total_tokens_exceed_the_budget() {
        // 10+2 + 15+3 = 30 total; budget 25 <= blown.
        let events = vec![usage_round(10, 2), usage_round(15, 3)];
        let mut t = task(None, false, None);
        t.expect_max_tokens = Some(25);
        let result = metrics(&t, &events, RunOutcome::Converged);
        assert!(!result.passed);
        assert_eq!(result.expected_tokens_within_budget, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionMaxTokens));
    }

    // --- estimated_cost_usd + expect_max_cost_usd enforcement
    // (Paquete 3, docs/AUDITORIA-2026-07-v6.md) ---

    fn rates(input: f64, output: f64) -> crate::backend_spec::PricingRates {
        crate::backend_spec::PricingRates {
            input_usd_per_mtok: input,
            output_usd_per_mtok: output,
            cache_read_usd_per_mtok: None,
            cache_write_usd_per_mtok: None,
        }
    }

    fn metrics_priced(
        task: &TaskDef,
        events: &[AgentEvent],
        pricing: Option<crate::backend_spec::PricingRates>,
    ) -> TaskResult {
        compute_metrics(
            "openrouter:x",
            task,
            0,
            events,
            zero(),
            RunOutcome::Converged,
            None,
            None,
            MemoryRunMetrics::default(),
            pricing,
        )
    }

    /// The plain formula with no cache reporting: input*in + output*out.
    /// 1M input at $0.09 + 1M output at $0.18 = $0.27 exactly.
    #[test]
    fn estimated_cost_is_input_plus_output_when_no_cache_is_reported() {
        let events = vec![usage_round(1_000_000, 1_000_000)];
        let result = metrics_priced(&task(None, false, None), &events, Some(rates(0.09, 0.18)));
        let cost = result.estimated_cost_usd.expect("priced row must estimate");
        assert!((cost - 0.27).abs() < 1e-9, "got {cost}");
    }

    /// Cache-read tokens bill at their own rate when provided; the
    /// uncached remainder bills at the input rate.
    #[test]
    fn estimated_cost_bills_cache_reads_at_their_own_rate() {
        let events = vec![AgentEvent::Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            stop_reason: Some("end_turn".to_string()),
            cache_read_tokens: Some(500_000),
            cache_write_tokens: None,
        }];
        let mut priced = rates(1.0, 0.0);
        priced.cache_read_usd_per_mtok = Some(0.1);
        let result = metrics_priced(&task(None, false, None), &events, Some(priced));
        // 500k uncached at $1/M + 500k cached at $0.1/M = 0.5 + 0.05.
        let cost = result.estimated_cost_usd.unwrap();
        assert!((cost - 0.55).abs() < 1e-9, "got {cost}");
    }

    /// `Some(0.0)` (all-zero rates: Ollama) is a real answer, distinct
    /// from `None` (no pricing resolved) — mirror of the cache-token
    /// contract.
    #[test]
    fn a_zero_rate_row_estimates_zero_not_none() {
        let events = vec![usage_round(10_000, 500)];
        let priced = metrics_priced(&task(None, false, None), &events, Some(rates(0.0, 0.0)));
        assert_eq!(priced.estimated_cost_usd, Some(0.0));
        let unpriced = metrics_priced(&task(None, false, None), &events, None);
        assert_eq!(unpriced.estimated_cost_usd, None);
    }

    #[test]
    fn expect_max_cost_fails_the_task_when_the_estimate_exceeds_it() {
        let events = vec![usage_round(1_000_000, 1_000_000)]; // $0.27 at these rates
        let mut t = task(None, false, None);
        t.expect_max_cost_usd = Some(0.10);
        let result = metrics_priced(&t, &events, Some(rates(0.09, 0.18)));
        assert!(!result.passed);
        assert_eq!(result.expected_cost_within_budget, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionMaxCost));
    }

    #[test]
    fn expect_max_cost_passes_when_the_estimate_fits() {
        let events = vec![usage_round(100_000, 10_000)];
        let mut t = task(None, false, None);
        t.expect_max_cost_usd = Some(0.05);
        let result = metrics_priced(&t, &events, Some(rates(0.09, 0.18)));
        assert!(result.passed);
        assert_eq!(result.expected_cost_within_budget, Some(true));
    }

    /// A declared budget on an UNPRICED row is "not evaluated" (`None`),
    /// never a free pass or a guessed failure.
    #[test]
    fn a_cost_budget_without_pricing_is_not_evaluated() {
        let events = vec![usage_round(1_000_000, 1_000_000)];
        let mut t = task(None, false, None);
        t.expect_max_cost_usd = Some(0.000001); // absurdly tight...
        let result = metrics_priced(&t, &events, None); // ...but unpriced
        assert!(result.passed, "must not fail on a price it doesn't know");
        assert_eq!(result.expected_cost_within_budget, None);
    }

    /// The priority order in `compute_metrics`'s failure-cause chain:
    /// a correctness failure is reported *over* a budget failure when
    /// both would fail (a small model that got the wrong answer AND took
    /// too many rounds is broken on correctness first — the budget
    /// wouldn't have rescued it). Exactly mirrors how `AssertionFiles`
    /// already takes priority over `AssertionMaxRounds` below it.
    #[test]
    fn a_correctness_failure_takes_priority_over_a_budget_failure_in_the_reported_cause() {
        let events = vec![usage_round(10, 2), usage_round(15, 3), usage_round(12, 1)];
        let mut t = task(None, false, Some("respuesta-correcta"));
        t.expect_max_rounds = Some(2);
        let result = metrics(&t, &events, RunOutcome::Converged);
        assert!(!result.passed);
        // Both the text check and the rounds budget fail here — the
        // report should name the correctness one, not the budget one.
        assert_eq!(result.expected_text_found, Some(false));
        assert_eq!(result.expected_rounds_within_budget, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionText));
    }
}
