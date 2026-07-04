//! [`ChannelTaskNotifier`]: the concrete [`TaskNotifier`] implementation
//! this crate ships, backed by `tokio::spawn` + `tokio::sync::mpsc` —
//! exactly the "background dispatch + push notification" responsibility
//! this crate declares in PLAN.md.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use braze_types::ToolResult;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::notify::{BackgroundTask, TaskHandle, TaskNotifier};

/// `tokio::spawn` per task + an unbounded mpsc completion channel.
/// `next_completed` needs `&mut self` on the receiver but the
/// [`TaskNotifier`] trait only gives us `&self`, hence the
/// [`tokio::sync::Mutex`] around the receiver half.
pub struct ChannelTaskNotifier {
    tx: UnboundedSender<(TaskHandle, ToolResult)>,
    rx: Mutex<UnboundedReceiver<(TaskHandle, ToolResult)>>,
    next_handle: AtomicU64,
}

impl ChannelTaskNotifier {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Mutex::new(rx),
            next_handle: AtomicU64::new(0),
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
        tokio::spawn(async move {
            let result = task.work.await;
            // If the receiver has been dropped there is nobody left to
            // notify; nothing more to do with the result.
            let _ = tx.send((handle, result));
        });
        handle
    }

    async fn next_completed(&self, timeout: Duration) -> Option<(TaskHandle, ToolResult)> {
        let mut rx = self.rx.lock().await;
        tokio::time::timeout(timeout, rx.recv())
            .await
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
