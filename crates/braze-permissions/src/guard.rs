use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::action::ActionDescriptor;
use crate::allowlist::WorkdirAllowlist;
use crate::classifier::{ActionClassifier, Reversibility};
use crate::confirm::ConfirmationPrompt;
use crate::error::PermissionError;

/// Session-scoped "remember this decision" key. Two occurrences of the
/// same action (by this coarser identity, not full equality — e.g. two
/// `shell_exec` calls with the same program+subcommand but different
/// trailing arguments) are treated as the same decision, so the user is
/// only asked once per session per key. Never persisted to disk — this is
/// purely an in-memory de-duplication of prompts within a single run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RememberKey {
    Shell {
        program: String,
        subcommand: Option<String>,
    },
    WriteFile {
        path: PathBuf,
    },
    DeleteFile {
        path: PathBuf,
    },
    McpToolCall {
        server: String,
        tool: String,
    },
}

pub struct PermissionGuard {
    allowlist: WorkdirAllowlist,
    classifier: Box<dyn ActionClassifier>,
    prompt: Box<dyn ConfirmationPrompt>,
    /// Keys of previously *confirmed* irreversible actions in this session.
    /// A denial is never recorded here — see `check`.
    remembered: Mutex<HashSet<RememberKey>>,
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
            remembered: Mutex::new(HashSet::new()),
        }
    }

    /// Maps an action to its session-remember identity. `None` means the
    /// action is never remembered (either it can't produce a stable key, or
    /// — as with `Other` — it's always Reversible and this is never
    /// consulted for it in practice).
    fn remember_key(&self, action: &ActionDescriptor) -> Option<RememberKey> {
        match action {
            ActionDescriptor::ShellCommand { command } => {
                let program = command.first()?.clone();
                let subcommand = command.get(1).cloned();
                Some(RememberKey::Shell {
                    program,
                    subcommand,
                })
            }
            ActionDescriptor::WriteFile { path } => Some(RememberKey::WriteFile {
                path: self.allowlist.resolve(path),
            }),
            ActionDescriptor::DeleteFile { path } => Some(RememberKey::DeleteFile {
                path: self.allowlist.resolve(path),
            }),
            ActionDescriptor::McpToolCall { server, tool } => Some(RememberKey::McpToolCall {
                server: server.clone(),
                tool: tool.clone(),
            }),
            ActionDescriptor::Other { .. } => None,
        }
    }

    /// Reversible → proceeds silently. Irreversible → checks the
    /// session-remember cache first (a previously *confirmed* action with
    /// the same key is allowed without re-prompting), then blocks on
    /// prompt.confirm(); Err(Denied) if the user says no. A denial is never
    /// cached — the next attempt always re-prompts.
    pub async fn check(&self, action: &ActionDescriptor) -> Result<(), PermissionError> {
        match self.classifier.classify(action) {
            Reversibility::Reversible => Ok(()),
            Reversibility::Irreversible => {
                if let Some(key) = self.remember_key(action)
                    && self.remembered.lock().unwrap().contains(&key)
                {
                    return Ok(());
                }
                if self.prompt.confirm(action).await {
                    if let Some(key) = self.remember_key(action) {
                        self.remembered.lock().unwrap().insert(key);
                    }
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[tokio::test]
    async fn repeated_shell_command_is_only_confirmed_once_per_session() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls.clone());
        // "mv" is not on the safe shell allowlist -> Irreversible.
        let action = ActionDescriptor::ShellCommand {
            command: vec!["mv".to_string(), "a".to_string(), "b".to_string()],
        };
        assert!(guard.check(&action).await.is_ok());
        assert!(guard.check(&action).await.is_ok());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second check of the same remembered action must not re-prompt"
        );
    }

    #[tokio::test]
    async fn repeated_write_outside_allowlist_is_only_confirmed_once_per_session() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls.clone());
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert!(guard.check(&action).await.is_ok());
        assert!(guard.check(&action).await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_denied_action_is_never_remembered_and_always_reprompts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(false, calls.clone());
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/etc/passwd"),
        };
        assert!(guard.check(&action).await.is_err());
        assert!(guard.check(&action).await.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a denial must never be cached, every attempt re-prompts"
        );
    }

    #[tokio::test]
    async fn distinct_remember_keys_are_confirmed_independently() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls.clone());

        let mv = ActionDescriptor::ShellCommand {
            command: vec!["mv".to_string(), "a".to_string(), "b".to_string()],
        };
        let curl = ActionDescriptor::ShellCommand {
            command: vec!["curl".to_string(), "http://x".to_string()],
        };
        assert!(guard.check(&mv).await.is_ok());
        assert!(guard.check(&curl).await.is_ok());
        // Each key still only needs to be confirmed once even after both
        // have been seen.
        assert!(guard.check(&mv).await.is_ok());
        assert!(guard.check(&curl).await.is_ok());

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "two distinct keys must each be confirmed exactly once"
        );
    }
}
