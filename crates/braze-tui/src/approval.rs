//! [`ChannelConfirmationPrompt`]: the real approval flow for `--tui`,
//! replacing oleada 2's `AutoDenyConfirmationPrompt` stopgap now that the
//! app loop (`app.rs`) has an overlay to answer from. Mirrors
//! `braze-cli::TerminalConfirmationPrompt` closely — same
//! `PermissionRequested`/`PermissionDecided` persistence for `--resume`
//! replay — but asks the question over a channel to the TUI's event loop
//! instead of blocking on stdin (which doesn't work once the terminal is
//! in raw mode: no canonical-mode line editing, Enter sends `\r` not
//! `\n`).

use std::sync::{Arc, Mutex};

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

/// N-12 (docs/AUDITORIA-2026-07-v2.md): `session` is a *shared, mutable*
/// handle rather than a plain `SessionId` — `App::backtrack_to` writes a
/// fresh id into the same `Arc<Mutex<_>>` every `PermissionGuard` this
/// binary built was given (see `braze-cli::build_permission_guard`).
/// Without this, every `PermissionRequested`/`PermissionDecided` event
/// this prompt persists after a backtrack would keep landing in the
/// *pre-backtrack* session's rollout log forever: `--resume` on the new
/// session would find no permission history at all (re-asking
/// everything), while the old session — which the backtrack design
/// promises stays untouched and independently `--resume`-able — would
/// silently accumulate permission events from turns that never happened
/// in its own history.
pub struct ChannelConfirmationPrompt {
    session: Arc<Mutex<SessionId>>,
    store: Arc<dyn braze_session::SessionStore>,
    tx: mpsc::UnboundedSender<ApprovalRequest>,
}

impl ChannelConfirmationPrompt {
    pub fn new(
        session: Arc<Mutex<SessionId>>,
        store: Arc<dyn braze_session::SessionStore>,
        tx: mpsc::UnboundedSender<ApprovalRequest>,
    ) -> Self {
        Self { session, store, tx }
    }

    /// The session id to persist against for *this* `confirm()` call —
    /// read once and reused for both the `PermissionRequested` and
    /// `PermissionDecided` events it appends, so the pair always lands
    /// together even if a backtrack could somehow race between them (in
    /// practice it can't: backtrack is only reachable while idle, and a
    /// confirmation only happens mid-turn — see `app.rs`'s `on_key`,
    /// which routes every key to the pending-approval branch first while
    /// one is outstanding).
    fn current_session(&self) -> SessionId {
        *self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let session = self.current_session();

        if let Err(err) = self
            .store
            .append(
                &session,
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
                &session,
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
        let prompt =
            ChannelConfirmationPrompt::new(Arc::new(Mutex::new(session)), Arc::clone(&store), tx);

        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/x"),
        };
        let confirm = tokio::spawn(async move { prompt.confirm(&action).await });

        let request = rx.recv().await.expect("expected an ApprovalRequest");
        assert_eq!(request.description, "delete file /tmp/x");
        request.respond.send(true).expect("respond channel open");

        assert!(confirm.await.expect("task join"));

        let events = store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::PermissionRequested { .. }));
        match &events[1] {
            AgentEvent::PermissionDecided { allowed, .. } => assert!(*allowed),
            other => panic!("expected PermissionDecided, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-12 (docs/AUDITORIA-2026-07-v2.md): after
    /// something (in production, `App::backtrack_to`) writes a new id
    /// into the shared `session` handle, the *next* `confirm()` call must
    /// persist against the new session — not keep appending to the one
    /// the prompt was originally constructed with.
    #[tokio::test]
    async fn persists_against_whatever_session_the_shared_handle_points_to_now() {
        let (store, dir) = temp_store();
        let original_session = SessionId::new();
        let new_session = SessionId::new();
        let live_session = Arc::new(Mutex::new(original_session));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prompt =
            ChannelConfirmationPrompt::new(Arc::clone(&live_session), Arc::clone(&store), tx);

        // Simulates a backtrack switching the session before the next
        // confirmation happens.
        *live_session.lock().unwrap() = new_session;

        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/x"),
        };
        let confirm = tokio::spawn(async move { prompt.confirm(&action).await });
        let request = rx.recv().await.expect("expected an ApprovalRequest");
        request.respond.send(true).expect("respond channel open");
        assert!(confirm.await.expect("task join"));

        let new_events = store
            .load(&new_session)
            .await
            .expect("load events for the new session");
        assert_eq!(
            new_events.len(),
            2,
            "expected both events on the new session"
        );

        let original_events = store.load(&original_session).await;
        assert!(
            matches!(
                original_events,
                Err(braze_session::SessionError::NotFound(_))
            ),
            "the original session must stay untouched, got: {original_events:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_dropped_receiver_denies_instead_of_hanging() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx); // app loop already gone

        let prompt =
            ChannelConfirmationPrompt::new(Arc::new(Mutex::new(session)), Arc::clone(&store), tx);
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
        let prompt =
            ChannelConfirmationPrompt::new(Arc::new(Mutex::new(session)), Arc::clone(&store), tx);

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
