use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use braze_types::ToolResult;

/// Opaque handle to a task previously returned by [`TaskNotifier::spawn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskHandle(pub u64);

/// A unit of background work: a label for tracing/logging, and the future
/// that produces its eventual [`ToolResult`].
pub struct BackgroundTask {
    pub label: String,
    pub work: Pin<Box<dyn Future<Output = ToolResult> + Send>>,
}

/// Dispatches background work and notifies the caller on completion,
/// instead of requiring the caller to poll.
///
/// MVP implementation: `tokio::spawn` per task + a `tokio::sync::mpsc`
/// completion channel; `next_completed` awaits the channel (bounded by
/// `timeout` via `tokio::time::timeout`) rather than checking task status
/// in a loop.
#[async_trait::async_trait]
pub trait TaskNotifier: Send + Sync {
    /// Non-blocking: enqueue `task` and return its handle immediately.
    fn spawn(&self, task: BackgroundTask) -> TaskHandle;

    /// Blocks (up to `timeout`) on the next task to complete, or returns
    /// `None` on timeout. Called once per turn by the engine's main loop —
    /// never in a polling loop.
    async fn next_completed(&self, timeout: Duration) -> Option<(TaskHandle, ToolResult)>;

    /// Cancels a previously-spawned task if it hasn't completed yet — a
    /// no-op if `handle` already completed, was already aborted, or is
    /// unknown. Exists so a caller that gives up waiting on a task (e.g.
    /// `braze-engine`'s per-round completion timeout) can actually stop
    /// the underlying work instead of leaving it running unobserved —
    /// N-33, docs/AUDITORIA-2026-07-v2.md.
    fn abort(&self, handle: TaskHandle);
}
