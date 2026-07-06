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
    /// Total `complete` calls seen — the first `lead_turns` go to the
    /// lead unconditionally.
    calls: usize,
    /// Remaining calls of an active escalation (0 = not escalated).
    escalated_remaining: usize,
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
        state.calls += 1;

        if state.calls <= self.lead_turns {
            return RouteDecision::LeadOpening;
        }
        if state.escalated_remaining > 0 {
            state.escalated_remaining -= 1;
            return RouteDecision::LeadEscalated;
        }
        if trailing_failed_observations(&req.messages) >= self.failure_threshold {
            // This call plus (escalation_turns - 1) further ones.
            state.escalated_remaining = self.escalation_turns - 1;
            return RouteDecision::LeadEscalating;
        }
        RouteDecision::Worker
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
/// `ToolResult` blocks) contain at least one `is_error` result, walking
/// backwards. The scan skips Assistant messages (the tool_use/plan/text
/// between observations) and stops at the first clean observation or at
/// a real user Text message — either one means the worker isn't in a
/// failure streak *right now*.
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
        let any_error = message.content.iter().any(
            |block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }),
        );
        if !any_error {
            break;
        }
        failures += 1;
    }
    failures
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

    #[tokio::test]
    async fn the_lead_opens_and_the_worker_takes_over() {
        let (backend, lead, worker) = harness(2, 2, 3);
        for _ in 0..5 {
            let _ = backend.complete(request(vec![])).await.unwrap();
        }
        assert_eq!(lead.load(Ordering::SeqCst), 2, "lead opens lead_turns calls");
        assert_eq!(worker.load(Ordering::SeqCst), 3, "worker handles the rest");
    }

    #[tokio::test]
    async fn consecutive_failures_escalate_for_the_configured_window() {
        // lead_turns = 0: purely reactive.
        let (backend, lead, worker) = harness(0, 2, 2);

        // Clean history → worker.
        let _ = backend.complete(request(vec![])).await.unwrap();
        assert_eq!((lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)), (0, 1));

        // Two trailing failed observations → escalates (this call + 1 more).
        let failing = vec![observation("a", true), observation("b", true)];
        let _ = backend.complete(request(failing)).await.unwrap();
        assert_eq!((lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)), (1, 1));

        // Next call rides the escalation window even with clean history.
        let _ = backend.complete(request(vec![])).await.unwrap();
        assert_eq!((lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)), (2, 1));

        // Window exhausted → back to the worker.
        let _ = backend.complete(request(vec![])).await.unwrap();
        assert_eq!((lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)), (2, 2));
    }

    #[tokio::test]
    async fn a_single_failure_below_the_threshold_stays_on_the_worker() {
        let (backend, lead, worker) = harness(0, 2, 3);
        let _ = backend
            .complete(request(vec![observation("a", true)]))
            .await
            .unwrap();
        assert_eq!((lead.load(Ordering::SeqCst), worker.load(Ordering::SeqCst)), (0, 1));
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

    #[test]
    fn the_decorator_name_names_both_backends() {
        let (backend, _, _) = harness(1, 1, 1);
        assert_eq!(backend.name(), "escalating(lead->worker)");
    }
}
