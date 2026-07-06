//! Shared `reqwest::Client` construction for all three backends (N-20,
//! docs/AUDITORIA-2026-07-v2.md).
//!
//! `reqwest::Client::new()` sets neither a connect timeout nor a read
//! timeout — a server that accepts the TCP connection and then never
//! sends anything (or sends a few bytes and then stalls) blocks the
//! request forever, with no error and no way for the caller to recover
//! short of killing the process. `Engine`'s streaming loop has no
//! surrounding timeout of its own for model calls (only tool dispatch is
//! bounded), so this is the only place such a hang gets cut off.

use std::time::Duration;

/// Connect-phase timeout: generous for a real network round-trip
/// (including TLS) to any of the three backends, but still catches a
/// firewall black-holing the connection outright.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Read timeout: resets after *every* successful read (see
/// [`reqwest::ClientBuilder::read_timeout`]'s doc comment), so it bounds
/// how long the connection may go completely silent, not the total
/// request duration — a long but actively-streaming generation is never
/// affected. Set well above the worst-case latency this project has
/// actually observed for a CPU-only Ollama model under load (180-400s
/// per turn, PLAN.md § "Verificación end-to-end") so a slow-but-healthy
/// local model is never mistaken for a stalled connection.
const READ_TIMEOUT: Duration = Duration::from_secs(600);

/// Builds the `reqwest::Client` every backend constructor should use
/// instead of `reqwest::Client::new()`. Panics only if the underlying
/// TLS backend fails to initialize, exactly like `reqwest::Client::new()`
/// already does — this seam does not add a new fallible path.
pub(crate) fn build_client() -> reqwest::Client {
    build_client_with_timeouts(CONNECT_TIMEOUT, READ_TIMEOUT)
}

/// Parameterized on the actual durations so a test can verify the
/// timeout *wiring* (does a stalled connection actually get cut off?)
/// against a millisecond-scale bound instead of waiting out the real
/// production values (up to 600s). Also used directly by
/// `ollama::list_ollama_models`, whose non-streaming metadata request
/// deserves a much tighter read bound than a model generation.
pub(crate) fn build_client_with_timeouts(connect: Duration, read: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .read_timeout(read)
        .build()
        .expect("reqwest client with connect/read timeouts should always build")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for N-20 (docs/AUDITORIA-2026-07-v2.md): a
    /// connection that never sends a single byte after being accepted
    /// must eventually error out, not hang forever — proving the
    /// `read_timeout` wiring is actually effective (a bare
    /// `reqwest::Client::new()` would hang here indefinitely).
    #[tokio::test]
    async fn a_connection_that_never_responds_is_cut_off_by_the_read_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral local port");
        let addr = listener.local_addr().unwrap();

        // Accept the connection and then do nothing at all with it —
        // simulates a server that stalls after the TCP handshake.
        tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                // Keep the socket alive (dropping it would close the
                // connection, which is a different failure mode) without
                // ever writing a response.
                std::mem::forget(socket);
            }
        });

        let client =
            build_client_with_timeouts(Duration::from_millis(500), Duration::from_millis(200));

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.get(format!("http://{addr}/")).send(),
        )
        .await
        .expect("the request must fail on its own via read_timeout, not hang past our test bound");

        assert!(
            result.is_err(),
            "expected the stalled connection to time out"
        );
    }
}
