use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("request to model backend failed: {0}")]
    Request(String),

    #[error("model backend returned an unparseable response: {0}")]
    Decode(String),

    #[error("model backend rate-limited the request: {0}")]
    RateLimited(String),

    /// Something went wrong *after* the stream had already started —
    /// a transport error, a mid-stream provider error event (e.g.
    /// Anthropic's `overloaded_error`), or the connection closing before
    /// a terminal event was ever seen. Distinct from `Request` (which is
    /// reserved for failures before/while establishing the stream): this
    /// is what lets `Engine::run_turn` tell "the stream completed
    /// normally" apart from "the stream died partway through" instead of
    /// silently treating whatever partial text/tool-calls arrived as a
    /// complete, converged response.
    #[error("model backend's completion stream failed: {0}")]
    StreamError(String),

    /// A per-destination circuit breaker (`circuit_breaker.rs`) rejected
    /// this call without touching the network — enough recent calls to
    /// this same provider+URL failed that the breaker tripped, and its
    /// cooldown hasn't elapsed. Distinct from `Request`/`RateLimited`
    /// (which both mean "a real HTTP round-trip happened and failed")
    /// so a caller/log line can tell "the network said no" apart from
    /// "we didn't even ask, because it's been saying no repeatedly".
    #[error("{0}")]
    CircuitOpen(String),
}
