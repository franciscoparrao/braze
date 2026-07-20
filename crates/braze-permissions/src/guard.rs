use std::collections::HashSet;
use std::sync::Mutex;

use braze_types::PermissionKey;

use crate::action::ActionDescriptor;
use crate::allowlist::{WorkdirAllowlist, normalize_lexically};
use crate::classifier::{ActionClassifier, Reversibility};
use crate::confirm::ConfirmationPrompt;
use crate::error::PermissionError;

/// Maps an action to its session-remember identity. `None` means the
/// action is never remembered (either it can't produce a stable key, or —
/// as with `Other` — it's always Reversible and this is never consulted
/// for it in practice).
///
/// A free function (not a method on `PermissionGuard`) so that
/// `braze-cli`'s `TerminalConfirmationPrompt` — which has no
/// `WorkdirAllowlist` instance of its own — can derive the exact same key
/// independently, to persist it in `AgentEvent::PermissionRequested`/
/// `PermissionDecided` for later `--resume` replay (see
/// `PermissionGuard::seed_remembered`).
///
/// `WriteFile`/`DeleteFile` paths are normalized lexically (see
/// [`normalize_lexically`]) but, unlike `WorkdirAllowlist::resolve`, are
/// *not* joined against a cwd first — this function has no cwd context
/// available to it. In practice this only matters for the (already
/// Irreversible, since `check` only ever consults this for actions the
/// classifier rejected) case of a *relative* path escaping the workdir —
/// an edge case rare enough that a stable-but-cwd-unaware key is an
/// acceptable trade-off for letting this be a free function callable from
/// outside a `PermissionGuard` at all.
pub fn derive_permission_key(action: &ActionDescriptor) -> Option<PermissionKey> {
    match action {
        ActionDescriptor::ShellCommand { command } => {
            // The full argv, not just program+first-arg — see the
            // doc-comment on `PermissionKey::Shell` for why a coarser key
            // would let one approved invocation silently cover a more
            // dangerous one that merely shares a program and subcommand.
            command.first()?;
            Some(PermissionKey::Shell {
                command: command.clone(),
            })
        }
        ActionDescriptor::WriteFile { path } => Some(PermissionKey::WriteFile {
            path: normalize_lexically(path),
        }),
        ActionDescriptor::DeleteFile { path } => Some(PermissionKey::DeleteFile {
            path: normalize_lexically(path),
        }),
        ActionDescriptor::ReadPath { path } => Some(PermissionKey::ReadPath {
            path: normalize_lexically(path),
        }),
        ActionDescriptor::McpToolCall { server, tool } => Some(PermissionKey::McpToolCall {
            server: server.clone(),
            tool: tool.clone(),
        }),
        ActionDescriptor::Other { .. } => None,
    }
}

pub struct PermissionGuard {
    allowlist: WorkdirAllowlist,
    classifier: Box<dyn ActionClassifier>,
    prompt: Box<dyn ConfirmationPrompt>,
    /// Keys of previously *confirmed* irreversible actions in this session.
    /// A denial is never recorded here — see `check`.
    remembered: Mutex<HashSet<PermissionKey>>,
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

    /// Seeds the in-memory "already approved this session" set from
    /// previously-persisted decisions (used when resuming a session so
    /// approvals aren't re-asked). Additive only — never removes existing
    /// entries.
    pub fn seed_remembered(&self, keys: impl IntoIterator<Item = PermissionKey>) {
        let mut remembered = self.remembered.lock().unwrap();
        remembered.extend(keys);
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
                if let Some(key) = derive_permission_key(action)
                    && self.remembered.lock().unwrap().contains(&key)
                {
                    return Ok(());
                }
                // Incidente roam #3: el reloj del `tool_completion_timeout`
                // no debe correr contra la deliberación de la persona —
                // ver `crate::human_wait`. El guard marca el intervalo;
                // el dispatcher del engine lo descuenta.
                let waiting = crate::human_wait::HumanWait::start();
                let approved = self.prompt.confirm(action).await;
                drop(waiting);
                if approved {
                    if let Some(key) = derive_permission_key(action) {
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

    /// Regression test for the exact scenario the audit flagged: approving
    /// a narrowly-scoped destructive command must NOT silently approve a
    /// broader/more dangerous invocation that merely shares the program
    /// and first argument.
    #[tokio::test]
    async fn approving_a_narrow_rm_rf_does_not_auto_approve_a_broader_target() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls.clone());

        let narrow = ActionDescriptor::ShellCommand {
            command: vec![
                "rm".to_string(),
                "-rf".to_string(),
                "/tmp/build".to_string(),
            ],
        };
        let broad = ActionDescriptor::ShellCommand {
            command: vec!["rm".to_string(), "-rf".to_string(), "/".to_string()],
        };

        assert!(guard.check(&narrow).await.is_ok());
        assert!(guard.check(&broad).await.is_ok());

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a different target must re-prompt even though program+subcommand match"
        );
    }

    #[tokio::test]
    async fn seeding_a_remembered_key_skips_the_prompt_for_the_matching_action() {
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = guard_with(true, calls.clone());

        // "mv" is not on the safe shell allowlist -> Irreversible, so this
        // would normally have to go through `prompt.confirm`.
        let action = ActionDescriptor::ShellCommand {
            command: vec!["mv".to_string(), "a".to_string(), "b".to_string()],
        };
        let key = derive_permission_key(&action).expect("shell action must produce a key");

        guard.seed_remembered([key]);

        assert!(guard.check(&action).await.is_ok());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a replayed/seeded key must short-circuit the prompt entirely"
        );
    }
}
