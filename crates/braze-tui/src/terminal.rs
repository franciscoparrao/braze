//! Terminal setup/teardown for the inline-viewport chat UI (PLAN.md §
//! "Fase TUI — diseño"): raw mode, but deliberately *no* alternate
//! screen, so the finalized transcript lands in the terminal's own
//! native scrollback — see `docs/TUI-INVESTIGACION-2026-07.md`'s
//! convergence #1 (the pattern all three researched TUIs converged on).

use std::io::Stdout;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::error::TuiError;

pub type Backend = CrosstermBackend<Stdout>;

/// Rows the still-streaming, not-yet-flushed tail of the assistant's
/// current line gets to preview in — see `app.rs`'s `drain_ready_lines`.
/// Kept small and fixed: completed lines are pushed to the scrollback
/// immediately as they arrive, so this never needs to grow with the
/// length of the response.
pub const ACTIVE_ROWS: u16 = 2;
/// The hint/status line above the composer.
const HINT_ROWS: u16 = 1;
/// Rows the composer itself gets. A composer that grows with pasted or
/// wrapped content (rather than this fixed height) is deferred alongside
/// the rest of "fase TUI 2" (PLAN.md).
const COMPOSER_ROWS: u16 = 3;

pub const VIEWPORT_HEIGHT: u16 = ACTIVE_ROWS + HINT_ROWS + COMPOSER_ROWS;

/// RAII guard: disables raw mode and bracketed paste on drop — including
/// on panic unwind, via normal `Drop` semantics — so a crash never leaves
/// the user's shell in raw mode or paste-bracketing mode.
/// `crossterm::terminal::disable_raw_mode` is safe to call even if raw
/// mode was never actually entered (verified against 0.29's
/// implementation: it's a no-op if the terminal wasn't in raw mode); same
/// for `DisableBracketedPaste` if it was never enabled.
pub struct TerminalGuard {
    pub terminal: Terminal<Backend>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

/// N-10 (docs/AUDITORIA-2026-07-v2.md): without `EnableBracketedPaste`,
/// the terminal delivers a paste as a flood of individual key events —
/// every embedded `\r`/`\n` is then indistinguishable from the user
/// pressing Enter, submitting the pasted text's first line as its own
/// turn immediately. With it enabled, crossterm instead delivers the
/// whole paste as one `Event::Paste(String)` (see `App::on_paste`), which
/// `ratatui-textarea`'s `insert_str` inserts as literal multi-line
/// composer content in a single atomic edit.
pub fn setup() -> Result<TerminalGuard, TuiError> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT_HEIGHT),
        },
    )?;
    Ok(TerminalGuard { terminal })
}
