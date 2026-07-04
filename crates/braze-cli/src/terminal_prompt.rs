//! [`TerminalConfirmationPrompt`]: the real y/n confirmation prompt used
//! by the interactive/one-shot binary — the "capa de confirmación de
//! intención" callback [`braze_permissions::PermissionGuard`] blocks on
//! before letting an irreversible action through.

use async_trait::async_trait;
use braze_permissions::{ActionDescriptor, ConfirmationPrompt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct TerminalConfirmationPrompt;

#[async_trait]
impl ConfirmationPrompt for TerminalConfirmationPrompt {
    /// SAFETY DEFAULT (see `ConfirmationPrompt`'s doc comment): anything
    /// other than an unambiguous "yes" — including EOF or an I/O error on
    /// either stdout or stdin — returns `false`. Never treat a read
    /// failure as implicit allow.
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        let mut stdout = tokio::io::stdout();
        let prompt = format!("{action}\n¿Permitir? [y/N]: ");

        if stdout.write_all(prompt.as_bytes()).await.is_err() {
            return false;
        }
        if stdout.flush().await.is_err() {
            return false;
        }

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
    }
}
