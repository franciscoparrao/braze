//! Two-layer permission model: working-dir allowlist + irreversible-action
//! confirmation.
//!
//! Layer 1 ([`WorkdirAllowlist`]) is a soft, non-enforced directory scope —
//! no Landlock/seccomp yet, that is deferred to Fase 2. Layer 2
//! ([`ActionClassifier`] + [`ConfirmationPrompt`]) intercepts actions the
//! MVP fixed table (PLAN.md) considers irreversible — `git push`, `rm -rf`,
//! writes/deletes outside the allowlist — and blocks on a y/n callback
//! before letting them through. [`PermissionGuard`] is the single entry
//! point `braze-tools-local` and `braze-engine` call; everything else in
//! this crate is a building block for it.

mod action;
mod allowlist;
mod classifier;
mod confirm;
mod error;
mod guard;

pub use action::ActionDescriptor;
pub use allowlist::WorkdirAllowlist;
pub use classifier::{ActionClassifier, DefaultClassifier, Reversibility};
pub use confirm::ConfirmationPrompt;
pub use error::PermissionError;
pub use guard::PermissionGuard;
