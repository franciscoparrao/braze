//! [`ChannelConfirmationPrompt`]: the real approval flow for `--tui`,
//! replacing oleada 2's `AutoDenyConfirmationPrompt` stopgap now that the
//! app loop (`app.rs`) has an overlay to answer from. Mirrors
//! `braze-cli::TerminalConfirmationPrompt` closely — same
//! `PermissionRequested`/`PermissionDecided` persistence for `--resume`
//! replay — but asks the question over a channel to the TUI's event loop
//! instead of blocking on stdin (which doesn't work once the terminal is
//! in raw mode: no canonical-mode line editing, Enter sends `\r` not
//! `\n`).

use std::sync::Arc;

use async_trait::async_trait;
use braze_events::AgentEvent;
use braze_permissions::{ActionDescriptor, ConfirmationPrompt};
use braze_types::SessionId;
use tokio::sync::{mpsc, oneshot};

/// One pending confirmation, as the app loop sees it: a human-readable
/// description of the action (`ActionDescriptor`'s `Display`) and a
/// one-shot channel to send the answer back through. `respond` is
/// consumed exactly once — either by the user answering (`app.rs`'s
/// `answer_pending_approval`) or, if the app quits with this still
/// unanswered, by simply being dropped (whichever `confirm()` call is
/// awaiting it then sees a closed channel and denies — see
/// `ChannelConfirmationPrompt::confirm`'s safety-default handling).
pub struct ApprovalRequest {
    pub description: String,
    pub respond: oneshot::Sender<bool>,
}

pub struct ChannelConfirmationPrompt {
    session: SessionId,
    store: Arc<dyn braze_session::SessionStore>,
    tx: mpsc::UnboundedSender<ApprovalRequest>,
}

impl ChannelConfirmationPrompt {
    pub fn new(
        session: SessionId,
        store: Arc<dyn braze_session::SessionStore>,
        tx: mpsc::UnboundedSender<ApprovalRequest>,
    ) -> Self {
        Self { session, store, tx }
    }
}

#[async_trait]
impl ConfirmationPrompt for ChannelConfirmationPrompt {
    /// SAFETY DEFAULT (see `ConfirmationPrompt`'s doc comment): if the
    /// request can't be delivered (the app loop's receiver is gone —
    /// shutting down) or the answer channel closes without a reply (the
    /// app quit with this still pending), this returns `false`. Never
    /// treat an undeliverable/unanswered question as implicit allow.
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        let key = braze_permissions::derive_permission_key(action);

        if let Err(err) = self
            .store
            .append(
                &self.session,
                &AgentEvent::PermissionRequested {
                    action: action.to_string(),
                    reversible: false,
                    key: key.clone(),
                },
            )
            .await
        {
            tracing::warn!(error = %err, "failed to persist PermissionRequested event (non-fatal)");
        }

        let (respond_tx, respond_rx) = oneshot::channel();
        let request = ApprovalRequest {
            description: action.to_string(),
            respond: respond_tx,
        };
        let allowed = if self.tx.send(request).is_err() {
            false
        } else {
            respond_rx.await.unwrap_or(false)
        };

        if let Err(err) = self
            .store
            .append(
                &self.session,
                &AgentEvent::PermissionDecided {
                    action: action.to_string(),
                    allowed,
                    key,
                },
            )
            .await
        {
            tracing::warn!(error = %err, "failed to persist PermissionDecided event (non-fatal)");
        }

        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_store() -> (Arc<dyn braze_session::SessionStore>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "braze-tui-approval-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        (
            Arc::new(braze_session::FileSessionStore::new(dir.clone())),
            dir,
        )
    }

    #[tokio::test]
    async fn answering_yes_over_the_channel_allows_and_persists_both_events() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prompt = ChannelConfirmationPrompt::new(session, Arc::clone(&store), tx);

        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/x"),
        };
        let confirm = tokio::spawn(async move { prompt.confirm(&action).await });

        let request = rx.recv().await.expect("expected an ApprovalRequest");
        assert_eq!(request.description, "delete file /tmp/x");
        request.respond.send(true).expect("respond channel open");

        assert!(confirm.await.expect("task join"));

        let events = store.load(&session).await.expect("load events");
        assert!(matches!(
            events[0],
            AgentEvent::PermissionRequested { .. }
        ));
        match &events[1] {
            AgentEvent::PermissionDecided { allowed, .. } => assert!(*allowed),
            other => panic!("expected PermissionDecided, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_dropped_receiver_denies_instead_of_hanging() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx); // app loop already gone

        let prompt = ChannelConfirmationPrompt::new(session, Arc::clone(&store), tx);
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/x"),
        };
        assert!(!prompt.confirm(&action).await);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dropping_the_respond_sender_without_answering_denies() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prompt = ChannelConfirmationPrompt::new(session, Arc::clone(&store), tx);

        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/x"),
        };
        let confirm = tokio::spawn(async move { prompt.confirm(&action).await });

        let request = rx.recv().await.expect("expected an ApprovalRequest");
        drop(request); // simulates the app quitting with this still pending

        assert!(!confirm.await.expect("task join"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
