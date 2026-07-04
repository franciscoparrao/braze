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
//! [`PermissionGuard`] also remembers, in memory only (never persisted to
//! disk — this is not a `--resume`-able decision), every irreversible
//! action it has already gotten a "yes" for in this session, keyed by a
//! coarse action identity (program+subcommand for shell, resolved path for
//! writes/deletes, server+tool for MCP calls). A repeat of the same key
//! proceeds without re-prompting; a denial is never remembered, so the
//! next attempt always re-prompts.
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

pub use action::ActionDescriptor;
pub use allowlist::WorkdirAllowlist;
pub use classifier::{ActionClassifier, DefaultClassifier, Reversibility};
pub use confirm::ConfirmationPrompt;
pub use error::PermissionError;
pub use guard::PermissionGuard;
