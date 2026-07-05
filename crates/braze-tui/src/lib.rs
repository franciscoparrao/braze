//! braze terminal UI: an inline-viewport chat frontend over
//! `braze-engine`'s `TurnObserver` seam (PLAN.md § "Fase TUI — diseño").
//!
//! Opt-in via `braze chat --tui` (see `braze-cli`) — the plain-text
//! `chat`/`run` path is unchanged and remains the default. This is
//! oleada 2's skeleton: inline viewport + native scrollback, a
//! multi-line composer, and plain-text streaming to the transcript.
//! Tool-call/permission/compaction cells, the real approval overlay, and
//! markdown rendering are later oleadas — see PLAN.md for the full plan.

mod app;
mod approval;
mod error;
mod history_cell;
mod observer;
mod terminal;

pub use approval::AutoDenyConfirmationPrompt;
pub use error::TuiError;

use braze_engine::Engine;
use braze_types::SessionId;

/// Runs the interactive TUI chat loop for `session` against `engine`
/// until the user quits (Ctrl+C, or Ctrl+D on an empty composer). Owns
/// the terminal for the duration of the call — raw mode is restored on
/// return, including on error or panic (see `terminal::TerminalGuard`).
pub async fn run(engine: Engine, session: SessionId) -> Result<(), TuiError> {
    let mut guard = terminal::setup()?;
    app::run(&mut guard.terminal, engine, session).await
}
