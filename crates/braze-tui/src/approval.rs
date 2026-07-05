//! [`AutoDenyConfirmationPrompt`]: the stand-in `ConfirmationPrompt` used
//! while running under `--tui`, until the real approval overlay
//! (bottom-pane style, PLAN.md § "Fase TUI — diseño", oleada 4) exists.
//!
//! `braze-cli`'s existing `TerminalConfirmationPrompt` reads y/n answers
//! from stdin via `tokio::io::stdin()` line-buffered reads — which don't
//! work correctly once the terminal is in raw mode (no canonical-mode
//! line editing; Enter sends `\r`, not `\n`). Rather than risk silent
//! misbehavior (a hang, or worse, a stray keystroke misparsed as an
//! answer), this denies every irreversible action outright — consistent
//! with this codebase's existing safety default (see
//! `braze_permissions::ConfirmationPrompt`'s doc comment: "anything other
//! than an unambiguous yes... MUST return false"). Reversible actions are
//! entirely unaffected by this — the classifier never consults a
//! `ConfirmationPrompt` for them at all.

use async_trait::async_trait;
use braze_permissions::{ActionDescriptor, ConfirmationPrompt};

pub struct AutoDenyConfirmationPrompt;

#[async_trait]
impl ConfirmationPrompt for AutoDenyConfirmationPrompt {
    async fn confirm(&self, _action: &ActionDescriptor) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn always_denies_regardless_of_the_action() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/x"),
        };
        assert!(!AutoDenyConfirmationPrompt.confirm(&action).await);
    }
}
