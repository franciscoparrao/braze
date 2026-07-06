//! Named color presets — "fase TUI 2" (PLAN.md). Every color already
//! used across `history_cell.rs`/`app.rs` is a named ANSI slot (`Red`,
//! `Green`, `Yellow`, `DarkGray`), never a literal RGB — the terminal
//! emulator's own color scheme already decides the actual pixels, so
//! braze-tui's colors already adapt reasonably to a light or dark
//! terminal background without any theme system at all. What a `Theme`
//! adds on top is letting the user pick a different *semantic* mapping
//! (e.g. swap the warning color away from plain yellow, which reads
//! poorly on a light background in terminals that don't remap it) or
//! drop to a higher-contrast palette, not "fix" an RGB clash that
//! doesn't exist here.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub muted: Color,
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            muted: Color::DarkGray,
        }
    }

    /// Plain `Yellow` text is the classic low-contrast combination on a
    /// light background in terminals whose palette doesn't remap it —
    /// `Magenta` reads clearly on both light and dark backgrounds.
    pub const fn light() -> Self {
        Self {
            success: Color::Green,
            error: Color::Red,
            warning: Color::Magenta,
            muted: Color::DarkGray,
        }
    }

    /// No dimmed/muted tone at all — `DarkGray` is explicitly a
    /// *low*-contrast choice, which defeats the point of a
    /// high-contrast palette; `White` keeps every cell's secondary text
    /// just as legible as its primary text.
    pub const fn high_contrast() -> Self {
        Self {
            success: Color::Green,
            error: Color::Red,
            warning: Color::Cyan,
            muted: Color::White,
        }
    }

    /// `None` for an unrecognized name — callers decide whether that's
    /// a hard error (`braze-config`, at startup) or a silent fallback.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            "high-contrast" => Some(Self::high_contrast()),
            _ => None,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_recognizes_every_built_in_preset() {
        assert_eq!(Theme::from_name("dark"), Some(Theme::dark()));
        assert_eq!(Theme::from_name("light"), Some(Theme::light()));
        assert_eq!(
            Theme::from_name("high-contrast"),
            Some(Theme::high_contrast())
        );
    }

    #[test]
    fn from_name_rejects_an_unknown_name() {
        assert_eq!(Theme::from_name("solarized"), None);
    }

    #[test]
    fn default_theme_is_dark() {
        assert_eq!(Theme::default(), Theme::dark());
    }

    #[test]
    fn high_contrast_never_dims_its_muted_tone() {
        assert_ne!(Theme::high_contrast().muted, Color::DarkGray);
    }
}
