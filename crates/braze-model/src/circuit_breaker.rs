//! Generic three-state circuit breaker (Closed/Open/HalfOpen) guarding
//! `ModelBackend` HTTP calls, keyed by a stable identity (provider +
//! base URL + model) so a *fresh* backend instance — `braze-bench`
//! builds one per (task, repetition), see `runner.rs`'s
//! `spec.build_agent_model` call inside the per-task loop — still shares
//! failure state with every other instance pointed at the same
//! destination, without threading an `Arc` through
//! `BackendSpec::build()`'s API or changing any caller in
//! `braze-bench`/`braze-cli`/`braze-engine`.
//!
//! Motivated by 2026-07-17 grok-build research
//! (`docs/grok-build-research-2026-07-17.md` § "Reliability techniques")
//! plus this project's own lived experience the same day: a long
//! `braze-bench` sweep against a backend that goes down mid-sweep
//! (Nitro unreachable) currently pays H-19's retry cost
//! (`retry.rs::send_with_retry`, up to 3 attempts with backoff) on
//! *every subsequent task* before failing — repeatedly re-discovering
//! "still down" instead of remembering it. A tripped breaker
//! short-circuits later calls to an immediate `ModelError::CircuitOpen`
//! without touching the network, until a cooldown elapses and exactly
//! one probe call is let through to test recovery.
//!
//! Design decisions (AUDITORIA-2026-07-v8 K-1, replacing the first
//! draft's windowed error rate):
//!
//! - **Consecutive failures, no time window.** A failed sample against a
//!   slow destination is *expensive* — a full retry ladder costs ~43s
//!   (Anthropic/OpenRouter), a hung connection costs `http_client`'s
//!   600s read timeout — so any fixed-duration window short enough to be
//!   responsive can never accumulate `min_samples` slow failures before
//!   rolling over, and the breaker would mathematically never open for
//!   exactly the outages it was built for. Counting consecutive counted
//!   failures has no such interaction with failure *duration*.
//! - **Outcome classification, not `is_ok()`.** A deterministic 4xx
//!   ("model does not support tools", bad auth) proves the destination
//!   is *reachable and answering* — counting it would let one
//!   misconfigured sweep arm trip the breaker for every other arm
//!   sharing the server. Only transport-class failures count: connect/
//!   send errors, exhausted-5xx, and mid-stream `StreamError`. See
//!   [`classify`].
//! - **The keyed model.** The key includes the model tag so a broken
//!   model (HTTP 400 per call) can never starve healthy models on the
//!   same server — and 4xx being Neutral makes that a second, redundant
//!   layer of the same protection.
//! - **Stream-aware.** "The request succeeded" is only known when the
//!   *stream* ends: the ~2% Nitro failure mode is mid-generation, after
//!   headers arrive. [`acquire`] hands back a [`Guard`] that the
//!   backend's stream driver reports the terminal outcome to; a `Guard`
//!   dropped without reporting (user cancellation) records nothing.
//! - **Kill switch.** `BRAZE_CIRCUIT_BREAKER=off` disables the breaker
//!   process-wide (checked once) so A/B sweeps can isolate its effect
//!   and an operator can rule it out during debugging.
//!
//! Deliberately *not* wired through `send_with_retry` itself: unlike
//! retry (which Ollama opts out of per H-19's own doc comment — hammering
//! a saturated local backend doesn't help), tracking cross-call failure
//! state is useful for every backend including Ollama, so the breaker
//! wraps each backend's request path independently, above wherever that
//! backend's own retry (if any) happens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::ModelError;

/// Counted (transport-class) failures in a row that trip the breaker.
/// Successes reset the count; Neutral outcomes (4xx, rate limits,
/// decode errors — see [`classify`]) leave it untouched.
const DEFAULT_TRIP_THRESHOLD: u32 = 5;

/// How long a tripped breaker stays fully Open before allowing one
/// HalfOpen probe through. Long enough that a real outage (Nitro
/// rebooting, a network blip lasting longer than H-19's own retry
/// ceiling) isn't re-probed every single task; short enough that a sweep
/// resumes automatically once the backend recovers, without restarting
/// the process.
const DEFAULT_OPEN_DURATION: Duration = Duration::from_secs(30);

/// How long a claimed HalfOpen probe may stay unreported before a later
/// caller may reclaim the slot (the claimant panicked, was killed, or
/// `?`-propagated past its report). Must comfortably exceed a
/// *legitimate* slow call: CPU-bound local inference regularly runs
/// 90-400s and `http_client`'s read timeout is 600s — reclaiming at the
/// old 30s mark "abandoned" healthy-but-slow probes and piled concurrent
/// probes onto a recovering backend.
const PROBE_TIMEOUT: Duration = Duration::from_secs(600);

/// How a completed call feeds back into the breaker. Split three ways
/// rather than a bool because "the destination answered with a
/// deterministic error" is evidence the destination is *up* — it must
/// neither trip the breaker (like a Failure) nor mask real transport
/// failures by resetting the count (like a Success would).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// The call completed end-to-end (stream finished normally).
    Success,
    /// Transport-class failure: connect/send error, exhausted 5xx
    /// retries, or the stream dying mid-generation.
    Failure,
    /// The destination answered, but with a non-transient error (4xx,
    /// rate limit) or an unparseable body. Proves reachability; says
    /// nothing about outage. In HalfOpen this *closes* the breaker.
    Neutral,
}

/// Maps a `ModelError` to its breaker [`Outcome`]. Kept next to the
/// breaker (rather than each backend deciding) so all three backends
/// count the same things.
fn classify(err: &ModelError) -> Outcome {
    match err {
        // Never self-feed: a short-circuited call proves nothing new.
        ModelError::CircuitOpen(_) => Outcome::Neutral,
        // 429 means "slow down", not "I'm down" — opening the breaker on
        // sustained rate limiting would mask the Retry-After signal the
        // retry layer already honors.
        ModelError::RateLimited(_) => Outcome::Neutral,
        // A response arrived; we just couldn't parse it. Reachable.
        ModelError::Decode(_) => Outcome::Neutral,
        // Mid-stream death — the exact Nitro failure mode this breaker
        // exists for.
        ModelError::StreamError(_) => Outcome::Failure,
        ModelError::Request(message) => match http_status_in(message) {
            // Deterministic client errors ("does not support tools",
            // bad auth, model not found) prove the server is answering.
            Some(status) if (400..500).contains(&status) && status != 429 => Outcome::Neutral,
            // 5xx past the retry ladder, or no status at all
            // (connect/send-level failure): the destination is unwell.
            _ => Outcome::Failure,
        },
        // Exhaustive on purpose (same crate, `#[non_exhaustive]` doesn't
        // gate us): a future `ModelError` variant must make a conscious
        // choice here instead of silently inheriting a default.
    }
}

/// Extracts the HTTP status from a `ModelError::Request` message of the
/// shape `http_error.rs::http_error_to_model_error` produces —
/// `"{provider} HTTP {status}: {message}"`, where `{status}` displays as
/// e.g. `400 Bad Request`. Connect/send-level failures ("{provider}
/// request failed: ...") carry no status and return `None`. The format
/// coupling is pinned by `classify_reads_the_http_status_produced_by_
/// http_error_to_model_error` below.
fn http_status_in(message: &str) -> Option<u16> {
    let rest = message.split(" HTTP ").nth(1)?;
    rest.get(..3)?.parse().ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Closed,
    /// Short-circuiting every call. Carries when it opened, so `check()`
    /// can tell whether `open_duration` has elapsed.
    Open,
    /// Exactly one call has been let through to test recovery; every
    /// other caller sees `Open` until that probe resolves (or is
    /// abandoned — see `half_open_claimed_at`).
    HalfOpen,
}

struct Inner {
    state: State,
    /// Transport-class failures in a row (Neutral outcomes don't touch
    /// it, Success resets it). Compared against `trip_threshold`.
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    /// When the current HalfOpen probe was claimed. If a caller claims
    /// the probe (via `check()`) and never reports — a panic, a killed
    /// process, a dropped `Guard` — this timestamp lets a *later* caller
    /// reclaim the probe slot after [`PROBE_TIMEOUT`] instead of the
    /// breaker staying HalfOpen forever waiting for a report that will
    /// never come.
    half_open_claimed_at: Option<Instant>,
}

/// A single provider+destination+model's failure state. Constructed only
/// via [`breaker_for`] — never directly — so every caller for the same
/// key shares one instance.
pub(crate) struct CircuitBreaker {
    inner: Mutex<Inner>,
    trip_threshold: u32,
    open_duration: Duration,
    probe_timeout: Duration,
}

impl CircuitBreaker {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: State::Closed,
                consecutive_failures: 0,
                opened_at: None,
                half_open_claimed_at: None,
            }),
            trip_threshold: DEFAULT_TRIP_THRESHOLD,
            open_duration: DEFAULT_OPEN_DURATION,
            probe_timeout: PROBE_TIMEOUT,
        }
    }

    /// Whether a call may proceed right now. `Ok(())` means proceed
    /// (Closed, or this call is the one HalfOpen probe); `Err` means
    /// fail fast without touching the network.
    fn check(&self, key: &str) -> Result<(), ModelError> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();

        match inner.state {
            State::Closed => Ok(()),
            State::Open => {
                let opened_at = inner.opened_at.unwrap_or(now);
                if now.duration_since(opened_at) < self.open_duration {
                    return Err(ModelError::CircuitOpen(format!(
                        "circuit breaker for {key} open after {} consecutive \
                         transport failures — next probe in {:.0}s \
                         (BRAZE_CIRCUIT_BREAKER=off disables)",
                        inner.consecutive_failures,
                        (self.open_duration - now.duration_since(opened_at)).as_secs_f64()
                    )));
                }
                // Cooldown elapsed: transition to HalfOpen and claim the
                // probe for this call.
                inner.state = State::HalfOpen;
                inner.half_open_claimed_at = Some(now);
                tracing::info!(key, "circuit breaker half-open: letting one probe through");
                Ok(())
            }
            State::HalfOpen => {
                let claimed_at = inner.half_open_claimed_at.unwrap_or(now);
                if now.duration_since(claimed_at) < self.probe_timeout {
                    return Err(ModelError::CircuitOpen(format!(
                        "circuit breaker for {key} half-open — a probe call is \
                         already in flight"
                    )));
                }
                // The previous probe never reported back in time —
                // reclaim the slot rather than waiting forever.
                inner.half_open_claimed_at = Some(now);
                tracing::info!(key, "circuit breaker: reclaiming an abandoned half-open probe");
                Ok(())
            }
        }
    }

    /// Reports the outcome of a call [`Self::check`] allowed through.
    fn record(&self, outcome: Outcome, key: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());

        match inner.state {
            State::HalfOpen => match outcome {
                Outcome::Failure => {
                    inner.state = State::Open;
                    inner.opened_at = Some(Instant::now());
                    inner.half_open_claimed_at = None;
                    tracing::warn!(key, "circuit breaker: probe failed; reopening");
                }
                // Success — or any response at all: a 4xx proves the
                // destination is answering again.
                Outcome::Success | Outcome::Neutral => {
                    inner.state = State::Closed;
                    inner.consecutive_failures = 0;
                    inner.opened_at = None;
                    inner.half_open_claimed_at = None;
                    tracing::info!(key, "circuit breaker: probe succeeded; closed");
                }
            },
            State::Closed => match outcome {
                Outcome::Success => inner.consecutive_failures = 0,
                Outcome::Neutral => {}
                Outcome::Failure => {
                    inner.consecutive_failures += 1;
                    if inner.consecutive_failures >= self.trip_threshold {
                        inner.state = State::Open;
                        inner.opened_at = Some(Instant::now());
                        tracing::warn!(
                            key,
                            failures = inner.consecutive_failures,
                            cooldown_s = self.open_duration.as_secs(),
                            "circuit breaker tripped open"
                        );
                    }
                }
            },
            State::Open => {
                // A call that raced past `check()` just as the breaker
                // opened (lock released between two calls' `check()` and
                // `record()`) — harmless, the state is already what it
                // should be.
            }
        }
    }
}

/// Process-wide registry, one [`CircuitBreaker`] per key — see
/// [`breaker_for`].
static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<CircuitBreaker>>>> = OnceLock::new();

/// Returns the shared breaker for `key` (`"{provider}:{url}:{model}"`,
/// with the URL already normalized by the caller — `breaker_for` does
/// not re-normalize), creating it on first use. Every backend instance
/// built against the same destination — even a brand new one, as
/// `braze-bench` constructs per task — gets the same breaker.
pub(crate) fn breaker_for(key: &str) -> Arc<CircuitBreaker> {
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|p| p.into_inner());
    Arc::clone(
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(CircuitBreaker::new())),
    )
}

/// `BRAZE_CIRCUIT_BREAKER=off|0|false` disables the breaker for the
/// whole process. Read once — a mid-process flip is not supported (and
/// not useful: sweeps set their environment up front).
fn disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        matches!(
            std::env::var("BRAZE_CIRCUIT_BREAKER").ok().as_deref(),
            Some("off" | "0" | "false")
        )
    })
}

/// Checks the breaker for `key` and, if the call may proceed, returns a
/// [`Guard`] the caller must report the *terminal* outcome to — after
/// the stream ends, not when headers arrive. Fails fast with
/// `ModelError::CircuitOpen` when the breaker is open.
pub(crate) fn acquire(key: &str) -> Result<Guard, ModelError> {
    if disabled() {
        return Ok(Guard {
            breaker: None,
            key: String::new(),
        });
    }
    let breaker = breaker_for(key);
    breaker.check(key)?;
    Ok(Guard {
        breaker: Some(breaker),
        key: key.to_string(),
    })
}

/// One admitted call's reporting handle. Consuming (`observe_ok` /
/// `observe_err` take `self`) so an outcome can only be reported once;
/// dropping it without reporting records *nothing* — deliberate, so a
/// caller-side cancellation (user hit Esc, future dropped mid-stream)
/// doesn't count against the destination. An abandoned HalfOpen probe is
/// reclaimed by [`PROBE_TIMEOUT`], not by `Drop`.
pub(crate) struct Guard {
    breaker: Option<Arc<CircuitBreaker>>,
    key: String,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Guard {
    /// The call completed end-to-end (terminal stream event seen).
    pub(crate) fn observe_ok(self) {
        if let Some(breaker) = self.breaker {
            breaker.record(Outcome::Success, &self.key);
        }
    }

    /// The call failed — at send time, at status-check time, or
    /// mid-stream. [`classify`] decides whether it counts.
    pub(crate) fn observe_err(self, err: &ModelError) {
        if let Some(breaker) = self.breaker {
            breaker.record(classify(err), &self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_breaker() -> CircuitBreaker {
        CircuitBreaker::new()
    }

    fn fail(breaker: &CircuitBreaker) {
        breaker.check("test").expect("closed");
        breaker.record(Outcome::Failure, "test");
    }

    /// Below the trip threshold, an unbroken run of failures must not
    /// trip the breaker — a cold-started backend's first couple of
    /// retries failing must not immediately look like a sustained outage.
    #[test]
    fn stays_closed_below_the_trip_threshold() {
        let breaker = fresh_breaker();
        for _ in 0..(DEFAULT_TRIP_THRESHOLD - 1) {
            fail(&breaker);
        }
        assert!(breaker.check("test").is_ok(), "must still be closed");
    }

    /// Consecutive transport failures at the threshold trip the breaker
    /// and every subsequent `check()` fails fast without needing the
    /// caller to actually invoke the backend. No time window is involved
    /// — this is what makes the breaker able to open on *slow* failures
    /// (a 600s-read-timeout hang per sample) too, K-1a.
    #[test]
    fn trips_open_at_the_consecutive_failure_threshold() {
        let breaker = fresh_breaker();
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            fail(&breaker);
        }
        let err = breaker.check("test").expect_err("must be open now");
        assert!(
            matches!(err, ModelError::CircuitOpen(_)),
            "expected CircuitOpen, got {err:?}"
        );
    }

    /// A success anywhere in the run resets the consecutive count — an
    /// occasionally-flaky-but-working backend never trips.
    #[test]
    fn a_success_resets_the_consecutive_count() {
        let breaker = fresh_breaker();
        for _ in 0..10 {
            for _ in 0..(DEFAULT_TRIP_THRESHOLD - 1) {
                fail(&breaker);
            }
            breaker.check("test").expect("closed");
            breaker.record(Outcome::Success, "test");
        }
        assert!(breaker.check("test").is_ok());
    }

    /// Neutral outcomes (deterministic 4xx: "model does not support
    /// tools", bad auth) never trip the breaker no matter how many
    /// arrive in a row — K-1c's core requirement: one misconfigured
    /// sweep arm must not starve every other arm on the same server.
    #[test]
    fn neutral_outcomes_never_trip() {
        let breaker = fresh_breaker();
        for _ in 0..(DEFAULT_TRIP_THRESHOLD * 4) {
            breaker.check("test").expect("closed");
            breaker.record(Outcome::Neutral, "test");
        }
        assert!(breaker.check("test").is_ok());
    }

    /// ...and Neutral doesn't *reset* the count either: transport
    /// failures interleaved with 4xx still accumulate to a trip.
    #[test]
    fn neutral_outcomes_do_not_reset_the_failure_count() {
        let breaker = fresh_breaker();
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            fail(&breaker);
            // These would reset the count if Neutral were treated as
            // Success — the final check below would then pass.
            if breaker.check("test").is_ok() {
                breaker.record(Outcome::Neutral, "test");
            }
        }
        assert!(breaker.check("test").is_err(), "must have tripped");
    }

    /// A breaker that trips, then gets a successful probe after the
    /// cooldown, closes again — the whole point of HalfOpen is automatic
    /// recovery without restarting the process.
    #[test]
    fn recovers_to_closed_after_a_successful_half_open_probe() {
        let mut breaker = fresh_breaker();
        breaker.open_duration = Duration::from_millis(1);
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            fail(&breaker);
        }
        breaker.check("test").expect_err("open");

        std::thread::sleep(Duration::from_millis(5));
        breaker
            .check("test")
            .expect("cooldown elapsed — this call is the half-open probe");
        breaker.record(Outcome::Success, "test");

        breaker.check("test").expect("closed again after a healthy probe");
    }

    /// A probe that comes back with a deterministic 4xx proves the
    /// destination is answering — that closes the breaker too.
    #[test]
    fn a_neutral_probe_outcome_closes_the_breaker() {
        let mut breaker = fresh_breaker();
        breaker.open_duration = Duration::from_millis(1);
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            fail(&breaker);
        }
        breaker.check("test").expect_err("open");

        std::thread::sleep(Duration::from_millis(5));
        breaker.check("test").expect("half-open probe allowed through");
        breaker.record(Outcome::Neutral, "test");

        breaker.check("test").expect("closed — the destination answered");
    }

    /// A failed probe reopens the breaker (and restarts its own
    /// cooldown) instead of leaving it half-open indefinitely.
    #[test]
    fn a_failed_half_open_probe_reopens_the_breaker() {
        let mut breaker = fresh_breaker();
        breaker.open_duration = Duration::from_millis(1);
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            fail(&breaker);
        }
        breaker.check("test").expect_err("open");

        std::thread::sleep(Duration::from_millis(5));
        breaker.check("test").expect("half-open probe allowed through");
        breaker.record(Outcome::Failure, "test");

        let err = breaker.check("test").expect_err("must be open again");
        assert!(matches!(err, ModelError::CircuitOpen(_)));
    }

    /// While a probe is in flight, other callers are rejected — but the
    /// slot is reclaimable after `probe_timeout` (the claimant may have
    /// died without reporting).
    #[test]
    fn an_abandoned_probe_slot_is_reclaimable_after_the_probe_timeout() {
        let mut breaker = fresh_breaker();
        breaker.open_duration = Duration::from_millis(1);
        breaker.probe_timeout = Duration::from_millis(10);
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            fail(&breaker);
        }
        std::thread::sleep(Duration::from_millis(5));
        breaker.check("test").expect("first probe claimed");
        breaker
            .check("test")
            .expect_err("second caller rejected while the probe is in flight");

        std::thread::sleep(Duration::from_millis(15));
        breaker
            .check("test")
            .expect("probe slot reclaimed after probe_timeout");
    }

    /// Two different keys never share state — a dead Ollama must not
    /// trip the breaker guarding Anthropic calls, and (now that the key
    /// includes the model tag) a broken model must not trip the breaker
    /// guarding healthy models on the same server.
    #[test]
    fn different_keys_get_independent_breakers() {
        let a = breaker_for("test-independent-a");
        let b = breaker_for("test-independent-b");
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            a.check("a").expect("closed");
            a.record(Outcome::Failure, "a");
        }
        a.check("a").expect_err("a is open");
        b.check("b").expect("b is untouched and still closed");
    }

    /// The same key always resolves to the same breaker instance — this
    /// is the entire mechanism that lets a fresh `braze-bench` task's
    /// brand-new backend instance still see a previous task's failures.
    #[test]
    fn the_same_key_returns_the_same_breaker_across_calls() {
        let first = breaker_for("test-same-key-identity");
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            first.check("k").expect("closed");
            first.record(Outcome::Failure, "k");
        }
        let second = breaker_for("test-same-key-identity");
        second
            .check("k")
            .expect_err("must see the first handle's failures — same breaker");
    }

    /// `acquire`/`Guard` is the actual integration point every backend
    /// calls through — an open breaker rejects at `acquire` time, before
    /// any network work.
    #[test]
    fn acquire_fails_fast_once_open() {
        let key = "test-acquire-fails-fast";
        for _ in 0..DEFAULT_TRIP_THRESHOLD {
            let guard = acquire(key).expect("closed");
            guard.observe_err(&ModelError::Request("boom".to_string()));
        }
        let err = acquire(key).expect_err("must be open");
        assert!(matches!(err, ModelError::CircuitOpen(_)));
    }

    /// A dropped `Guard` records nothing: caller-side cancellation must
    /// not count against the destination.
    #[test]
    fn a_dropped_guard_records_nothing() {
        let key = "test-dropped-guard";
        for _ in 0..(DEFAULT_TRIP_THRESHOLD * 4) {
            let _guard = acquire(key).expect("closed");
            // dropped here without observe_*
        }
        assert!(acquire(key).is_ok(), "still closed — drops don't count");
    }

    // ------------------------------------------------------------------
    // classify(): which errors count toward tripping.
    // ------------------------------------------------------------------

    /// Pins the format coupling with `http_error_to_model_error`
    /// (`"{provider} HTTP {status}: {message}"`, status displayed as
    /// `400 Bad Request`): deterministic 4xx are Neutral, 5xx and
    /// status-less send failures count, mid-stream errors count.
    #[test]
    fn classify_reads_the_http_status_produced_by_http_error_to_model_error() {
        // The real-world case that motivated K-1c: gemma3:1b on Ollama.
        let unsupported = ModelError::Request(
            "ollama HTTP 400 Bad Request: \"gemma3:1b\" does not support tools".to_string(),
        );
        assert_eq!(classify(&unsupported), Outcome::Neutral);

        let auth = ModelError::Request("anthropic HTTP 401 Unauthorized: bad key".to_string());
        assert_eq!(classify(&auth), Outcome::Neutral);

        let server = ModelError::Request(
            "openrouter HTTP 500 Internal Server Error: upstream".to_string(),
        );
        assert_eq!(classify(&server), Outcome::Failure);

        let connect = ModelError::Request(
            "ollama request failed: error sending request for url".to_string(),
        );
        assert_eq!(classify(&connect), Outcome::Failure);

        let mid_stream = ModelError::StreamError("transport error: connection reset".to_string());
        assert_eq!(classify(&mid_stream), Outcome::Failure);

        let rate_limited = ModelError::RateLimited("anthropic rate-limited (HTTP 429)".to_string());
        assert_eq!(classify(&rate_limited), Outcome::Neutral);

        let decode = ModelError::Decode("invalid JSON".to_string());
        assert_eq!(classify(&decode), Outcome::Neutral);

        let circuit = ModelError::CircuitOpen("open".to_string());
        assert_eq!(classify(&circuit), Outcome::Neutral);
    }
}
