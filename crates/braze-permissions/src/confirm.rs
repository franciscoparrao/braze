use crate::action::ActionDescriptor;

/// Terminal y/n confirmation callback. async for dyn-compatibility and
/// consistency with the workspace's async-everywhere decision.
/// SAFETY DEFAULT: an implementation that cannot obtain a definitive "yes"
/// (EOF, I/O error, non-interactive stdin, ...) MUST return `false`. Never
/// treat a read failure as implicit allow.
#[async_trait::async_trait]
pub trait ConfirmationPrompt: Send + Sync {
    async fn confirm(&self, action: &ActionDescriptor) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Fixed-answer prompt used across this crate's tests.
    pub struct FixedPrompt(pub bool);

    #[async_trait::async_trait]
    impl ConfirmationPrompt for FixedPrompt {
        async fn confirm(&self, _action: &ActionDescriptor) -> bool {
            self.0
        }
    }

    #[tokio::test]
    async fn fixed_prompt_returns_configured_answer() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/foo"),
        };
        assert!(FixedPrompt(true).confirm(&action).await);
        assert!(!FixedPrompt(false).confirm(&action).await);
    }
}
