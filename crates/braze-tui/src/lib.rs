//! braze terminal UI: an inline-viewport chat frontend over
//! `braze-engine`'s `TurnObserver` seam (PLAN.md § "Fase TUI — diseño").
//!
//! Opt-in via `braze chat --tui` (see `braze-cli`) — the plain-text
//! `chat`/`run` path is unchanged and remains the default. Inline
//! viewport + native scrollback, a multi-line composer with `/command`
//! and `@mention` completion, markdown streaming with fence-aware commit
//! boundaries, tool-call cells (with Ctrl+T to expand the last one's
//! full output), a real permission approval overlay, Esc-to-interrupt, a
//! status bar, and a `Theme` preset (dark/light/high-contrast — see
//! `theme`). Backtrack and promoting `--tui` to the default are later
//! increments — see PLAN.md § "Diferido (fase TUI 2)" for the rest.

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
mod theme;

pub use approval::{ApprovalRequest, ChannelConfirmationPrompt};
pub use error::TuiError;
pub use theme::Theme;

use std::sync::Arc;

use braze_engine::Engine;
use braze_types::SessionId;
use tokio::sync::mpsc;

/// Async constructor for a replacement [`Engine`], used by the `/model`
/// command (PLAN.md § "fase TUI 2"): given a `backend[:modelo]` spec,
/// rebuilds a fresh engine — same composition as startup: permission
/// guards re-seeded from the live session's approvals, tool providers,
/// compactor, planner — plus its short "backend:model" status-bar label.
/// `braze-cli` implements it over the same `build_engine` startup uses;
/// this crate deliberately can't build engines itself (it depends on
/// neither `braze-config` nor `braze-model`). The error side is a
/// display string — the TUI only ever shows it, never matches on it.
pub type EngineFactory = Box<
    dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(Engine, String), String>> + Send>,
        > + Send,
>;

/// Runs the interactive TUI chat loop for `session` against `engine`
/// until the user quits (Ctrl+C, or Ctrl+D on an empty composer). Owns
/// the terminal for the duration of the call — raw mode is restored on
/// return, including on error or panic (see `terminal::TerminalGuard`).
///
/// `live_session` is the same shared handle every `ChannelConfirmationPrompt`
/// this session's `PermissionGuard`s were built with reads from (N-12,
/// docs/AUDITORIA-2026-07-v2.md) — `braze-cli` constructs it and must
/// pass the identical `Arc` into every guard *and* into this call, or a
/// backtrack has no way to keep future permission decisions landing in
/// the right session. `approvals` is the receiving end of the channel
/// those same guards send into. `store` is the same `SessionStore`
/// handle `engine` was built with, passed separately so Ctrl+T can read
/// the rollout log back (see `app::expand_last_tool_call`). `status_line`
/// is a short, static "backend:model" label shown in the status bar.
/// `theme` picks the color preset every `HistoryCell` renders with —
/// `braze-cli` resolves it from `Config::tui_theme` before calling this.
/// `engine_factory` rebuilds the engine for the `/model` command, and
/// `model_candidates` are the `backend[:modelo]` specs its no-args
/// picker offers — computed once at startup (e.g. from the config's
/// backends plus the Ollama server's installed models), not refreshed
/// mid-session.
#[allow(clippy::too_many_arguments)] // the composition-root seam: one param per collaborator
pub async fn run(
    engine: Engine,
    live_session: Arc<std::sync::Mutex<SessionId>>,
    store: Arc<dyn braze_session::SessionStore>,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    status_line: String,
    theme: Theme,
    engine_factory: EngineFactory,
    model_candidates: Vec<String>,
) -> Result<(), TuiError> {
    let mut guard = terminal::setup()?;
    app::run(
        &mut guard.terminal,
        engine,
        live_session,
        store,
        approvals,
        status_line,
        theme,
        engine_factory,
        model_candidates,
    )
    .await
}
