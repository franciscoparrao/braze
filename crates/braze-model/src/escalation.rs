//! [`EscalatingBackend`] — reactive lead/worker escalation as a
//! [`ModelBackend`] decorator (ítem 6 del backlog 2026-07-06, préstamo
//! de Goose's `GOOSE_LEAD_MODEL`, docs/SOTA-2026-07.md § Goose).
//!
//! Two backends wrapped as one: the **lead** (stronger/costlier) handles
//! the first `lead_turns` calls of a session — the framing/planning
//! moves where capability matters most — then the **worker**
//! (cheaper/faster, typically a small local model) takes over. When the
//! worker visibly flounders — the request's message history ends in
//! `failure_threshold`+ consecutive failed tool observations — the lead
//! is brought back for `escalation_turns` calls, then the worker
//! resumes. braze's planner/executor split changes models *proactively*
//! (by phase); this decorator changes them *reactively* (by observed
//! failure) — the SOTA review's point is that the two compose.
//!
//! Failure detection is stateless per request: it re-reads the trailing
//! run of failed observations from `CompletionRequest::messages` on
//! every call, so retries/best-of-n candidates (which re-send the same
//! history) can't double-count, and a fresh user message naturally
//! resets the streak. Only the turn *counter* and the remaining
//! escalation budget live in decorator state.

use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::{Stream, StreamExt};

use braze_types::{ContentBlock, Message, Role};

use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;

/// Goose's `GOOSE_LEAD_TURNS` default: the lead opens the session.
const DEFAULT_LEAD_TURNS: usize = 3;
/// Consecutive failed observations before escalating back to the lead.
const DEFAULT_FAILURE_THRESHOLD: usize = 2;
/// How many calls the lead handles per escalation before the worker
/// resumes.
const DEFAULT_ESCALATION_TURNS: usize = 3;

/// See the module doc comment. Construct with
/// [`EscalatingBackend::new`], tune with the `with_*` builders.
pub struct EscalatingBackend {
    lead: Box<dyn ModelBackend>,
    worker: Box<dyn ModelBackend>,
    lead_turns: usize,
    failure_threshold: usize,
    escalation_turns: usize,
    /// Precomputed so `name()` can return `&str`.
    name: String,
    state: Mutex<EscalationState>,
}

#[derive(Default)]
struct EscalationState {
    /// Total *rounds* routed so far — the first `lead_turns` go to the
    /// lead unconditionally. Not "total `complete` calls": see
    /// `last_round_message_count`/`last_decision` below (hallazgo D4).
    calls: usize,
    /// Remaining calls of an active escalation (0 = not escalated).
    escalated_remaining: usize,
    /// Fingerprint of `req.messages` the last time `route` advanced the
    /// counters above — D4 (docs/AUDITORIA-2026-07-v3.md): best-of-n
    /// (`Engine::complete_with_best_of_n`) calls `complete` N times per
    /// round with an *identical* request (all N candidates answer the
    /// same turn). Without this, each candidate consumed its own
    /// `lead_turns`/`escalated_remaining` slot — "the lead opens the
    /// session" could exhaust itself inside a single round of voting, and
    /// the vote ended up comparing candidates from different models as
    /// if they were interchangeable.
    ///
    /// A content fingerprint, NOT `messages.len()` (J-1,
    /// docs/AUDITORIA-2026-07-v7.md): under budget-driven compaction the
    /// rendered history no longer grows monotonically within a turn — it
    /// folds to `[summary] + tail`, and two consecutive rounds with the
    /// same event shape (one tool call per round) render to the SAME
    /// message count. With a bare count, `route` replayed the previous
    /// round's decision forever: a stale `Worker` decision meant the
    /// failure streak was never re-evaluated (escalation could never
    /// fire exactly in the floundering turns it exists for), and a stale
    /// `LeadEscalating` re-stamped its trigger every round (inflating
    /// `leader_escalations` and never consuming the escalation window).
    /// Hashing the message *contents* keeps the best-of-n dedup (the N
    /// candidate requests are byte-identical) while telling genuinely
    /// different rounds apart regardless of shape.
    last_round_fingerprint: Option<u64>,
    /// The decision made the last time `route` actually advanced the
    /// counters — replayed for every later call in the same round
    /// (detected via `last_round_fingerprint`) instead of routing them
    /// independently.
    last_decision: Option<RouteDecision>,
}

impl EscalatingBackend {
    pub fn new(lead: Box<dyn ModelBackend>, worker: Box<dyn ModelBackend>) -> Self {
        let name = format!("escalating({}->{})", lead.name(), worker.name());
        Self {
            lead,
            worker,
            lead_turns: DEFAULT_LEAD_TURNS,
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            escalation_turns: DEFAULT_ESCALATION_TURNS,
            name,
            state: Mutex::new(EscalationState::default()),
        }
    }

    /// How many initial calls the lead handles (0 = worker from the
    /// start, purely reactive).
    pub fn with_lead_turns(mut self, lead_turns: usize) -> Self {
        self.lead_turns = lead_turns;
        self
    }

    /// Consecutive failed observations that trigger an escalation. A
    /// value of 0 would escalate on every call — clamped to 1.
    pub fn with_failure_threshold(mut self, failure_threshold: usize) -> Self {
        self.failure_threshold = failure_threshold.max(1);
        self
    }

    /// Calls the lead handles per escalation before the worker resumes.
    pub fn with_escalation_turns(mut self, escalation_turns: usize) -> Self {
        self.escalation_turns = escalation_turns.max(1);
        self
    }

    /// Applies the three escalation knobs from optional config values in
    /// one call — `None` keeps the decorator's own default (the constants
    /// at the top of this module). The single seam both composition roots
    /// (`braze-cli::build_engine`, `braze-bench::BackendSpec::
    /// build_agent_model`) share, so neither can silently forget one knob
    /// — before this existed, NO caller applied any of them, and every
    /// `+lead:` A/B ran with the proactive 3-turn opening instead of the
    /// reactive escalation it claimed to measure (I-1,
    /// docs/AUDITORIA-2026-07-v6.md, confirmed live: error_recovery
    /// 0/3→3/3 with `leader_escalations = 0`).
    pub fn with_configured_knobs(
        mut self,
        lead_turns: Option<usize>,
        failure_threshold: Option<usize>,
        escalation_turns: Option<usize>,
    ) -> Self {
        if let Some(n) = lead_turns {
            self = self.with_lead_turns(n);
        }
        if let Some(n) = failure_threshold {
            self = self.with_failure_threshold(n);
        }
        if let Some(n) = escalation_turns {
            self = self.with_escalation_turns(n);
        }
        self
    }

    /// The configured opening-window size — observability for
    /// composition-root wiring tests (I-1): the knobs are internal
    /// routing state, and without getters neither `braze-cli` nor
    /// `braze-bench` can assert their config plumbing actually reached
    /// the decorator.
    pub fn lead_turns(&self) -> usize {
        self.lead_turns
    }

    /// The configured failure threshold — see [`Self::lead_turns`].
    pub fn failure_threshold(&self) -> usize {
        self.failure_threshold
    }

    /// The configured escalation-window size — see [`Self::lead_turns`].
    pub fn escalation_turns(&self) -> usize {
        self.escalation_turns
    }

    /// Picks the backend for this call and updates the counters — split
    /// from `complete` so the routing decision is directly testable
    /// without streaming machinery.
    fn route(&self, req: &CompletionRequest) -> RouteDecision {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // D4: another best-of-n candidate for the round just routed —
        // reuse that round's decision instead of advancing the counters
        // again for it. Detected by content fingerprint, not message
        // count (J-1): see `last_round_fingerprint`'s doc comment.
        let this_round_fingerprint = request_fingerprint(&req.messages);
        if state.last_round_fingerprint == Some(this_round_fingerprint)
            && let Some(decision) = state.last_decision
        {
            return decision;
        }
        state.last_round_fingerprint = Some(this_round_fingerprint);

        state.calls += 1;

        let decision = if state.calls <= self.lead_turns {
            RouteDecision::LeadOpening
        } else if state.escalated_remaining > 0 {
            state.escalated_remaining -= 1;
            RouteDecision::LeadEscalated
        } else if trailing_failed_observations(&req.messages) >= self.failure_threshold {
            // This round plus (escalation_turns - 1) further ones.
            state.escalated_remaining = self.escalation_turns - 1;
            RouteDecision::LeadEscalating
        } else {
            RouteDecision::Worker
        };
        state.last_decision = Some(decision);
        decision
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum RouteDecision {
    /// One of the first `lead_turns` calls.
    LeadOpening,
    /// The call that *triggers* an escalation.
    LeadEscalating,
    /// A call inside an already-active escalation window.
    LeadEscalated,
    Worker,
}

impl RouteDecision {
    fn is_lead(self) -> bool {
        !matches!(self, RouteDecision::Worker)
    }
}

#[async_trait]
impl ModelBackend for EscalatingBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let decision = self.route(&req);
        // H-3 (docs/AUDITORIA-2026-07-v5.md): only the round that *triggers*
        // the escalation window gets a `trigger` — rounds already inside an
        // active window (`LeadEscalated`) reuse it silently, same as they
        // already reuse the routing decision itself (D4 above). This is
        // what makes counting `AgentEvent::EscalationToLead` downstream
        // count escalation *episodes*, not raw lead-model calls.
        let trigger = if decision == RouteDecision::LeadEscalating {
            let n = trailing_failed_observations(&req.messages);
            tracing::info!(
                threshold = self.failure_threshold,
                escalation_turns = self.escalation_turns,
                "worker flounders (consecutive failed observations) — escalating to the lead model"
            );
            Some(format!(
                "{n} consecutive failed observations (threshold {})",
                self.failure_threshold
            ))
        } else {
            None
        };

        let stream = if decision.is_lead() {
            self.lead.complete(req).await?
        } else {
            self.worker.complete(req).await?
        };

        // Only the triggering round pays for the wrapper — every other
        // round (the overwhelming majority) returns the inner stream
        // untouched.
        let Some(trigger) = trigger else {
            return Ok(stream);
        };
        Ok(Box::pin(stream.map(move |item| {
            item.map(|event| match event {
                CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                    cache_read_tokens,
                    cache_write_tokens,
                    ..
                } => CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                    cache_read_tokens,
                    cache_write_tokens,
                    escalation_trigger: Some(trigger.clone()),
                },
                other => other,
            })
        })))
    }
}

/// Order-insensitive-to-nothing content hash of a request's messages —
/// the round-identity signal `route`'s D4 dedup keys on (see
/// `EscalationState::last_round_fingerprint`). Hashes every block's
/// discriminant and payload, so any appended message, any compaction
/// fold, and any cleared/collapsed observation produces a different
/// fingerprint, while best-of-n's byte-identical candidate requests
/// collide exactly as intended.
fn request_fingerprint(messages: &[Message]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    messages.len().hash(&mut hasher);
    for message in messages {
        std::mem::discriminant(&message.role).hash(&mut hasher);
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    0u8.hash(&mut hasher);
                    text.hash(&mut hasher);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    1u8.hash(&mut hasher);
                    id.hash(&mut hasher);
                    name.hash(&mut hasher);
                    input.to_string().hash(&mut hasher);
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    2u8.hash(&mut hasher);
                    tool_use_id.hash(&mut hasher);
                    content.hash(&mut hasher);
                    is_error.hash(&mut hasher);
                }
            }
        }
    }
    hasher.finish()
}

/// User-role Text messages the harness itself injects into the rendered
/// history — NOT the human speaking. J-2 (docs/AUDITORIA-2026-07-v7.md):
/// `trailing_failed_observations` must skip these rather than treat them
/// as episode boundaries, because they arrive exactly when the streak
/// matters — the task-list summary rides as a trailing user message on
/// EVERY round while tasks are open, and harness notes fire when the
/// turn is degrading (budget/iteration caps). Breaking on them zeroed
/// the streak permanently: in a `+lead:`+task-list arm the reactive
/// escalation was dead by construction. Acoplamiento por convención
/// (same as `[tool result cleared:` above and the post-edit marker):
/// these prefixes are owned by `braze-engine`'s render layer.
fn is_harness_injected_user_text(message: &Message) -> bool {
    const HARNESS_TEXT_PREFIXES: &[&str] = &[
        // `history.rs` render of `AgentEvent::HarnessNote`.
        "[harness] ",
        // The ephemeral task-list summary (`task_list.rs::summary_line`).
        "Task list: ",
        // `history.rs` render of `AgentEvent::PlanCreated` (user role).
        "Plan for this request",
        // The durable-summary placeholder prepended after compaction.
        "[Resumen de contexto previo]",
    ];
    message.content.iter().any(|block| {
        matches!(block, ContentBlock::Text { text }
            if HARNESS_TEXT_PREFIXES.iter().any(|prefix| text.starts_with(prefix)))
    })
}

/// Counts the *trailing* run of failed observations in `messages`: how
/// many of the most recent tool-result messages (User messages carrying
/// `ToolResult` blocks) contain at least one observation
/// [`observation_is_a_failure`] treats as a failure, walking backwards.
/// The scan skips Assistant messages (the tool_use/plan/text between
/// observations) and harness-injected user text (J-2, see
/// [`is_harness_injected_user_text`]), and stops at the first clean
/// observation or at a real user Text message — either one means the
/// worker isn't in a failure streak *right now*.
fn trailing_failed_observations(messages: &[Message]) -> usize {
    let mut failures = 0;
    for message in messages.iter().rev() {
        if message.role != Role::User {
            continue;
        }
        let has_tool_results = message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }));
        if !has_tool_results {
            if is_harness_injected_user_text(message) {
                // The harness talking to the model, not the human: the
                // streak continues through it (J-2).
                continue;
            }
            // A real user message: whatever failed before it is a
            // previous episode, not this streak.
            break;
        }
        let any_failure = message.content.iter().any(observation_is_a_failure);
        if !any_failure {
            break;
        }
        failures += 1;
    }
    failures
}

/// Marker `braze-tools-local`'s post-edit guardrail (`post_edit_check.rs`)
/// prepends to its feedback — deliberately `is_error: false` (the edit
/// itself was applied successfully; the feedback is the *next* problem
/// to fix). Acoplamiento por convención, not a crate dependency:
/// `braze-model` has no reason to depend on `braze-tools-local` just to
/// share one string constant.
const POST_EDIT_CHECK_FAILURE_MARKER: &str = "[post-edit check]";

/// `true` for a `ContentBlock::ToolResult` [`trailing_failed_observations`]
/// should count toward the escalation streak. Two adjustments on top of
/// the raw `is_error` flag:
///
/// - **F3** (docs/AUDITORIA-2026-07-v3.md): a post-edit-check regression
///   is deliberately persisted with `is_error: false` (the edit itself
///   applied) — without this, the two palancas cancel each other out
///   exactly in the edit-flounder scenario the harness exists to help
///   with: a worker whose edits keep applying but keep breaking the
///   build never trips the streak, so the lead never comes back.
/// - **D3** (docs/AUDITORIA-2026-07-v3.md): a `ToolResult` shaped like an
///   environment/state fact rather than a signal about the model's own
///   reasoning — the two shapes named in the audit ("exit codes,
///   not-found") — doesn't count. Counting these penalizes a worker
///   legitimately exploring (a file that turns out not to exist, a shell
///   command that exits non-zero as its normal protocol) with an
///   expensive lead-model escalation unrelated to its capability.
///   Deliberately narrow: everything else (schema validation, unknown
///   tool, the repeated-call nudge, `edit_file`'s ambiguous/not-found
///   matching, MCP tool-level errors) still counts as before — a
///   broader taxonomy risks *under*-escalating for genuine floundering
///   without the `ToolResult`-level cause field the "proper" fix would
///   need.
fn observation_is_a_failure(block: &ContentBlock) -> bool {
    let ContentBlock::ToolResult {
        content, is_error, ..
    } = block
    else {
        return false;
    };
    // I-3 (docs/AUDITORIA-2026-07-v6.md): the durable-clearing render
    // (`braze-engine::history::event_to_block_cleared` — acoplamiento por
    // convención, same as the post-edit marker below) replaces an old
    // result's content with this placeholder while PRESERVING `is_error`.
    // The classification signals this function refines on (the
    // environment-signal shapes, the post-edit marker) are gone with the
    // content, so an old `exit_code`-style state fact would count as a
    // model failure purely because its refinement got cleared. A cleared
    // result is settled old history, not the current streak — never a
    // failure.
    if content.starts_with("[tool result cleared:") {
        return false;
    }
    if content.contains(POST_EDIT_CHECK_FAILURE_MARKER) {
        return true;
    }
    *is_error && !is_environment_signal(content)
}

/// `true` when `content` is one of the two environment/state-fact shapes
/// named by hallazgo D3 — see [`observation_is_a_failure`].
fn is_environment_signal(content: &str) -> bool {
    // `shell_exec`'s recoverable-failure summary is a JSON object
    // carrying `exit_code` — a command failing with a non-zero exit
    // status is often the command's normal way of reporting a state fact
    // ("no match", "assertion false"), not a sign the model constructed
    // the command wrong.
    content.contains("\"exit_code\"")
        // `read_file`/`write_file`'s I/O failure messages
        // (`provider.rs`/`read_file.rs`/`write_file.rs`): "failed to
        // read/write '<path>': <os error>" — a path that turns out not
        // to exist (or a missing parent directory) is a fact about the
        // filesystem, discovered by legitimate exploration.
        || content.starts_with("failed to read '")
        || content.starts_with("failed to write '")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::stream;

    /// Minimal fake backend: counts its `complete` calls and streams a
    /// fixed label — enough to observe routing without any HTTP.
    struct CountingBackend {
        label: &'static str,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelBackend for CountingBackend {
        fn name(&self) -> &str {
            self.label
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(stream::iter(vec![Ok(CompletionEvent::Done)])))
        }
    }

    fn harness(
        lead_turns: usize,
        threshold: usize,
        escalation_turns: usize,
    ) -> (EscalatingBackend, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let lead_calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::new(AtomicUsize::new(0));
        let backend = EscalatingBackend::new(
            Box::new(CountingBackend {
                label: "lead",
                calls: Arc::clone(&lead_calls),
            }),
            Box::new(CountingBackend {
                label: "worker",
                calls: Arc::clone(&worker_calls),
            }),
        )
        .with_lead_turns(lead_turns)
        .with_failure_threshold(threshold)
        .with_escalation_turns(escalation_turns);
        (backend, lead_calls, worker_calls)
    }

    /// Fake backend that actually emits a `Usage` event before `Done` —
    /// `CountingBackend` above deliberately doesn't, so it can't exercise
    /// the H-3 (docs/AUDITORIA-2026-07-v5.md) `escalation_trigger`
    /// stamping, which only touches the `Usage` variant.
    struct UsageEmittingBackend {
        label: &'static str,
    }

    #[async_trait]
    impl ModelBackend for UsageEmittingBackend {
        fn name(&self) -> &str {
            self.label
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            Ok(Box::pin(stream::iter(vec![
                Ok(CompletionEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    stop_reason: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                }),
                Ok(CompletionEvent::Done),
            ])))
        }
    }

    fn request(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            messages,
            tool_stubs: vec![],
            system_prompt: String::new(),
            max_tokens: 100,
        }
    }

    fn observation(id: &str, is_error: bool) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: if is_error { "boom" } else { "ok" }.to_string(),
                is_error,
            }],
        }
    }

    /// `n` plain User text messages — never `ToolResult` blocks, so they
    /// read as "clean" to `trailing_failed_observations` regardless of
    /// `n`. Used purely to vary the request contents between calls that
    /// are meant to represent distinct rounds: D4's same-round dedup
    /// (docs/AUDITORIA-2026-07-v3.md) is keyed on a content fingerprint
    /// (J-1, docs/AUDITORIA-2026-07-v7.md), and real engine usage never
    /// sends the exact same, unchanged history to two genuinely different
    /// rounds — these tests honor that invariant instead of the
    /// unrealistic "identical empty history, 5 different rounds" shape a
    /// raw `vec![]` repeated would exercise.
    fn filler(n: usize) -> Vec<Message> {
        (0..n).map(|_| Message::text(Role::User, "...")).collect()
    }

    #[tokio::test]
    async fn the_lead_opens_and_the_worker_takes_over() {
        let (backend, lead, worker) = harness(2, 2, 3);
        for round in 1..=5 {
            let _ = backend.complete(request(filler(round))).await.unwrap();
        }
        assert_eq!(
            lead.load(Ordering::SeqCst),
            2,
            "lead opens lead_turns calls"
        );
        assert_eq!(worker.load(Ordering::SeqCst), 3, "worker handles the rest");
    }

    #[tokio::test]
    async fn consecutive_failures_escalate_for_the_configured_window() {
        // lead_turns = 0: purely reactive.
        let (backend, lead, worker) = harness(0, 2, 2);

        // Clean history → worker.
        let _ = backend.complete(request(vec![])).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (0, 1)
        );

        // Two trailing failed observations → escalates (this call + 1 more).
        let failing = vec![observation("a", true), observation("b", true)];
        let _ = backend.complete(request(failing)).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (1, 1)
        );

        // Next call rides the escalation window even with clean history —
        // `filler(1)` keeps the message count distinct from every prior
        // call (D4's same-round dedup).
        let _ = backend.complete(request(filler(1))).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (2, 1)
        );

        // Window exhausted → back to the worker.
        let _ = backend.complete(request(filler(2))).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (2, 2)
        );
    }

    // --- D4 (docs/AUDITORIA-2026-07-v3.md): best-of-n candidates share
    // one round's counters instead of each consuming their own ---

    #[tokio::test]
    async fn identical_repeat_requests_are_treated_as_one_round_not_several() {
        // Simulates `Engine::complete_with_best_of_n` calling `complete`
        // N times with a *cloned, identical* request for the same round.
        let (backend, lead, worker) = harness(1, 1, 1);
        let same = filler(3);

        let _ = backend.complete(request(same.clone())).await.unwrap();
        let _ = backend.complete(request(same.clone())).await.unwrap();
        let _ = backend.complete(request(same)).await.unwrap();

        assert_eq!(
            lead.load(Ordering::SeqCst),
            3,
            "all 3 candidates of the lead-opening round must go to the lead"
        );
        assert_eq!(
            worker.load(Ordering::SeqCst),
            0,
            "no candidate should have fallen through to the worker"
        );
    }

    #[tokio::test]
    async fn best_of_n_candidates_of_an_escalated_round_do_not_drain_the_window_early() {
        // lead_turns = 0: purely reactive. escalation_turns = 2, so the
        // escalating round plus exactly one more round should go to the
        // lead — regardless of how many best-of-n candidates the
        // escalating round itself had.
        let (backend, lead, worker) = harness(0, 2, 2);

        let failing = vec![observation("a", true), observation("b", true)];
        // 3 best-of-n candidates for the SAME escalating round.
        let _ = backend.complete(request(failing.clone())).await.unwrap();
        let _ = backend.complete(request(failing.clone())).await.unwrap();
        let _ = backend.complete(request(failing)).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (3, 0),
            "all 3 candidates of the escalating round go to the lead"
        );

        // The escalation window must still have exactly 1 round left —
        // not exhausted by the extra candidates above.
        let _ = backend.complete(request(filler(1))).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (4, 0),
            "the escalation window's one remaining round must still go to the lead"
        );

        let _ = backend.complete(request(filler(2))).await.unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (4, 1),
            "the window is now exhausted — back to the worker"
        );
    }

    // --- J-1 (docs/AUDITORIA-2026-07-v7.md): under compaction the
    // rendered history stops growing monotonically — two genuinely
    // different rounds can share a message count. The dedup must key on
    // content, not on count. ---

    #[tokio::test]
    async fn two_rounds_with_the_same_message_count_but_different_content_route_independently() {
        // Simulates the budget-compaction regime: every round renders as
        // `[summary] + tail` with a constant shape. Round 1 and round 2
        // both have exactly 1 message, but different contents.
        let (backend, lead, worker) = harness(1, 1, 1);

        let round_1 = vec![Message::text(Role::User, "[Resumen] ronda uno")];
        let round_2 = vec![Message::text(Role::User, "[Resumen] ronda dos")];

        let _ = backend.complete(request(round_1)).await.unwrap();
        let _ = backend.complete(request(round_2)).await.unwrap();

        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (1, 1),
            "round 2 must advance the counters (lead opening consumed, worker takes over) — \
             a count-keyed dedup would have replayed round 1's LeadOpening decision"
        );
    }

    // --- J-2 (docs/AUDITORIA-2026-07-v7.md): harness-injected user text
    // (task-list summary, harness notes, plan render, compaction
    // placeholder) must not zero the failure streak. ---

    #[test]
    fn harness_injected_user_text_does_not_break_the_failure_streak() {
        // The exact shape of a `+lead:`+task-list round: two failed
        // observations, then the ephemeral task-list summary trailing the
        // request. Before J-2 the summary zeroed the streak on EVERY
        // round (it rides while any task is open), killing reactive
        // escalation by construction.
        let messages = vec![
            observation("a", true),
            observation("b", true),
            Message::text(
                Role::User,
                "Task list: 1 [pending] leer archivo. Mark progress with task_update(id, status).",
            ),
        ];
        assert_eq!(trailing_failed_observations(&messages), 2);

        // Same for a harness note — it fires exactly when the turn is
        // degrading, which is when the streak matters most.
        let messages = vec![
            observation("a", true),
            observation("b", true),
            Message::text(
                Role::User,
                "[harness] The next round is this turn's last (round 8 of 8).",
            ),
        ];
        assert_eq!(trailing_failed_observations(&messages), 2);
    }

    #[test]
    fn a_real_user_message_still_ends_the_streak() {
        // The J-2 skip is prefix-scoped: genuine human text remains an
        // episode boundary.
        let messages = vec![
            observation("a", true),
            observation("b", true),
            Message::text(Role::User, "mejor intenta con el otro archivo"),
        ];
        assert_eq!(trailing_failed_observations(&messages), 0);
    }

    #[tokio::test]
    async fn a_single_failure_below_the_threshold_stays_on_the_worker() {
        let (backend, lead, worker) = harness(0, 2, 3);
        let _ = backend
            .complete(request(vec![observation("a", true)]))
            .await
            .unwrap();
        assert_eq!(
            (lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)),
            (0, 1)
        );
    }

    #[test]
    fn trailing_failures_count_only_the_unbroken_tail() {
        // err, ok, err, err → tail is 2 (the `ok` breaks the streak).
        let messages = vec![
            observation("a", true),
            observation("b", false),
            observation("c", true),
            observation("d", true),
        ];
        assert_eq!(trailing_failed_observations(&messages), 2);
    }

    #[test]
    fn assistant_messages_between_observations_do_not_break_the_streak() {
        let messages = vec![
            observation("a", true),
            Message::text(Role::Assistant, "let me retry"),
            observation("b", true),
        ];
        assert_eq!(trailing_failed_observations(&messages), 2);
    }

    #[test]
    fn a_fresh_user_text_message_resets_the_streak() {
        let messages = vec![
            observation("a", true),
            observation("b", true),
            Message::text(Role::User, "olvida eso, haz otra cosa"),
        ];
        assert_eq!(trailing_failed_observations(&messages), 0);
    }

    #[test]
    fn a_mixed_observation_with_any_error_counts_as_failed() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "a".to_string(),
                    content: "ok".to_string(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "b".to_string(),
                    content: "boom".to_string(),
                    is_error: true,
                },
            ],
        }];
        assert_eq!(trailing_failed_observations(&messages), 1);
    }

    // --- F3 (docs/AUDITORIA-2026-07-v3.md): the post-edit-check
    // regression marker counts even with is_error=false ---

    fn tool_result_message(content: &str, is_error: bool) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "x".to_string(),
                content: content.to_string(),
                is_error,
            }],
        }
    }

    #[test]
    fn a_post_edit_check_regression_counts_despite_is_error_being_false() {
        let messages = vec![
            tool_result_message(
                "edited src/lib.rs\n\n[post-edit check] `cargo check` fails in . after this edit \
                 (the edit itself was applied). Fix these before moving on:\nerror[E0308]: ...",
                false,
            ),
            tool_result_message(
                "edited src/lib.rs\n\n[post-edit check] `cargo check` fails in . after this edit \
                 (the edit itself was applied). Fix these before moving on:\nerror[E0308]: ...",
                false,
            ),
        ];
        assert_eq!(
            trailing_failed_observations(&messages),
            2,
            "both edits kept the build broken — must count as a failure streak \
             even though neither is_error"
        );
    }

    /// I-3 (docs/AUDITORIA-2026-07-v6.md): the durable-clearing render
    /// replaces an old result's content with a placeholder while keeping
    /// `is_error` — the classification refinements (environment signals,
    /// post-edit marker) are gone with the content, so counting the bare
    /// `is_error` would turn old state facts (a nonzero exit_code from
    /// legitimate exploration) into spurious escalation fuel.
    #[test]
    fn a_cleared_placeholder_never_counts_as_a_failure_despite_is_error() {
        let messages = vec![tool_result_message(
            "[tool result cleared: 4813 chars removed to keep context small; the tool call \
             above is preserved]",
            true,
        )];
        assert_eq!(trailing_failed_observations(&messages), 0);
    }

    #[test]
    fn a_clean_edit_with_no_guardrail_marker_does_not_count() {
        let messages = vec![tool_result_message("edited src/lib.rs", false)];
        assert_eq!(trailing_failed_observations(&messages), 0);
    }

    // --- D3 (docs/AUDITORIA-2026-07-v3.md): environment-caused failures
    // (exit codes, not-found) don't count toward the streak ---

    #[test]
    fn a_shell_exec_nonzero_exit_code_does_not_count_as_a_model_failure() {
        let messages = vec![tool_result_message(
            r#"{"exit_code":1,"stdout":"","stderr":""}"#,
            true,
        )];
        assert_eq!(
            trailing_failed_observations(&messages),
            0,
            "a command's own non-zero exit status is an environment fact, not a model failure"
        );
    }

    #[test]
    fn a_read_file_not_found_error_does_not_count_as_a_model_failure() {
        let messages = vec![tool_result_message(
            "failed to read '/tmp/does-not-exist.txt': No such file or directory (os error 2)",
            true,
        )];
        assert_eq!(trailing_failed_observations(&messages), 0);
    }

    #[test]
    fn a_write_file_failure_does_not_count_as_a_model_failure() {
        let messages = vec![tool_result_message(
            "failed to write '/nonexistent/dir/out.txt': No such file or directory (os error 2)",
            true,
        )];
        assert_eq!(trailing_failed_observations(&messages), 0);
    }

    #[test]
    fn a_schema_validation_failure_still_counts_as_a_model_failure() {
        // Not an environment signal — must keep counting as before.
        let messages = vec![tool_result_message(
            "Tool call 'read_file' failed schema validation: ...",
            true,
        )];
        assert_eq!(trailing_failed_observations(&messages), 1);
    }

    #[test]
    fn an_edit_file_ambiguous_match_failure_still_counts_as_a_model_failure() {
        let messages = vec![tool_result_message(
            "old_string is ambiguous in 'src/lib.rs': found 2 occurrences",
            true,
        )];
        assert_eq!(trailing_failed_observations(&messages), 1);
    }

    #[test]
    fn the_decorator_name_names_both_backends() {
        let (backend, _, _) = harness(1, 1, 1);
        assert_eq!(backend.name(), "escalating(lead->worker)");
    }

    /// I-1 (docs/AUDITORIA-2026-07-v6.md): `None` keeps each default,
    /// `Some(n)` overrides just that knob — including `Some(0)` for
    /// `lead_turns`, the purely-reactive mode the SI-2 A/B needed and
    /// couldn't express.
    #[test]
    fn with_configured_knobs_applies_some_and_keeps_defaults_for_none() {
        let fresh = || {
            EscalatingBackend::new(
                Box::new(CountingBackend {
                    label: "lead",
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                Box::new(CountingBackend {
                    label: "worker",
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
            )
        };

        let untouched = fresh().with_configured_knobs(None, None, None);
        assert_eq!(untouched.lead_turns(), DEFAULT_LEAD_TURNS);
        assert_eq!(untouched.failure_threshold(), DEFAULT_FAILURE_THRESHOLD);
        assert_eq!(untouched.escalation_turns(), DEFAULT_ESCALATION_TURNS);

        let purely_reactive = fresh().with_configured_knobs(Some(0), Some(4), Some(2));
        assert_eq!(purely_reactive.lead_turns(), 0, "lead_turns=0 must be expressible");
        assert_eq!(purely_reactive.failure_threshold(), 4);
        assert_eq!(purely_reactive.escalation_turns(), 2);

        // Partial override: only the threshold, the other two keep defaults.
        let partial = fresh().with_configured_knobs(None, Some(1), None);
        assert_eq!(partial.lead_turns(), DEFAULT_LEAD_TURNS);
        assert_eq!(partial.failure_threshold(), 1);
        assert_eq!(partial.escalation_turns(), DEFAULT_ESCALATION_TURNS);
    }

    /// H-3 (docs/AUDITORIA-2026-07-v5.md): the round that *triggers* an
    /// escalation must stamp `escalation_trigger` on its `Usage` event; a
    /// normal worker round (clean history, no escalation) must not.
    #[tokio::test]
    async fn the_triggering_round_stamps_escalation_trigger_on_its_usage_event() {
        let backend = EscalatingBackend::new(
            Box::new(UsageEmittingBackend { label: "lead" }),
            Box::new(UsageEmittingBackend { label: "worker" }),
        )
        .with_lead_turns(0) // purely reactive
        .with_failure_threshold(2)
        .with_escalation_turns(2);

        async fn collect_triggers(
            stream: Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
        ) -> Vec<Option<String>> {
            stream
                .filter_map(|event| async move {
                    match event {
                        Ok(CompletionEvent::Usage {
                            escalation_trigger, ..
                        }) => Some(escalation_trigger),
                        _ => None,
                    }
                })
                .collect()
                .await
        }

        // Round 1: clean history -> worker, no trigger.
        let stream = backend.complete(request(vec![])).await.unwrap();
        assert_eq!(collect_triggers(stream).await, vec![None]);

        // Round 2: 2 consecutive failed observations, at the threshold ->
        // this round triggers the escalation.
        let messages = vec![observation("1", true), observation("2", true)];
        let stream = backend.complete(request(messages)).await.unwrap();
        let triggers = collect_triggers(stream).await;
        assert_eq!(triggers.len(), 1);
        let trigger = triggers[0]
            .as_ref()
            .expect("the triggering round must stamp a trigger");
        assert!(
            trigger.contains("2 consecutive failed observations"),
            "got: {trigger}"
        );
        assert!(trigger.contains("threshold 2"), "got: {trigger}");
    }
}
