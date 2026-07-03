use crate::action::ActionDescriptor;
use crate::allowlist::WorkdirAllowlist;
use crate::classifier::{ActionClassifier, Reversibility};
use crate::confirm::ConfirmationPrompt;
use crate::error::PermissionError;

pub struct PermissionGuard {
    allowlist: WorkdirAllowlist,
    classifier: Box<dyn ActionClassifier>,
    prompt: Box<dyn ConfirmationPrompt>,
}

impl PermissionGuard {
    pub fn new(
        allowlist: WorkdirAllowlist,
        classifier: Box<dyn ActionClassifier>,
        prompt: Box<dyn ConfirmationPrompt>,
    ) -> Self {
        Self {
            allowlist,
            classifier,
            prompt,
        }
    }

    /// Reversible → proceeds silently. Irreversible → blocks on
    /// prompt.confirm(); Err(Denied) if the user says no.
    pub async fn check(&self, action: &ActionDescriptor) -> Result<(), PermissionError> {
        match self.classifier.classify(action) {
            Reversibility::Reversible => Ok(()),
            Reversibility::Irreversible => {
                if self.prompt.confirm(action).await {
                    Ok(())
                } else {
                    Err(PermissionError::Denied(action.to_string()))
                }
            }
        }
    }

    pub fn allowlist(&self) -> &WorkdirAllowlist {
        &self.allowlist
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::DefaultClassifier;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Confirmation prompt with a fixed answer that also counts how many
    /// times it was invoked, so tests can assert Reversible actions never
    /// reach it.
    struct CountingPrompt {
        answer: bool,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ConfirmationPrompt for CountingPrompt {
        async fn confirm(&self, _action: &ActionDescriptor) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer
        }
    }

    fn guard_with(answer: bool, calls: Arc<AtomicUsize>) -> PermissionGuard {
        let allowlist = WorkdirAllowlist::new("/home/user/project");
        let classifier = Box::new(DefaultClassifier::new(WorkdirAllowlist::new(
            "/home/user/project",
        )));
        let prompt = Box::new(CountingPrompt { answer, calls });
        PermissionGuard::new(allowlist, classifier, prompt)
    }

    #[tokio::test]
    async fn reversible_action_proceeds_without_confirming() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(false, calls.clone());
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("src/main.rs"),
        };
        assert!(guard.check(&action).await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn irreversible_action_denied_when_confirm_false() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(false, calls.clone());
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        let result = guard.check(&action).await;
        assert!(matches!(result, Err(PermissionError::Denied(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn irreversible_action_allowed_when_confirm_true() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls.clone());
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert!(guard.check(&action).await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn allowlist_accessor_returns_configured_allowlist() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls);
        assert!(guard.allowlist().is_allowed(&PathBuf::from("src/main.rs")));
    }
}
