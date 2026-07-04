//! [`TerminalConfirmationPrompt`]: the real y/n confirmation prompt used
//! by the interactive/one-shot binary — the "capa de confirmación de
//! intención" callback [`braze_permissions::PermissionGuard`] blocks on
//! before letting an irreversible action through.
//!
//! Also persists a `PermissionRequested`/`PermissionDecided` pair (each
//! carrying the coarse `derive_permission_key` identity of the action, if
//! any) to the session's [`braze_session::SessionStore`] on every call —
//! best-effort, never fatal — so that a later `braze chat --resume`
//! replays previously-*approved* decisions back into a fresh
//! `PermissionGuard` (see `PermissionGuard::seed_remembered` and
//! `main.rs`) instead of asking again for the same session.

use std::sync::Arc;

use async_trait::async_trait;
use braze_permissions::{ActionDescriptor, ConfirmationPrompt};
use braze_types::SessionId;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct TerminalConfirmationPrompt {
    session: SessionId,
    store: Arc<dyn braze_session::SessionStore>,
}

impl TerminalConfirmationPrompt {
    pub fn new(session: SessionId, store: Arc<dyn braze_session::SessionStore>) -> Self {
        Self { session, store }
    }
}

#[async_trait]
impl ConfirmationPrompt for TerminalConfirmationPrompt {
    /// SAFETY DEFAULT (see `ConfirmationPrompt`'s doc comment): anything
    /// other than an unambiguous "yes" — including EOF or an I/O error on
    /// either stdout or stdin — returns `false`. Never treat a read
    /// failure as implicit allow.
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        let key = braze_permissions::derive_permission_key(action);

        if let Err(err) = self
            .store
            .append(
                &self.session,
                &braze_events::AgentEvent::PermissionRequested {
                    action: action.to_string(),
                    reversible: false,
                    key: key.clone(),
                },
            )
            .await
        {
            tracing::warn!(error = %err, "failed to persist PermissionRequested event (non-fatal)");
        }

        let mut stdout = tokio::io::stdout();
        let prompt = format!("{action}\n¿Permitir? [y/N]: ");

        let allowed = if stdout.write_all(prompt.as_bytes()).await.is_err()
            || stdout.flush().await.is_err()
        {
            false
        } else {
            let mut reader = BufReader::new(tokio::io::stdin());
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => false, // EOF: no definitive "yes" was ever read.
                Ok(_) => {
                    let answer = line.trim().to_ascii_lowercase();
                    answer == "y" || answer == "yes"
                }
                Err(_) => false,
            }
        };

        if let Err(err) = self
            .store
            .append(
                &self.session,
                &braze_events::AgentEvent::PermissionDecided {
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
