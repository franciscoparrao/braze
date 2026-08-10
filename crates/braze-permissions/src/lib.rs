//! Two-layer permission model: working-dir allowlist + irreversible-action
//! confirmation.
//!
//! Layer 1 ([`WorkdirAllowlist`]) is a soft, non-enforced directory scope —
//! no Landlock/seccomp yet, that is deferred to Fase 2.
//!
//! Layer 2 ([`ActionClassifier`] + [`ConfirmationPrompt`]) intercepts
//! actions [`DefaultClassifier`] considers irreversible and blocks on a
//! y/n callback before letting them through. Shell commands are
//! default-deny: `git push`/`rm -rf` are always irreversible, and every
//! other command is irreversible unless it matches an explicit allowlist
//! of safe, non-mutating commands (read-only utilities, plus a narrow
//! subset of `find`/`git`) — this replaced an earlier "allow by default,
//! deny two patterns" table. Writes/deletes are irreversible outside the
//! [`WorkdirAllowlist`]. MCP tool calls ([`ActionDescriptor::McpToolCall`])
//! are always irreversible: an MCP server is arbitrary code the user chose
//! to wire up, with no safe-by-construction subset to allowlist.
//!
//! [`PermissionGuard`] also remembers, in memory only, every irreversible
//! action it has already gotten a "yes" for in this session, keyed by an
//! action identity ([`braze_types::PermissionKey`]: the full argv for
//! shell — approving `rm -rf /tmp/build` must never auto-approve `rm -rf
//! /`, so the key cannot stop at program+subcommand — resolved path for
//! writes/deletes, server+tool for MCP calls, derived via the free
//! function [`derive_permission_key`]). A repeat of the
//! same key proceeds without re-prompting; a denial is never remembered, so
//! the next attempt always re-prompts. This crate itself never persists
//! that cache to disk — but [`PermissionGuard::seed_remembered`] lets a
//! caller (`braze-cli`) replay previously-approved decisions it *did*
//! persist elsewhere (as `AgentEvent::PermissionDecided` in the session's
//! rollout log) back into a freshly-built guard, which is what makes a
//! `--resume`d session not re-ask for the same action.
//!
//! [`PermissionGuard`] is the single entry point `braze-tools-local`,
//! `braze-mcp-client`, and `braze-engine` call; everything else in this
//! crate is a building block for it.

mod action;
mod allowlist;
mod classifier;
mod confirm;
mod error;
mod guard;
mod human_wait;
mod question;

pub use action::{ActionDescriptor, sanitize_control_chars};
pub use allowlist::WorkdirAllowlist;
pub use classifier::{
    ActionClassifier, AlwaysIrreversibleClassifier, DefaultClassifier, Reversibility,
};
pub use confirm::ConfirmationPrompt;
pub use error::PermissionError;
pub use guard::{PermissionGuard, derive_permission_key};
pub use human_wait::{
    HumanWait, accumulated as human_wait_accumulated, is_waiting as human_is_waiting,
};
pub use question::QuestionPrompt;
