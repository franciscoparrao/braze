//! [`ChannelTaskNotifier`]: the concrete [`TaskNotifier`] this binary uses.
//!
//! This responsibility ("dispatch de tareas en background vía
//! `tokio::spawn` + `tokio::sync::mpsc`, notificación push no polling") is
//! declared as `braze-events`'s in PLAN.md, and it would be more natural
//! for this type to live there. It is implemented here instead, for
//! simplicity in this integration phase — `braze-cli` is currently the
//! only binary that needs a `TaskNotifier`, so there is no shared-code
//! pressure yet. If a second binary (e.g. a future TUI) needs one too,
//! move this into `braze-events` so both can depend on it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use braze_events::{BackgroundTask, TaskHandle, TaskNotifier};
use braze_types::ToolResult;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

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
