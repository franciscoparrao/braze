//! Test-only helpers shared across this crate's `#[cfg(test)]` modules:
//! unique temp directories (no `tempfile` dependency in the workspace)
//! and fake [`ConfirmationPrompt`] implementations for building
//! throwaway [`PermissionGuard`]s, since `braze-tools-local` never
//! constructs its own guard in production code.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use braze_permissions::{
    ActionDescriptor, ConfirmationPrompt, DefaultClassifier, PermissionGuard, WorkdirAllowlist,
};

/// A directory under the OS temp dir, unique per call within this process
/// (label + pid + monotonic counter) — good enough for test isolation
/// without a `tempfile` dependency.
pub fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "braze-tools-local-test-{label}-{}-{n}",
        std::process::id()
    ))
}

/// Always answers "yes" — used to build guards that let an irreversible
/// action through.
pub struct AlwaysAllow;

#[async_trait::async_trait]
impl ConfirmationPrompt for AlwaysAllow {
    async fn confirm(&self, _action: &ActionDescriptor) -> bool {
        true
    }
}

/// Always answers "no" — used to build guards that must deny.
pub struct AlwaysDeny;

#[async_trait::async_trait]
impl ConfirmationPrompt for AlwaysDeny {
    async fn confirm(&self, _action: &ActionDescriptor) -> bool {
        false
    }
}

/// A guard whose allowlist root is `root` and whose confirmation prompt
/// always answers "yes": actions inside `root` succeed silently
/// (`Reversible`, prompt never called), actions outside it succeed after
/// an (accepted) prompt.
pub fn allow_guard(root: impl Into<PathBuf>) -> PermissionGuard {
    let root = root.into();
    PermissionGuard::new(
        WorkdirAllowlist::new(root.clone()),
        Box::new(DefaultClassifier::new(WorkdirAllowlist::new(root))),
        Box::new(AlwaysAllow),
    )
}

/// A guard whose confirmation prompt always answers "no": any
/// irreversible action (writes/deletes outside `root`, `git push`,
/// `rm -rf`, ...) is denied.
pub fn deny_guard(root: impl Into<PathBuf>) -> PermissionGuard {
    let root = root.into();
    PermissionGuard::new(
        WorkdirAllowlist::new(root.clone()),
        Box::new(DefaultClassifier::new(WorkdirAllowlist::new(root))),
        Box::new(AlwaysDeny),
    )
}
