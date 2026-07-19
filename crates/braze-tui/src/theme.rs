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
    /// braze's identity color — the banner icon, the user `>` marker,
    /// the composer's border, the spinner, and `/command` names in the
    /// slash popup. Deliberately NOT one of the semantic outcome colors
    /// (success/error/warning): accent marks "this is braze / this is
    /// yours", never "this went well/badly". Still a named ANSI slot
    /// like every other color here (module doc) — each preset picks a
    /// hue that collides with none of its own semantic colors.
    pub accent: Color,
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            muted: Color::DarkGray,
            accent: Color::Cyan,
        }
    }

    /// Plain `Yellow` text is the classic low-contrast combination on a
    /// light background in terminals whose palette doesn't remap it —
    /// `Magenta` reads clearly on both light and dark backgrounds. The
    /// accent moves to `Blue` for the same reason: `Cyan` is the other
    /// classically washed-out hue on light backgrounds.
    pub const fn light() -> Self {
        Self {
            success: Color::Green,
            error: Color::Red,
            warning: Color::Magenta,
            muted: Color::DarkGray,
            accent: Color::Blue,
        }
    }

    /// No dimmed/muted tone at all — `DarkGray` is explicitly a
    /// *low*-contrast choice, which defeats the point of a
    /// high-contrast palette; `White` keeps every cell's secondary text
    /// just as legible as its primary text. Accent is `Magenta` here
    /// because this preset's warning already claims `Cyan`.
    pub const fn high_contrast() -> Self {
        Self {
            success: Color::Green,
            error: Color::Red,
            warning: Color::Cyan,
            muted: Color::White,
            accent: Color::Magenta,
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

    /// The accent marks identity, never outcome — if a preset's accent
    /// equaled its warning or error, "this is braze's UI chrome" and
    /// "something needs attention" would become indistinguishable.
    #[test]
    fn accent_never_collides_with_the_same_presets_semantic_colors() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            assert_ne!(theme.accent, theme.warning);
            assert_ne!(theme.accent, theme.error);
            assert_ne!(theme.accent, theme.success);
            assert_ne!(theme.accent, theme.muted);
        }
    }
}
