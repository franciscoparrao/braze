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
use futures::Stream;

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
    /// `req.messages.len()` the last time `route` advanced the counters
    /// above — D4 (docs/AUDITORIA-2026-07-v3.md): best-of-n
    /// (`Engine::complete_with_best_of_n`) calls `complete` N times per
    /// round with an *identical* request (all N candidates answer the
    /// same turn). Without this, each candidate consumed its own
    /// `lead_turns`/`escalated_remaining` slot — "the lead opens the
    /// session" could exhaust itself inside a single round of voting, and
    /// the vote ended up comparing candidates from different models as
    /// if they were interchangeable. History only grows within a turn, so
    /// two genuinely different rounds never share the same message
    /// count; two calls that do share it are the same round's candidates.
    last_round_message_count: Option<usize>,
    /// The decision made the last time `route` actually advanced the
    /// counters — replayed for every later call in the same round
    /// (detected via `last_round_message_count`) instead of routing them
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
        // again for it.
        let this_round_message_count = req.messages.len();
        if state.last_round_message_count == Some(this_round_message_count)
            && let Some(decision) = state.last_decision
        {
            return decision;
        }
        state.last_round_message_count = Some(this_round_message_count);

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
        if decision == RouteDecision::LeadEscalating {
            tracing::info!(
                threshold = self.failure_threshold,
                escalation_turns = self.escalation_turns,
                "worker flounders (consecutive failed observations) — escalating to the lead model"
            );
        }
        if decision.is_lead() {
            self.lead.complete(req).await
        } else {
            self.worker.complete(req).await
        }
    }
}

/// Counts the *trailing* run of failed observations in `messages`: how
/// many of the most recent tool-result messages (User messages carrying
/// `ToolResult` blocks) contain at least one observation
/// [`observation_is_a_failure`] treats as a failure, walking backwards.
/// The scan skips Assistant messages (the tool_use/plan/text between
/// observations) and stops at the first clean observation or at a real
/// user Text message — either one means the worker isn't in a failure
/// streak *right now*.
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
            // A real user message (or summary placeholder): whatever
            // failed before it is a previous episode, not this streak.
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
    /// `n`. Used purely to vary `req.messages.len()` between calls that
    /// are meant to represent distinct rounds: D4's same-round dedup
    /// (docs/AUDITORIA-2026-07-v3.md) is keyed on message count, and real
    /// engine usage never sends the exact same, unchanged history to two
    /// genuinely different rounds (the log only grows within a turn) —
    /// these tests honor that invariant instead of the unrealistic
    /// "identical empty history, 5 different rounds" shape a raw `vec![]`
    /// repeated would exercise.
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
}
