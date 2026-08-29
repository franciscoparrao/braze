//! [`FailoverBackend`] — provider failover on rate limiting, as a
//! [`ModelBackend`] decorator.
//!
//! A chain of interchangeable backends tried in preference order: when
//! the preferred one answers "429 / slow down", the next one takes the
//! round instead of the turn dying. Named *failover*, not *fallback*,
//! because `braze-engine`'s `engine::fallback` already owns that word
//! for the tools-free summary round — two different mechanisms sharing
//! a name would make every log line ambiguous.
//!
//! WHAT ALREADY HANDLES 429, AND WHY THAT ISN'T ENOUGH. `retry.rs`
//! (H-19) retries a rate-limited *initial* request up to 3 times,
//! honoring `Retry-After` capped at 15s. That absorbs the transient
//! blip. This decorator handles what comes after: the provider is still
//! refusing, and we have another one sitting idle. The two compose —
//! the inner backend exhausts its own retry ladder, then this layer
//! moves on.
//!
//! PRE-STREAM ONLY, deliberately, inheriting H-19's own scope note.
//! `complete` returns `Result<Stream>`: an error *before* the stream
//! opens has produced no observable effect, so re-issuing the request
//! to another provider is invisible to the engine. Once the stream has
//! yielded a `TextDelta` or a `ToolCallRequested`, switching providers
//! would duplicate content the engine already consumed — replaying a
//! partially-consumed completion needs idempotency no provider offers.
//! A mid-stream 429 therefore surfaces as it does today.
//!
//! COOLDOWN, or the feature costs more than it saves. Without memory of
//! who just refused, every round re-hits the limited provider and pays
//! its full retry ladder (up to ~15s) before arriving at the same
//! conclusion. A refusing backend is marked unavailable for
//! [`FailoverBackend::cooldown`], so subsequent rounds go straight to
//! the next one. The cooldown is a *hint*, never a prohibition: if
//! every backend is cooling down, the one whose cooldown expires
//! soonest is tried anyway — a stale estimate must not turn into a
//! refusal we invented ourselves.
//!
//! The cooldown is a configured constant rather than the provider's own
//! `Retry-After` because that header never reaches this layer: it is
//! consumed inside `send_with_retry`, and `ModelError::RateLimited`
//! carries only a message. For OpenCode Zen the point is moot — its
//! 429s carry no rate-limit headers at all (measured 2026-08-29:
//! responses expose only `date`, `content-type`, `content-length`,
//! `server`, `cf-ray`, `cf-placement`).

use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Stream;

use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;

/// How long a backend stays skipped after refusing. A minute is long
/// enough to outlast a per-minute burst window (the limit shape most
/// gateways impose) and short enough that a daily-quota misdiagnosis
/// self-corrects within a session.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);

/// See the module doc comment. Construct with [`FailoverBackend::new`],
/// tune with [`FailoverBackend::with_cooldown`].
pub struct FailoverBackend {
    /// Preference order. Non-empty by construction: `new` takes the
    /// preferred backend separately from the rest, so there is no
    /// "empty chain" state for `complete` to discover at runtime.
    backends: Vec<Box<dyn ModelBackend>>,
    cooldown: Duration,
    /// Precomputed so `name()` can return `&str`.
    name: String,
    state: Mutex<FailoverState>,
}

struct FailoverState {
    /// Per-backend instant until which it is considered rate-limited.
    /// Parallel to `backends` by index.
    limited_until: Vec<Option<Instant>>,
    /// Rounds served by a backend other than the preferred one —
    /// observability for the composition roots, same role as
    /// `EscalatingBackend`'s knob getters (I-1).
    failovers: usize,
}

impl FailoverBackend {
    /// `preferred` is tried first on every round it isn't cooling down;
    /// `rest` follows in order. Taking the head separately makes the
    /// non-empty invariant a type-level fact.
    pub fn new(preferred: Box<dyn ModelBackend>, rest: Vec<Box<dyn ModelBackend>>) -> Self {
        let mut backends = Vec::with_capacity(1 + rest.len());
        backends.push(preferred);
        backends.extend(rest);
        let name = format!(
            "failover({})",
            backends
                .iter()
                .map(|b| b.name())
                .collect::<Vec<_>>()
                .join("->")
        );
        let limited_until = vec![None; backends.len()];
        Self {
            backends,
            cooldown: DEFAULT_COOLDOWN,
            name,
            state: Mutex::new(FailoverState {
                limited_until,
                failovers: 0,
            }),
        }
    }

    /// How long a refusing backend stays skipped. `Duration::ZERO`
    /// disables the memory entirely (every round re-tries the preferred
    /// backend first) — the ablation arm.
    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// The configured cooldown — observability for composition-root
    /// wiring tests, so a config value that never reached the decorator
    /// is detectable (the I-1 failure mode: a knob that exists, is
    /// documented, and is silently never applied).
    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    /// How many backends are in the chain, preferred included.
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// Always false — the chain holds at least the preferred backend.
    /// Present because clippy requires it alongside `len`.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Rounds served by something other than the preferred backend.
    pub fn failovers(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .failovers
    }

    /// Indices to try, in order: every backend not currently cooling
    /// down. If all of them are, the single one whose cooldown expires
    /// soonest — a cooldown is an estimate about the provider, and an
    /// estimate must not become a refusal of our own making. Split from
    /// `complete` so the ordering is testable without streaming.
    fn candidate_order(&self, now: Instant) -> Vec<usize> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let fresh: Vec<usize> = (0..self.backends.len())
            .filter(|index| match state.limited_until[*index] {
                Some(until) => until <= now,
                None => true,
            })
            .collect();
        if !fresh.is_empty() {
            return fresh;
        }

        // Everything is cooling down: pick the one closest to expiring.
        let soonest = state
            .limited_until
            .iter()
            .enumerate()
            .filter_map(|(index, until)| until.map(|until| (until, index)))
            .min()
            .map(|(_, index)| index)
            .unwrap_or(0);
        vec![soonest]
    }

    fn mark_limited(&self, index: usize, now: Instant) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.limited_until[index] = Some(now + self.cooldown);
    }

    /// Clears a backend's cooldown after it answers — it demonstrably
    /// isn't limited anymore, whatever the estimate said.
    fn mark_available(&self, index: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.limited_until[index] = None;
        if index != 0 {
            state.failovers += 1;
        }
    }
}

/// Errors that move the round to the next backend. Deliberately narrow
/// (decision of 2026-08-29):
///
/// - `RateLimited` is the whole point of the decorator.
/// - `CircuitOpen` means a per-destination breaker already declared this
///   provider unreachable and is short-circuiting without touching the
///   network. Failing the turn while a healthy provider sits idle is
///   strictly worse than trying it. Note this does NOT change the
///   breaker's own classification: `circuit_breaker::classify` still
///   treats a 429 as `Neutral` ("slow down", not "I'm down").
///
/// Everything else fails the turn as before. `Request` in particular
/// stays out: a 400 "does not support tools", a bad key, or an unknown
/// model are deterministic and identical on the next provider, so
/// failing over would hide the real diagnosis behind a second, equally
/// doomed call — exactly the "does not support tools" trap already
/// documented in CLAUDE.md.
fn triggers_failover(err: &ModelError) -> bool {
    matches!(
        err,
        ModelError::RateLimited(_) | ModelError::CircuitOpen(_)
    )
}

#[async_trait]
impl ModelBackend for FailoverBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let now = Instant::now();
        let order = self.candidate_order(now);
        let last_position = order.len() - 1;

        // The request is cloned for every attempt but the last, which
        // takes ownership — so a single-candidate round (the common one
        // once a chain has settled) clones nothing.
        let mut pending = Some(req);
        let mut last_error: Option<ModelError> = None;

        for (position, index) in order.into_iter().enumerate() {
            let attempt = if position == last_position {
                pending.take().expect("the last attempt owns the request")
            } else {
                pending
                    .as_ref()
                    .expect("a non-final attempt still holds the request")
                    .clone()
            };

            match self.backends[index].complete(attempt).await {
                Ok(stream) => {
                    if index != 0 {
                        tracing::warn!(
                            preferred = self.backends[0].name(),
                            serving = self.backends[index].name(),
                            cooldown_s = self.cooldown.as_secs(),
                            "preferred backend is rate-limited; this round is served by a failover backend"
                        );
                    }
                    self.mark_available(index);
                    return Ok(stream);
                }
                Err(err) if triggers_failover(&err) => {
                    tracing::warn!(
                        backend = self.backends[index].name(),
                        error = %err,
                        cooldown_s = self.cooldown.as_secs(),
                        "backend refused (rate limit / open breaker); marking it cooling down"
                    );
                    self.mark_limited(index, now);
                    last_error = Some(err);
                }
                // Deterministic failure: the next provider would fail
                // the same way. Surface it instead of burning the chain.
                Err(err) => return Err(err),
            }
        }

        Err(last_error.expect("the loop only exits here after at least one failover-class error"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::stream;

    /// Fake backend that counts calls and either streams `Done` or
    /// fails with a scripted error — enough to observe chain routing
    /// without any HTTP.
    struct ScriptedBackend {
        label: &'static str,
        calls: Arc<AtomicUsize>,
        /// Produced fresh per call so the same backend can fail
        /// repeatedly (`ModelError` isn't `Clone`).
        error: Option<fn() -> ModelError>,
    }

    impl ScriptedBackend {
        fn ok(label: &'static str, calls: Arc<AtomicUsize>) -> Box<dyn ModelBackend> {
            Box::new(Self {
                label,
                calls,
                error: None,
            })
        }

        fn failing(
            label: &'static str,
            calls: Arc<AtomicUsize>,
            error: fn() -> ModelError,
        ) -> Box<dyn ModelBackend> {
            Box::new(Self {
                label,
                calls,
                error: Some(error),
            })
        }
    }

    #[async_trait]
    impl ModelBackend for ScriptedBackend {
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
            match self.error {
                Some(make) => Err(make()),
                None => Ok(Box::pin(stream::iter(vec![Ok(CompletionEvent::Done)]))),
            }
        }
    }

    fn rate_limited() -> ModelError {
        ModelError::RateLimited("zen rate-limited (HTTP 429)".to_string())
    }

    fn circuit_open() -> ModelError {
        ModelError::CircuitOpen("breaker open for zen".to_string())
    }

    fn request_error() -> ModelError {
        ModelError::Request("anthropic HTTP 400: does not support tools".to_string())
    }

    fn req() -> CompletionRequest {
        CompletionRequest {
            messages: Vec::new(),
            tool_stubs: Vec::new(),
            system_prompt: String::new(),
            max_tokens: 128,
        }
    }

    /// The preferred backend answering means the rest are never touched
    /// — the decorator must be inert on the happy path.
    #[tokio::test]
    async fn healthy_preferred_backend_serves_alone() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::ok("primary", Arc::clone(&first)),
            vec![ScriptedBackend::ok("secondary", Arc::clone(&second))],
        );

        assert!(chain.complete(req()).await.is_ok());
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 0);
        assert_eq!(chain.failovers(), 0, "no failover happened");
    }

    /// The feature itself: a 429 moves the round to the next backend
    /// instead of failing the turn.
    #[tokio::test]
    async fn rate_limited_preferred_falls_over_to_the_next() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::failing("primary", Arc::clone(&first), rate_limited),
            vec![ScriptedBackend::ok("secondary", Arc::clone(&second))],
        );

        assert!(chain.complete(req()).await.is_ok());
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);
        assert_eq!(chain.failovers(), 1);
    }

    /// `CircuitOpen` is the second trigger (decision of 2026-08-29):
    /// a short-circuited provider shouldn't fail the turn while a
    /// healthy one is idle.
    #[tokio::test]
    async fn open_breaker_also_falls_over() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::failing("primary", Arc::clone(&first), circuit_open),
            vec![ScriptedBackend::ok("secondary", Arc::clone(&second))],
        );

        assert!(chain.complete(req()).await.is_ok());
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }

    /// A deterministic error must NOT burn the chain: the next provider
    /// would fail identically, and failing over would hide the real
    /// diagnosis.
    #[tokio::test]
    async fn deterministic_error_does_not_fall_over() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::failing("primary", Arc::clone(&first), request_error),
            vec![ScriptedBackend::ok("secondary", Arc::clone(&second))],
        );

        let err = chain.complete(req()).await.err().expect("should fail");
        assert!(matches!(err, ModelError::Request(_)), "got {err:?}");
        assert_eq!(
            second.load(Ordering::SeqCst),
            0,
            "the second backend must never be tried on a deterministic failure"
        );
    }

    /// The cooldown's whole purpose: the round AFTER a 429 must not
    /// re-pay the limited provider's retry ladder.
    #[tokio::test]
    async fn cooldown_skips_the_limited_backend_on_later_rounds() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::failing("primary", Arc::clone(&first), rate_limited),
            vec![ScriptedBackend::ok("secondary", Arc::clone(&second))],
        )
        .with_cooldown(Duration::from_secs(300));

        assert!(chain.complete(req()).await.is_ok());
        assert!(chain.complete(req()).await.is_ok());
        assert!(chain.complete(req()).await.is_ok());

        assert_eq!(
            first.load(Ordering::SeqCst),
            1,
            "the limited backend must be tried once, then skipped while cooling down"
        );
        assert_eq!(second.load(Ordering::SeqCst), 3);
        assert_eq!(chain.failovers(), 3);
    }

    /// With the memory disabled, every round re-tries the preferred
    /// backend — the ablation arm, and the proof that the skipping above
    /// is the cooldown's doing and not a permanent demotion.
    #[tokio::test]
    async fn zero_cooldown_retries_the_preferred_backend_every_round() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::failing("primary", Arc::clone(&first), rate_limited),
            vec![ScriptedBackend::ok("secondary", Arc::clone(&second))],
        )
        .with_cooldown(Duration::ZERO);

        assert!(chain.complete(req()).await.is_ok());
        assert!(chain.complete(req()).await.is_ok());

        assert_eq!(first.load(Ordering::SeqCst), 2);
        assert_eq!(second.load(Ordering::SeqCst), 2);
    }

    /// Every backend cooling down must still produce ONE attempt, not a
    /// refusal we invented: the cooldown is an estimate about the
    /// provider, not a fact.
    #[tokio::test]
    async fn all_cooling_down_still_tries_exactly_one() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::failing("primary", Arc::clone(&first), rate_limited),
            vec![ScriptedBackend::failing(
                "secondary",
                Arc::clone(&second),
                rate_limited,
            )],
        )
        .with_cooldown(Duration::from_secs(300));

        // Round 1: both refuse, both start cooling down.
        let err = chain.complete(req()).await.err().expect("should fail");
        assert!(matches!(err, ModelError::RateLimited(_)), "got {err:?}");
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);

        // Round 2: nothing is fresh, so exactly one backend is tried —
        // the one whose cooldown expires soonest (the primary, marked
        // first) — and the turn fails with its real error.
        let err = chain.complete(req()).await.err().expect("should fail");
        assert!(matches!(err, ModelError::RateLimited(_)), "got {err:?}");
        assert_eq!(
            first.load(Ordering::SeqCst) + second.load(Ordering::SeqCst),
            3,
            "exactly one extra attempt across the whole chain"
        );
    }

    /// A backend that answers clears its own cooldown — the estimate
    /// loses to the evidence.
    #[tokio::test]
    async fn answering_clears_the_cooldown() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::ok("primary", Arc::clone(&calls)),
            Vec::new(),
        )
        .with_cooldown(Duration::from_secs(300));

        chain.mark_limited(0, Instant::now());
        assert!(chain.complete(req()).await.is_ok());
        assert_eq!(
            chain.candidate_order(Instant::now()),
            vec![0],
            "the cooldown must be cleared after a successful round"
        );
    }

    /// The chain's name carries the whole order, so a log line or a
    /// bench arm identifies which providers were composed.
    #[test]
    fn name_spells_out_the_chain() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chain = FailoverBackend::new(
            ScriptedBackend::ok("zen:hy3-free", Arc::clone(&calls)),
            vec![ScriptedBackend::ok("ollama:qwen2.5", Arc::clone(&calls))],
        );
        assert_eq!(chain.name(), "failover(zen:hy3-free->ollama:qwen2.5)");
        assert_eq!(chain.len(), 2);
    }
}
