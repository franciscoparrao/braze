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
mod question;
mod slash_commands;
mod status_bar;
mod terminal;
mod theme;

pub use approval::{ApprovalRequest, ChannelConfirmationPrompt};
pub use question::{ChannelQuestionPrompt, QuestionRequest};

/// One entry the `/skills` picker offers: the normalized skill name (as
/// `$name` mentions resolve it) and its frontmatter description. Plain
/// data — this crate deliberately doesn't depend on `braze-skills`;
/// `braze-cli` maps the discovered `SkillStub`s into these at startup,
/// the same way `model_candidates` are computed once and passed in.
#[derive(Debug, Clone)]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
}
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
    questions: mpsc::UnboundedReceiver<QuestionRequest>,
    status_line: String,
    theme: Theme,
    engine_factory: EngineFactory,
    model_candidates: Vec<String>,
    skill_candidates: Vec<SkillCandidate>,
) -> Result<(), TuiError> {
    print_banner(&theme, &status_line);
    let mut guard = terminal::setup()?;
    app::run(
        &mut guard.terminal,
        engine,
        live_session,
        store,
        approvals,
        questions,
        status_line,
        theme,
        engine_factory,
        model_candidates,
        skill_candidates,
    )
    .await
}

/// Small block-icon + wordmark, printed once *before* `terminal::setup()`
/// switches the terminal into raw/inline-viewport mode — a plain stdout
/// write here becomes permanent native scrollback with zero interaction
/// with the inline-viewport machinery (`terminal.rs`'s module doc:
/// deliberately no alternate screen, so anything printed before raw mode
/// began just... stays, like any other line the shell already had).
/// The icon renders in `theme.accent` — braze's identity color (see
/// `Theme::accent`) — and the info lines reuse the same
/// "backend:model" `status_line` the status bar shows all session, so
/// the banner always reflects what's actually running instead of a
/// hardcoded placeholder (docs/usability-log-2026-07-07-si2.md,
/// comparación contra el cookbook de OpenRouter). The version comes
/// from the crate itself (`CARGO_PKG_VERSION`), so it can never drift
/// from what was actually built.
fn print_banner(theme: &Theme, status_line: &str) {
    use crossterm::style::Stylize;

    let icon = to_crossterm_color(theme.accent);
    let text = to_crossterm_color(theme.muted);

    println!();
    println!("  {}", "▛▀▜".with(icon));
    println!(
        "  {}  {} {}",
        "▙ ▟".with(icon),
        "braze".bold(),
        concat!("v", env!("CARGO_PKG_VERSION")).with(text)
    );
    println!(
        "  {}  {}",
        "▘▀▘".with(icon),
        format!("{status_line} · /help para comandos y atajos").with(text)
    );
    println!();
}

/// `Theme` deliberately stays on `ratatui::style::Color` (what every
/// `HistoryCell`/widget actually renders with) — `print_banner` is the
/// one place that needs the `crossterm::style::Color` equivalent, since
/// it writes plain ANSI-styled text before any `ratatui::Terminal`
/// exists to render through. Only the variants `Theme`'s three presets
/// actually use are covered (`theme.rs`); anything else falls back to
/// the terminal's default foreground rather than guessing.
fn to_crossterm_color(color: ratatui::style::Color) -> crossterm::style::Color {
    use crossterm::style::Color as CColor;
    use ratatui::style::Color as RColor;
    match color {
        RColor::Green => CColor::Green,
        RColor::Red => CColor::Red,
        RColor::Yellow => CColor::Yellow,
        RColor::Magenta => CColor::Magenta,
        RColor::Cyan => CColor::Cyan,
        RColor::Blue => CColor::Blue,
        RColor::White => CColor::White,
        RColor::DarkGray => CColor::DarkGrey,
        _ => CColor::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every color every built-in `Theme` preset actually uses
    /// (`theme.rs`'s `dark`/`light`/`high_contrast`) must round-trip to
    /// its named crossterm equivalent, not the `Reset` fallback — the
    /// whole point of a hand-written match instead of a generic
    /// conversion.
    #[test]
    fn to_crossterm_color_covers_every_color_the_built_in_themes_use() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            for color in [
                theme.success,
                theme.error,
                theme.warning,
                theme.muted,
                theme.accent,
            ] {
                assert_ne!(
                    to_crossterm_color(color),
                    crossterm::style::Color::Reset,
                    "{color:?} from a built-in theme must not fall back to Reset"
                );
            }
        }
    }

    #[test]
    fn to_crossterm_color_maps_known_variants_correctly() {
        assert_eq!(
            to_crossterm_color(ratatui::style::Color::Green),
            crossterm::style::Color::Green
        );
        assert_eq!(
            to_crossterm_color(ratatui::style::Color::DarkGray),
            crossterm::style::Color::DarkGrey
        );
    }

    #[test]
    fn to_crossterm_color_falls_back_to_reset_for_an_uncovered_variant() {
        assert_eq!(
            to_crossterm_color(ratatui::style::Color::Rgb(1, 2, 3)),
            crossterm::style::Color::Reset
        );
    }
}
