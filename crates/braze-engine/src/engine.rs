//! [`Engine`]: the agentic loop. Composition root — this is the only crate
//! that talks to `braze-model`, `braze-tools-core`, `braze-session` and
//! `braze-events` at the same time (see PLAN.md, dependency graph).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use braze_events::{AgentEvent, BackgroundTask, TaskHandle, TaskNotifier, TurnObserver};
use braze_model::{CompletionEvent, CompletionRequest, ModelBackend};
use braze_session::{ContextCompactor, DurableState, SessionError, SessionStore};
use braze_tools_core::ToolRegistry;
use braze_types::{ContentBlock, Message, SessionId, ToolCall, ToolResult, ToolStub};

use crate::error::EngineError;
use crate::history::{
    MAX_FULL_OBSERVATIONS_TOTAL_CHARS, TACTICAL_FULL_OBSERVATIONS,
    build_messages_with_full_observations, render_durable_events, render_tactical_events,
};

/// Default number of raw tactical events above which [`Engine::run_turn`]
/// triggers a compaction pass before building the next model request. See
/// [`Engine::new`].
pub const DEFAULT_TACTICAL_COMPACTION_THRESHOLD: usize = 40;

/// Default safety cap on model/tool-call round trips within a single
/// [`Engine::run_turn`] call, so a model that never converges on a
/// text-only response can't hang the turn forever. Now exposed as
/// [`Engine::max_turn_iterations`] (defaults to this constant) —
/// configurable via `Config::max_turn_iterations`
/// (`BRAZE_MAX_TURN_ITERATIONS`, v4 P0.2/healf rounds) because the
/// optimum depends on model capacity (a SLM converging in ~3-5 rondas
/// benefits less from a high cap than one prone to floundering).
const MAX_TURN_ITERATIONS: usize = 20;

/// How long to wait for a single background tool task to complete before
/// treating it (and only it — sibling tasks keep waiting) as failed. See
/// the doc comment on the completion-collection loop in
/// [`Engine::run_turn`] for the documented MVP limitation this implies.
const TOOL_COMPLETION_TIMEOUT: Duration = Duration::from_secs(120);

/// Minimum number of raw tactical events always rendered verbatim to the
/// model, even in the same round a compaction just ran — see
/// [`Engine::load_messages`]. Without this, a compaction discarded the
/// *entire* tactical window (including the user's message for the current
/// turn, just appended in `run_turn`), so the model's next request
/// contained no trace of what was actually being asked.
const KEEP_RAW_TAIL: usize = 6;

/// Local tool names whose successful dispatch may change filesystem/
/// environment state that another tool's result depends on — F6
/// (docs/AUDITORIA-2026-07-v3.md): the repeated-call nudge in
/// `dispatch_tool_calls` claims "the result has not changed", which is
/// false for the canonical `read_file(x) → write_file(x) → read_file(x)`
/// pattern (re-verifying an edit) — without this, the second `read_file`
/// gets nudged and blocked from actually re-running instead of returning
/// the file's real, now-different content. Read-only-by-default is the
/// safe assumption here (mirrors `history.rs`'s `NEVER_CLEAR_TOOLS`
/// starting empty) — MCP tools aren't covered; revisit with a read/write
/// annotation on `ToolStub` if that becomes a real gap.
const MUTATING_TOOL_NAMES: &[&str] = &["write_file", "edit_file", "shell_exec"];

/// Consecutive zero-tool-call turns before `run_turn` injects the
/// narration-without-action reminder (D5, docs/AUDITORIA-2026-07-v3.md).
/// `2`: the *third* such turn in a row gets the reminder — one narrated
/// response alone is often a legitimate conversational answer; two in a
/// row against a request that should have produced a tool call is the
/// pattern actually observed.
const NARRATION_WITHOUT_ACTION_THRESHOLD: u32 = 2;

/// Default cap on the planning round's `max_tokens` (PLAN.md § "Split
/// planificador/ejecutor") — plans are short numbered lists; letting the
/// planner spend the executor's full `max_tokens` budget would just be
/// cost with no benefit. The effective value is
/// `min(self.max_tokens, self.planner_max_tokens)`, so a caller with a
/// *smaller* overall budget is still respected. Now exposed as
/// [`Engine::planner_max_tokens`] (defaults to this constant) —
/// configurable via `Config::planner_max_tokens`
/// (`BRAZE_PLANNER_MAX_TOKENS`, v4 P0.2/healf rounds).
const PLANNER_MAX_TOKENS: u32 = 1024;

/// The agentic loop. Orchestrates model calls, tool dispatch (via
/// background tasks + push notification), differential context
/// compaction, and session persistence.
pub struct Engine {
    model: Box<dyn ModelBackend>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn SessionStore>,
    compactor: Box<dyn ContextCompactor>,
    notifier: Box<dyn TaskNotifier>,
    system_prompt: String,
    max_tokens: u32,
    tactical_compaction_threshold: usize,
    /// How many of the tactical window's most recent observations stay
    /// full instead of collapsing to one line — see
    /// `history::TACTICAL_FULL_OBSERVATIONS`'s doc comment. Defaults to
    /// that same constant; overridable via
    /// [`Engine::with_tactical_full_observations`] for `braze-bench`'s
    /// `+ablate:full-observations=N` (E1, docs/AUDITORIA-2026-07-v3.md).
    tactical_full_observations: usize,
    /// Gates the ACI collapse of old observations — `false` renders every
    /// tactical observation full regardless of age/size. See
    /// [`Engine::with_observation_collapse_enabled`] (`+ablate:no-prune`).
    observation_collapse_enabled: bool,
    /// Gates tactical compaction entirely (both the event-count and the
    /// token-budget triggers) — see
    /// [`Engine::with_compaction_enabled`] (`+ablate:no-compaction`).
    compaction_enabled: bool,
    /// Cumulative per-turn token circuit breaker (v4 P0.2): once a
    /// turn's summed `input + output` across rounds exceeds this, the
    /// loop stops gracefully instead of re-sending an ever-growing
    /// history. `None` (the default) = disabled. See
    /// [`Engine::with_max_turn_total_tokens`].
    max_turn_total_tokens: Option<u64>,
    /// Approximate token budget for the durable+tactical portion of the
    /// prompt (i.e. excluding `system_prompt`/tool schemas, which the
    /// caller should already have reserved headroom for when computing
    /// this). `None` (the default) means compaction is triggered purely
    /// by `tactical_compaction_threshold`'s raw event count, as before —
    /// set via [`Engine::with_context_budget`] for backends with a small,
    /// known context window (e.g. Ollama's `num_ctx`), where a single
    /// large tool result can blow the budget long before the event count
    /// does. See [`Engine::load_messages`].
    context_budget_tokens: Option<u32>,
    /// Number of independent candidates per round for técnica G10
    /// (docs/AUDITORIA-2026-07.md, Best-of-n / Test-Time Scaling). `1`
    /// (the default) or `0` disable the technique entirely — the round
    /// goes through [`Engine::complete_once`] directly, exactly the same
    /// code path as before G10 existed. Only `> 1` routes the round
    /// through [`Engine::complete_with_best_of_n`].
    best_of_n: usize,
    /// Overrides [`TOOL_COMPLETION_TIMEOUT`] — see
    /// [`Engine::with_tool_completion_timeout`]. Kept configurable (rather
    /// than only ever the module constant) purely so tests can exercise
    /// the timeout-then-abort path in `dispatch_tool_calls` without
    /// actually waiting 120 real seconds.
    tool_completion_timeout: Duration,
    /// Safety cap on model/tool-call round trips within a single
    /// [`Engine::run_turn`] call. Defaults to [`MAX_TURN_ITERATIONS`];
    /// overridden via [`Engine::with_max_turn_iterations`] from
    /// `Config::max_turn_iterations` (v4 P0.2/mitad rondas).
    max_turn_iterations: usize,
    /// Cap on the planning round's `max_tokens` (effective value is
    /// `min(self.max_tokens, self.planner_max_tokens)`). Defaults to
    /// [`PLANNER_MAX_TOKENS`]; overridden via
    /// [`Engine::with_planner_max_tokens`] from `Config::planner_max_tokens`
    /// (v4 P0.2/mitad rondas).
    planner_max_tokens: u32,
    /// Set for the duration of a [`Engine::run_turn`] call, cleared on
    /// every exit path via [`TurnGuard`]'s `Drop`. N-17
    /// (docs/AUDITORIA-2026-07-v2.md): two concurrent `run_turn` calls on
    /// the same `Engine` would share one `TaskNotifier`'s single
    /// completion channel — `dispatch_tool_calls`'s "stale completion"
    /// check (see its doc comment) would discard the *other* turn's real
    /// completions as stale, and that turn would eventually persist a
    /// false timeout error. Not reachable via any current caller (every
    /// caller of `Engine::run_turn` serializes turns), but was previously
    /// neither guarded against nor documented — this turns the misuse
    /// into an explicit, diagnosable error instead of silent cross-talk
    /// if that ever changes.
    turn_in_progress: std::sync::atomic::AtomicBool,
    /// How many *consecutive* turns ended with zero tool calls dispatched
    /// (a plain text final answer, never a `dispatch_tool_calls` call) —
    /// D5 (docs/AUDITORIA-2026-07-v3.md): the repeated-call nudge (A5) is
    /// intra-turn and only fires in response to an actual tool call, so
    /// it never catches this project's own documented failure mode — a
    /// model that just keeps *narrating* an intended action turn after
    /// turn without ever calling the tool for it (observed live against
    /// qwen2.5:3b, `prompt.rs`'s doc comment). Reset to 0 the moment any
    /// turn dispatches a real tool call; incremented only on the "final
    /// text answer, no tool calls" success path in `run_turn`. Persists
    /// across `run_turn` calls on the same `Engine` instance (a CLI
    /// session's whole lifetime, barring a manual model switch or
    /// process restart) — never read back from the session log.
    consecutive_turns_without_tool_calls: std::sync::atomic::AtomicU32,
    /// Gates the textual tool-call rescue in [`Engine::complete_once`] —
    /// see [`Engine::with_textual_rescue_enabled`]. `true` (the default)
    /// preserves the existing behavior.
    textual_rescue_enabled: bool,
    /// Optional planner backend (PLAN.md § "Split planificador/ejecutor"):
    /// a stronger model that produces a one-shot plan before the turn's
    /// first executor round, persisted as [`AgentEvent::PlanCreated`].
    /// `None` (the default) means zero behavior change — see
    /// [`Engine::with_planner`] and [`Engine::attempt_planning_round`].
    planner: Option<Box<dyn ModelBackend>>,
}

/// The resolved outcome of one full model completion — everything the
/// round loop in [`Engine::run_turn`] needs to decide what happens next,
/// whether it came from a single attempt ([`Engine::complete_once`]) or
/// was chosen by vote among several ([`Engine::complete_with_best_of_n`]).
/// One round's `Usage` report — a named struct instead of a growing tuple
/// so the ~4 sites that build/sum/destructure it (`complete_once_with`,
/// `complete_with_best_of_n`, `run_turn`'s persist call) can't mix up
/// positional fields (docs/usability-log-2026-07-07-si2.md, prompt-caching
/// design — this grew from 3 fields to 5 when cache token counts were
/// added, past the point a tuple stays readable).
#[derive(Debug, Clone)]
struct RoundUsage {
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: Option<String>,
    cache_read_tokens: Option<u32>,
    cache_write_tokens: Option<u32>,
    /// Set when `EscalatingBackend` stamped this round's `Usage` event as
    /// the one that triggered a reactive escalation (H-3,
    /// docs/AUDITORIA-2026-07-v5.md) — see
    /// `CompletionEvent::Usage::escalation_trigger`'s doc comment.
    escalation_trigger: Option<String>,
}

struct RoundOutcome {
    text_buffer: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<RoundUsage>,
    /// Set when the backend reported `stop_reason: "max_tokens"`/`"length"`
    /// for this round — N-24 (docs/AUDITORIA-2026-07-v2.md): `run_turn`
    /// checks this before treating an empty-`tool_calls` round as a
    /// legitimate final answer, since a truncated response may be cut off
    /// mid-sentence (or mid-tool-call-JSON, which then fails to parse
    /// downstream with no other indication why).
    truncated: bool,
    /// Which rung of the textual-rescue ladder recovered a tool call this
    /// round, if any (H-3, docs/AUDITORIA-2026-07-v5.md) — see
    /// `AgentEvent::TextualRescueApplied`'s doc comment.
    rescue_applied: Option<String>,
}

/// Per-turn mutable state `dispatch_tool_calls` threads across every round
/// of one `run_turn` call — bundled into one struct rather than a growing
/// list of `&mut` parameters. Constructed fresh in `run_turn` and never
/// persisted or reused across turns.
struct TurnDispatchState {
    /// Per-tool-name retry counter for the "one round of schema-repair
    /// context" mechanism in `dispatch_tool_calls`.
    schema_retry_counts: HashMap<String, u32>,
    /// (tool name, canonical arguments) pairs already dispatched this
    /// turn — see `dispatch_tool_calls`'s repetition check (A5,
    /// docs/AUDITORIA-2026-07.md).
    seen_calls: HashSet<(String, String)>,
    /// Every `tool_use` id already in this session's history plus every
    /// one minted so far this turn — see `ensure_unique_tool_call_id` and
    /// N-14, docs/AUDITORIA-2026-07-v2.md.
    known_tool_call_ids: HashSet<String>,
}

/// RAII guard for [`Engine::turn_in_progress`] — see that field's doc
/// comment (N-17, docs/AUDITORIA-2026-07-v2.md). `acquire` fails if a
/// turn is already in flight; the flag is cleared on `Drop`, covering
/// every exit path from `run_turn` (success, any `?`-propagated error, or
/// an unwind) from one construction at the top instead of touching each
/// exit point individually.
struct TurnGuard<'a> {
    flag: &'a std::sync::atomic::AtomicBool,
}

impl<'a> TurnGuard<'a> {
    fn acquire(flag: &'a std::sync::atomic::AtomicBool) -> Result<Self, EngineError> {
        flag.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .map_err(|_| EngineError::ConcurrentTurn)?;
        Ok(Self { flag })
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Engine {
    /// Builds an `Engine` with [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] as
    /// its compaction trigger. `tools` is wrapped internally in an `Arc` so
    /// it can be shared into the `'static` background-task futures
    /// [`TaskNotifier::spawn`] takes ownership of — `ToolRegistry` itself
    /// is not `Clone` (it owns a `Vec<Box<dyn ToolProvider>>`), so an `Arc`
    /// is the seam that lets the same registry be dispatched against from
    /// many concurrently-spawned tasks without cloning its providers.
    pub fn new(
        model: Box<dyn ModelBackend>,
        tools: ToolRegistry,
        store: Arc<dyn SessionStore>,
        compactor: Box<dyn ContextCompactor>,
        notifier: Box<dyn TaskNotifier>,
        system_prompt: String,
        max_tokens: u32,
    ) -> Self {
        Self {
            model,
            tools: Arc::new(tools),
            store,
            compactor,
            notifier,
            system_prompt,
            max_tokens,
            tactical_compaction_threshold: DEFAULT_TACTICAL_COMPACTION_THRESHOLD,
            tactical_full_observations: crate::history::TACTICAL_FULL_OBSERVATIONS,
            observation_collapse_enabled: true,
            compaction_enabled: true,
            max_turn_total_tokens: None,
            context_budget_tokens: None,
            best_of_n: 1,
            tool_completion_timeout: TOOL_COMPLETION_TIMEOUT,
            max_turn_iterations: MAX_TURN_ITERATIONS,
            planner_max_tokens: PLANNER_MAX_TOKENS,
            turn_in_progress: std::sync::atomic::AtomicBool::new(false),
            consecutive_turns_without_tool_calls: std::sync::atomic::AtomicU32::new(0),
            textual_rescue_enabled: true,
            planner: None,
        }
    }

    /// Sets an approximate token budget for the durable+tactical portion
    /// of the prompt, above which a compaction triggers regardless of raw
    /// event count — see the field's doc comment and
    /// [`Engine::load_messages`]. Chainable, e.g.
    /// `Engine::new(...).with_context_budget(6000)`.
    pub fn with_context_budget(mut self, tokens: u32) -> Self {
        self.context_budget_tokens = Some(tokens);
        self
    }

    /// Overrides [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] with a
    /// caller-supplied value (C10, docs/AUDITORIA-2026-07.md) — e.g. from
    /// `braze_config::Config::tactical_compaction_threshold`. Chainable,
    /// same shape as [`Engine::with_context_budget`].
    pub fn with_tactical_compaction_threshold(mut self, threshold: usize) -> Self {
        self.tactical_compaction_threshold = threshold;
        self
    }

    /// Overrides how many of the tactical window's most recent
    /// observations stay full instead of collapsing — see
    /// `tactical_full_observations`'s field doc comment. Chainable, same
    /// shape as [`Engine::with_context_budget`].
    pub fn with_tactical_full_observations(mut self, full_observations: usize) -> Self {
        self.tactical_full_observations = full_observations;
        self
    }

    /// Disables the ACI collapse of old observations entirely — every
    /// tactical observation renders full, no matter how old or large
    /// (opencode ítem 2 / `+ablate:no-prune`, docs/AUDITORIA-2026-07-v6.md):
    /// the collapse is a central lever of the SLM-first thesis, and its
    /// contribution can't be measured without a way to turn it OFF for a
    /// bench row. `true` (the default) keeps the existing behavior.
    /// Chainable, same shape as [`Engine::with_tactical_full_observations`].
    pub fn with_observation_collapse_enabled(mut self, enabled: bool) -> Self {
        self.observation_collapse_enabled = enabled;
        self
    }

    /// Disables tactical compaction entirely — both the event-count and
    /// the token-budget triggers (E1 / `+ablate:no-compaction`,
    /// docs/AUDITORIA-2026-07-v6.md § roadmap). A long turn can then blow
    /// the model's real context window; that's the ablation's point.
    /// `true` (the default) keeps the existing behavior. Chainable.
    pub fn with_compaction_enabled(mut self, enabled: bool) -> Self {
        self.compaction_enabled = enabled;
        self
    }

    /// Cumulative per-turn token budget (v4 P0.2,
    /// docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3): once the turn's
    /// summed `input + output` tokens exceed `budget`, the next iteration
    /// stops the loop gracefully — same tools-free summary attempt the
    /// iteration cap gets — instead of re-sending an ever-growing history
    /// until `max_turn_iterations`. `None` (the default) disables the
    /// breaker. Token-based, not USD-based, on purpose: this crate has no
    /// pricing knowledge (that lives in config/bench), and a caller that
    /// thinks in dollars can convert with its own rates. Chainable.
    pub fn with_max_turn_total_tokens(mut self, budget: Option<u64>) -> Self {
        self.max_turn_total_tokens = budget;
        self
    }

    /// Sets the number of independent candidates each round generates
    /// before voting on which one to use — técnica G10
    /// (docs/AUDITORIA-2026-07.md), e.g. from
    /// `braze_config::Config::best_of_n`. `n <= 1` is a no-op (the round
    /// loop already treats that as "disabled"). Chainable, same shape as
    /// [`Engine::with_context_budget`].
    pub fn with_best_of_n(mut self, n: usize) -> Self {
        self.best_of_n = n;
        self
    }

    /// Overrides [`TOOL_COMPLETION_TIMEOUT`] with a caller-supplied value.
    /// Chainable, same shape as [`Engine::with_context_budget`].
    pub fn with_tool_completion_timeout(mut self, timeout: Duration) -> Self {
        self.tool_completion_timeout = timeout;
        self
    }

    /// Overrides [`MAX_TURN_ITERATIONS`] — the safety cap on
    /// model/tool-call round trips within a single [`Engine::run_turn`]
    /// call. Configurable via `Config::max_turn_iterations`
    /// (`BRAZE_MAX_TURN_ITERATIONS`, v4 P0.2/mitad rondas). Chainable,
    /// same shape as [`Engine::with_context_budget`].
    pub fn with_max_turn_iterations(mut self, cap: usize) -> Self {
        self.max_turn_iterations = cap;
        self
    }

    /// Overrides [`PLANNER_MAX_TOKENS`] — the cap on the planning
    /// round's `max_tokens` (effective `min(self.max_tokens, self.planner_max_tokens)`).
    /// Configurable via `Config::planner_max_tokens`
    /// (`BRAZE_PLANNER_MAX_TOKENS`, v4 P0.2/mitad rondas). Chainable,
    /// same shape as [`Engine::with_context_budget`].
    pub fn with_planner_max_tokens(mut self, cap: u32) -> Self {
        self.planner_max_tokens = cap;
        self
    }

    /// Disables (`enabled: false`) the textual tool-call rescue (B5,
    /// docs/AUDITORIA-2026-07.md) — N-15 (docs/AUDITORIA-2026-07-v2.md):
    /// the rescue is purely syntactic (any response that's entirely a
    /// `{"name":..., "arguments":...}` JSON blob gets dispatched as a
    /// real tool call), so a user literally asking to see the JSON for a
    /// *real* tool name (e.g. "muéstrame el JSON para invocar
    /// write_file") gets that example executed for real. Chainable, same
    /// shape as [`Engine::with_context_budget`].
    pub fn with_textual_rescue_enabled(mut self, enabled: bool) -> Self {
        self.textual_rescue_enabled = enabled;
        self
    }

    /// Enables the planner/executor split (PLAN.md § "Split
    /// planificador/ejecutor"): `planner` — typically a stronger/cloud
    /// model — produces a one-shot plan at the start of every turn, which
    /// the executor (`self.model`) then follows. Purely additive: without
    /// this call the turn loop is byte-identical to before the feature
    /// existed. Chainable, same shape as [`Engine::with_context_budget`].
    pub fn with_planner(mut self, planner: Box<dyn ModelBackend>) -> Self {
        self.planner = Some(planner);
        self
    }

    /// Persists `event` to the session store and mirrors it into the
    /// turn's [`TurnObserver`] — the live seam frontends consume (see
    /// PLAN.md § "Fase TUI — diseño"). Persistence stays the source of
    /// truth: the observer is only notified *after* a successful append,
    /// so a frontend can never display an event the rollout log doesn't
    /// have.
    async fn append_and_notify(
        &self,
        session: &SessionId,
        event: &AgentEvent,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        self.store.append(session, event).await?;
        observer.on_event(event);
        Ok(())
    }

    /// Makes one completion call and consumes its stream fully into a
    /// [`RoundOutcome`] — the single-attempt building block both the
    /// normal path and técnica G10's best-of-n voting
    /// (docs/AUDITORIA-2026-07.md) are built from; extracted verbatim
    /// from what used to be inline in `run_turn`'s round loop, no
    /// behavior change for the `best_of_n <= 1` (default) path.
    ///
    /// `emit_deltas` controls whether text deltas reach `observer` as
    /// they stream in: `false` when this is one of several best-of-n
    /// candidates being generated — there is no single "the" answer to
    /// show live until voting picks one (see
    /// `complete_with_best_of_n`'s doc comment for what happens to the
    /// winner's text instead).
    async fn complete_once(
        &self,
        req: CompletionRequest,
        observer: &mut dyn TurnObserver,
        emit_deltas: bool,
    ) -> Result<RoundOutcome, EngineError> {
        self.complete_once_with(
            self.model.as_ref(),
            req,
            observer,
            emit_deltas,
            self.textual_rescue_enabled,
        )
        .await
    }

    /// [`Engine::complete_once`], parameterized on which backend answers
    /// — the executor (`self.model`) on the normal path, or the optional
    /// planner (`self.planner`) for the planning round (PLAN.md § "Split
    /// planificador/ejecutor"). Everything else (stream consumption,
    /// truncation flag) is identical by construction.
    ///
    /// `rescue_enabled` is threaded separately from `self.textual_rescue_enabled`
    /// — F7 (docs/AUDITORIA-2026-07-v3.md): the planning round always
    /// passes `false` regardless of the executor's setting. A local
    /// planner emitting its own native tool-template leak (e.g. Qwen's
    /// `<tool_call>{...}</tool_call>`) while listing the concrete tools it
    /// would use — exactly what `planning_system_prompt` asks for — would
    /// otherwise have those blocks *removed* from the plan text before
    /// `attempt_planning_round` even looks at `outcome.tool_calls`
    /// (already ignored there), silently deleting the very steps that
    /// named a tool from the persisted `PlanCreated` plan.
    async fn complete_once_with(
        &self,
        model: &dyn ModelBackend,
        req: CompletionRequest,
        observer: &mut dyn TurnObserver,
        emit_deltas: bool,
        rescue_enabled: bool,
    ) -> Result<RoundOutcome, EngineError> {
        let mut stream = model.complete(req).await?;

        let mut text_buffer = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<RoundUsage> = None;
        let mut saw_done = false;
        let mut truncated = false;

        while let Some(event) = stream.next().await {
            match event {
                Ok(CompletionEvent::TextDelta(delta)) => {
                    if emit_deltas {
                        observer.on_text_delta(&delta);
                    }
                    text_buffer.push_str(&delta);
                }
                Ok(CompletionEvent::ToolCallRequested {
                    id,
                    name,
                    arguments,
                }) => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                Ok(CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                    cache_read_tokens,
                    cache_write_tokens,
                    escalation_trigger,
                }) => {
                    tracing::debug!(
                        input_tokens,
                        output_tokens,
                        stop_reason = stop_reason.as_deref(),
                        cache_read_tokens,
                        cache_write_tokens,
                        "model usage this round"
                    );
                    // "max_tokens"/"length" means the round's output
                    // (which may have been a tool call's JSON arguments,
                    // mid-construction) was cut off by the token budget
                    // rather than the model finishing on its own — the
                    // tool call then fails to parse and gets silently
                    // dropped downstream with no other indication of
                    // why. Surfacing it here at least makes that
                    // diagnosable from logs.
                    if matches!(stop_reason.as_deref(), Some("max_tokens") | Some("length")) {
                        tracing::warn!(
                            output_tokens,
                            max_tokens = self.max_tokens,
                            "model output was truncated by max_tokens this round; a tool call's \
                             arguments may have been cut off mid-construction"
                        );
                        truncated = true;
                    }
                    usage = Some(RoundUsage {
                        input_tokens,
                        output_tokens,
                        stop_reason,
                        cache_read_tokens,
                        cache_write_tokens,
                        escalation_trigger,
                    });
                }
                Ok(CompletionEvent::Done) => {
                    saw_done = true;
                    break;
                }
                Err(err) => {
                    // Whatever text/tool-calls arrived before the stream
                    // failed must NOT be treated as a complete, converged
                    // response — nothing from this attempt has been
                    // persisted yet, so propagating here silently
                    // discards the partial attempt instead of persisting
                    // it as if it were real. See `ModelError::StreamError`'s
                    // doc comment / docs/AUDITORIA-2026-07.md hallazgo A3/B4.
                    return Err(err.into());
                }
            }
        }

        // A `ModelBackend` must uphold the invariant that its stream
        // either yields an `Err` or ends with `Ok(Done)` as its last item
        // (see the trait's doc comment) — this only trips if a backend
        // implementation violates that, but the same reasoning as the
        // `Err` arm above applies: an unconverged, possibly-truncated
        // attempt must not be persisted as if it were complete.
        if !saw_done {
            return Err(EngineError::IncompleteStream);
        }

        // Fallback for models that don't emit a structured tool call —
        // small/local models, or a template without native tool-call
        // support — but instead write the call out in plain text.
        // Rescuing it here beats treating it as the model's final
        // answer, which would end the turn having silently ignored what
        // was clearly meant to be a tool call. See
        // docs/AUDITORIA-2026-07.md hallazgo B5. Applied per-attempt
        // (not after best-of-n voting) so a textually described tool
        // call counts as a real candidate signature for the vote too.
        //
        // Escalera, most-specific first: (1) the `<tool_call>…</tool_call>`
        // tagged format Qwen/Hermes models emit natively (explicit
        // markers — admits surrounding prose and several calls per
        // response; the inner grammar may be qwen2.5's JSON or
        // qwen3-coder's `<function=…>` XML), then (2) a bare
        // `<function=…>` XML block without the wrapper, then (3) Llama
        // 3.x's native "pythonic" format (C2, docs/AUDITORIA-2026-07-v3.md
        // — the escalera covered Qwen's two formats but nothing for
        // Llama, one of the most commonly installed local model
        // families), then (4) a bare JSON object that is the entire
        // response (optionally ```json-fenced).
        // H-3 (docs/AUDITORIA-2026-07-v5.md): which rung (if any) actually
        // rescued a tool call this round, threaded into `RoundOutcome` so
        // `run_turn` can persist `AgentEvent::TextualRescueApplied` — the
        // action already existed (the `tracing::info!` calls below), this
        // just gives it a counted, bench-readable trail too.
        let mut rescue_applied: Option<String> = None;
        if rescue_enabled && tool_calls.is_empty() {
            type TextualExtractor = fn(&str) -> (Vec<ToolCall>, String);
            const RESCUE_LADDER: &[(TextualExtractor, &str)] = &[
                (
                    extract_tagged_tool_calls,
                    "<tool_call> tagged (Qwen/Hermes)",
                ),
                (
                    extract_function_xml_tool_calls,
                    "<function=> XML (qwen3-coder)",
                ),
                (extract_pythonic_tool_calls, "pythonic [func(...)] (Llama)"),
            ];

            let mut rescued_from_ladder = false;
            for (extract, format) in RESCUE_LADDER {
                let (calls, remaining_text) = extract(&text_buffer);
                if !calls.is_empty() {
                    tracing::info!(
                        count = calls.len(),
                        format,
                        "rescued tool call(s) emitted as text in a native tool-template format instead of structured tool_calls entries"
                    );
                    tool_calls.extend(calls);
                    text_buffer = remaining_text;
                    rescued_from_ladder = true;
                    rescue_applied = Some((*format).to_string());
                    break;
                }
            }

            if !rescued_from_ladder && let Some(rescued) = try_parse_textual_tool_call(&text_buffer)
            {
                tracing::info!(
                    tool = %rescued.name,
                    "rescued a tool call the model emitted as plain text instead of a structured tool_calls entry"
                );
                tool_calls.push(rescued);
                text_buffer.clear();
                rescue_applied = Some("plain-text fallback".to_string());
            }
        }

        Ok(RoundOutcome {
            text_buffer,
            tool_calls,
            usage,
            rescue_applied,
            truncated,
        })
    }

    /// Técnica G10 (docs/AUDITORIA-2026-07.md): generates `self.best_of_n`
    /// independent completions of the same request and picks the one
    /// whose outcome — canonicalized as the sorted set of (tool name,
    /// canonical arguments) pairs it requested, or an empty set for a
    /// "no tool call, final answer" candidate — is the most common among
    /// all attempts (plurality vote; ties keep the earliest-generated
    /// candidate, never `Iterator::max_by_key`'s "last wins" default).
    /// Cheap test-time scaling with no training required: the evidence
    /// behind this (docs/AUDITORIA-2026-07.md § 6, técnica 10 — Corradini
    /// et al. 2025, BDCC) is that letting a small model try several times
    /// and vote beats one greedy attempt, particularly on the ambiguous
    /// decisions this project's own sweep flagged as weak
    /// (`error_recovery`, `distractor_selection` — both 0/5 for
    /// `qwen2.5:3b`/`7b`, see `CLAUDE.md`).
    ///
    /// Deltas are not streamed live during the `best_of_n` attempts (see
    /// `complete_once`'s `emit_deltas` parameter) — there is no single
    /// "the" answer to show token-by-token until voting resolves one.
    /// The winner's full text is delivered to `observer` as a single
    /// delta right after the vote, so downstream consumers (the plain
    /// CLI, `braze-tui`) still receive it exactly the way they receive
    /// every other round's text, just without the live streaming feel
    /// for this specific round — an accepted, deliberate trade-off.
    ///
    /// The persisted usage for the round is the *sum* across every
    /// candidate (this makes `self.best_of_n` real model calls, and
    /// token/cost accounting must reflect that), with `stop_reason`
    /// taken specifically from the winning candidate.
    async fn complete_with_best_of_n(
        &self,
        req: &CompletionRequest,
        observer: &mut dyn TurnObserver,
    ) -> Result<RoundOutcome, EngineError> {
        // N-13 (docs/AUDITORIA-2026-07-v2.md): a transient error on one
        // candidate (e.g. a rate-limit blip on attempt 3 of 5) must not
        // discard the other candidates already paid for and abort the
        // whole round — that would multiply the effective failure
        // probability by `best_of_n`, backwards from what this technique
        // is for. Vote among whichever candidates actually succeeded;
        // only propagate an error if every single one failed.
        let mut candidates = Vec::with_capacity(self.best_of_n);
        let mut last_error = None;
        for attempt in 0..self.best_of_n {
            match self.complete_once(req.clone(), observer, false).await {
                Ok(outcome) => {
                    tracing::debug!(
                        attempt,
                        n_tool_calls = outcome.tool_calls.len(),
                        "best-of-n candidate generated"
                    );
                    candidates.push(outcome);
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        error = %err,
                        "best-of-n candidate failed; continuing with the remaining attempts"
                    );
                    last_error = Some(err);
                }
            }
        }

        if candidates.is_empty() {
            return Err(last_error.unwrap_or(EngineError::TurnDidNotConverge(self.max_turn_iterations)));
        }

        let signatures: Vec<Vec<(String, String)>> =
            candidates.iter().map(candidate_signature).collect();
        let mut winner_index = 0;
        let mut winner_votes = 0;
        for (i, signature) in signatures.iter().enumerate() {
            let votes = signatures.iter().filter(|s| *s == signature).count();
            if votes > winner_votes {
                winner_votes = votes;
                winner_index = i;
            }
        }

        let total_input_tokens: u32 = candidates
            .iter()
            .filter_map(|c| c.usage.as_ref())
            .map(|u| u.input_tokens)
            .sum();
        let total_output_tokens: u32 = candidates
            .iter()
            .filter_map(|c| c.usage.as_ref())
            .map(|u| u.output_tokens)
            .sum();
        // Cache tokens sum the same way input/output do, for the same
        // reason: this is cost accounting, and every candidate's request
        // really was sent (and really did read/write cache), not just the
        // winner's — summing `None`s as 0 would understate real spend, so
        // stay `None` unless at least one candidate reported a cache
        // token count.
        let total_cache_read_tokens = sum_optional_u32(
            candidates.iter().filter_map(|c| c.usage.as_ref().and_then(|u| u.cache_read_tokens)),
        );
        let total_cache_write_tokens = sum_optional_u32(
            candidates
                .iter()
                .filter_map(|c| c.usage.as_ref().and_then(|u| u.cache_write_tokens)),
        );
        let any_usage_reported = candidates.iter().any(|c| c.usage.is_some());
        let winner_stop_reason = candidates[winner_index]
            .usage
            .as_ref()
            .and_then(|u| u.stop_reason.clone());
        // H-3 (docs/AUDITORIA-2026-07-v5.md): every candidate this round
        // shares the same routing decision (D4's same-round dedup in
        // `EscalatingBackend::route` keys on `req.messages.len()`, which
        // every best-of-n attempt sends unchanged) — taking it from the
        // winner specifically is just the least surprising of several
        // equivalent choices, not a real distinction.
        let winner_escalation_trigger = candidates[winner_index]
            .usage
            .as_ref()
            .and_then(|u| u.escalation_trigger.clone());

        tracing::debug!(
            winner_index,
            winner_votes,
            n_candidates = candidates.len(),
            "best-of-n vote resolved"
        );

        let mut winner = candidates.swap_remove(winner_index);
        winner.usage = any_usage_reported.then_some(RoundUsage {
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            stop_reason: winner_stop_reason,
            cache_read_tokens: total_cache_read_tokens,
            cache_write_tokens: total_cache_write_tokens,
            escalation_trigger: winner_escalation_trigger,
        });

        if !winner.text_buffer.is_empty() {
            observer.on_text_delta(&winner.text_buffer);
        }

        Ok(winner)
    }

    /// Runs one complete turn: append the user's message, then loop
    /// model-completion <-> tool-dispatch rounds until the model responds
    /// with text and no further tool calls (or the safety cap is hit).
    /// `observer` receives each text fragment as it streams in
    /// ([`TurnObserver::on_text_delta`]) plus a live mirror of every
    /// [`AgentEvent`] persisted during the turn
    /// ([`TurnObserver::on_event`]) — the seam a frontend (plain CLI
    /// today, `braze-tui` next) renders from. Headless callers pass
    /// [`braze_events::NoopObserver`].
    ///
    /// A9 (docs/AUDITORIA-2026-07.md): `#[instrument]` wraps the whole
    /// call in an `info_span!`-equivalent "turn" span carrying `session`,
    /// so every log statement this turn produces — including ones nested
    /// several calls deep (`load_messages`, `dispatch_tool_calls`) — is
    /// automatically tagged with it. Diagnosing the "which turn produced
    /// this pathological sequence of tool calls" failure mode with
    /// `RUST_LOG=debug` was previously impossible; this is what makes it
    /// possible without threading `session` through every log call by
    /// hand.
    #[tracing::instrument(name = "turn", skip(self, user_input, observer), fields(session = %session))]
    pub async fn run_turn(
        &self,
        session: &SessionId,
        user_input: &str,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        // N-17 (docs/AUDITORIA-2026-07-v2.md): held for the rest of this
        // call via `TurnGuard`'s `Drop`, covering every exit path
        // (success, any `?`-propagated error, or an unwind) without
        // touching each one individually.
        let _turn_guard = TurnGuard::acquire(&self.turn_in_progress)?;

        // N-4 (docs/AUDITORIA-2026-07-v2.md): repair any tool_use orphaned
        // by a crash/kill/power-loss in a *previous* run *before* this
        // turn's `UserMessage` is appended — `load_messages` also repairs
        // (so a direct caller of it still gets the invariant), but by
        // then the new `UserMessage` would already sit between the
        // orphaned tool_use and its synthesized result, producing a
        // sequence Anthropic rejects with a permanent 400 (the repair
        // itself would be the thing making the session unresumable).
        let existing_events = self.load_and_repair(session, observer).await?;

        self.append_and_notify(
            session,
            &AgentEvent::UserMessage {
                text: user_input.to_string(),
            },
            observer,
        )
        .await?;

        // D5: the streak reflects *previous* turns only (this turn hasn't
        // run yet) — reusing the plain `UserMessage` event kind rather
        // than adding a new `AgentEvent` variant just for this.
        if self
            .consecutive_turns_without_tool_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            >= NARRATION_WITHOUT_ACTION_THRESHOLD
        {
            self.append_and_notify(
                session,
                &AgentEvent::UserMessage {
                    text: "[Reminder] Your last few responses described an intended action \
                           without actually calling the tool for it. If you're being asked \
                           to do something, call the appropriate tool now instead of \
                           describing or restating the plan."
                        .to_string(),
                },
                observer,
            )
            .await?;
        }

        let mut messages = self.load_messages(session, observer).await?;

        // PLAN.md § "Split planificador/ejecutor": optional one-shot
        // planning round before the executor loop. Doesn't count against
        // `MAX_TURN_ITERATIONS`, and can only *add* a persisted
        // `PlanCreated` (in which case messages are reloaded so the plan
        // reaches the executor's first request) — every planner failure
        // mode degrades to an unplanned turn instead of failing it.
        if self
            .attempt_planning_round(session, &messages, observer)
            .await?
        {
            messages = self.load_messages(session, observer).await?;
        }

        // Per-turn state threaded through `dispatch_tool_calls` across
        // every round of this call — schema-repair retry counts, the
        // repeated-call detector (A5), and the id-uniqueness guard (N-14,
        // docs/AUDITORIA-2026-07-v2.md, seeded from this session's
        // history so a collision with a *previous* turn — e.g. a
        // backend's synthetic-id fallback restarting its counter after
        // `--resume` — gets caught too, not just collisions within this
        // turn). Lives and dies with this `run_turn` call — none of it is
        // a field on `Engine` or persists across turns.
        let mut dispatch_state = TurnDispatchState {
            schema_retry_counts: HashMap::new(),
            seen_calls: HashSet::new(),
            known_tool_call_ids: Self::existing_tool_call_ids(&existing_events),
        };

        // D5: whether *any* round of this specific turn has dispatched a
        // tool call yet — decides, at every exit point below, whether
        // this turn counts toward `consecutive_turns_without_tool_calls`
        // or breaks the streak. Local to this call, unlike the `Engine`
        // field it eventually updates: a turn that calls a tool in an
        // early round and then converges with a plain-text answer in a
        // later round must NOT count as "narration only" just because its
        // *last* round happened to have no tool calls.
        let mut any_tool_calls_this_turn = false;
        // v4 P0.2: cumulative input+output across this turn's rounds —
        // the quantity `max_turn_total_tokens` breaks on. Local to the
        // call, like `any_tool_calls_this_turn`.
        let mut turn_total_tokens: u64 = 0;

        for round in 0..self.max_turn_iterations {
            // v4 P0.2 (docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3):
            // checked at the top of the NEXT iteration, not right after a
            // round — a round that converges to a final answer within
            // budget+ε must return normally, and one more model call
            // against an over-budget history is exactly what this breaker
            // exists to prevent. Same graceful degradation the iteration
            // cap gets below: summarize what was found instead of failing
            // outright.
            if let Some(budget) = self.max_turn_total_tokens
                && turn_total_tokens > budget
            {
                tracing::warn!(
                    round,
                    budget_tokens = budget,
                    spent_tokens = turn_total_tokens,
                    "turn blew its cumulative token budget; attempting a final tools-free summary \
                     round instead of continuing to re-send a growing history"
                );
                self.consecutive_turns_without_tool_calls
                    .store(0, std::sync::atomic::Ordering::SeqCst);
                if self
                    .attempt_tools_free_summary_round(session, &messages, observer)
                    .await?
                {
                    return Ok(());
                }
                return Err(EngineError::TurnBudgetExhausted {
                    budget_tokens: budget,
                    spent_tokens: turn_total_tokens,
                });
            }

            // N-16 (docs/AUDITORIA-2026-07-v2.md): the lossy variant
            // degrades a provider that fails to list its stubs (e.g. an
            // MCP server that died mid-session) instead of aborting every
            // subsequent turn, including ones that only need local tools.
            let tool_stubs = self.tools.all_stubs_lossy().await;
            let req = CompletionRequest {
                messages: messages.clone(),
                tool_stubs: tool_stubs.clone(),
                system_prompt: self.system_prompt.clone(),
                max_tokens: self.max_tokens,
            };

            // técnica G10 (docs/AUDITORIA-2026-07.md): `best_of_n <= 1`
            // takes the exact single-call path that existed before G10 —
            // `complete_once` is a straight extraction of what used to be
            // inline here, not new behavior.
            let RoundOutcome {
                text_buffer,
                tool_calls,
                usage,
                truncated,
                rescue_applied,
            } = if self.best_of_n > 1 {
                self.complete_with_best_of_n(&req, observer).await?
            } else {
                self.complete_once(req, observer, true).await?
            };

            // Persisted once per round (if the backend reported it) so
            // tooling like `braze-bench` can read per-round token usage
            // back out of the rollout log — see `AgentEvent::Usage`'s doc
            // comment. Order relative to the round's other events doesn't
            // matter: it's audit-only and never rendered into a `Message`
            // (see `history::event_to_message`). Under G10, this already
            // reflects the *summed* cost of every candidate this round
            // generated, not just the winner's — see
            // `complete_with_best_of_n`.
            if let Some(round_usage) = usage {
                // v4 P0.2: feed the turn's cumulative-token breaker (the
                // check at the top of the next iteration).
                turn_total_tokens +=
                    u64::from(round_usage.input_tokens) + u64::from(round_usage.output_tokens);
                // H-3 (docs/AUDITORIA-2026-07-v5.md): `AgentEvent::Usage`
                // itself gains no new field — the escalation fact gets its
                // own persisted event below instead, captured here before
                // `round_usage`'s other fields move into the `Usage`
                // literal.
                let escalation_trigger = round_usage.escalation_trigger;
                self.append_and_notify(
                    session,
                    &AgentEvent::Usage {
                        input_tokens: round_usage.input_tokens,
                        output_tokens: round_usage.output_tokens,
                        stop_reason: round_usage.stop_reason,
                        cache_read_tokens: round_usage.cache_read_tokens,
                        cache_write_tokens: round_usage.cache_write_tokens,
                    },
                    observer,
                )
                .await?;
                if let Some(trigger) = escalation_trigger {
                    self.append_and_notify(
                        session,
                        &AgentEvent::EscalationToLead { trigger },
                        observer,
                    )
                    .await?;
                }
            }

            // H-3 (docs/AUDITORIA-2026-07-v5.md): the action already
            // happened (inside `complete_once_with`'s rescue ladder,
            // logged via `tracing::info!`) — this just gives it a
            // persisted, bench-countable trail alongside the log line.
            if let Some(parser) = rescue_applied {
                self.append_and_notify(
                    session,
                    &AgentEvent::TextualRescueApplied { parser },
                    observer,
                )
                .await?;
            }

            // A9 (docs/AUDITORIA-2026-07.md): the round-level fact
            // `RUST_LOG=debug` previously had no way to see — which round
            // of this turn this was, and how many tool calls it produced
            // — nested under the "turn" span's `session` field.
            tracing::debug!(round, n_tool_calls = tool_calls.len(), "round completed");

            if tool_calls.is_empty() {
                // N-24 (docs/AUDITORIA-2026-07-v2.md): a truncated round
                // with no tool calls used to be persisted as a normal,
                // converged final answer — indistinguishable downstream
                // from a response the model actually finished on its own.
                // Surface it as an error instead of silently keeping (and
                // showing the user) a possibly mid-sentence answer.
                if truncated {
                    return Err(EngineError::TruncatedFinalResponse);
                }
                // Bajo (docs/AUDITORIA-2026-07-v2.md, "una completion
                // vacía termina el turno como éxito silencioso"): no text
                // and no tool calls is not a legitimate final answer —
                // under best-of-n several empty candidates can share the
                // same (empty) signature and win the vote outright.
                // Treat it as a failure to converge for this round rather
                // than a silent no-op success.
                if text_buffer.is_empty() {
                    // U-1 (docs/usability-log-template.md, hallado en vivo
                    // 2026-07-07 contra qwen3.5-coder/Nitro): a real session
                    // asked for a hardware report; the model called
                    // `shell_exec`×3 and `write_file` (all persisted, the
                    // file landed on disk), then its *next* round — asked
                    // to wrap up with no further tool calls pending — came
                    // back with neither text nor a tool call. Failing the
                    // whole turn here reported an error even though the
                    // actual task had already succeeded. If this turn
                    // already made real progress (dispatched at least one
                    // tool call), give it the same one-more-shot fallback
                    // `MAX_TURN_ITERATIONS` exhaustion gets below, instead
                    // of discarding that progress behind a hard failure. A
                    // turn whose *very first* round comes back empty (no
                    // progress at all) still fails immediately — nothing to
                    // summarize, and the best-of-n false-convergence risk
                    // this error exists for still applies in full.
                    if any_tool_calls_this_turn {
                        tracing::warn!(
                            round,
                            "round produced neither text nor a tool call after this turn already \
                             dispatched at least one; attempting a tools-free summary round \
                             instead of discarding that progress"
                        );
                        if self
                            .attempt_tools_free_summary_round(session, &messages, observer)
                            .await?
                        {
                            self.consecutive_turns_without_tool_calls
                                .store(0, std::sync::atomic::Ordering::SeqCst);
                            return Ok(());
                        }
                    }
                    return Err(EngineError::EmptyModelResponse);
                }
                // Final response: no further tool calls requested.
                self.append_and_notify(
                    session,
                    &AgentEvent::AssistantText { text: text_buffer },
                    observer,
                )
                .await?;
                // D5: only a turn that *never* dispatched a tool call
                // (not just "this round didn't") counts toward the streak.
                if any_tool_calls_this_turn {
                    self.consecutive_turns_without_tool_calls
                        .store(0, std::sync::atomic::Ordering::SeqCst);
                } else {
                    self.consecutive_turns_without_tool_calls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                return Ok(());
            }

            // Text preceding this round's tool calls (if any) is persisted
            // first, preserving the order the model actually produced it
            // in, before the tool_use blocks that followed it.
            if !text_buffer.is_empty() {
                self.append_and_notify(
                    session,
                    &AgentEvent::AssistantText { text: text_buffer },
                    observer,
                )
                .await?;
            }

            self.dispatch_tool_calls(
                session,
                &tool_calls,
                &tool_stubs,
                &mut dispatch_state,
                observer,
            )
            .await?;
            any_tool_calls_this_turn = true;

            messages = self.load_messages(session, observer).await?;
        }

        // Fell through with `MAX_TURN_ITERATIONS` rounds exhausted — every
        // one of them had a non-empty `tool_calls` (any empty round would
        // have returned above), so this turn definitely isn't "narration
        // only"; D5's streak resets here too.
        self.consecutive_turns_without_tool_calls
            .store(0, std::sync::atomic::Ordering::SeqCst);

        tracing::warn!(
            max_iterations = self.max_turn_iterations,
            "turn did not converge; attempting a final tools-free summary round instead of failing outright"
        );
        if self
            .attempt_tools_free_summary_round(session, &messages, observer)
            .await?
        {
            return Ok(());
        }
        Err(EngineError::TurnDidNotConverge(self.max_turn_iterations))
    }

    /// The optional planning round (PLAN.md § "Split
    /// planificador/ejecutor"): asks `self.planner` — if configured — for
    /// a short plan of the turn, persists it as
    /// [`AgentEvent::PlanCreated`], and returns whether a plan was
    /// actually persisted (so `run_turn` knows to reload messages).
    ///
    /// Tools are *inlined as text* in the planning system prompt
    /// (name + summary from the stubs), with `tool_stubs` left empty on
    /// the request: the planner needs tool *awareness*, never invocation
    /// — the same only-names-in-context philosophy as deferred loading.
    ///
    /// Degradation, never failure (same philosophy as N-13's best-of-n
    /// fix — an optional enhancement must not kill the turn):
    /// 1. planner call errors ⇒ warn, proceed without a plan;
    /// 2. planner attempted tool calls ⇒ ignored with a warn, its text
    ///    is still used;
    /// 3. response truncated by the token budget ⇒ plan discarded (a
    ///    cut-off plan can mislead mid-step — espíritu N-24);
    /// 4. empty/whitespace text ⇒ plan discarded.
    ///
    /// The planner's `Usage` is persisted even when the plan is
    /// discarded — the cost was real either way, and hiding it would
    /// skew exactly the A/B accounting this feature exists to enable.
    ///
    /// Only `Err` for session-store failures (persisting `Usage`/
    /// `PlanCreated`) — those are real turn failures, not planner ones.
    async fn attempt_planning_round(
        &self,
        session: &SessionId,
        messages: &[Message],
        observer: &mut dyn TurnObserver,
    ) -> Result<bool, EngineError> {
        let Some(planner) = &self.planner else {
            return Ok(false);
        };

        let tool_stubs = self.tools.all_stubs_lossy().await;
        let req = CompletionRequest {
            messages: messages.to_vec(),
            tool_stubs: Vec::new(),
            system_prompt: planning_system_prompt(&self.system_prompt, &tool_stubs),
            max_tokens: self.max_tokens.min(self.planner_max_tokens),
        };

        // `emit_deltas: false`: the plan reaches frontends once, as the
        // `PlanCreated` event mirror — streaming its text live too would
        // render it twice in the TUI (markdown preview + PlanCell).
        // `rescue_enabled: false` (F7, docs/AUDITORIA-2026-07-v3.md): a
        // plan step naming a tool in the planner's own native
        // tool-template syntax must survive as plan *text*, not be
        // extracted and discarded by the textual rescue before this
        // function even sees it.
        let outcome = match self
            .complete_once_with(planner.as_ref(), req, observer, false, false)
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "planner call failed; proceeding without a plan"
                );
                return Ok(false);
            }
        };

        if let Some(round_usage) = outcome.usage {
            self.append_and_notify(
                session,
                &AgentEvent::Usage {
                    input_tokens: round_usage.input_tokens,
                    output_tokens: round_usage.output_tokens,
                    stop_reason: round_usage.stop_reason,
                    cache_read_tokens: round_usage.cache_read_tokens,
                    cache_write_tokens: round_usage.cache_write_tokens,
                },
                observer,
            )
            .await?;
        }

        if !outcome.tool_calls.is_empty() {
            tracing::warn!(
                n_tool_calls = outcome.tool_calls.len(),
                "planner attempted tool calls despite the planning prompt; ignoring them"
            );
        }
        if outcome.truncated {
            tracing::warn!(
                "planner response was truncated by the token budget; discarding the partial plan"
            );
            return Ok(false);
        }
        let plan = outcome.text_buffer.trim();
        if plan.is_empty() {
            tracing::warn!("planner returned no usable text; proceeding without a plan");
            return Ok(false);
        }

        self.append_and_notify(
            session,
            &AgentEvent::PlanCreated {
                plan: plan.to_string(),
            },
            observer,
        )
        .await?;
        Ok(true)
    }

    /// Makes one last tools-free request asking the model to summarize
    /// whatever it learned and answer with that — persisted as a normal
    /// `AssistantText` (`Ok(true)`) on success. Two call sites, both
    /// "the turn didn't converge normally but there may already be real
    /// progress worth summarizing instead of just failing outright":
    /// [`Engine::run_turn`] exhausting [`MAX_TURN_ITERATIONS`], and (U-1,
    /// found live 2026-07-07 against qwen3.5-coder/Nitro) a round mid-turn
    /// coming back with neither text nor a tool call *after* this turn
    /// already dispatched at least one successful tool call — each caller
    /// logs its own context and picks its own `EngineError` fallback on
    /// `Ok(false)`, since "exhausted the iteration cap" and "went empty
    /// right after real work" are different failures worth telling apart
    /// in logs.
    ///
    /// `Ok(false)` when this attempt itself fails or produces nothing
    /// usable — a legitimate hard failure (e.g. the backend is
    /// unreachable) is surfaced by the caller as an error rather than
    /// silently swallowed here.
    async fn attempt_tools_free_summary_round(
        &self,
        session: &SessionId,
        messages: &[Message],
        observer: &mut dyn TurnObserver,
    ) -> Result<bool, EngineError> {
        // H-3 (docs/AUDITORIA-2026-07-v5.md): records that this fallback was
        // *reached for*, regardless of whether it goes on to succeed —
        // success is separately visible as the `AssistantText` this
        // function may or may not append below.
        self.append_and_notify(session, &AgentEvent::SummaryFallbackAttempted, observer)
            .await?;

        let req = CompletionRequest {
            messages: messages.to_vec(),
            tool_stubs: Vec::new(),
            system_prompt: format!(
                "{}\n\nDo not call any tool — none are available in this request. Summarize \
                 what you found so far and answer the user with the best answer you can give \
                 from the information already gathered.",
                self.system_prompt
            ),
            max_tokens: self.max_tokens,
        };

        // Bajo (docs/AUDITORIA-2026-07-v2.md, "attempt_final_summary_round
        // traga el error real"): both error paths below used to discard
        // the actual cause (a real backend failure — auth, network,
        // rate limit — looked identical to "the model just didn't
        // produce anything usable"). Logging it here makes the real cause
        // visible instead of silently lost, regardless of which
        // `EngineError` the caller ultimately raises on `Ok(false)`.
        let mut stream = match self.model.complete(req).await {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "tools-free summary round's model call itself failed"
                );
                return Ok(false);
            }
        };

        // Deltas are *not* streamed live here (unlike the main round in
        // `complete_once_with`) — same trade-off `complete_with_best_of_n`
        // already makes and documents: there is no single "the" answer to
        // show token-by-token until the full response is in hand and
        // known-clean. Here that's because a leaked tool-call block
        // (hallazgo U-16 below) can only be detected and stripped once the
        // whole buffer has arrived; streaming it live first would show the
        // user the raw garbage regardless of what happens to the
        // *persisted* text afterward.
        let mut text_buffer = String::new();
        let mut saw_done = false;
        let mut usage: Option<RoundUsage> = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(CompletionEvent::TextDelta(delta)) => {
                    text_buffer.push_str(&delta);
                }
                Ok(CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                    cache_read_tokens,
                    cache_write_tokens,
                    escalation_trigger,
                }) => {
                    // H-4 (docs/AUDITORIA-2026-07-v5.md): this used to be
                    // skipped as "not worth the same bookkeeping as a
                    // normal round" — but the fallback re-sends the ENTIRE
                    // conversation history as its prompt, making it one of
                    // the most expensive single calls a turn can make, and
                    // dropping its Usage made that cost invisible to the
                    // bench (`TaskResult::input_tokens` under-reported
                    // exactly on the degraded turns a cost analysis most
                    // needs to see). H-3 counts that the fallback was
                    // *attempted*; this records what it *cost*.
                    usage = Some(RoundUsage {
                        input_tokens,
                        output_tokens,
                        stop_reason,
                        cache_read_tokens,
                        cache_write_tokens,
                        escalation_trigger,
                    });
                }
                Ok(CompletionEvent::Done) => {
                    saw_done = true;
                    break;
                }
                // No tools were offered in this request, so a tool call
                // here would itself be a violation of the request — ignore
                // rather than act on it.
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "tools-free summary round's stream failed mid-response"
                    );
                    break;
                }
            }
        }

        // Persisted regardless of whether the fallback produced usable
        // text below — the model call happened and was paid for either
        // way, same reasoning as `SummaryFallbackAttempted` counting
        // attempts rather than successes. One `Usage` per model round is
        // the invariant `TaskResult::rounds` counts on; this round is a
        // real model round.
        if let Some(round_usage) = usage {
            let escalation_trigger = round_usage.escalation_trigger;
            self.append_and_notify(
                session,
                &AgentEvent::Usage {
                    input_tokens: round_usage.input_tokens,
                    output_tokens: round_usage.output_tokens,
                    stop_reason: round_usage.stop_reason,
                    cache_read_tokens: round_usage.cache_read_tokens,
                    cache_write_tokens: round_usage.cache_write_tokens,
                },
                observer,
            )
            .await?;
            // Same pairing `run_turn` maintains: if THIS call happened to
            // be the one that triggered a reactive escalation (the
            // fallback goes through the same `ModelBackend`, decorator
            // included), the episode is persisted here too instead of
            // silently dropped.
            if let Some(trigger) = escalation_trigger {
                self.append_and_notify(
                    session,
                    &AgentEvent::EscalationToLead { trigger },
                    observer,
                )
                .await?;
            }
        }

        if saw_done && !text_buffer.is_empty() {
            // U-16 (docs/usability-log-2026-07-07-si2.md): this request
            // explicitly carries no tool stubs and tells the model not to
            // call any tool, but a model habituated to its own native
            // tool-call template (`z-ai/glm-5.2`, observed live) can still
            // emit one as plain text anyway. Unlike the main round, this
            // one has no rescue ladder to dispatch it to — there is
            // nothing to dispatch it *to* — so the only choice is between
            // showing the leaked block to the user as if it were the real
            // answer, or stripping it. Showing it is strictly worse: it
            // turns a recoverable "the model tried to call a tool it
            // couldn't" into "the turn succeeded" with garbage as the
            // punchline.
            let cleaned = strip_leaked_tool_call_shapes(&text_buffer);
            if cleaned.trim().is_empty() {
                return Ok(false);
            }
            observer.on_text_delta(&cleaned);
            self.append_and_notify(
                session,
                &AgentEvent::AssistantText { text: cleaned },
                observer,
            )
            .await?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Records each requested tool call, spawns it as a background task via
    /// [`TaskNotifier`], and blocks until every task from this round has
    /// reported completion (persisting a `ToolCallCompleted` event for
    /// each), or times out — in which case every still-pending task is
    /// actually cancelled via [`TaskNotifier::abort`] (N-33,
    /// docs/AUDITORIA-2026-07-v2.md), not merely forgotten.
    async fn dispatch_tool_calls(
        &self,
        session: &SessionId,
        tool_calls: &[ToolCall],
        available_tools: &[ToolStub],
        state: &mut TurnDispatchState,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        let TurnDispatchState {
            schema_retry_counts: retry_counts,
            seen_calls,
            known_tool_call_ids,
        } = state;

        let mut handle_to_id: HashMap<TaskHandle, String> = HashMap::new();
        let mut pending: HashSet<TaskHandle> = HashSet::new();
        // F6: resolves a completed call's id back to its tool name, so a
        // successful completion can be checked against
        // `MUTATING_TOOL_NAMES` without threading the name through the
        // background task machinery.
        let mut id_to_name: HashMap<String, String> = HashMap::new();

        for call in tool_calls {
            // N-14 (docs/AUDITORIA-2026-07-v2.md): shadow `call` with an
            // owned copy whose id is guaranteed unique against every id
            // this session has ever used (history + this turn so far)
            // *before* anything below persists or dispatches it — every
            // remaining `call.id`/`call.clone()` use in this loop body
            // is unchanged and now operates on the deduped id. Without
            // this, a duplicate id (a backend's synthetic-id counter
            // restarting after `--resume`, or two calls the model itself
            // gave the same id) enters the append-only log as two
            // `tool_use`/`tool_result` pairs sharing one id — Anthropic
            // rejects that with a permanent 400 on every future request.
            let mut call = ToolCall {
                id: ensure_unique_tool_call_id(call.id.clone(), known_tool_call_ids),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            };

            self.append_and_notify(
                session,
                &AgentEvent::AssistantToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
                observer,
            )
            .await?;

            // Exact repeat of a (name, arguments) pair already dispatched
            // earlier in this same turn — the dominant non-convergence
            // pattern for small/local models (they re-issue an identical
            // call instead of using the result they already got, or
            // giving up and answering in text). `arguments.to_string()` is
            // a canonical key: `serde_json::Value` objects serialize their
            // keys in sorted order by default (no `preserve_order`
            // feature), so structurally-identical arguments compare equal
            // regardless of the field order the model happened to emit
            // this time. Nudge instead of re-running the tool.
            let call_key = (call.name.clone(), call.arguments.to_string());
            if !seen_calls.insert(call_key) {
                tracing::warn!(
                    tool = %call.name,
                    "model repeated an identical tool call this turn; nudging instead of re-dispatching"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content: format!(
                                "You already called '{}' with these exact arguments \
                                 earlier in this turn — the result has not changed. Do \
                                 not repeat this call; either use the result you already \
                                 have, or respond to the user with text instead of \
                                 calling a tool.",
                                call.name
                            ),
                            is_error: true,
                        },
                    },
                    observer,
                )
                .await?;
                continue;
            }

            // Real schema validation before dispatch (closes the gap noted
            // in Fase 3/5, see docs/SOTA-2026-07.md § 1): resolve the
            // tool's real schema and validate the model-produced arguments
            // against it. `ToolRegistry::resolve` returns
            // `Result<ToolSchema, ToolError>`, not
            // `Result<Option<ToolSchema>, ToolError>` — `ToolError::NotFound`
            // is exactly the "no provider advertises this tool" case (every
            // `ToolProvider::resolve_schema` implementation returns
            // `Ok(None)` for an unrecognized name; the registry only turns
            // that into `NotFound` once *no* provider claims the tool), so
            // it's handled like the "no schema to validate against" case
            // rather than a hard resolution failure.
            match self.tools.resolve(&call.name).await {
                Ok(schema) => {
                    // F2 (docs/AUDITORIA-2026-07-v3.md): the qwen3-coder
                    // XML rescue (`parse_function_xml_tool_call`) has no
                    // native number/boolean grammar, so every scalar
                    // param comes back as a string, and a code-carrying
                    // string param can come back mis-parsed as a JSON
                    // object — both fail schema validation deterministically
                    // even though the model's *intent* was correct. A
                    // no-op for wire-sourced calls, whose backend already
                    // sends correctly-typed JSON.
                    coerce_arguments_to_schema(&mut call.arguments, &schema.input_schema);

                    if let Err(validation_err) =
                        jsonschema::validate(&schema.input_schema, &call.arguments)
                    {
                        // Retry counter is keyed by tool *name*, not by
                        // individual call id: if the model calls the same
                        // tool multiple times in one turn with different
                        // arguments, this can't distinguish "first real
                        // failure for this call" from "a different call to
                        // the same tool that also happens to fail". This is
                        // a deliberately simple, turn-bounded heuristic —
                        // not a precise per-call correlation mechanism.
                        let attempt_counter = retry_counts.entry(call.name.clone()).or_insert(0);
                        *attempt_counter += 1;
                        let attempt = *attempt_counter;

                        let repair_message = if attempt == 1 {
                            format!(
                                "Tool call '{}' failed schema validation: {validation_err}. \
                                 Expected input schema:\n{}\n\
                                 Retry this tool call with corrected arguments.",
                                call.name, schema.input_schema
                            )
                        } else {
                            format!(
                                "Tool call '{}' failed schema validation again: {validation_err}. \
                                 No further automatic repair hints will be given for this tool \
                                 this turn.",
                                call.name
                            )
                        };

                        tracing::warn!(
                            tool = %call.name,
                            attempt,
                            error = %validation_err,
                            "tool call arguments failed schema validation before dispatch"
                        );

                        self.append_and_notify(
                            session,
                            &AgentEvent::ToolCallCompleted {
                                id: call.id.clone(),
                                result: ToolResult {
                                    tool_call_id: call.id.clone(),
                                    content: repair_message,
                                    is_error: true,
                                },
                            },
                            observer,
                        )
                        .await?;

                        continue;
                    }
                }
                Err(braze_tools_core::ToolError::NotFound(_)) => {
                    // A hallucinated tool name — frequent with small
                    // models. Dispatching anyway would just fail identically
                    // and, worse, the error the model saw ("tool not
                    // found: X") gave it no way to self-correct. List the
                    // names that actually exist (already at hand from this
                    // round's stubs) so the model can retry with a valid
                    // one instead of repeating the same hallucination.
                    let available = available_tools
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    tracing::warn!(
                        tool = %call.name,
                        "no provider advertises this tool; not dispatching"
                    );
                    self.append_and_notify(
                        session,
                        &AgentEvent::ToolCallCompleted {
                            id: call.id.clone(),
                            result: ToolResult {
                                tool_call_id: call.id.clone(),
                                content: format!(
                                    "Unknown tool '{}'. Available tools are: {available}. \
                                     Retry using one of these exact names.",
                                    call.name
                                ),
                                is_error: true,
                            },
                        },
                        observer,
                    )
                    .await?;
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        tool = %call.name,
                        error = %err,
                        "failed to resolve tool schema before dispatch (MVP does not validate strictly)"
                    );
                }
            }

            // A9 (docs/AUDITORIA-2026-07.md): per-tool-call visibility for
            // the ordinary/successful dispatch path — previously only
            // failures (schema rejection, unknown tool, timeout) logged
            // anything at all, so a `RUST_LOG=debug` trace of a healthy
            // turn showed no tool-call activity whatsoever.
            tracing::debug!(tool = %call.name, id = %call.id, "dispatching tool call");

            self.append_and_notify(
                session,
                &AgentEvent::ToolCallStarted {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    background: true,
                },
                observer,
            )
            .await?;

            id_to_name.insert(call.id.clone(), call.name.clone());

            let tools = Arc::clone(&self.tools);
            let call_owned = call.clone();
            let label = call.name.clone();
            let task = BackgroundTask {
                label,
                work: Box::pin(async move {
                    match tools.dispatch(&call_owned).await {
                        Ok(result) => result,
                        Err(err) => ToolResult {
                            tool_call_id: call_owned.id.clone(),
                            content: err.to_string(),
                            is_error: true,
                        },
                    }
                }),
            };

            let handle = self.notifier.spawn(task);
            handle_to_id.insert(handle, call.id.clone());
            pending.insert(handle);
        }

        // If `next_completed` times out, every still-pending task is
        // aborted (N-33, docs/AUDITORIA-2026-07-v2.md) and treated as
        // failed so the turn proceeds rather than hanging forever.
        while !pending.is_empty() {
            match self
                .notifier
                .next_completed(self.tool_completion_timeout)
                .await
            {
                Some((handle, result)) => {
                    if !pending.remove(&handle) {
                        // Stale completion from a task that already timed
                        // out in a previous round: the MVP limitation
                        // above means a timed-out task is never cancelled,
                        // it keeps running unobserved and can eventually
                        // deliver its real result here, long after that
                        // earlier round already persisted a synthetic
                        // timeout `ToolCallCompleted` for the same
                        // `tool_call_id`. Persisting this one too would
                        // append a *second* `tool_result` for the same
                        // `tool_use_id` — Anthropic rejects that with a
                        // permanent 400 on every subsequent turn, since the
                        // rollout log is append-only. Discard it instead.
                        tracing::warn!(
                            ?handle,
                            tool_call_id = %result.tool_call_id,
                            "discarding a stale tool completion from a previous round"
                        );
                        continue;
                    }
                    let id = handle_to_id
                        .remove(&handle)
                        .unwrap_or_else(|| result.tool_call_id.clone());
                    tracing::debug!(
                        tool_call_id = %id,
                        is_error = result.is_error,
                        "tool call completed"
                    );
                    // F6 (docs/AUDITORIA-2026-07-v3.md): a successful
                    // mutating tool call invalidates the whole
                    // repeated-call streak — any prior `read_file`
                    // (or other) call recorded in `seen_calls` might now
                    // return something different, so a later identical
                    // repeat must be allowed to actually re-run instead
                    // of being nudged with a claim ("the result has not
                    // changed") that's no longer true.
                    if !result.is_error
                        && id_to_name
                            .get(&id)
                            .is_some_and(|name| MUTATING_TOOL_NAMES.contains(&name.as_str()))
                    {
                        seen_calls.clear();
                    }
                    self.append_and_notify(
                        session,
                        &AgentEvent::ToolCallCompleted { id, result },
                        observer,
                    )
                    .await?;
                }
                None => {
                    tracing::error!(
                        pending = pending.len(),
                        timeout_secs = self.tool_completion_timeout.as_secs(),
                        "timed out waiting for background tool task(s); aborting and treating remaining as failed"
                    );
                    for handle in pending.drain() {
                        self.notifier.abort(handle);
                        let id = handle_to_id.remove(&handle).unwrap_or_default();
                        self.append_and_notify(
                            session,
                            &AgentEvent::ToolCallCompleted {
                                id: id.clone(),
                                result: ToolResult {
                                    tool_call_id: id,
                                    content: "tool call timed out waiting for completion"
                                        .to_string(),
                                    is_error: true,
                                },
                            },
                            observer,
                        )
                        .await?;
                    }
                }
            }
        }

        Ok(())
    }

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
    async fn load_and_repair(
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
    fn existing_tool_call_ids(events: &[AgentEvent]) -> HashSet<String> {
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
    async fn load_messages(
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

            let summary = self.compactor.compact_tactical(&tactical)?;
            // Bajo (docs/AUDITORIA-2026-07-v2.md, "dropped_tokens_estimate
            // cuenta como perdido el tail que se conserva"): `start` must
            // be known *before* estimating what got dropped — only
            // `tactical[..start]` is actually folded into the summary;
            // `tactical[start..]` (the live tail) survives verbatim into
            // this same request. Estimating over the whole `tactical`
            // slice counted retained events as if they were gone.
            let start = pair_aware_tail_start(&tactical, KEEP_RAW_TAIL);
            let dropped_tokens_estimate = estimate_dropped_tokens(&tactical[..start]);

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
    async fn repair_orphaned_tool_calls(
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

/// Ids of every `AssistantToolCall` in `events` with no matching
/// `ToolCallCompleted` anywhere in the same slice.
fn orphaned_tool_call_ids(events: &[AgentEvent]) -> Vec<String> {
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
fn build_orphan_repair(id: String) -> AgentEvent {
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
fn ensure_unique_tool_call_id(id: String, known_ids: &mut HashSet<String>) -> String {
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
fn pair_aware_tail_start(tactical: &[AgentEvent], min_keep: usize) -> usize {
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
fn merge_summary(mut durable: DurableState, summary: String) -> DurableState {
    if durable.summary.is_empty() {
        durable.summary = summary;
    } else {
        durable.summary = format!("{} {summary}", durable.summary);
    }
    durable
}

/// Sums an optional-per-item `u32` (a cache token count some backends
/// don't report at all) into a single optional total: `None` only if
/// *every* item was `None` (nothing reported anything — stay silent
/// rather than claim "0 tokens cached"), `Some(sum)` treating any
/// individual `None` as 0 once at least one item did report a value.
/// Shared by `Engine::complete_with_best_of_n`'s cache-token summing —
/// see that call site's comment for why this differs from just summing
/// `u32`s directly.
fn sum_optional_u32(values: impl Iterator<Item = u32>) -> Option<u32> {
    let mut sum = 0u32;
    let mut any = false;
    for v in values {
        sum += v;
        any = true;
    }
    any.then_some(sum)
}

/// Canonical signature of a completion outcome for técnica G10's vote
/// (`Engine::complete_with_best_of_n`): the sorted set of (tool name,
/// canonical arguments) pairs it requested — sorted so two candidates
/// that requested the same calls in a different order compare equal —
/// or an empty vec for a "no tool call, final answer" candidate. Reuses
/// the exact canonicalization `dispatch_tool_calls`'s repeated-call
/// detection already relies on (`arguments.to_string()`, stable because
/// `serde_json::Value` serializes object keys in sorted order without
/// the `preserve_order` feature).
fn candidate_signature(outcome: &RoundOutcome) -> Vec<(String, String)> {
    let mut signature: Vec<(String, String)> = outcome
        .tool_calls
        .iter()
        .map(|call| (call.name.clone(), call.arguments.to_string()))
        .collect();
    signature.sort();
    signature
}

/// Best-effort rescue of a tool call a model emitted as plain text instead
/// of a structured `tool_calls` entry — e.g. `{"name": "read_file",
/// "arguments": {"path": "x.txt"}}`, optionally wrapped in a ```json
/// fence. Returns `None` (not an error) for anything that doesn't parse as
/// such — most final text responses legitimately aren't JSON at all, and
/// this must never mistake prose for a tool call. The *whole* response
/// must be the JSON (modulo fences): with no explicit markers, prose
/// around a JSON-looking fragment is too ambiguous to touch. For the
/// explicitly tagged variant that does admit surrounding prose, see
/// [`extract_tagged_tool_calls`].
fn try_parse_textual_tool_call(text: &str) -> Option<ToolCall> {
    parse_tool_call_json(trim_json_fences(text))
}

/// Strips an optional ```json / ``` fence (and surrounding whitespace)
/// from a candidate JSON fragment — shared by both textual-rescue
/// formats: some models fence the bare-JSON variant, and some fence the
/// JSON *inside* their `<tool_call>` tags too.
fn trim_json_fences(text: &str) -> &str {
    text.trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

/// The shared shape check for every textual-rescue format: a JSON object
/// with a string `name` and an object `arguments` (or `parameters`, the
/// synonym some templates use). `None` for anything else — never an
/// error, since the caller treats "doesn't parse" as "not a tool call".
///
/// The synthesized id only needs to be unique within this session's event
/// log (for `tool_use`/`tool_result` correlation) — a real backend id
/// never applies here since none was ever assigned.
fn parse_tool_call_json(candidate: &str) -> Option<ToolCall> {
    let value: serde_json::Value = serde_json::from_str(candidate).ok()?;
    let name = value.get("name")?.as_str()?.to_string();
    let arguments = value
        .get("arguments")
        .or_else(|| value.get("parameters"))?
        .clone();
    if !arguments.is_object() || looks_like_json_schema_definition(&arguments) {
        return None;
    }
    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name,
        arguments,
    })
}

/// `true` when `value` has the shape of a JSON-Schema object
/// (`{"type":"object","properties":{...}}`) rather than actual tool-call
/// arguments — F1 (docs/AUDITORIA-2026-07-v3.md): `parameters` doubles as
/// OpenAI's name for both a tool call's *arguments* and a tool
/// *definition*'s schema, so `{"name":"get_weather","parameters":
/// {"type":"object","properties":{...}}}` — the single most common shape
/// in tool-calling documentation, a very plausible response to "explain
/// how to define a tool" — would otherwise pass the shape check (an
/// object) and get despatched with the schema as if it were the
/// arguments. Real tool-call arguments essentially never carry both a
/// literal `type: "object"` field and a `properties` field at their own
/// top level.
fn looks_like_json_schema_definition(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    matches!(obj.get("type"), Some(serde_json::Value::String(t)) if t == "object")
        && obj.contains_key("properties")
}

/// Rescue for the tagged textual format the Qwen family (and other
/// Hermes-template models) emits natively when its tool-calling template
/// isn't honored end-to-end: `<tool_call>\n{"name": ..., "arguments":
/// ...}\n</tool_call>` — per the Qwen technical report, this is the
/// single highest-leverage textual format for small local models
/// (docs/SOTA-2026-07.md, técnica G6). Unlike the bare-JSON rescue, the
/// explicit tags make surrounding prose unambiguous, so this admits (and
/// preserves) text around the blocks and accepts *several* blocks in one
/// response (Qwen emits one pair of tags per call for parallel calls).
///
/// Returns the parsed calls plus the response text with the parsed
/// blocks removed — the model's surrounding prose is still its text for
/// the round (`run_turn` already persists round text before its tool
/// calls). A tagged block whose inner JSON doesn't parse as a tool call
/// stays in the text verbatim rather than being swallowed; an empty
/// `Vec` means no rescue at all, and the caller must leave the text
/// untouched.
fn extract_tagged_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    const OPEN_TAG: &str = "<tool_call>";
    const CLOSE_TAG: &str = "</tool_call>";

    let mut calls = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN_TAG) {
        let after_open = &rest[start + OPEN_TAG.len()..];
        let Some(inner_len) = after_open.find(CLOSE_TAG) else {
            // Unclosed tag (e.g. the round got cut off mid-block): stop
            // scanning; the final push below keeps everything from the
            // dangling tag onward in the text, visible instead of lost.
            break;
        };
        let block_end = start + OPEN_TAG.len() + inner_len + CLOSE_TAG.len();
        let inner = &after_open[..inner_len];

        // F1 (docs/AUDITORIA-2026-07-v3.md): a block sitting inside a
        // fenced code sample is the model *showing* the format (e.g.
        // answering "how does Qwen emit tool calls?"), not a real leaked
        // attempt — a genuine leak is never fenced, since fencing it
        // would be a successful, intentional act of formatting, not the
        // failure to honor the tool-call template this rescue exists to
        // recover from.
        let absolute_start = text.len() - rest.len() + start;
        if is_inside_code_fence(text, absolute_start) {
            remaining.push_str(&rest[..block_end]);
            rest = &rest[block_end..];
            continue;
        }

        // The wrapper admits three inner grammars: qwen2.5's JSON object,
        // qwen3-coder's `<function=...>` XML (which its template nests
        // inside the same `<tool_call>` tags), and z-ai/glm-5.2's
        // `name<arg_key>K</arg_key><arg_value>V</arg_value>...` tags
        // (docs/usability-log-2026-07-07-si2.md, hallazgo U-15 — observed
        // via OpenRouter when GLM's native tool-calling template isn't
        // honored end-to-end).
        match parse_tool_call_json(trim_json_fences(inner))
            .or_else(|| parse_function_xml_tool_call(inner))
            .or_else(|| parse_glm_arg_tag_tool_call(inner))
        {
            Some(call) => {
                calls.push(call);
                remaining.push_str(&rest[..start]);
            }
            // Malformed inner content: keep the whole tagged block in
            // the text — clearly *meant* as a tool call, but inventing a
            // repair here risks running something the model didn't say.
            None => remaining.push_str(&rest[..block_end]),
        }
        rest = &rest[block_end..];
    }
    if calls.is_empty() {
        return (calls, text.to_string());
    }
    remaining.push_str(rest);
    (calls, remaining.trim().to_string())
}

/// `true` when `offset` (a byte index into `text`) falls inside a
/// ``` ... ``` fenced region — toggled each time a literal "```" marker
/// is seen (doesn't require the fence to be alone on its own line; models
/// are consistent enough about this marker that the simpler check is
/// worth the tiny false-positive risk of a stray triple-backtick in
/// prose). Used to keep the tagged/XML rescues from firing on a fenced
/// example rather than a genuine leaked tool call (hallazgo F1).
fn is_inside_code_fence(text: &str, offset: usize) -> bool {
    let mut in_fence = false;
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find("```") {
        let idx = search_from + rel;
        if idx >= offset {
            break;
        }
        in_fence = !in_fence;
        search_from = idx + 3;
    }
    in_fence
}

/// Rescue for *bare* `<function=...>` blocks — qwen3-coder's XML grammar
/// emitted without its usual `<tool_call>` wrapper (observed leak mode
/// when the template isn't honored end-to-end). Same contract as
/// [`extract_tagged_tool_calls`]: parsed blocks are removed, surrounding
/// prose is preserved, malformed blocks stay in the text, empty `Vec`
/// means "leave the text untouched".
fn extract_function_xml_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    const OPEN_MARK: &str = "<function=";
    const CLOSE_TAG: &str = "</function>";

    let mut calls = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN_MARK) {
        let Some(inner_len) = rest[start..].find(CLOSE_TAG) else {
            break; // unclosed: the final push keeps everything visible
        };
        let block_end = start + inner_len + CLOSE_TAG.len();

        // F1 (docs/AUDITORIA-2026-07-v3.md): see the identical check in
        // `extract_tagged_tool_calls` — a fenced occurrence is the model
        // showing an example, not a real leaked call.
        let absolute_start = text.len() - rest.len() + start;
        if is_inside_code_fence(text, absolute_start) {
            remaining.push_str(&rest[..block_end]);
            rest = &rest[block_end..];
            continue;
        }

        match parse_function_xml_tool_call(&rest[start..block_end]) {
            Some(call) => {
                calls.push(call);
                remaining.push_str(&rest[..start]);
            }
            None => remaining.push_str(&rest[..block_end]),
        }
        rest = &rest[block_end..];
    }
    if calls.is_empty() {
        return (calls, text.to_string());
    }
    remaining.push_str(rest);
    (calls, remaining.trim().to_string())
}

/// Parses one qwen3-coder function-XML block (docs/SOTA-2026-07.md §
/// Adenda — the grammar designed to avoid JSON escaping in
/// code-carrying arguments):
///
/// ```text
/// <function=read_file>
/// <parameter=path>
/// x.txt
/// </parameter>
/// </function>
/// ```
///
/// Parameter values are kept as **strings** (trimmed of the
/// template's surrounding newlines) unless the whole value is clearly
/// structured (starts with `{`/`[` and parses as JSON) — this
/// project's tool arguments are overwhelmingly strings, and coercing a
/// scalar-looking value (`"42"`, `"true"`) into a JSON number/bool
/// would break a `path: String`-style schema downstream, the exact
/// kind of silent damage a rescue must not cause. `None` for anything
/// that doesn't match the grammar.
fn parse_function_xml_tool_call(block: &str) -> Option<ToolCall> {
    let trimmed = block.trim();
    let rest = trimmed.strip_prefix("<function=")?;
    let (name, body) = rest.split_once('>')?;
    let name = name.trim();
    if name.is_empty() || name.contains(['<', '\n']) {
        return None;
    }
    let body = body.strip_suffix("</function>")?;

    let mut arguments = serde_json::Map::new();
    let mut cursor = body;
    while let Some(param_start) = cursor.find("<parameter=") {
        let after_mark = &cursor[param_start + "<parameter=".len()..];
        let (key, after_key) = after_mark.split_once('>')?;
        let key = key.trim();
        let value_len = after_key.find("</parameter>")?;
        if key.is_empty() || key.contains(['<', '\n']) {
            return None;
        }
        let raw_value = after_key[..value_len].trim();
        let value = if raw_value.starts_with(['{', '[']) {
            serde_json::from_str(raw_value)
                .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()))
        } else {
            serde_json::Value::String(raw_value.to_string())
        };
        arguments.insert(key.to_string(), value);
        cursor = &after_key[value_len + "</parameter>".len()..];
    }

    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Parses `z-ai/glm-5.2`'s tool-call grammar as observed inside the
/// shared `<tool_call>...</tool_call>` wrapper (docs/usability-log-2026-07-07-si2.md,
/// hallazgo U-15): the tool name as bare text, followed by zero or more
/// `<arg_key>NAME</arg_key><arg_value>VALUE</arg_value>` pairs — distinct
/// from both qwen2.5's JSON payload and qwen3-coder's `<function=...>`
/// XML the same wrapper already covers:
///
/// ```text
/// <tool_call>read_file<arg_key>limit</arg_key><arg_value>120</arg_value><arg_key>offset</arg_key><arg_value>63</arg_value></tool_call>
/// ```
///
/// Requires at least one `<arg_key>` pair to fire — a bare name with no
/// tags at all is indistinguishable from ordinary prose (the same
/// ambiguity [`extract_pythonic_tool_calls`]'s doc comment calls out for
/// unbracketed `name(...)`), so a genuinely argument-less tool call in
/// this grammar isn't rescued; nothing in the observed leak shows that
/// shape yet. Argument values are kept as strings (trimmed) unless
/// clearly structured JSON (starts with `{`/`[` and parses) — same rule
/// [`parse_function_xml_tool_call`] uses, for the same reason: coercing a
/// scalar-looking value into a JSON number/bool would break a
/// `path: String`-style schema downstream. `None` for anything that
/// doesn't match the grammar, including a key/value pair that isn't
/// immediately adjacent (only whitespace between `</arg_key>` and the
/// next `<arg_value>`) — inventing a repair for a shape this rescue
/// wasn't built for risks running something the model didn't say.
fn parse_glm_arg_tag_tool_call(inner: &str) -> Option<ToolCall> {
    const KEY_OPEN: &str = "<arg_key>";
    const KEY_CLOSE: &str = "</arg_key>";
    const VALUE_OPEN: &str = "<arg_value>";
    const VALUE_CLOSE: &str = "</arg_value>";

    let first_tag = inner.find(KEY_OPEN)?;
    let name = inner[..first_tag].trim();
    if name.is_empty() || name.contains(['<', '\n']) {
        return None;
    }

    let mut arguments = serde_json::Map::new();
    let mut cursor = &inner[first_tag..];
    while let Some(key_start) = cursor.find(KEY_OPEN) {
        let after_key_open = &cursor[key_start + KEY_OPEN.len()..];
        let key_len = after_key_open.find(KEY_CLOSE)?;
        let key = after_key_open[..key_len].trim();
        if key.is_empty() || key.contains(['<', '\n']) {
            return None;
        }
        let after_key = &after_key_open[key_len + KEY_CLOSE.len()..];

        let value_start = after_key.find(VALUE_OPEN)?;
        if !after_key[..value_start].trim().is_empty() {
            return None;
        }
        let after_value_open = &after_key[value_start + VALUE_OPEN.len()..];
        let value_len = after_value_open.find(VALUE_CLOSE)?;
        let raw_value = after_value_open[..value_len].trim();
        let value = if raw_value.starts_with(['{', '[']) {
            serde_json::from_str(raw_value)
                .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()))
        } else {
            serde_json::Value::String(raw_value.to_string())
        };
        arguments.insert(key.to_string(), value);

        cursor = &after_value_open[value_len + VALUE_CLOSE.len()..];
    }

    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Rescue for Llama 3.x's native "pythonic" tool-call format — distinct
/// from Qwen's tagged/XML formats above: one or more comma-separated
/// `name(key=value, ...)` call expressions wrapped in a single pair of
/// square brackets, e.g. `[get_weather(city="SF", metric="celsius")]`.
/// See docs/AUDITORIA-2026-07-v3.md, hallazgo C2 — the rescue escalera
/// covered Qwen's two native formats but nothing for Llama, one of the
/// most commonly installed local model families via Ollama.
///
/// Same contract as [`extract_tagged_tool_calls`]/[`extract_function_xml_tool_calls`]:
/// parsed calls are removed from the returned text, surrounding prose is
/// preserved verbatim, and a bracketed block that doesn't parse cleanly
/// is left in the text rather than guessed at (`None` for anything not
/// unambiguously a call: an unrecognized argument shape fails the whole
/// call, not just that one argument). The bracket wrapper is what makes
/// this safe to scan for unprompted — plain prose describing a function
/// call rarely wraps it in literal `[...]`, unlike a bare `name(...)`
/// pattern which risks matching prose like "call read_file(path) to
/// check" — the exact ambiguity this project's other rescues are careful
/// to avoid.
fn extract_pythonic_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    let mut calls = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;

    while let Some(start) = find_pythonic_block_start(rest) {
        let Some(close_offset) = matching_bracket_end(&rest[start..]) else {
            break; // unclosed '[': stop scanning, leave the rest visible
        };
        let block_end = start + close_offset + 1; // include the ']'
        let inner = &rest[start + 1..block_end - 1];
        match parse_pythonic_calls(inner) {
            Some(parsed) => {
                calls.extend(parsed);
                remaining.push_str(&rest[..start]);
            }
            // Malformed inner content: keep the whole bracketed block in
            // the text — clearly *meant* as a tool call, but inventing a
            // repair here risks running something the model didn't say.
            None => remaining.push_str(&rest[..block_end]),
        }
        rest = &rest[block_end..];
    }

    if calls.is_empty() {
        return (calls, text.to_string());
    }
    remaining.push_str(rest);
    (calls, remaining.trim().to_string())
}

/// Strips any tool-call-shaped block the rescue ladder recognizes,
/// keeping only the surrounding prose — used by
/// [`Engine::attempt_tools_free_summary_round`] only, whose request
/// explicitly carries `tool_stubs: Vec::new()` and tells the model no
/// tool is available: a recognized block there can never be legitimately
/// dispatched (there is nothing to dispatch it *to*), so the choice is
/// between showing it to the user as if it were the real answer or
/// discarding it. Showing it is strictly worse — it replaces one
/// confusing failure (`EmptyModelResponse`) with a worse one (garbage
/// presented as a successful summary). docs/usability-log-2026-07-07-si2.md,
/// hallazgo U-16: `z-ai/glm-5.2` emitted its native (malformed)
/// `<tool_call>` syntax even after being told not to call any tool, and
/// this round — unlike the main one in [`Engine::complete_once_with`] —
/// had no rescue logic at all to catch it before persisting it verbatim.
fn strip_leaked_tool_call_shapes(text: &str) -> String {
    for extract in [
        extract_tagged_tool_calls as fn(&str) -> (Vec<ToolCall>, String),
        extract_function_xml_tool_calls,
        extract_pythonic_tool_calls,
    ] {
        let (calls, remaining) = extract(text);
        if !calls.is_empty() {
            return remaining;
        }
    }
    text.to_string()
}

/// Finds the byte offset of a `[` in `text` that's immediately (modulo
/// whitespace) followed by what looks like the start of a call
/// expression (`identifier(`) — the signal that this bracket is a
/// pythonic tool-call wrapper, not an unrelated list literal or markdown
/// link.
fn find_pythonic_block_start(text: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find('[') {
        let idx = search_from + rel;
        if looks_like_pythonic_call_start(&text[idx + 1..]) {
            return Some(idx);
        }
        search_from = idx + 1;
    }
    None
}

/// `true` when `s` (the text right after a candidate `[`) starts with an
/// identifier immediately followed by `(` — allowing leading whitespace
/// before the identifier, none between the identifier and `(`.
fn looks_like_pythonic_call_start(s: &str) -> bool {
    let s = s.trim_start();
    let ident_len: usize = s
        .char_indices()
        .take_while(|&(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic() || c == '_'
            } else {
                c.is_ascii_alphanumeric() || c == '_'
            }
        })
        .count();
    ident_len > 0 && s[ident_len..].starts_with('(')
}

/// Given `s` starting with `[`, finds the byte offset (within `s`) of the
/// matching `]` — tracking nested `[`/`]` depth and skipping
/// bracket-like characters inside quoted string arguments (`"..."`/
/// `'...'`, with backslash-escaped quotes). `None` if never closed.
fn matching_bracket_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits `s` on top-level occurrences of `sep` — one not nested inside
/// `(...)`/`[...]` or a quoted string. Shared by [`extract_pythonic_tool_calls`]'s
/// two split points: several calls inside the outer brackets, and
/// `key=value` pairs inside one call's parens.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;

    for (i, c) in s.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Parses `inner` (the content between the outer `[`/`]`) as one or more
/// top-level `name(args)` call expressions. `None` if any expression
/// doesn't unambiguously parse as a call — the whole bracketed block is
/// then left as text by the caller rather than partially rescued.
fn parse_pythonic_calls(inner: &str) -> Option<Vec<ToolCall>> {
    let mut calls = Vec::new();
    for expr in split_top_level(inner, ',') {
        let expr = expr.trim();
        if expr.is_empty() {
            continue;
        }
        calls.push(parse_pythonic_call(expr)?);
    }
    if calls.is_empty() { None } else { Some(calls) }
}

/// Parses one `name(key=value, ...)` call expression.
fn parse_pythonic_call(expr: &str) -> Option<ToolCall> {
    let open = expr.find('(')?;
    let name = expr[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = expr[open..].trim();
    let args_str = rest.strip_prefix('(')?.strip_suffix(')')?;

    let mut arguments = serde_json::Map::new();
    for pair in split_top_level(args_str, ',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (key, value_str) = pair.split_once('=')?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        arguments.insert(key.to_string(), parse_pythonic_value(value_str.trim())?);
    }

    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Parses one pythonic scalar literal: a quoted string, `true`/`false`
/// (Python or JSON casing), or a number. No lists/dicts/`None` —
/// deliberately scoped to hallazgo C2's ask (string/number/bool); an
/// argument shaped like anything else fails the whole call rather than
/// guessing at a representation.
fn parse_pythonic_value(s: &str) -> Option<serde_json::Value> {
    if let Some(unquoted) = strip_matching_quotes(s) {
        return Some(serde_json::Value::String(unquoted));
    }
    match s {
        "true" | "True" => return Some(serde_json::Value::Bool(true)),
        "false" | "False" => return Some(serde_json::Value::Bool(false)),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(serde_json::Value::Number(n.into()));
    }
    if let Ok(n) = s.parse::<f64>()
        && let Some(num) = serde_json::Number::from_f64(n)
    {
        return Some(serde_json::Value::Number(num));
    }
    None
}

/// Strips a matching pair of leading/trailing `"`/`'` quotes and
/// unescapes `\"`/`\'`/`\\` — the minimal escape set; anything else
/// (`\n`, etc.) passes through literally rather than guessing at Python
/// string-escape semantics. `None` if `s` isn't quoted (or the quotes
/// don't match).
fn strip_matching_quotes(s: &str) -> Option<String> {
    if s.len() < 2 {
        return None;
    }
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    if !s.ends_with(quote) {
        return None;
    }
    let inner = &s[quote.len_utf8()..s.len() - quote.len_utf8()];

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&next) if next == quote || next == '\\' => {
                    out.push(next);
                    chars.next();
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    Some(out)
}

/// Coerces `arguments` in place to match `schema`'s declared property
/// types, one level deep — narrowly targeted at
/// [`parse_function_xml_tool_call`]'s grammar, which has no native
/// number/boolean type, so every scalar param comes back as a JSON
/// string, and a code-carrying string value can come back mis-parsed as
/// a JSON object (docs/AUDITORIA-2026-07-v3.md, hallazgo F2). Two
/// directions:
/// - a string where the schema declares `integer`/`number`/`boolean` is
///   parsed into that type; left untouched if parsing fails — schema
///   validation, not this function, is what surfaces the real error to
///   the model;
/// - an object/array where the schema declares `string` is re-serialized
///   back to compact JSON text (the mirror-image mistake: the grammar
///   treats any value starting with `{`/`[` as structured, so
///   `<parameter=content>{"a":1}</parameter>` for a `content: string`
///   field parses as an object instead of the literal text).
///
/// A no-op for arguments that already match their schema's declared
/// types — the common case for wire-sourced tool calls, whose backend
/// already sends correctly-typed JSON — so this is safe to call
/// unconditionally before validating/dispatching any call, rescued or
/// not.
fn coerce_arguments_to_schema(arguments: &mut serde_json::Value, schema: &serde_json::Value) {
    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    let Some(args) = arguments.as_object_mut() else {
        return;
    };

    for (key, prop_schema) in properties {
        let Some(value) = args.get_mut(key) else {
            continue;
        };
        let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) else {
            continue;
        };

        if let Some(s) = value.as_str() {
            let s = s.trim().to_string();
            match expected_type {
                "integer" => {
                    if let Ok(n) = s.parse::<i64>() {
                        *value = serde_json::Value::Number(n.into());
                    }
                }
                "number" => {
                    if let Ok(n) = s.parse::<f64>()
                        && let Some(num) = serde_json::Number::from_f64(n)
                    {
                        *value = serde_json::Value::Number(num);
                    }
                }
                "boolean" => match s.as_str() {
                    "true" => *value = serde_json::Value::Bool(true),
                    "false" => *value = serde_json::Value::Bool(false),
                    _ => {}
                },
                _ => {}
            }
        } else if matches!(
            value,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
        ) && expected_type == "string"
            && let Ok(text) = serde_json::to_string(value)
        {
            *value = serde_json::Value::String(text);
        }
    }
}

/// System prompt for the planning round (PLAN.md § "Split
/// planificador/ejecutor"): the base prompt plus planning instructions
/// and the tool list inlined as text — the planner sees the same working
/// context the executor will, plus what tools exist (names + summaries
/// only, deferred-loading style), minus the ability to call any of them.
/// The triviality clause keeps `no_tool`/`single_tool` requests from
/// being inflated with overhead plans.
fn planning_system_prompt(base: &str, stubs: &[ToolStub]) -> String {
    let mut tools_list = String::new();
    for stub in stubs {
        tools_list.push_str(&format!("- {}: {}\n", stub.name, stub.summary));
    }
    if tools_list.is_empty() {
        tools_list.push_str("(none)\n");
    }
    format!(
        "{base}\n\n\
         You are the planning step for this turn. Do NOT call any tool — none are \
         available in this request. Write a short numbered plan (3-7 steps) for how \
         to fulfill the user's latest request, naming the concrete tools you would \
         use from the list below and their key arguments where possible. If the \
         request is trivial (a single obvious action, or directly answerable), \
         reply with just that single step.\n\n\
         Available tools:\n{tools_list}"
    )
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
fn estimate_dropped_tokens(events: &[AgentEvent]) -> u32 {
    let chars: usize = events.iter().map(event_text_len).sum();
    (chars / 4) as u32
}

/// User-visible text length of one `AgentEvent`, for [`estimate_dropped_tokens`].
/// Audit-only variants (`ToolCallStarted`, `Usage`, `Unknown`) carry
/// nothing that ever reaches the model, so they count as `0`.
fn event_text_len(event: &AgentEvent) -> usize {
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
        AgentEvent::ToolCallStarted { .. }
        | AgentEvent::Usage { .. }
        // H-3 (docs/AUDITORIA-2026-07-v5.md) lever events: audit-only,
        // same as `Usage` above — never reached the model, nothing to
        // count as dropped.
        | AgentEvent::TextualRescueApplied { .. }
        | AgentEvent::EscalationToLead { .. }
        | AgentEvent::SummaryFallbackAttempted
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
const NO_CONTEXT_BUDGET_SCALE_MULTIPLIER: usize = 10;

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
fn tactical_cap_scale(context_budget_tokens: Option<u32>) -> usize {
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
fn full_observations_byte_budget(context_budget_tokens: Option<u32>) -> usize {
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
fn effective_tactical_compaction_threshold(
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
fn effective_tactical_full_observations(
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
fn estimate_prompt_tokens(
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
fn estimate_message_tokens(messages: &[Message]) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    use async_trait::async_trait;
    use braze_events::{NoopObserver, TextDeltaObserver};
    use braze_model::ModelError;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
    use braze_types::{ContentBlock, ToolStub};
    use futures::Stream;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::mpsc;

    /// Fixed sequence of "rounds" of `CompletionEvent`s: each call to
    /// `complete` pops and streams the next round, so a test can script a
    /// multi-round exchange (e.g. tool call round, then a final text-only
    /// round).
    struct ScriptedModel {
        rounds: AsyncMutex<std::collections::VecDeque<Vec<CompletionEvent>>>,
    }

    impl ScriptedModel {
        fn new(rounds: Vec<Vec<CompletionEvent>>) -> Self {
            Self {
                rounds: AsyncMutex::new(rounds.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl ModelBackend for ScriptedModel {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            let mut rounds = self.rounds.lock().await;
            let round = rounds
                .pop_front()
                .unwrap_or_else(|| vec![CompletionEvent::Done]);
            Ok(Box::pin(futures::stream::iter(round.into_iter().map(Ok))))
        }
    }

    /// A `ModelBackend` whose stream yields some text then fails mid-round
    /// with a `StreamError` — used to verify `run_turn` never persists the
    /// partial text as if it were a complete response (see A3/B4).
    struct ErroringModel;

    #[async_trait]
    impl ModelBackend for ErroringModel {
        fn name(&self) -> &str {
            "erroring"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            let items = vec![
                Ok(CompletionEvent::TextDelta(
                    "Voy a leer el archi".to_string(),
                )),
                Err(ModelError::StreamError("connection reset".to_string())),
            ];
            Ok(Box::pin(futures::stream::iter(items)))
        }
    }

    /// A `ModelBackend` whose N-th call (0-indexed, `fail_on_attempt`)
    /// errors and every other call succeeds with the same scripted round
    /// — used to prove best-of-n (N-13, docs/AUDITORIA-2026-07-v2.md)
    /// votes among whichever candidates succeeded instead of aborting the
    /// whole round the instant any one of them fails.
    struct FlakyBestOfNModel {
        fail_on_attempt: u32,
        calls: AtomicU32,
        good_round: Vec<CompletionEvent>,
    }

    #[async_trait]
    impl ModelBackend for FlakyBestOfNModel {
        fn name(&self) -> &str {
            "flaky-best-of-n"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            if attempt == self.fail_on_attempt {
                return Err(ModelError::Request(
                    "simulated transient failure".to_string(),
                ));
            }
            Ok(Box::pin(futures::stream::iter(
                self.good_round.clone().into_iter().map(Ok),
            )))
        }
    }

    /// A `ModelBackend` whose single scripted round only resolves after
    /// `delay` — used to force two `run_turn` calls to genuinely overlap
    /// in time (N-17, docs/AUDITORIA-2026-07-v2.md) instead of one
    /// finishing before the other starts.
    struct SlowModel {
        delay: Duration,
        round: Vec<CompletionEvent>,
    }

    #[async_trait]
    impl ModelBackend for SlowModel {
        fn name(&self) -> &str {
            "slow"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            tokio::time::sleep(self.delay).await;
            Ok(Box::pin(futures::stream::iter(
                self.round.clone().into_iter().map(Ok),
            )))
        }
    }

    /// Wraps any `ModelBackend` and validates every
    /// `CompletionRequest.messages` against the Anthropic message-ordering
    /// protocol (`crate::protocol_check`) before delegating to `inner` —
    /// converts what would be a production `400` (or, on a backend that
    /// doesn't validate, a silently wrong conversation) into an immediate,
    /// precisely-diagnosed test failure at the exact call site that built
    /// the bad `Vec<Message>`. Precondition for Grupo I,
    /// docs/AUDITORIA-2026-07-v2.md: several context-pipeline fixes (A1/C1,
    /// A2/C2, C4) had gaps (N-1, N-2, N-4) that no existing test caught,
    /// because `ScriptedModel` never looks at the messages it's handed —
    /// wrapping it in this turns those gaps into a red test right here.
    struct ProtocolValidatingModel<M> {
        inner: M,
    }

    impl<M> ProtocolValidatingModel<M> {
        fn new(inner: M) -> Self {
            Self { inner }
        }
    }

    #[async_trait]
    impl<M: ModelBackend> ModelBackend for ProtocolValidatingModel<M> {
        fn name(&self) -> &str {
            self.inner.name()
        }

        async fn complete(
            &self,
            req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            if let Err(violation) =
                crate::protocol_check::check_anthropic_message_protocol(&req.messages)
            {
                panic!(
                    "invalid message sequence would be rejected by the real Anthropic \
                     API: {violation}\nfull message list: {:#?}",
                    req.messages
                );
            }
            self.inner.complete(req).await
        }
    }

    /// Wraps any `ModelBackend` and records every `CompletionRequest` it
    /// receives — lets a test assert on what a backend was actually
    /// *asked* (e.g. that the executor's first request contains the
    /// plan the planner produced), which `ScriptedModel` alone can't:
    /// it ignores its request entirely.
    struct RequestCapturingModel<M> {
        inner: M,
        requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
    }

    #[async_trait]
    impl<M: ModelBackend> ModelBackend for RequestCapturingModel<M> {
        fn name(&self) -> &str {
            self.inner.name()
        }

        async fn complete(
            &self,
            req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            self.requests.lock().unwrap().push(req.clone());
            self.inner.complete(req).await
        }
    }

    /// Minimal `TaskNotifier`: `tokio::spawn` per task + an mpsc
    /// completion channel, same shape `braze-cli::ChannelTaskNotifier`
    /// uses in the real binary — duplicated here (rather than depending on
    /// `braze-cli`, which would be a backwards dependency) purely so
    /// `Engine`'s tests don't need a real binary-level notifier.
    struct TestNotifier {
        tx: mpsc::UnboundedSender<(TaskHandle, ToolResult)>,
        rx: AsyncMutex<mpsc::UnboundedReceiver<(TaskHandle, ToolResult)>>,
        next: AtomicU64,
        // Mirrors `braze_events::ChannelTaskNotifier`'s own tracking, so
        // tests can prove `dispatch_tool_calls`'s timeout path really
        // cancels a task instead of just forgetting about it (N-33).
        handles: std::sync::Mutex<HashMap<TaskHandle, tokio::task::JoinHandle<()>>>,
    }

    impl TestNotifier {
        fn new() -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            Self {
                tx,
                rx: AsyncMutex::new(rx),
                next: AtomicU64::new(0),
                handles: std::sync::Mutex::new(HashMap::new()),
            }
        }

        /// Queues a completion for a handle that was never returned by
        /// `spawn` — simulating a task from an earlier round that finally
        /// finished after that round already gave up on it (timeout), so
        /// its handle is no longer in the current round's `pending` set.
        /// `TaskHandle(u64::MAX)` is guaranteed never to collide with a
        /// real handle from `spawn`'s monotonic counter, which starts at 0.
        fn inject_stale_completion(&self, tool_call_id: &str) {
            let stale = ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: "stale result that must never be persisted".to_string(),
                is_error: false,
            };
            let _ = self.tx.send((TaskHandle(u64::MAX), stale));
        }
    }

    #[async_trait]
    impl TaskNotifier for TestNotifier {
        fn spawn(&self, task: BackgroundTask) -> TaskHandle {
            let handle = TaskHandle(self.next.fetch_add(1, Ordering::SeqCst));
            let tx = self.tx.clone();
            let join = tokio::spawn(async move {
                let result = task.work.await;
                let _ = tx.send((handle, result));
            });
            self.handles.lock().unwrap().insert(handle, join);
            handle
        }

        async fn next_completed(&self, timeout: Duration) -> Option<(TaskHandle, ToolResult)> {
            let mut rx = self.rx.lock().await;
            let completed = tokio::time::timeout(timeout, rx.recv())
                .await
                .ok()
                .flatten();
            if let Some((handle, _)) = &completed {
                self.handles.lock().unwrap().remove(handle);
            }
            completed
        }

        fn abort(&self, handle: TaskHandle) {
            if let Some(join) = self.handles.lock().unwrap().remove(&handle) {
                join.abort();
            }
        }
    }

    /// Fake `ToolProvider` owning exactly one tool, `echo`, which returns
    /// its `text` argument back verbatim. Its schema requires `text` (a
    /// real schema with a required field, not the generic permissive
    /// `{"type":"object"}` this provider originally had) so tests can
    /// exercise real validation failures. `invocations` is an `Arc` shared
    /// with the test that constructs it, so a test can assert `invoke` was
    /// never called for a call that should have been rejected by schema
    /// validation before ever reaching dispatch.
    struct EchoToolProvider {
        invocations: Arc<AtomicU32>,
    }

    impl EchoToolProvider {
        fn new(invocations: Arc<AtomicU32>) -> Self {
            Self { invocations }
        }
    }

    #[async_trait]
    impl ToolProvider for EchoToolProvider {
        fn provider_id(&self) -> &str {
            "test:echo"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            Ok(vec![ToolStub {
                name: "echo".to_string(),
                summary: "echoes its input".to_string(),
                source: "test:echo".to_string(),
                input_schema: None,
            }])
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name == "echo" {
                Ok(Some(ToolSchema {
                    name: "echo".to_string(),
                    description: "echoes its input".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    }),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let text = call
                .arguments
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("echoed: {text}"),
                is_error: false,
            })
        }
    }

    /// Like `EchoToolProvider`, but its schema declares an `integer`
    /// field — used to test F2 (docs/AUDITORIA-2026-07-v3.md):
    /// `coerce_arguments_to_schema` must turn a stringified integer from
    /// the qwen3-coder XML rescue (whose grammar has no native number
    /// type) into a real JSON number before validation/dispatch, instead
    /// of failing schema validation and burning a repair round.
    struct EchoWithLimitToolProvider {
        invocations: Arc<AtomicU32>,
        received_limit: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    }

    impl EchoWithLimitToolProvider {
        fn new(
            invocations: Arc<AtomicU32>,
            received_limit: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
        ) -> Self {
            Self {
                invocations,
                received_limit,
            }
        }
    }

    #[async_trait]
    impl ToolProvider for EchoWithLimitToolProvider {
        fn provider_id(&self) -> &str {
            "test:echo_limit"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            Ok(vec![ToolStub {
                name: "echo_limit".to_string(),
                summary: "echoes with a numeric limit".to_string(),
                source: "test:echo_limit".to_string(),
                input_schema: None,
            }])
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name == "echo_limit" {
                Ok(Some(ToolSchema {
                    name: "echo_limit".to_string(),
                    description: "echoes with a numeric limit".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "text": {"type": "string"},
                            "limit": {"type": "integer"},
                        },
                        "required": ["text", "limit"],
                    }),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            *self.received_limit.lock().unwrap() = call.arguments.get("limit").cloned();
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "ok".to_string(),
                is_error: false,
            })
        }
    }

    /// Mock provider offering tools named exactly like two of the real
    /// local built-ins — used to test F6
    /// (docs/AUDITORIA-2026-07-v3.md) against the actual names
    /// `MUTATING_TOOL_NAMES` lists, not stand-ins.
    struct ReadWriteToolProvider {
        read_invocations: Arc<AtomicU32>,
    }

    impl ReadWriteToolProvider {
        fn new(read_invocations: Arc<AtomicU32>) -> Self {
            Self { read_invocations }
        }
    }

    #[async_trait]
    impl ToolProvider for ReadWriteToolProvider {
        fn provider_id(&self) -> &str {
            "test:read_write"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            Ok(vec![
                ToolStub {
                    name: "read_file".to_string(),
                    summary: "reads a file".to_string(),
                    source: "test:read_write".to_string(),
                    input_schema: None,
                },
                ToolStub {
                    name: "write_file".to_string(),
                    summary: "writes a file".to_string(),
                    source: "test:read_write".to_string(),
                    input_schema: None,
                },
            ])
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name != "read_file" && name != "write_file" {
                return Ok(None);
            }
            Ok(Some(ToolSchema {
                name: name.to_string(),
                description: name.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                }),
            }))
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            match call.name.as_str() {
                "read_file" => {
                    self.read_invocations.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolResult {
                        tool_call_id: call.id.clone(),
                        content: "contenido".to_string(),
                        is_error: false,
                    })
                }
                "write_file" => Ok(ToolResult {
                    tool_call_id: call.id.clone(),
                    content: "wrote".to_string(),
                    is_error: false,
                }),
                other => Err(ToolError::NotFound(other.to_string())),
            }
        }
    }

    /// Like `EchoToolProvider`, but the delay before resolving is taken
    /// from the call's `delay_ms` argument — lets a test make a round's
    /// concurrently-dispatched tool calls resolve in a chosen order
    /// (e.g. reverse of dispatch order) via real `tokio::spawn`/`sleep`
    /// scheduling, instead of only ever simulating that race by seeding
    /// events directly in a specific order.
    struct ReorderingEchoToolProvider {
        invocations: Arc<AtomicU32>,
    }

    impl ReorderingEchoToolProvider {
        fn new(invocations: Arc<AtomicU32>) -> Self {
            Self { invocations }
        }
    }

    #[async_trait]
    impl ToolProvider for ReorderingEchoToolProvider {
        fn provider_id(&self) -> &str {
            "test:reordering-echo"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            Ok(vec![ToolStub {
                name: "echo".to_string(),
                summary: "echoes its input after an id-dependent delay".to_string(),
                source: "test:reordering-echo".to_string(),
                input_schema: None,
            }])
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name == "echo" {
                Ok(Some(ToolSchema {
                    name: "echo".to_string(),
                    description: "echoes its input".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "text": {"type": "string"},
                            "delay_ms": {"type": "number"},
                        },
                        "required": ["text"],
                    }),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let text = call
                .arguments
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let delay_ms = call
                .arguments
                .get("delay_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("echoed: {text}"),
                is_error: false,
            })
        }
    }

    /// A `ToolProvider` whose one tool sleeps a fixed delay then flips a
    /// shared flag before returning — used to prove a tool-completion
    /// timeout genuinely cancels the background task (the flag stays
    /// `false`) rather than merely giving up on waiting for it (the flag
    /// would eventually flip `true` regardless of what the engine decided
    /// to do with it). See N-33, docs/AUDITORIA-2026-07-v2.md.
    struct SlowToolProvider {
        delay: Duration,
        completed: Arc<AtomicBool>,
    }

    impl SlowToolProvider {
        fn new(delay: Duration, completed: Arc<AtomicBool>) -> Self {
            Self { delay, completed }
        }
    }

    #[async_trait]
    impl ToolProvider for SlowToolProvider {
        fn provider_id(&self) -> &str {
            "test:slow"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            Ok(vec![ToolStub {
                name: "slow".to_string(),
                summary: "sleeps then completes".to_string(),
                source: "test:slow".to_string(),
                input_schema: None,
            }])
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name == "slow" {
                Ok(Some(ToolSchema {
                    name: "slow".to_string(),
                    description: "sleeps then completes".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            tokio::time::sleep(self.delay).await;
            self.completed.store(true, Ordering::SeqCst);
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "done".to_string(),
                is_error: false,
            })
        }
    }

    fn temp_store() -> (FileSessionStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "braze-engine-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        (FileSessionStore::new(dir.clone()), dir)
    }

    /// Regression test for A3/B4: a stream that fails mid-round (after
    /// delivering partial text) must propagate as an error from
    /// `run_turn`, and the partial text must never be persisted as if it
    /// were a complete `AssistantText` response.
    #[tokio::test]
    async fn a_mid_stream_error_propagates_and_does_not_persist_partial_text() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let engine = Engine::new(
            Box::new(ErroringModel),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::Model(_))),
            "expected the stream error to propagate as EngineError::Model, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "the partial text from the failed round must never be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-33 (docs/AUDITORIA-2026-07-v2.md): when
    /// `dispatch_tool_calls`'s wait for a round's tool completions times
    /// out, every still-pending task must be genuinely cancelled via
    /// `TaskNotifier::abort`, not merely given up on while it keeps
    /// running unobserved.
    #[tokio::test]
    async fn a_tool_completion_timeout_actually_cancels_the_still_running_task() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let completed = Arc::new(AtomicBool::new(false));

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "slow".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(SlowToolProvider::new(
                Duration::from_millis(300),
                Arc::clone(&completed),
            ))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_completion_timeout(Duration::from_millis(20));

        engine
            .run_turn(&session, "please run the slow tool", &mut NoopObserver)
            .await
            .expect(
                "turn should still converge — the timeout is treated as a \
                 recoverable tool failure, not a hard error",
            );

        // Longer than the tool's own delay — if the background task
        // hadn't really been cancelled, the flag would be true by now.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !completed.load(Ordering::SeqCst),
            "the tool call kept running in the background after the engine \
             timed out waiting for it"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-13 (docs/AUDITORIA-2026-07-v2.md): a
    /// transient error on one best-of-n candidate must not discard the
    /// other candidates already generated and fail the whole round —
    /// the turn must still converge by voting among the survivors.
    #[tokio::test]
    async fn best_of_n_votes_among_successful_candidates_when_one_attempt_errors() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = FlakyBestOfNModel {
            fail_on_attempt: 1,
            calls: AtomicU32::new(0),
            good_round: vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        };

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(3);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should still succeed despite one candidate erroring");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { text } if text == "done")),
            "expected the winning candidate's text to be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-13: if *every* best-of-n candidate fails,
    /// the round must still propagate an error — voting among survivors
    /// must not mask a genuine total failure as success.
    #[tokio::test]
    async fn best_of_n_fails_the_round_only_when_every_candidate_fails() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ErroringModel;
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(3);

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::Model(_))),
            "expected every candidate to fail and the error to propagate, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-17 (docs/AUDITORIA-2026-07-v2.md): a second
    /// `run_turn` call on the same `Engine` while a first one is still in
    /// flight must be rejected explicitly instead of racing it over the
    /// shared `TaskNotifier` completion channel.
    #[tokio::test]
    async fn a_second_concurrent_run_turn_is_rejected_while_the_first_is_in_flight() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = SlowModel {
            delay: Duration::from_millis(200),
            round: vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        };

        let engine = Arc::new(Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        ));

        let engine_a = Arc::clone(&engine);
        let first = tokio::spawn(async move {
            let mut observer = NoopObserver;
            engine_a.run_turn(&session, "hola", &mut observer).await
        });

        // Give the first call time to acquire the guard before the second
        // one starts.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut observer_b = NoopObserver;
        let result_b = engine
            .run_turn(&session, "hola de nuevo", &mut observer_b)
            .await;
        assert!(
            matches!(result_b, Err(EngineError::ConcurrentTurn)),
            "expected the second call to be rejected, got {result_b:?}"
        );

        let result_a = first.await.expect("first turn's task should not panic");
        assert!(result_a.is_ok(), "the first turn should still succeed");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-24 (docs/AUDITORIA-2026-07-v2.md): a round
    /// with no tool calls whose `stop_reason` reports token-budget
    /// truncation must not be persisted as a normal converged answer.
    #[tokio::test]
    async fn a_truncated_final_response_is_reported_as_an_error_not_a_silent_success() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("partial answer cut off mid".to_string()),
            CompletionEvent::Usage {
                input_tokens: 10,
                output_tokens: 100,
                stop_reason: Some("max_tokens".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::TruncatedFinalResponse)),
            "expected a truncated final response to be reported as an error, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "the truncated text must never be persisted as a normal final answer"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the "una completion vacía termina el turno
    /// como éxito silencioso" bajo (docs/AUDITORIA-2026-07-v2.md): a
    /// round with no text and no tool calls at all must not be treated as
    /// a legitimate, silent convergence.
    #[tokio::test]
    async fn an_empty_completion_is_reported_as_an_error_not_a_silent_success() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![CompletionEvent::Done]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse)),
            "expected an empty completion to be reported as an error, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for U-1 (docs/usability-log-template.md, hallado en
    /// vivo 2026-07-07 contra qwen3.5-coder/Nitro): a turn that already
    /// dispatched a successful tool call, then gets a completely empty
    /// round (no text, no tool calls) — the exact shape a real session
    /// hit right after `write_file` succeeded — must recover via the
    /// tools-free summary round instead of discarding that already-done
    /// work behind a hard `EmptyModelResponse` error.
    #[tokio::test]
    async fn an_empty_round_after_a_dispatched_tool_call_recovers_via_the_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Done,
            ],
            // The round right after the tool call comes back with
            // nothing at all — no text, no further tool calls.
            vec![CompletionEvent::Done],
            // The tools-free summary attempt — reports Usage like any
            // real model round would (H-4: it used to be dropped).
            vec![
                CompletionEvent::TextDelta("listo, ya lo hice".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 900,
                    output_tokens: 15,
                    stop_reason: Some("end_turn".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected the turn to recover via the summary round, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "listo, ya lo hice"
            )),
            "expected the summary round's text to be persisted, got: {events:?}"
        );
        // The already-dispatched tool call's own result must still be
        // there too — this is the actual work the original bug threw away.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        );
        // H-3 (docs/AUDITORIA-2026-07-v5.md): the fallback being *reached
        // for* is now persisted, independent of whether it went on to
        // produce usable text.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "expected SummaryFallbackAttempted to be persisted, got: {events:?}"
        );
        // H-4 (docs/AUDITORIA-2026-07-v5.md): the fallback's own Usage is
        // persisted too — it re-sends the whole history as its prompt, so
        // dropping it under-reported cost exactly on degraded turns. The
        // fallback round is the only scripted round that reports Usage in
        // this test, so exactly one must land, with its numbers.
        let usage_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => Some((*input_tokens, *output_tokens)),
                _ => None,
            })
            .collect();
        assert_eq!(
            usage_events,
            vec![(900, 15)],
            "the summary fallback's Usage must be persisted, got: {events:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Same shape as the recovery test above, but the tools-free summary
    /// attempt *also* comes back empty — the turn must still surface
    /// `EmptyModelResponse` rather than silently reporting success with
    /// nothing to show for the follow-up round.
    #[tokio::test]
    async fn an_empty_round_after_a_dispatched_tool_call_still_fails_if_the_summary_round_is_also_empty()
     {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            // The tools-free summary attempt is ALSO empty.
            vec![CompletionEvent::Done],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse)),
            "expected the still-empty summary round to surface as an error, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_with_no_tool_calls_streams_text_and_persists_it() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("Hola ".to_string()),
            CompletionEvent::TextDelta("mundo".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "Hola mundo");

        // Re-open the same on-disk store to verify persistence directly.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_persists_usage_reported_by_the_backend() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Usage {
                input_tokens: 42,
                output_tokens: 7,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: Some(30),
                cache_write_tokens: Some(5),
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        match &events[1] {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                assert_eq!(*input_tokens, 42);
                assert_eq!(*output_tokens, 7);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(*cache_read_tokens, Some(30));
                assert_eq!(*cache_write_tokens, Some(5));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(events[2], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_with_a_tool_call_round_trips_end_to_end() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "please echo hi",
                &mut TextDeltaObserver(|chunk: &str| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "done");
        // Valid arguments: unchanged behavior — the real tool actually ran,
        // exactly once.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantToolCall { .. }));
        assert!(matches!(events[2], AgentEvent::ToolCallStarted { .. }));
        match &events[3] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.content, "echoed: hi");
                assert!(!result.is_error);
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }
        assert!(matches!(events[4], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Smoke test for `ProtocolValidatingModel`: the exact same scenario as
    /// `run_turn_with_a_tool_call_round_trips_end_to_end` above, but with
    /// the model wrapped in the validator — proves the harness itself is
    /// usable (doesn't false-positive on a normal, well-formed exchange)
    /// before it's relied on by the regression tests below.
    #[tokio::test]
    async fn run_turn_with_a_tool_call_passes_protocol_validation() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]));

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed and every request should pass protocol validation");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-14.
    ///
    /// Nothing previously checked that a `ToolCallRequested`'s id was
    /// unique before persisting it as an `AssistantToolCall` — a model
    /// that (accidentally or via a buggy backend's synthetic-id fallback)
    /// issues two calls sharing one id would get two `tool_use`/
    /// `tool_result` pairs with the same id in the append-only log,
    /// which Anthropic rejects permanently on every future request.
    #[tokio::test]
    async fn duplicate_tool_use_ids_in_one_round_are_renamed_to_stay_unique() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "a" }),
                },
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "b" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]));

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo two things", &mut NoopObserver)
            .await
            .expect(
                "turn should succeed despite the duplicate id — and every \
                     request built along the way must still pass protocol validation",
            );

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            2,
            "both calls have different arguments, so both must dispatch \
             (not be treated as an identical repeated call)"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let tool_use_ids: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_use_ids.len(), 2);
        assert_ne!(
            tool_use_ids[0], tool_use_ids[1],
            "the second call's id must have been renamed to stay unique"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-4.
    ///
    /// `load_messages_repairs_an_orphaned_tool_use_with_no_result` (below)
    /// proves the repair *happens*, but it seeds the orphan and calls
    /// `load_messages` directly with nothing in between — so the repaired
    /// `ToolCallCompleted` lands right after the orphaned `AssistantToolCall`
    /// and the sequence is trivially valid. That's not what actually
    /// happens in production: `run_turn` appends the turn's new
    /// `UserMessage` *before* calling `load_messages` (see `run_turn`'s
    /// first few lines), so the repair — which only runs inside
    /// `load_messages` — ends up appended *after* that `UserMessage`
    /// instead of immediately after the tool_use it repairs. The resulting
    /// log order (`tool_use`, unrelated `user` text, `tool_result`) is
    /// exactly what `ProtocolValidatingModel` is built to catch, and it
    /// persists to disk — every future resume repeats it.
    ///
    /// Fixed: `run_turn` now calls `repair_session` (which repairs and
    /// persists, discarding the loaded events) *before* appending the
    /// turn's `UserMessage` — see `Engine::run_turn`/`Engine::repair_session`.
    #[tokio::test]
    async fn resuming_after_a_crash_with_an_orphaned_tool_call_stays_protocol_valid() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Simulate a process that crashed between persisting the tool_use
        // and receiving its result: an `AssistantToolCall` with no
        // matching `ToolCallCompleted` anywhere in the log yet.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "please echo hi".to_string(),
                },
            )
            .await
            .expect("seed the original user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
            )
            .await
            .expect("seed an orphaned tool_use — process 'crashed' right here");

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("done".to_string()),
            CompletionEvent::Done,
        ]]));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        // Resuming the session and sending a new message must repair the
        // orphan into a protocol-valid sequence — `ProtocolValidatingModel`
        // panics inside `complete()` if it instead produces the
        // tool_use/user-text/tool_result order the bug creates.
        engine
            .run_turn(&session, "are you still there?", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let _ = tokio::fs::remove_dir_all(&dir).await;
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

    /// PLAN.md § "Split planificador/ejecutor", oleada 1: the shape a
    /// planned turn produces — `UserMessage`, `PlanCreated`, then the
    /// first round's tool calls — must render into a request the real
    /// Anthropic API accepts. The plan becomes an assistant Text message
    /// immediately before the round's assistant tool_use message
    /// (consecutive assistant messages — already the exact shape a
    /// text-before-tools round produces today), and the tool_use/result
    /// pairing must survive the plan sitting in between.
    #[tokio::test]
    async fn a_planned_turn_shape_renders_protocol_valid_messages() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        for event in [
            AgentEvent::UserMessage {
                text: "haz tres cosas".to_string(),
            },
            AgentEvent::PlanCreated {
                plan: "1. echo a\n2. echo b\n3. responder".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": "a" }),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "echoed: a".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "listo".to_string(),
            },
        ] {
            store.append(&session, &event).await.expect("seed event");
        }

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

        crate::protocol_check::check_anthropic_message_protocol(&messages)
            .expect("a planned turn's rendered request must be protocol-valid");

        assert!(
            messages
                .iter()
                .any(|m| m.role == braze_types::Role::Assistant
                    && m.content.iter().any(
                        |b| matches!(b, ContentBlock::Text { text } if text.starts_with("Plan:\n"))
                    )),
            "the plan must actually reach the rendered request, got: {messages:#?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- oleada 2: Engine::with_planner (PLAN.md § "Split planificador/ejecutor") ---

    /// End-to-end happy path: the planner's text is persisted as
    /// `PlanCreated` (with its `Usage` before it), the executor's first
    /// request actually contains the rendered plan, and the whole planned
    /// turn stays protocol-valid.
    #[tokio::test]
    async fn a_planned_turn_persists_the_plan_and_the_executor_sees_it() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. echo hi\n2. responder".to_string()),
            CompletionEvent::Usage {
                input_tokens: 50,
                output_tokens: 12,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);

        let executor_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = RequestCapturingModel {
            inner: ProtocolValidatingModel::new(ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": "hi" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ])),
            requests: Arc::clone(&executor_requests),
        };

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "haz echo de hi", &mut NoopObserver)
            .await
            .expect("planned turn should succeed");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        match &events[1] {
            AgentEvent::Usage { input_tokens, .. } => assert_eq!(*input_tokens, 50),
            other => panic!("expected the planner's Usage first, got {other:?}"),
        }
        match &events[2] {
            AgentEvent::PlanCreated { plan } => {
                assert_eq!(plan, "1. echo hi\n2. responder");
            }
            other => panic!("expected PlanCreated, got {other:?}"),
        }

        {
            let requests = executor_requests.lock().unwrap();
            assert!(
                requests[0].messages.iter().any(|m| m.content.iter().any(
                    |b| matches!(b, ContentBlock::Text { text } if text.starts_with("Plan:\n1. echo hi"))
                )),
                "the executor's first request must contain the rendered plan"
            );
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 1: a planner whose call fails must not fail the
    /// turn — it proceeds unplanned.
    #[tokio::test]
    async fn a_failing_planner_degrades_to_an_unplanned_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(ErroringModel));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the turn must survive a failing planner");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "no plan must be persisted when the planner fails"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { text } if text == "hola")),
            "the executor's answer must still be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 3 (espíritu N-24): a plan truncated by the token
    /// budget is discarded — a cut-off plan can mislead mid-step — but
    /// its `Usage` is still persisted: the cost was real.
    #[tokio::test]
    async fn a_truncated_plan_is_discarded_but_its_usage_is_persisted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. paso cortado a mit".to_string()),
            CompletionEvent::Usage {
                input_tokens: 40,
                output_tokens: 1024,
                stop_reason: Some("max_tokens".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the turn must survive a truncated plan");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "a truncated plan must be discarded"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Usage {
                    input_tokens: 40,
                    ..
                }
            )),
            "the planner's Usage must be persisted even when its plan is discarded"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 4: an empty planner response degrades to an
    /// unplanned turn (contrast with the *executor*, where an empty
    /// completion on the turn's very first round is a hard
    /// `EmptyModelResponse` error — one occurring after the turn already
    /// dispatched a tool call instead gets one tools-free summary attempt,
    /// see `attempt_tools_free_summary_round`).
    #[tokio::test]
    async fn an_empty_planner_response_degrades_to_an_unplanned_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![CompletionEvent::Done]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the turn must survive an empty planner response");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. }))
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 2: a planner that attempts tool calls despite the
    /// planning prompt has them ignored — never dispatched — while its
    /// text is still used as the plan.
    #[tokio::test]
    async fn a_planner_that_attempts_tool_calls_has_them_ignored_but_its_text_used() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::ToolCallRequested {
                id: "planner-call".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": "should never run" }),
            },
            CompletionEvent::TextDelta("1. hacer echo de hi".to_string()),
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the planner's tool call must never be dispatched"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::PlanCreated { plan } if plan == "1. hacer echo de hi"
            )),
            "the planner's text must still be used as the plan"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F7 (docs/AUDITORIA-2026-07-v3.md): `planning_system_prompt` asks
    /// the planner to name the concrete tools it would use. A local
    /// planner answering in its own native tool-template syntax (e.g.
    /// Qwen's `<tool_call>{...}</tool_call>`) must have that block survive
    /// as plain plan *text* — the textual rescue, shared with the
    /// executor by default before this fix, would otherwise extract and
    /// remove it from the plan before `attempt_planning_round` even
    /// looks at `outcome.tool_calls` (already ignored there regardless).
    #[tokio::test]
    async fn a_planners_native_tool_template_leak_survives_as_plan_text_not_rescued() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(
                "1. <tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"x\"}}</tool_call>\n\
                 2. responder"
                    .to_string(),
            ),
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the leaked block must never be dispatched as a real call"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::PlanCreated { .. }))
            .expect("expected a PlanCreated event")
        {
            AgentEvent::PlanCreated { plan } => {
                assert!(
                    plan.contains("<tool_call>"),
                    "the tagged block must survive as plan text, got: {plan}"
                );
                assert!(plan.contains("read_file"), "got: {plan}");
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Records everything the engine mirrors into it, for asserting the
    /// live `TurnObserver` seam (PLAN.md § "Fase TUI — diseño", oleada 1)
    /// sees exactly what gets persisted, in the same order.
    struct RecordingObserver {
        deltas: Vec<String>,
        events: Vec<AgentEvent>,
    }

    impl TurnObserver for RecordingObserver {
        fn on_text_delta(&mut self, delta: &str) {
            self.deltas.push(delta.to_string());
        }
        fn on_event(&mut self, event: &AgentEvent) {
            self.events.push(event.clone());
        }
    }

    /// Variant name only — the mirror test cares about kind and order,
    /// not payload equality.
    fn event_kind(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::UserMessage { .. } => "UserMessage",
            AgentEvent::AssistantText { .. } => "AssistantText",
            AgentEvent::AssistantToolCall { .. } => "AssistantToolCall",
            AgentEvent::ToolCallStarted { .. } => "ToolCallStarted",
            AgentEvent::ToolCallCompleted { .. } => "ToolCallCompleted",
            AgentEvent::CompactionOccurred { .. } => "CompactionOccurred",
            AgentEvent::Usage { .. } => "Usage",
            _ => "Other",
        }
    }

    /// The observer must receive a live mirror of every event the turn
    /// persists, in persistence order, plus the raw text deltas — the
    /// contract `braze-tui` will render from.
    #[tokio::test]
    async fn the_observer_mirrors_every_persisted_event_in_order() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("do".to_string()),
                CompletionEvent::TextDelta("ne".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut observer = RecordingObserver {
            deltas: Vec::new(),
            events: Vec::new(),
        };
        engine
            .run_turn(&session, "please echo hi", &mut observer)
            .await
            .expect("turn should succeed");

        assert_eq!(observer.deltas, vec!["do", "ne"]);

        // The mirrored sequence must match the persisted log exactly —
        // same kinds, same order, nothing extra and nothing missing.
        let verify_store = FileSessionStore::new(dir.clone());
        let persisted = verify_store.load(&session).await.expect("load events");
        assert_eq!(
            observer.events.iter().map(event_kind).collect::<Vec<_>>(),
            persisted.iter().map(event_kind).collect::<Vec<_>>(),
        );
        assert_eq!(
            observer.events.iter().map(event_kind).collect::<Vec<_>>(),
            vec![
                "UserMessage",
                "AssistantToolCall",
                "ToolCallStarted",
                "ToolCallCompleted",
                "AssistantText",
            ],
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A4: a completion delivered for a handle that
    /// isn't part of the current round's `pending` set (simulating a task
    /// that finally finished after an earlier round already gave up on it
    /// via timeout) must be discarded, not persisted as a second
    /// `ToolCallCompleted` — which would otherwise corrupt the session
    /// with two `tool_result`s for a single `tool_use_id`.
    #[tokio::test]
    async fn stale_completion_from_an_earlier_round_is_discarded_not_persisted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let notifier = TestNotifier::new();
        // Queued before dispatch even starts, so it is guaranteed to be
        // the first completion `next_completed` yields — exactly the
        // ordering a stale, previously-timed-out task's late delivery
        // would produce.
        notifier.inject_stale_completion("call-1");

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(notifier),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let completions: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
            .collect();
        // Exactly one `ToolCallCompleted` for call-1 — the real one — not
        // two.
        assert_eq!(completions.len(), 1);
        match completions[0] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invalid_args_get_one_round_of_schema_repair_context_then_the_retry_succeeds() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                // First attempt: missing the required `text` field.
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Second attempt (scripted as if the model read the repair
                // context and corrected itself): valid arguments.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantToolCall { .. }));

        // The rejected call never gets a `ToolCallStarted` (it never
        // reaches dispatch) — its `ToolCallCompleted` follows the
        // `AssistantToolCall` directly, and carries the resolved schema so
        // the model has something concrete to correct itself with.
        match &events[2] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
                // "properties" only appears in the serialized schema dump,
                // never in `jsonschema`'s own error text (which reads
                // along the lines of `"text" is a required property`,
                // singular) — a reliable signal the schema was included.
                assert!(result.content.contains("properties"));
                assert!(result.content.contains("text"));
                // The real tool must never have run for the rejected call.
                assert_ne!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted for call-1, got {other:?}"),
        }

        assert!(matches!(events[3], AgentEvent::AssistantToolCall { .. }));
        assert!(matches!(events[4], AgentEvent::ToolCallStarted { .. }));
        match &events[5] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-2");
                assert!(!result.is_error);
                assert_eq!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted for call-2, got {other:?}"),
        }

        // `invoke` ran exactly once: only for the corrected, valid call.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_second_invalid_call_to_the_same_tool_in_one_turn_gets_no_more_schema_context() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same tool, still invalid — the model didn't correct
                // itself this time.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "wrong_field": 1 }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("giving up".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let first_message = match &events[2] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
                assert!(result.content.contains("properties"));
                result.content.clone()
            }
            other => panic!("expected ToolCallCompleted for call-1, got {other:?}"),
        };

        match &events[4] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-2");
                assert!(result.is_error);
                // Second failure of the same tool name this turn: no
                // schema dump this time, and a visibly shorter/different
                // message than the first repair-context one.
                assert!(!result.content.contains("properties"));
                assert_ne!(result.content, first_message);
                assert!(result.content.len() < first_message.len());
            }
            other => panic!("expected ToolCallCompleted for call-2, got {other:?}"),
        }

        // Both calls were rejected before dispatch — `invoke` never ran.
        assert_eq!(invocations.load(Ordering::SeqCst), 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A5: the model repeating an identical
    /// (name, arguments) tool call within the same turn must be nudged
    /// instead of re-dispatched — the dominant non-convergence pattern for
    /// small/local models, which otherwise burn a round (and, in Ollama's
    /// case, real CPU time) re-running a call whose result can't change.
    #[tokio::test]
    async fn an_identical_repeated_tool_call_is_nudged_not_re_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same tool, same arguments, different id — a small model
                // re-issuing the identical call instead of using the
                // result it already has.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi twice", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        // The real tool only ran once — the repeat was nudged, not
        // re-dispatched.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-2"))
            .expect("expected a ToolCallCompleted for call-2")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(result.is_error);
                assert!(result.content.contains("already called"));
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F6 (docs/AUDITORIA-2026-07-v3.md): `read_file(x)` → `write_file(x)`
    /// → `read_file(x)` again is a legitimate re-verification pattern —
    /// the second `read_file` must actually re-run (the write may have
    /// changed what it returns), not get nudged with a now-false "the
    /// result has not changed" claim.
    #[tokio::test]
    async fn a_repeated_read_after_a_mutating_call_actually_redispatches() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let read_args = serde_json::json!({ "path": "x.txt" });
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: read_args.clone(),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "x.txt", "content": "new" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same (name, arguments) as call-1 — but a write happened
                // in between, so this must actually re-run.
                CompletionEvent::ToolCallRequested {
                    id: "call-3".to_string(),
                    name: "read_file".to_string(),
                    arguments: read_args,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let read_invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::clone(
                &read_invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "read x, write x, read x again", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            read_invocations.load(Ordering::SeqCst),
            2,
            "the second read_file, after an intervening write_file, must actually re-run"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-3"))
            .expect("expected a ToolCallCompleted for call-3")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(!result.is_error, "must not be nudged: {result:?}");
                assert_eq!(result.content, "contenido");
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- D5 (docs/AUDITORIA-2026-07-v3.md): narration-without-action
    // across several turns ---

    /// The nudge that already exists (A5) only fires in response to a
    /// *tool call* being repeated — it never catches a model that just
    /// keeps narrating across turns without ever calling a tool. After
    /// `NARRATION_WITHOUT_ACTION_THRESHOLD` (2) such turns, the 3rd turn
    /// must carry an extra reminder alongside the user's own message.
    #[tokio::test]
    async fn narration_without_action_across_several_turns_injects_a_reminder() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("ok, entiendo".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("voy a hacerlo".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("dale, ahora si".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "guarda el archivo", &mut NoopObserver)
            .await
            .expect("turn 1 should succeed");
        engine
            .run_turn(&session, "hazlo ahora", &mut NoopObserver)
            .await
            .expect("turn 2 should succeed");
        engine
            .run_turn(&session, "por favor hazlo", &mut NoopObserver)
            .await
            .expect("turn 3 should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let user_texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::UserMessage { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec![
                "guarda el archivo",
                "hazlo ahora",
                "por favor hazlo",
                "[Reminder] Your last few responses described an intended action without \
                 actually calling the tool for it. If you're being asked to do something, \
                 call the appropriate tool now instead of describing or restating the plan.",
            ],
            "the reminder must appear exactly once, appended right after the 3rd turn's \
             own user message (2 prior narration-only turns crossed the threshold)"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A turn that dispatches a real tool call resets the streak to 0,
    /// even though the model still converges with a plain-text final
    /// answer *in that same turn* (a later round with no tool calls must
    /// not, on its own, count that whole turn as "narration only" — see
    /// `any_tool_calls_this_turn`'s doc comment). Sequence: 1 narration
    /// turn (streak→1), 1 turn that calls a tool then answers in text
    /// (streak must reset, not reach 2), then 2 more narration turns —
    /// without the reset, the 4th turn here would already be the 3rd
    /// *consecutive* narration-only turn and would wrongly carry the
    /// reminder.
    #[tokio::test]
    async fn a_dispatched_tool_call_resets_the_narration_streak() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let invocations = Arc::new(AtomicU32::new(0));
        let model = ScriptedModel::new(vec![
            // Turn 1: narration only (streak 0 -> 1).
            vec![
                CompletionEvent::TextDelta("ok".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 2: dispatches a tool call...
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            // ...then converges with a plain-text final answer in the
            // very same turn — must still reset the streak to 0, not
            // leave/advance it based on this last round alone.
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 3: narration only (streak 0 -> 1, thanks to turn 2's reset).
            vec![
                CompletionEvent::TextDelta("ok".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 4: narration only (streak 1 -> 2) — still below
            // threshold, so this turn must NOT carry the reminder.
            vec![
                CompletionEvent::TextDelta("ok again".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "a", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "b", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "c", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "d", &mut NoopObserver)
            .await
            .unwrap();

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "turn 2's tool call must have actually dispatched"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events.iter().any(
                |e| matches!(e, AgentEvent::UserMessage { text } if text.contains("[Reminder]"))
            ),
            "turn 2's dispatched tool call must have reset the streak — only 2 \
             consecutive narration-only turns (3, 4) followed it, below the threshold"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A7: a hallucinated tool name must not be
    /// dispatched, and the error the model sees must list the tools that
    /// actually exist so a small model has something concrete to
    /// self-correct with, instead of a bare "tool not found".
    #[tokio::test]
    async fn a_hallucinated_tool_name_is_not_dispatched_and_lists_valid_tools() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_files".to_string(), // hallucinated; only "echo" exists
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please read a file", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        // The hallucinated call never reached `invoke`.
        assert_eq!(invocations.load(Ordering::SeqCst), 0);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-1"))
            .expect("expected a ToolCallCompleted for call-1")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(result.is_error);
                assert!(result.content.contains("read_files"));
                assert!(
                    result.content.contains("echo"),
                    "expected the available tool name to be listed, got: {}",
                    result.content
                );
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A6: a turn that never converges within
    /// `MAX_TURN_ITERATIONS` must degrade gracefully — one final
    /// tools-free round asking the model to summarize — instead of
    /// failing outright with nothing to show for it.
    #[tokio::test]
    async fn a_turn_that_never_converges_gets_a_final_tools_free_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let mut rounds: Vec<Vec<CompletionEvent>> = (0..MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("attempt {i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        // One round beyond the cap: the tools-free summary attempt.
        rounds.push(vec![
            CompletionEvent::TextDelta("aqui esta lo que encontre".to_string()),
            CompletionEvent::Done,
        ]);

        let model = ScriptedModel::new(rounds);
        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully instead of erroring");

        assert_eq!(streamed, "aqui esta lo que encontre");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text == "aqui esta lo que encontre"
        )));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `Engine::with_max_turn_iterations` (v4 P0.2/mitad rondas,
    /// `Config::max_turn_iterations` / `BRAZE_MAX_TURN_ITERATIONS`):
    /// reducing the cap from the default 20 to 2 must make `run_turn`
    /// abort after exactly 2 non-converging rounds and emit the
    /// tools-free summary fallback — instead of the default cap's
    /// 20 rounds. The scripted model below has only 2 tool-call rounds
    /// + 1 fallback round, so if the cap weren't honored the engine
    /// would exhaust its script and panic; that's the failure mode the
    /// test catches by construction.
    #[tokio::test]
    async fn a_lower_max_turn_iterations_caps_the_loop_instead_of_20() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Two tool-call rounds (the new cap), then one fallback summary
        // round — exactly the three rounds a correctly-capped engine
        // needs. Were `max_turn_iterations` still hardcoded at 20, the
        // engine would keep pulling from an exhausted script.
        let rounds: Vec<Vec<CompletionEvent>> = vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-0".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "r0"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "r1"}),
                },
                CompletionEvent::Done,
            ],
            // After the cap: the tools-free summary round that degrades
            // gracefully instead of failing outright.
            vec![
                CompletionEvent::TextDelta("fallback summary".to_string()),
                CompletionEvent::Done,
            ],
        ];

        let model = ScriptedModel::new(rounds);
        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_iterations(2);

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully to a summary");

        assert_eq!(streamed, "fallback summary");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        // Two tool-call rounds produced two `AssistantToolCall`s — the
        // cap must have been exactly 2, not the default 20, or else
        //ScriptedModel would have panicked pulling from an exhausted
        // vec and this assertion point would be unreachable.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::AssistantToolCall { .. }))
                .count(),
            2,
            "the cap must stop the loop after exactly 2 tool-call rounds"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text == "fallback summary"
        )));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for U-16 (docs/usability-log-2026-07-07-si2.md):
    /// `z-ai/glm-5.2` kept emitting its native (malformed) `<tool_call>`
    /// syntax even in the tools-free summary round, which explicitly tells
    /// the model no tool is available — before the fix, that leaked block
    /// was persisted verbatim as the turn's final answer instead of being
    /// stripped like it is in every other round.
    #[tokio::test]
    async fn a_leaked_tool_call_in_the_summary_round_is_stripped_not_shown_verbatim() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let mut rounds: Vec<Vec<CompletionEvent>> = (0..MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("attempt {i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta(
                "Esto es lo que encontré.\n<tool_call>read_file<arg_key>path</arg_key>\
                 <arg_value>x.txt</arg_value></tool_call>"
                    .to_string(),
            ),
            CompletionEvent::Done,
        ]);

        let model = ScriptedModel::new(rounds);
        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully instead of erroring");

        assert!(
            !streamed.contains("<tool_call>"),
            "the leaked block must never reach the user verbatim: {streamed}"
        );
        assert_eq!(streamed, "Esto es lo que encontré.");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C4: an `AssistantToolCall` with no matching
    /// `ToolCallCompleted` anywhere in the log (what an interrupted
    /// process leaves behind — see `dispatch_tool_calls`, which persists
    /// the `tool_use` before dispatch) must be repaired with a synthetic
    /// error result on the next `load_messages`, not left dangling —
    /// otherwise Anthropic rejects every future request against this
    /// session with a permanent 400.
    #[tokio::test]
    async fn load_messages_repairs_an_orphaned_tool_use_with_no_result() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "lee el archivo".to_string(),
                },
            )
            .await
            .expect("seed user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-orphan".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "x.txt"}),
                },
            )
            .await
            .expect("seed orphaned tool_use — process 'crashed' right here");

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

        // The reconstructed history must be a valid tool_use/tool_result
        // pair, not a dangling tool_use — otherwise Anthropic's API
        // rejects the request outright.
        assert!(messages.iter().any(|m| matches!(
            &m.content[0],
            ContentBlock::ToolResult { tool_use_id, is_error, .. }
                if tool_use_id == "call-orphan" && *is_error
        )));

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let completions: Vec<_> = events
            .iter()
            .filter(
                |e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-orphan"),
            )
            .collect();
        assert_eq!(
            completions.len(),
            1,
            "expected exactly one synthetic repair to be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The repair must be idempotent: calling `load_messages` again after
    /// the first repair must not persist a second `ToolCallCompleted` for
    /// the same id (it now has one, so it's no longer an orphan).
    #[tokio::test]
    async fn repairing_an_orphaned_tool_use_twice_only_persists_one_completion() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-orphan".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "x.txt"}),
                },
            )
            .await
            .expect("seed orphaned tool_use");

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
            .expect("first load_messages should repair the orphan");
        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("second load_messages should be a no-op repair-wise");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let completions = events
            .iter()
            .filter(
                |e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-orphan"),
            )
            .count();
        assert_eq!(completions, 1, "the second call must not repair again");

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
        let threshold = effective_tactical_compaction_threshold(
            DEFAULT_TACTICAL_COMPACTION_THRESHOLD,
            None,
        );
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

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-6.
    ///
    /// Once a large tool result has settled into `durable_events` (past
    /// the compactor's tactical window), the token-budget estimate must
    /// reflect the *cleared* render actually sent to the model — a short
    /// "[tool result cleared: N chars removed...]" placeholder — not the
    /// raw, uncleared payload. Before this fix, `estimate_prompt_tokens`
    /// measured `durable_events` via `format!("{event:?}")` over the raw
    /// event, so a 5000-char tool result kept counting as ~5000 chars
    /// forever: since compacting only ever folds `tactical` (never
    /// shrinks `durable_events`), `over_token_budget` would stay `true` on
    /// every single `load_messages` call no matter how many times it
    /// compacted — a `CompactionOccurred` appended on every round,
    /// indefinitely, for content that was already small once rendered.
    #[tokio::test]
    async fn a_large_settled_tool_result_does_not_permanently_blow_the_token_budget() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        for event in [
            AgentEvent::UserMessage {
                text: "lee el archivo grande".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "big.txt" }),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "x".repeat(5_000),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "ok, lo leí".to_string(),
            },
            AgentEvent::UserMessage {
                text: "gracias".to_string(),
            },
            AgentEvent::AssistantText {
                text: "de nada".to_string(),
            },
        ] {
            store.append(&session, &event).await.expect("seed event");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            // A small window (2) settles the tool call pair into
            // `durable_events` immediately, exactly like a real session
            // long past its first ~20 events.
            Box::new(SimpleContextCompactor::new(2)),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        // Far below the 5000-char raw content, comfortably above the
        // cleared render (a short placeholder plus a handful of small
        // events) — this is the whole point: the *rendered* size is what
        // must be compared against the budget.
        .with_context_budget(100);

        for _ in 0..3 {
            engine
                .load_messages(&session, &mut NoopObserver)
                .await
                .expect("load_messages should succeed");
        }

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "the cleared render of the settled tool result is well under budget — \
             compaction must not trigger (let alone repeatedly) just because the \
             raw, already-settled payload is large"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- full_observations_byte_budget (hallazgo U-17,
    // docs/usability-log-2026-07-07-si2.md) ---

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
        assert_eq!(
            effective_tactical_compaction_threshold(40, Some(8192)),
            40
        );
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

    // --- tactical_cap_scale (I-2, docs/AUDITORIA-2026-07-v6.md): the
    // caps scale with the budget's VALUE, not its mere presence ---

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

    /// v4 P0.2 (docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3): a turn
    /// whose cumulative tokens blow the budget stops at the top of the
    /// next iteration — gracefully when the tools-free summary produces
    /// text, as here.
    #[tokio::test]
    async fn a_turn_over_its_token_budget_stops_gracefully_via_the_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let expensive_usage = || CompletionEvent::Usage {
            input_tokens: 90_000,
            output_tokens: 500,
            stop_reason: Some("tool_use".to_string()),
            cache_read_tokens: None,
            cache_write_tokens: None,
            escalation_trigger: None,
        };
        let model = ScriptedModel::new(vec![
            // Round 1: a tool call whose usage alone blows the 50k budget.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                expensive_usage(),
                CompletionEvent::Done,
            ],
            // The tools-free summary attempt (round 2 never runs as a
            // normal round — the breaker fires first).
            vec![
                CompletionEvent::TextDelta("resumen de lo hecho".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_total_tokens(Some(50_000));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected graceful summary recovery, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "resumen de lo hecho"
            )),
            "the summary text must be persisted, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "the breaker goes through the same instrumented fallback path"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Same breaker, but the summary attempt comes back empty — the turn
    /// surfaces `TurnBudgetExhausted` with the real numbers instead of
    /// pretending it converged.
    #[tokio::test]
    async fn a_turn_over_its_token_budget_with_an_empty_summary_errors_with_the_spent_count() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 90_000,
                    output_tokens: 500,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // Empty summary attempt.
            vec![CompletionEvent::Done],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_total_tokens(Some(50_000));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        match result {
            Err(EngineError::TurnBudgetExhausted {
                budget_tokens,
                spent_tokens,
            }) => {
                assert_eq!(budget_tokens, 50_000);
                assert_eq!(spent_tokens, 90_500);
            }
            other => panic!("expected TurnBudgetExhausted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `None` (the default) never trips, no matter how much a turn spends
    /// — zero behavior change for existing callers.
    #[tokio::test]
    async fn without_a_token_budget_an_expensive_turn_completes_normally() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("respuesta".to_string()),
            CompletionEvent::Usage {
                input_tokens: 5_000_000,
                output_tokens: 100,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(result.is_ok(), "got {result:?}");

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

    /// `+ablate:no-prune` (opencode ítem 2): with the collapse disabled,
    /// an old observation far beyond the full-observations window renders
    /// FULL — no "[old observation collapsed:" marker anywhere.
    #[tokio::test]
    async fn with_collapse_disabled_old_observations_render_full() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // 8 tool-call pairs (16 events) — with TACTICAL_FULL_OBSERVATIONS
        // = 5, the oldest 3 observations would normally collapse. Each
        // observation is large enough that collapsing saves space (the
        // collapse is skipped for tiny contents).
        for i in 0..8 {
            let id = format!("call-{i}");
            store
                .append(
                    &session,
                    &AgentEvent::AssistantToolCall {
                        id: id.clone(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": format!("f{i}.txt")}),
                    },
                )
                .await
                .expect("seed call");
            store
                .append(
                    &session,
                    &AgentEvent::ToolCallCompleted {
                        id: id.clone(),
                        result: braze_types::ToolResult {
                            tool_call_id: id,
                            content: format!("contenido {i}\n{}", "línea\n".repeat(100)),
                            is_error: false,
                        },
                    },
                )
                .await
                .expect("seed result");
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
        .with_observation_collapse_enabled(false);

        let messages = engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let rendered: String = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                braze_types::ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("[old observation collapsed:"),
            "collapse disabled: every observation must render full"
        );
        assert!(
            rendered.contains("contenido 0"),
            "the oldest observation's real content must be present"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
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

    /// Confirms the generic permissive schema `braze-model` sends to the
    /// model today (`{"type":"object","additionalProperties":true}`) would
    /// not itself reject arbitrary arguments if it were ever validated
    /// against — the new validation in `dispatch_tool_calls` is exactly as
    /// strict as the real resolved schema says and no stricter, it doesn't
    /// introduce false rejections for that permissive case.
    #[test]
    fn generic_permissive_schema_accepts_arbitrary_arguments() {
        let schema = serde_json::json!({"type": "object", "additionalProperties": true});
        let instance = serde_json::json!({"cualquier_cosa": 123});
        assert!(jsonschema::validate(&schema, &instance).is_ok());
    }

    // --- coerce_arguments_to_schema (hallazgo F2) ---

    fn limit_and_flag_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "integer"},
                "ratio": {"type": "number"},
                "recursive": {"type": "boolean"},
            },
        })
    }

    #[test]
    fn coerces_a_stringified_integer_to_a_number() {
        let mut args = serde_json::json!({"path": "x", "limit": "50"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["limit"], serde_json::json!(50));
    }

    #[test]
    fn coerces_a_stringified_float_to_a_number() {
        let mut args = serde_json::json!({"ratio": "0.5"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["ratio"], serde_json::json!(0.5));
    }

    #[test]
    fn coerces_stringified_booleans() {
        let mut args = serde_json::json!({"recursive": "true"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["recursive"], serde_json::json!(true));

        let mut args = serde_json::json!({"recursive": "false"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["recursive"], serde_json::json!(false));
    }

    #[test]
    fn an_unparseable_string_is_left_untouched_for_validation_to_reject() {
        let mut args = serde_json::json!({"limit": "not a number"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["limit"], serde_json::json!("not a number"));
    }

    #[test]
    fn a_json_object_is_re_serialized_to_a_string_when_the_schema_wants_one() {
        // The mirror-image mistake: `<parameter=path>{"a":1}</parameter>`
        // parses as a JSON object because the XML grammar treats any
        // value starting with `{`/`[` as structured — but the schema says
        // `path` is a string.
        let mut args = serde_json::json!({"path": {"a": 1}});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["path"], serde_json::json!(r#"{"a":1}"#));
    }

    #[test]
    fn already_correctly_typed_arguments_are_left_alone() {
        // The common case (wire-sourced calls): coercion must be a no-op.
        let mut args = serde_json::json!({
            "path": "x", "limit": 50, "ratio": 0.5, "recursive": true
        });
        let before = args.clone();
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args, before);
    }

    #[test]
    fn a_string_value_for_a_string_field_is_left_alone() {
        let mut args = serde_json::json!({"path": "src/main.rs"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["path"], serde_json::json!("src/main.rs"));
    }

    #[test]
    fn a_non_object_schema_or_arguments_is_a_no_op() {
        let mut args = serde_json::json!("not an object");
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args, serde_json::json!("not an object"));

        let mut args = serde_json::json!({"limit": "50"});
        coerce_arguments_to_schema(&mut args, &serde_json::json!({"type": "object"}));
        assert_eq!(args["limit"], serde_json::json!("50"));
    }

    // --- try_parse_textual_tool_call (hallazgo B5) ---

    #[test]
    fn parses_a_bare_json_tool_call() {
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "read_file", "arguments": {"path": "x.txt"}}"#)
                .expect("should parse");
        assert_eq!(rescued.name, "read_file");
        assert_eq!(rescued.arguments, serde_json::json!({"path": "x.txt"}));
    }

    #[test]
    fn parses_a_tool_call_fenced_in_json_code_block() {
        let text = "```json\n{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}\n```";
        let rescued = try_parse_textual_tool_call(text).expect("should parse");
        assert_eq!(rescued.name, "echo");
    }

    #[test]
    fn parses_a_tool_call_fenced_in_a_bare_code_block() {
        let text = "```\n{\"name\": \"echo\", \"arguments\": {}}\n```";
        let rescued = try_parse_textual_tool_call(text).expect("should parse");
        assert_eq!(rescued.name, "echo");
    }

    #[test]
    fn accepts_parameters_as_a_synonym_for_arguments() {
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "echo", "parameters": {"text": "hi"}}"#)
                .expect("should parse");
        assert_eq!(rescued.arguments, serde_json::json!({"text": "hi"}));
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_a_tool_call() {
        assert!(try_parse_textual_tool_call("El archivo tiene 3 lineas.").is_none());
    }

    #[test]
    fn json_without_a_name_field_is_not_a_tool_call() {
        assert!(try_parse_textual_tool_call(r#"{"arguments": {"path": "x.txt"}}"#).is_none());
    }

    #[test]
    fn non_object_arguments_are_rejected() {
        assert!(
            try_parse_textual_tool_call(r#"{"name": "echo", "arguments": "just a string"}"#)
                .is_none()
        );
    }

    // --- F1 (docs/AUDITORIA-2026-07-v3.md): reject OpenAI-style tool
    // *definitions* masquerading as a call via `parameters` ---

    #[test]
    fn an_openai_style_tool_definition_is_not_mistaken_for_a_call() {
        let text = r#"{"name": "get_weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}"#;
        assert!(
            try_parse_textual_tool_call(text).is_none(),
            "a JSON-Schema-shaped `parameters` must not be treated as arguments"
        );
    }

    #[test]
    fn a_genuine_object_typed_argument_named_type_is_still_accepted() {
        // Must not over-trigger: a real argument object that happens to
        // have a `type` field but no `properties` is not a schema.
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "set_status", "arguments": {"type": "busy"}}"#)
                .expect("should still parse — no `properties` key present");
        assert_eq!(rescued.arguments, serde_json::json!({"type": "busy"}));
    }

    // --- F1: fenced examples are not real leaked tool calls ---

    #[test]
    fn a_tagged_call_inside_a_markdown_fence_is_not_executed() {
        let text = "Así es como Qwen emite tool calls:\n```\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"/etc/shadow\"}}\n</tool_call>\n```\n";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_bare_function_xml_inside_a_markdown_fence_is_not_executed() {
        let text = "Ejemplo:\n```\n<function=read_file>\n<parameter=path>\n/etc/shadow\n</parameter>\n</function>\n```\n";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unfenced_tagged_call_after_fenced_prose_is_still_rescued() {
        // The fence-toggle logic must correctly track state across
        // multiple fences, not just detect "any fence exists somewhere".
        let text = "Aquí un ejemplo:\n```\nesto es solo texto\n```\n<tool_call>\n{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}\n</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "the real call after the fence must still rescue"
        );
    }

    #[test]
    fn each_rescued_call_gets_a_distinct_id() {
        let a = try_parse_textual_tool_call(r#"{"name": "echo", "arguments": {}}"#).unwrap();
        let b = try_parse_textual_tool_call(r#"{"name": "echo", "arguments": {}}"#).unwrap();
        assert_ne!(a.id, b.id);
    }

    // --- extract_tagged_tool_calls (formato nativo Qwen/Hermes, ítem 2
    // del backlog 2026-07-06) ---

    #[test]
    fn extracts_a_single_qwen_tagged_tool_call() {
        let text = "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"x.txt\"}}\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert!(remaining.is_empty());
    }

    #[test]
    fn extracts_several_tagged_calls_from_one_response() {
        // Qwen emits one pair of tags per call for parallel calls.
        let text = concat!(
            "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a\"}}\n</tool_call>\n",
            "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"b\"}}\n</tool_call>",
        );
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "a"}));
        assert_eq!(calls[1].arguments, serde_json::json!({"path": "b"}));
        assert!(remaining.is_empty());
        assert_ne!(calls[0].id, calls[1].id);
    }

    #[test]
    fn prose_around_a_tagged_call_is_preserved_as_the_round_text() {
        let text = "Voy a leer el archivo.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"x\"}}\n</tool_call>\nListo.";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "Voy a leer el archivo.\n\nListo.");
    }

    #[test]
    fn a_fenced_json_inside_the_tags_still_parses() {
        let text = "<tool_call>```json\n{\"name\": \"echo\", \"arguments\": {}}\n```</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn a_malformed_tagged_block_stays_in_the_text_instead_of_being_swallowed() {
        let text = "<tool_call>\n{\"name\": \"echo\", \"arguments\": no-es-json}\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_malformed_block_next_to_a_valid_one_keeps_only_the_malformed_text() {
        let text = concat!(
            "<tool_call>{broken</tool_call>",
            "<tool_call>{\"name\": \"echo\", \"arguments\": {}}</tool_call>",
        );
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "<tool_call>{broken</tool_call>");
    }

    #[test]
    fn an_unclosed_tag_rescues_nothing_and_keeps_the_text_intact() {
        // E.g. a round cut off mid-block: better visible than lost.
        let text = "algo de texto <tool_call>\n{\"name\": \"echo\"";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_valid_block_followed_by_an_unclosed_tag_keeps_the_dangling_tail() {
        let text =
            "<tool_call>{\"name\": \"echo\", \"arguments\": {}}</tool_call> y <tool_call>{\"na";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "y <tool_call>{\"na");
    }

    #[test]
    fn plain_prose_without_tags_is_not_rescued_by_the_tagged_extractor() {
        let (calls, remaining) = extract_tagged_tool_calls("El archivo tiene 3 lineas.");
        assert!(calls.is_empty());
        assert_eq!(remaining, "El archivo tiene 3 lineas.");
    }

    #[test]
    fn tagged_extraction_accepts_parameters_as_a_synonym_for_arguments() {
        let text =
            "<tool_call>{\"name\": \"echo\", \"parameters\": {\"text\": \"hi\"}}</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"text": "hi"}));
    }

    // --- gramática XML <function=...> de qwen3-coder (extensión del
    // ítem 2, destrancada 2026-07-06 al haber qwen3.5-coder en Nitro) ---

    /// The exact shape qwen3-coder's chat template documents: XML-ish
    /// tags, parameter values on their own lines, wrapped in the same
    /// `<tool_call>` tags qwen2.5 uses around JSON.
    #[test]
    fn function_xml_inside_tool_call_wrapper_parses() {
        let text = "<tool_call>\n<function=read_file>\n<parameter=path>\nx.txt\n</parameter>\n</function>\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert!(remaining.is_empty());
    }

    #[test]
    fn bare_function_xml_with_prose_around_parses_and_preserves_the_prose() {
        let text = "Voy a leerlo.\n<function=read_file>\n<parameter=path>\nx.txt\n</parameter>\n</function>\ndespués te cuento";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert_eq!(remaining, "Voy a leerlo.\n\ndespués te cuento");
    }

    #[test]
    fn function_xml_with_several_parameters_collects_them_all() {
        let text = "<function=edit_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n<parameter=old_string>\nlet x = 1;\n</parameter>\n<parameter=new_string>\nlet x = 2;\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(call.name, "edit_file");
        assert_eq!(
            call.arguments,
            serde_json::json!({
                "path": "src/main.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;",
            })
        );
    }

    /// The whole point of the XML grammar: code-carrying values need no
    /// JSON escaping — inner quotes/braces arrive verbatim as a string.
    #[test]
    fn function_xml_keeps_code_carrying_values_as_verbatim_strings() {
        let text = "<function=write_file>\n<parameter=path>\na.json\n</parameter>\n<parameter=content>\nfn main() { println!(\"{:?}\", vec![1]); }\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(
            call.arguments["content"],
            serde_json::json!("fn main() { println!(\"{:?}\", vec![1]); }")
        );
    }

    /// Scalar-looking values stay strings ("42" must not become 42 —
    /// a `path: String` schema downstream would reject the number),
    /// while a clearly structured value (`{...}`) is parsed.
    #[test]
    fn function_xml_coerces_only_clearly_structured_values() {
        let text = "<function=echo>\n<parameter=text>\n42\n</parameter>\n<parameter=options>\n{\"deep\": true}\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(call.arguments["text"], serde_json::json!("42"));
        assert_eq!(call.arguments["options"], serde_json::json!({"deep": true}));
    }

    #[test]
    fn malformed_function_xml_stays_in_the_text() {
        // Missing </parameter> close.
        let text = "<function=echo>\n<parameter=text>\nhola\n</function>";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn function_xml_without_parameters_is_a_zero_argument_call() {
        let call =
            parse_function_xml_tool_call("<function=list_tools>\n</function>").expect("parses");
        assert_eq!(call.name, "list_tools");
        assert_eq!(call.arguments, serde_json::json!({}));
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_function_xml() {
        let (calls, remaining) =
            extract_function_xml_tool_calls("la función f(x) = x + 1 es creciente");
        assert!(calls.is_empty());
        assert_eq!(remaining, "la función f(x) = x + 1 es creciente");
    }

    // --- gramática <arg_key>/<arg_value> de z-ai/glm-5.2 (hallazgo U-15,
    // docs/usability-log-2026-07-07-si2.md — observada 2026-07-07 vía
    // OpenRouter) ---

    /// The exact shape observed leaking from `z-ai/glm-5.2`: no
    /// `<function=...>` wrapper, just the bare name followed by
    /// `<arg_key>`/`<arg_value>` pairs, all inside the same `<tool_call>`
    /// tags qwen2.5/qwen3-coder use.
    #[test]
    fn glm_arg_tags_inside_tool_call_wrapper_parses() {
        let text = "<tool_call>read_file<arg_key>limit</arg_key><arg_value>120</arg_value><arg_key>offset</arg_key><arg_value>63</arg_value><arg_key>path</arg_key><arg_value>crates/braze-bench/src/backend_spec.rs</arg_value></tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({
                "limit": "120",
                "offset": "63",
                "path": "crates/braze-bench/src/backend_spec.rs",
            })
        );
        assert!(remaining.is_empty());
    }

    #[test]
    fn glm_arg_tags_with_prose_around_are_preserved() {
        let text = "Voy a leerlo.\n<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>\ndespués te cuento";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert_eq!(remaining, "Voy a leerlo.\n\ndespués te cuento");
    }

    /// Scalar-looking values stay strings, same rule as the qwen3-coder
    /// XML rescue — a `path: String` schema downstream must not receive a
    /// JSON number just because the raw text looked numeric.
    #[test]
    fn glm_arg_tags_coerce_only_clearly_structured_values() {
        let text = "<tool_call>echo<arg_key>text</arg_key><arg_value>42</arg_value><arg_key>options</arg_key><arg_value>{\"deep\": true}</arg_value></tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("42"));
        assert_eq!(calls[0].arguments["options"], serde_json::json!({"deep": true}));
    }

    #[test]
    fn glm_arg_tags_without_any_arg_key_are_not_mistaken_for_the_grammar() {
        // No `<arg_key>` at all: indistinguishable from prose that merely
        // mentions a tool by name — must fall through unrescued rather
        // than being guessed at as a zero-argument call.
        assert!(parse_glm_arg_tag_tool_call("read_file").is_none());
    }

    #[test]
    fn malformed_glm_arg_tags_stay_in_the_text() {
        // Missing the closing </arg_value>.
        let text = "<tool_call>echo<arg_key>text</arg_key><arg_value>hola</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    // --- extract_pythonic_tool_calls (hallazgo C2, Llama's native format) ---

    #[test]
    fn parses_a_single_pythonic_call() {
        let (calls, remaining) =
            extract_pythonic_tool_calls(r#"[get_weather(city="SF", metric="celsius")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"city": "SF", "metric": "celsius"})
        );
        assert_eq!(remaining, "");
    }

    #[test]
    fn pythonic_call_preserves_surrounding_prose() {
        let text = r#"Claro, reviso el clima.[get_weather(city="SF")]Listo."#;
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "Claro, reviso el clima.Listo.");
    }

    #[test]
    fn pythonic_call_parses_numbers_and_booleans() {
        let (calls, _) =
            extract_pythonic_tool_calls("[read_file(path=\"a.txt\", offset=5, recursive=true)]");
        assert_eq!(calls[0].arguments["offset"], serde_json::json!(5));
        assert_eq!(calls[0].arguments["recursive"], serde_json::json!(true));
    }

    #[test]
    fn pythonic_call_parses_floats() {
        let (calls, _) = extract_pythonic_tool_calls("[set_ratio(value=0.5)]");
        assert_eq!(calls[0].arguments["value"], serde_json::json!(0.5));
    }

    #[test]
    fn several_pythonic_calls_in_one_bracket_are_all_parsed() {
        let (calls, remaining) = extract_pythonic_tool_calls(r#"[echo(text="a"), echo(text="b")]"#);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a"));
        assert_eq!(calls[1].arguments["text"], serde_json::json!("b"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn pythonic_call_without_arguments_is_a_zero_argument_call() {
        let (calls, _) = extract_pythonic_tool_calls("[list_tools()]");
        assert_eq!(calls[0].name, "list_tools");
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn a_comma_inside_a_quoted_argument_does_not_split_the_call() {
        let (calls, _) = extract_pythonic_tool_calls(r#"[echo(text="a, b, c")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a, b, c"));
    }

    #[test]
    fn a_bracket_inside_a_quoted_argument_does_not_close_the_block_early() {
        let (calls, remaining) = extract_pythonic_tool_calls(r#"[echo(text="a] b")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a] b"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_a_pythonic_call() {
        // No literal brackets around the call — must not match (this is
        // exactly the ambiguity the bracket-wrapper requirement avoids).
        let text = "puedes llamar a leer(archivo) para revisar el contenido";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_ordinary_list_literal_is_not_mistaken_for_a_call() {
        // `[1, 2, 3]` has no `identifier(` right after the `[`.
        let text = "los valores son [1, 2, 3] en ese orden";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unrecognized_argument_shape_leaves_the_whole_block_in_the_text() {
        // A nested list value isn't in scope (string/number/bool only) —
        // the whole call must be left untouched, not partially rescued.
        let text = "[echo(items=[1, 2])]";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unclosed_pythonic_bracket_stays_in_the_text() {
        let text = "[get_weather(city=\"SF\"";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    // --- strip_leaked_tool_call_shapes (hallazgo U-16,
    // docs/usability-log-2026-07-07-si2.md: attempt_tools_free_summary_round
    // had no rescue logic at all, so a leaked tool-call block there used
    // to get persisted verbatim as if it were the model's real answer) ---

    #[test]
    fn a_leaked_tagged_call_with_no_other_text_strips_to_empty() {
        let text = "<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>";
        assert_eq!(strip_leaked_tool_call_shapes(text), "");
    }

    #[test]
    fn a_leaked_call_alongside_real_prose_keeps_only_the_prose() {
        let text = "Basado en lo que leí hasta ahora, el fix consiste en...\n<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>";
        assert_eq!(
            strip_leaked_tool_call_shapes(text),
            "Basado en lo que leí hasta ahora, el fix consiste en..."
        );
    }

    #[test]
    fn plain_prose_with_no_leaked_call_is_returned_unchanged() {
        let text = "No hay nada raro acá, solo texto normal.";
        assert_eq!(strip_leaked_tool_call_shapes(text), text);
    }

    /// Regression test for the rescue escalera's ordering: a `<tool_call>`
    /// tagged block must win even if the response also happens to contain
    /// bracketed text that looks pythonic-shaped elsewhere.
    #[tokio::test]
    async fn a_llama_pythonic_call_is_rescued_end_to_end_when_no_structured_call_arrives() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Voy a revisar.[echo(text=\"hi\")]".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the pythonic call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Voy a revisar.")
            )),
            "the surrounding prose must be persisted as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("echo(")
            )),
            "the bracketed call must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- synthesize_orphan_repairs (N-26, docs/AUDITORIA-2026-07-v2.md) ---

    #[test]
    fn synthesize_orphan_repairs_finds_a_tool_use_with_no_matching_result() {
        let events = vec![
            AgentEvent::UserMessage {
                text: "please echo hi".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": "hi" }),
            },
        ];

        let repairs = synthesize_orphan_repairs(&events);
        assert_eq!(repairs.len(), 1);
        match &repairs[0] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_orphan_repairs_is_a_no_op_when_every_call_already_has_a_result() {
        let events = vec![
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "echoed: hi".to_string(),
                    is_error: false,
                },
            },
        ];

        assert!(synthesize_orphan_repairs(&events).is_empty());
    }

    /// Regression test for B5: a model that emits the tool call as plain
    /// text (no structured `tool_calls` entry — the failure mode for
    /// small/local models or templates without native tool-call support)
    /// must still have the tool actually run, and the raw JSON must not
    /// be persisted as if it were a normal conversational reply.
    #[tokio::test]
    async fn a_tool_call_emitted_as_plain_text_is_rescued_and_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the rescued call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the raw JSON must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Ítem 2 del backlog (2026-07-06): a model emitting its native
    /// Qwen/Hermes `<tool_call>{json}</tool_call>` tagged format — with
    /// prose around it — must have the call dispatched, the prose kept
    /// as the round's text, and the tags/JSON stripped from what's
    /// persisted as conversation.
    #[tokio::test]
    async fn a_qwen_tagged_tool_call_with_surrounding_prose_is_rescued_and_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Voy a usar echo.\n<tool_call>\n".to_string()),
                CompletionEvent::TextDelta(
                    r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
                ),
                CompletionEvent::TextDelta("\n</tool_call>".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the tagged call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        // The prose survives as round text; the tags and JSON don't.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Voy a usar echo.")
            )),
            "the surrounding prose must be persisted as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("<tool_call>") || text.contains("\"name\"")
            )),
            "the tagged block must not be persisted as conversational text"
        );
        // H-3 (docs/AUDITORIA-2026-07-v5.md): the rescue actually
        // happening was already visible via `tracing::info!` before this
        // — this pins that it's also persisted, bench-countable.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::TextualRescueApplied { parser } if parser.contains("Qwen/Hermes")
            )),
            "a rescued <tool_call> block must persist TextualRescueApplied naming that rung"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F2 (docs/AUDITORIA-2026-07-v3.md): qwen3-coder's bare `<function=>`
    /// XML grammar has no native number type, so a `limit: integer`
    /// parameter comes back from the rescue as the string `"5"` — without
    /// schema-guided coercion this fails validation deterministically
    /// (every call to a tool with a numeric param, rescued via this
    /// format, would burn a repair round it can't even fix, since the
    /// XML grammar has no way to emit a JSON number). With the fix, the
    /// call dispatches on the first attempt and the tool receives a real
    /// JSON number.
    #[tokio::test]
    async fn qwen3_coder_xml_with_a_stringified_integer_param_gets_coerced_before_dispatch() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    "<function=echo_limit>\n<parameter=text>\nhi\n</parameter>\n\
                     <parameter=limit>\n5\n</parameter>\n</function>"
                        .to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let received_limit = Arc::new(std::sync::Mutex::new(None));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoWithLimitToolProvider::new(
                Arc::clone(&invocations),
                Arc::clone(&received_limit),
            ))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi with limit 5", &mut NoopObserver)
            .await
            .expect("turn should succeed — coercion must let validation pass on the first try");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "must dispatch exactly once — no schema-repair retry round needed"
        );
        assert_eq!(
            received_limit.lock().unwrap().clone(),
            Some(serde_json::json!(5)),
            "the tool must receive a real JSON number, not the string \"5\""
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-15 (docs/AUDITORIA-2026-07-v2.md):
    /// `with_textual_rescue_enabled(false)` must stop the rescue from
    /// dispatching a real tool a user only asked to see the JSON for —
    /// the raw text is persisted as ordinary conversational text instead.
    #[tokio::test]
    async fn textual_rescue_can_be_disabled() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(
                r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
            ),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_textual_rescue_enabled(false);

        engine
            .run_turn(
                &session,
                "muéstrame el JSON para invocar echo",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the tool must never actually be invoked when the rescue is disabled"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the raw JSON must be persisted as ordinary text instead of dispatched"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- técnica G10: best-of-n / test-time scaling (docs/AUDITORIA-2026-07.md) ---

    /// Regression test for G10's core value proposition: a 2-vote
    /// majority ("hi") beats a 1-vote dissenter ("wrong") among 3
    /// candidates, and only the winning call is ever dispatched.
    #[tokio::test]
    async fn best_of_n_dispatches_the_majority_tool_call_signature() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-a".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-b".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "wrong" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-c".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(3);

        engine
            .run_turn(
                &session,
                "please echo hi (with a dissenting distractor)",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "only the winning candidate's call should ever reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        {
            Some(AgentEvent::ToolCallCompleted { result, .. }) => {
                assert_eq!(
                    result.content, "echoed: hi",
                    "the 2-vote majority ('hi') must win over the 1-vote dissenter ('wrong')"
                );
            }
            other => panic!("expected a ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A 1-vs-1 tie must resolve deterministically to the
    /// earliest-generated candidate — never `Iterator::max_by_key`'s
    /// "last wins" default, which would make the outcome depend on
    /// implementation details of the vote-counting loop.
    #[tokio::test]
    async fn best_of_n_breaks_ties_by_keeping_the_earliest_candidate() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-a".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "first" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-b".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "second" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(2);

        engine
            .run_turn(&session, "please echo something", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        {
            Some(AgentEvent::ToolCallCompleted { result, .. }) => {
                assert_eq!(
                    result.content, "echoed: first",
                    "a 1-vs-1 tie must keep the earliest-generated candidate"
                );
            }
            other => panic!("expected a ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `self.best_of_n` real model calls happen per round — the
    /// persisted `Usage` must reflect the *summed* cost across every
    /// candidate, not just the winner's, or token/cost accounting
    /// silently under-reports by every discarded candidate's share.
    /// `stop_reason` is taken from the winning candidate specifically.
    #[tokio::test]
    async fn best_of_n_sums_usage_across_candidates_and_keeps_the_winners_stop_reason() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Both candidates answer with the same plain text (no tool call
        // — same "no tool call" signature, so it's a 1-vs-1 tie and
        // candidate 0 wins per the tie-break rule tested above).
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    stop_reason: Some("end_turn".to_string()),
                    // Deliberately mismatched Some/None across candidates
                    // — exercises `sum_optional_u32`'s "at least one
                    // candidate reported it" rule, not just the trivial
                    // both-Some or both-None cases.
                    cache_read_tokens: Some(6),
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 20,
                    output_tokens: 8,
                    stop_reason: Some("stop_sequence".to_string()),
                    cache_read_tokens: Some(4),
                    cache_write_tokens: Some(2),
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(2);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::Usage { .. }))
        {
            Some(AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            }) => {
                assert_eq!(
                    *input_tokens, 30,
                    "usage must sum every candidate's cost, not just the winner's"
                );
                assert_eq!(*output_tokens, 13);
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("end_turn"),
                    "stop_reason must reflect the winning candidate specifically"
                );
                assert_eq!(
                    *cache_read_tokens,
                    Some(10),
                    "cache_read_tokens must sum across candidates like input/output do"
                );
                assert_eq!(
                    *cache_write_tokens,
                    Some(2),
                    "one candidate's None must not zero out the other's reported value"
                );
            }
            other => panic!("expected a Usage event, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `best_of_n: 0` (e.g. from a misconfigured env var) must degrade
    /// gracefully to the same single-call path as the default (`1`),
    /// not panic on an empty candidate vec.
    #[tokio::test]
    async fn best_of_n_set_to_zero_behaves_like_disabled_not_a_panic() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(0);

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk: &str| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "hola");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Deltas from individual best-of-n candidates never reach the
    /// observer live (there's no single "the" answer to show until the
    /// vote resolves one) — only the winner's full text arrives, as one
    /// delta, right after voting.
    #[tokio::test]
    async fn best_of_n_suppresses_live_deltas_but_delivers_the_winners_full_text_once() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("respuesta ".to_string()),
                CompletionEvent::TextDelta("candidata".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("otra ".to_string()),
                CompletionEvent::TextDelta("respuesta".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(2);

        let mut observer = RecordingObserver {
            deltas: Vec::new(),
            events: Vec::new(),
        };
        engine
            .run_turn(&session, "hola", &mut observer)
            .await
            .expect("turn should succeed");

        // Neither candidate's individual deltas streamed live — exactly
        // one delta arrives, carrying the (tied, so earliest-kept)
        // winner's whole text in one shot.
        assert_eq!(observer.deltas, vec!["respuesta candidata".to_string()]);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
