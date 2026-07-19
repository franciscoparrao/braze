//! Status line rendered in the right half of the hint row (`app.rs`'s
//! `draw`) — backend/model, a short session id prefix, and cumulative
//! token usage (from `AgentEvent::Usage`, accumulated in `App`). A pure
//! string-formatting function rather than its own viewport row: keeps
//! `VIEWPORT_HEIGHT` unchanged from oleada 2/3 (the hint row grows a
//! second, right-aligned half instead of the viewport growing a row),
//! and makes the format independently testable without a `Terminal`.

use braze_types::SessionId;

/// Prefix length for the shortened session id — long enough to
/// disambiguate at a glance (like a short git commit hash), short enough
/// not to crowd out backend/model/tokens on a narrow terminal.
const SESSION_PREFIX_LEN: usize = 8;

pub fn render(
    status_line: &str,
    session: SessionId,
    total_input_tokens: u64,
    total_output_tokens: u64,
    loaded_skills: usize,
) -> String {
    let session_str = session.to_string();
    let short_session = &session_str[..SESSION_PREFIX_LEN.min(session_str.len())];
    // Skills only appear once one is actually loaded — a permanent
    // "skills 0" would spend scarce right-half columns on the common
    // case (no skills configured at all).
    let skills = if loaded_skills > 0 {
        format!(" · skills {loaded_skills}")
    } else {
        String::new()
    };
    format!(
        "{status_line} · {short_session}{skills} · tokens {total_input_tokens}↑/{total_output_tokens}↓"
    )
}

/// Fits an already-rendered status line into `max_cols` display columns
/// by keeping its TAIL and prefixing a `…` marker — verified live
/// (pty 100×30, 2026-07-19): the right-aligned `Paragraph` clips an
/// overflowing line, and what got cut was exactly the dynamic segments
/// (skills/tokens) this bar exists to show. The head (`backend:model`)
/// is the static part — it's in the startup banner and only changes via
/// `/model`, which prints its own confirmation cell — so it's the right
/// thing to sacrifice on a narrow terminal. Budgets display columns via
/// `unicode_width` (the `↑`/`↓`/`·` glyphs are single-column, but the
/// status line is arbitrary config text — same CJK reasoning as
/// `app::truncate_for_display`).
pub fn fit_right(status: &str, max_cols: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    if status.width() <= max_cols {
        return status.to_string();
    }
    let budget = max_cols.saturating_sub(1); // room for the marker
    let mut tail: Vec<char> = Vec::new();
    let mut width_so_far = 0;
    for c in status.chars().rev() {
        let w = c.width().unwrap_or(0);
        if width_so_far + w > budget {
            break;
        }
        width_so_far += w;
        tail.push(c);
    }
    tail.push('…');
    tail.into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_the_status_line_and_token_totals() {
        let session = SessionId::new();
        let rendered = render("ollama:qwen2.5:3b", session, 42, 7, 0);
        assert!(rendered.contains("ollama:qwen2.5:3b"));
        assert!(rendered.contains("42↑"));
        assert!(rendered.contains("7↓"));
    }

    #[test]
    fn shortens_the_session_id_to_a_fixed_prefix() {
        let session = SessionId::new();
        let rendered = render("x", session, 0, 0, 0);
        let full = session.to_string();
        assert!(rendered.contains(&full[..SESSION_PREFIX_LEN]));
        assert!(!rendered.contains(&full));
    }

    #[test]
    fn fit_right_leaves_a_fitting_status_untouched() {
        assert_eq!(fit_right("corto", 10), "corto");
    }

    /// The live-verified failure mode (pty 100×30): an overflowing
    /// status must keep its tail (the dynamic tokens segment), drop the
    /// head, and mark the cut — never exceed the column budget.
    #[test]
    fn fit_right_keeps_the_tail_within_budget_with_a_marker() {
        use unicode_width::UnicodeWidthStr;

        let status = "ollama:qwen2.5:7b · 20abe986 · skills 1 · tokens 2768↑/29↓";
        let fitted = fit_right(status, 30);
        assert!(fitted.starts_with('…'), "got: {fitted}");
        assert!(fitted.ends_with("tokens 2768↑/29↓"), "got: {fitted}");
        assert!(fitted.width() <= 30, "width {} > 30", fitted.width());
    }

    #[test]
    fn loaded_skills_appear_only_once_nonzero() {
        let session = SessionId::new();
        assert!(!render("x", session, 0, 0, 0).contains("skills"));
        assert!(render("x", session, 0, 0, 2).contains("skills 2"));
    }
}
