//! [`ChannelTaskNotifier`]: the concrete [`TaskNotifier`] implementation
//! this crate ships, backed by `tokio::spawn` + `tokio::sync::mpsc` —
//! exactly the "background dispatch + push notification" responsibility
//! this crate declares in PLAN.md.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use braze_types::ToolResult;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::notify::{BackgroundTask, TaskHandle, TaskNotifier};

/// `tokio::spawn` per task + an unbounded mpsc completion channel.
/// `next_completed` needs `&mut self` on the receiver but the
/// [`TaskNotifier`] trait only gives us `&self`, hence the
/// [`tokio::sync::Mutex`] around the receiver half.
///
/// Also tracks each task's `JoinHandle` (a plain [`std::sync::Mutex`] —
/// only ever locked for a synchronous map operation, never held across an
/// `.await`) so [`TaskNotifier::abort`] and `Drop` can actually cancel
/// still-running work instead of just forgetting about it — N-33,
/// docs/AUDITORIA-2026-07-v2.md.
pub struct ChannelTaskNotifier {
    tx: UnboundedSender<(TaskHandle, ToolResult)>,
    rx: Mutex<UnboundedReceiver<(TaskHandle, ToolResult)>>,
    next_handle: AtomicU64,
    handles: std::sync::Mutex<HashMap<TaskHandle, JoinHandle<()>>>,
}

impl ChannelTaskNotifier {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Mutex::new(rx),
            next_handle: AtomicU64::new(0),
            handles: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl Default for ChannelTaskNotifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskNotifier for ChannelTaskNotifier {
    fn spawn(&self, task: BackgroundTask) -> TaskHandle {
        let handle = TaskHandle(self.next_handle.fetch_add(1, Ordering::SeqCst));
        let tx = self.tx.clone();
        let join = tokio::spawn(async move {
            let result = task.work.await;
            // If the receiver has been dropped there is nobody left to
            // notify; nothing more to do with the result.
            let _ = tx.send((handle, result));
        });
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(handle, join);
        handle
    }

    async fn next_completed(&self, timeout: Duration) -> Option<(TaskHandle, ToolResult)> {
        let mut rx = self.rx.lock().await;
        let completed = tokio::time::timeout(timeout, rx.recv())
            .await
            .ok()
            .flatten();
        if let Some((handle, _)) = &completed {
            // Already finished (that's how we got a completion for it) —
            // just stop tracking it, nothing to abort.
            self.handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(handle);
        }
        completed
    }

    fn abort(&self, handle: TaskHandle) {
        if let Some(join) = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&handle)
        {
            join.abort();
        }
    }
}

impl Drop for ChannelTaskNotifier {
    /// Anything still tracked here never got an explicit `abort()` or a
    /// chance to be collected by `next_completed` — most notably, the
    /// whole `Engine` (and this notifier along with it) getting dropped
    /// because an outer wall-clock timeout (e.g. `braze-bench`'s per-task
    /// budget) gave up on `run_turn` entirely, well before any background
    /// tool task finished. Aborting here is what actually stops the
    /// underlying `tokio::spawn`ed work — and, via `kill_on_drop` on any
    /// `tokio::process::Command` it may be awaiting — the child process,
    /// instead of leaving it to keep consuming CPU/RAM for the rest of a
    /// sweep. See N-33, docs/AUDITORIA-2026-07-v2.md.
    fn drop(&mut self) {
        let handles = self
            .handles
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (_, join) in handles.drain() {
            join.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn spawn_and_next_completed_roundtrip() {
        let notifier = ChannelTaskNotifier::new();
        let handle = notifier.spawn(BackgroundTask {
            label: "test".to_string(),
            work: Box::pin(async {
                ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "done".to_string(),
                    is_error: false,
                }
            }),
        });

        let (completed_handle, result) = notifier
            .next_completed(Duration::from_secs(5))
            .await
            .expect("task should complete within timeout");

        assert_eq!(completed_handle, handle);
        assert_eq!(result.content, "done");
    }

    #[tokio::test]
    async fn next_completed_times_out_when_nothing_pending() {
        let notifier = ChannelTaskNotifier::new();
        let result = notifier.next_completed(Duration::from_millis(50)).await;
        assert!(result.is_none());
    }

    /// Regression test for N-33: `abort()` must actually stop the
    /// underlying `tokio::spawn`ed work, not just make this notifier
    /// forget about it. Proven via a flag the task only sets *after* a
    /// delay — if `abort` didn't really cancel the task, the flag would
    /// still flip to `true` once that delay elapses regardless.
    #[tokio::test]
    async fn abort_stops_a_task_before_it_sets_its_completion_flag() {
        let notifier = ChannelTaskNotifier::new();
        let ran_to_completion = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran_to_completion);

        let handle = notifier.spawn(BackgroundTask {
            label: "slow".to_string(),
            work: Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                flag.store(true, Ordering::SeqCst);
                ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "done".to_string(),
                    is_error: false,
                }
            }),
        });

        notifier.abort(handle);

        // Longer than the task's own delay — if abort hadn't really
        // cancelled it, the flag would be true by now.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !ran_to_completion.load(Ordering::SeqCst),
            "aborted task kept running to completion"
        );
    }

    /// Regression test for N-33: dropping the notifier itself (e.g. the
    /// whole `Engine` going out of scope because a caller's outer
    /// wall-clock timeout gave up on the turn) must abort every task still
    /// tracked, not just ones explicitly aborted one at a time.
    #[tokio::test]
    async fn dropping_the_notifier_aborts_every_still_pending_task() {
        let ran_to_completion = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran_to_completion);

        {
            let notifier = ChannelTaskNotifier::new();
            notifier.spawn(BackgroundTask {
                label: "slow".to_string(),
                work: Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    flag.store(true, Ordering::SeqCst);
                    ToolResult {
                        tool_call_id: "call-1".to_string(),
                        content: "done".to_string(),
                        is_error: false,
                    }
                }),
            });
            // `notifier` drops here, at the end of this block.
        }

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !ran_to_completion.load(Ordering::SeqCst),
            "task kept running after its notifier was dropped"
        );
    }
}
