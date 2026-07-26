//! H-19 (docs/AUDITORIA-2026-07-v5.md): jittered-backoff retry for the
//! INITIAL request of a cloud backend's completion call — a transient
//! 429/5xx blip used to abort the whole turn (and, in a sweep, count as
//! a model failure: the 3-arm A/B of 2026-07-10 logged transient
//! `model_backend_error` rows contaminating `error_recovery`'s
//! measurement, exactly the F5 "harness failures ≠ model failures"
//! distinction).
//!
//! Deliberately scoped to the pre-stream phase: once a stream has
//! started, a mid-stream failure surfaces as `ModelError::StreamError`
//! and is NOT retried here — replaying a partially-consumed completion
//! would need idempotency the providers don't offer.
//!
//! Ollama gets no retry, per the v5 dictamen: hammering a saturated
//! local backend doesn't help (the failure mode is resource exhaustion,
//! not a transient network blip), so `OllamaBackend` simply doesn't call
//! this helper.

use std::time::Duration;

use crate::error::ModelError;
use crate::http_error::http_error_to_model_error;

/// Retries after the first attempt — 4 total attempts at the default.
/// The value from H-19's arreglo; overridable per backend via
/// `with_max_retries` (0 = single attempt, old behavior).
pub(crate) const DEFAULT_MAX_RETRIES: u32 = 3;

/// Base of the exponential backoff (500ms, 1s, 2s + jitter). Small
/// enough that a single blip costs sub-second latency, large enough that
/// three retries actually span a provider hiccup.
const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(500);

/// Ceiling on any single sleep, including one requested via
/// `Retry-After` — a provider asking us to wait minutes is a signal to
/// fail the turn and let the caller decide, not to block silently.
const MAX_DELAY: Duration = Duration::from_secs(15);

/// Sends `build()`'s request up to `1 + max_retries` times, retrying on
/// transient failures only: connect/send errors, HTTP 429, and HTTP 5xx.
/// A 4xx other than 429 is not transient (bad request/auth) and fails
/// immediately. On a retried 429, a `Retry-After: <seconds>` header is
/// honored (capped at [`MAX_DELAY`]); otherwise exponential backoff with
/// per-attempt jitter derived from the clock's subsecond nanos — enough
/// to decorrelate concurrent best-of-n candidates without pulling in a
/// `rand` dependency for one number.
///
/// `build` is a closure (not a pre-built request) because
/// `reqwest::RequestBuilder` is consumed by `send()` — each attempt
/// needs a fresh one.
pub(crate) async fn send_with_retry(
    provider: &str,
    max_retries: u32,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, ModelError> {
    let mut attempt = 0u32;
    loop {
        let send_result = build().send().await;
        let retries_left = max_retries.saturating_sub(attempt);

        match send_result {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status().as_u16();
                let transient = status == 429 || (500..600).contains(&status);
                if !transient || retries_left == 0 {
                    return Err(http_error_to_model_error(response, provider).await);
                }
                let delay = retry_after_seconds(&response)
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| backoff_delay(attempt))
                    .min(MAX_DELAY);
                tracing::warn!(
                    provider,
                    status,
                    attempt,
                    retries_left,
                    delay_ms = delay.as_millis() as u64,
                    "transient HTTP status; retrying the initial request (H-19)"
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => {
                if retries_left == 0 {
                    return Err(ModelError::Request(format!(
                        "{provider} request failed: {err}"
                    )));
                }
                let delay = backoff_delay(attempt).min(MAX_DELAY);
                tracing::warn!(
                    provider,
                    error = %err,
                    attempt,
                    retries_left,
                    delay_ms = delay.as_millis() as u64,
                    "request send failed; retrying (H-19)"
                );
                tokio::time::sleep(delay).await;
            }
        }
        attempt += 1;
    }
}

/// `Retry-After` in its delta-seconds form (the HTTP-date form is rare
/// from these providers and not worth a date parser — unparseable values
/// just fall back to exponential backoff).
fn retry_after_seconds(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// `base * 2^attempt` plus up to ~25% jitter from the clock's subsecond
/// nanos — decorrelates retries from concurrent callers (best-of-n
/// candidates fire together) without a `rand` dependency.
fn backoff_delay(attempt: u32) -> Duration {
    let base = DEFAULT_BASE_DELAY.saturating_mul(2u32.saturating_pow(attempt));
    let jitter_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let jitter = Duration::from_millis(jitter_nanos % (base.as_millis().max(4) as u64 / 4));
    base + jitter
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single 429 followed by a healthy response: the retry absorbs
    /// the blip and the caller never sees an error.
    #[tokio::test]
    async fn a_transient_429_is_absorbed_by_one_retry() {
        let addr = crate::test_support::spawn_sequenced_http_server(vec![
            (
                429,
                "application/json",
                br#"{"error":"slow down"}"#.to_vec(),
            ),
            (200, "application/json", br#"{"ok":true}"#.to_vec()),
        ])
        .await;

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/");
        let response = send_with_retry("test", 3, || client.post(&url).body("{}"))
            .await
            .expect("the second attempt must succeed");
        assert!(response.status().is_success());
    }

    /// Retries exhausted on a persistent 429 → the terminal error is the
    /// same `RateLimited` a no-retry caller would have gotten, so
    /// downstream mapping/classification is unchanged.
    #[tokio::test]
    async fn a_persistent_429_still_maps_to_rate_limited_after_retries() {
        let addr = crate::test_support::spawn_sequenced_http_server(vec![
            (
                429,
                "application/json",
                br#"{"error":"slow down"}"#.to_vec(),
            ),
            (
                429,
                "application/json",
                br#"{"error":"slow down"}"#.to_vec(),
            ),
        ])
        .await;

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/");
        let err = send_with_retry("test", 1, || client.post(&url).body("{}"))
            .await
            .expect_err("both attempts 429 → error");
        assert!(
            matches!(err, ModelError::RateLimited(_)),
            "expected RateLimited, got {err:?}"
        );
    }

    /// A non-429 4xx (bad auth, malformed request) is NOT transient —
    /// exactly one attempt, no useless retries against a request that
    /// can never succeed.
    #[tokio::test]
    async fn a_401_fails_immediately_without_retries() {
        let addr = crate::test_support::spawn_sequenced_http_server(vec![(
            401,
            "application/json",
            br#"{"error":"bad key"}"#.to_vec(),
        )])
        .await;

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/");
        let err = send_with_retry("test", 3, || client.post(&url).body("{}"))
            .await
            .expect_err("401 is terminal");
        assert!(
            matches!(err, ModelError::Request(_)),
            "expected Request, got {err:?}"
        );
        // The sequenced server only had one response scripted — a retry
        // would have hung/errored differently, so reaching here at all
        // proves single-attempt behavior.
    }

    /// A 500 is transient — absorbed like the 429, minus Retry-After.
    #[tokio::test]
    async fn a_transient_500_is_absorbed_by_one_retry() {
        let addr = crate::test_support::spawn_sequenced_http_server(vec![
            (500, "application/json", br#"{"error":"boom"}"#.to_vec()),
            (200, "application/json", br#"{"ok":true}"#.to_vec()),
        ])
        .await;

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/");
        let response = send_with_retry("test", 3, || client.post(&url).body("{}"))
            .await
            .expect("the second attempt must succeed");
        assert!(response.status().is_success());
    }
}
