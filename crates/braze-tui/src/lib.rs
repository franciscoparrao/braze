//! braze terminal UI: an inline-viewport chat frontend over
//! `braze-engine`'s `TurnObserver` seam (PLAN.md § "Fase TUI — diseño").
//!
//! Opt-in via `braze chat --tui` (see `braze-cli`) — the plain-text
//! `chat`/`run` path is unchanged and remains the default. Inline
//! viewport + native scrollback, a multi-line composer with `/command`
//! and `@mention` completion, markdown streaming with fence-aware commit
//! boundaries, tool-call cells (with Ctrl+T to expand the last one's
//! full output), a real permission approval overlay, Esc-to-interrupt,
//! and a status bar. Themes, backtrack, and promoting `--tui` to the
//! default are later increments — see PLAN.md § "Diferido (fase TUI 2)"
//! for the rest.

mod app;
mod approval;
mod composer_trigger;
mod error;
mod history_cell;
mod markdown_stream;
mod mentions;
mod observer;
mod slash_commands;
mod status_bar;
mod terminal;

pub use approval::{ApprovalRequest, ChannelConfirmationPrompt};
pub use error::TuiError;

use std::sync::Arc;

use braze_engine::Engine;
use braze_types::SessionId;
use tokio::sync::mpsc;

/// Runs the interactive TUI chat loop for `session` against `engine`
/// until the user quits (Ctrl+C, or Ctrl+D on an empty composer). Owns
/// the terminal for the duration of the call — raw mode is restored on
/// return, including on error or panic (see `terminal::TerminalGuard`).
///
/// `approvals` is the receiving end of the channel every
/// `ChannelConfirmationPrompt` this session's `PermissionGuard`s were
/// built with sends into — `braze-cli` constructs the channel and passes
/// the sender half into each guard before ever calling this. `store` is
/// the same `SessionStore` handle `engine` was built with, passed
/// separately so Ctrl+T can read the rollout log back (see
/// `app::expand_last_tool_call`). `status_line` is a short, static
/// "backend:model" label shown in the status bar.
pub async fn run(
    engine: Engine,
    session: SessionId,
    store: Arc<dyn braze_session::SessionStore>,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    status_line: String,
) -> Result<(), TuiError> {
    let mut guard = terminal::setup()?;
    app::run(&mut guard.terminal, engine, session, store, approvals, status_line).await
}
