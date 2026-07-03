//! Test-only helper: a minimal one-shot HTTP/1.1 server over
//! `tokio::net::TcpListener` that serves a single canned response, so
//! `AnthropicBackend`/`OllamaBackend` can be tested end-to-end (SSE/NDJSON
//! streaming, HTTP error mapping) without any network-mocking crate — none
//! is a workspace dependency and the task asked not to add one unless
//! genuinely necessary.
//!
//! Deliberately dumb: it does not parse the incoming request beyond
//! draining enough bytes to unblock the client's write, and it serves
//! exactly one connection then exits. That's all these tests need.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Spawns a background task that accepts exactly one TCP connection,
/// drains the incoming request (best-effort), then writes a complete
/// HTTP/1.1 response built from `status`, `content_type`, and `body`.
/// Returns the address to connect to.
pub(crate) async fn spawn_canned_http_server(
    status: u16,
    content_type: &str,
    body: Vec<u8>,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind test listener");
    let addr = listener.local_addr().expect("failed to read local addr");

    let content_type = content_type.to_string();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };

        // Best-effort drain of the request. Small JSON request bodies from
        // this crate's tests arrive in one or two reads; we don't need to
        // parse them, only to avoid leaving the client's write half
        // blocked.
        let mut scratch = [0u8; 8192];
        let mut seen = Vec::new();
        loop {
            let read = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                socket.read(&mut scratch),
            )
            .await;
            match read {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => {
                    seen.extend_from_slice(&scratch[..n]);
                    if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Ok(Err(_)) => break,
            }
        }

        let status_line = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
        let headers = format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );

        let mut response = Vec::new();
        response.extend_from_slice(status_line.as_bytes());
        response.extend_from_slice(headers.as_bytes());
        response.extend_from_slice(&body);

        let _ = socket.write_all(&response).await;
        let _ = socket.shutdown().await;
    });

    addr
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}
