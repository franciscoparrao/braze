//! [`HistoryCell`]: what gets committed to the terminal's native
//! scrollback (see `app.rs`'s `commit_cell`, and
//! `docs/TUI-INVESTIGACION-2026-07.md`'s convergence #1 — finalized
//! content is written once to the scrollback, never re-rendered).
//!
//! A trait rather than an enum so later oleadas (tool-call state machine,
//! permission overlay, turn summary — see PLAN.md § "Fase TUI — diseño")
//! can add cell kinds without every existing match arm needing an
//! update. Cells hold their source text, not pre-wrapped lines — wrapping
//! happens at commit time, against whatever the terminal's width is at
//! that moment (see `Paragraph::wrap` in `app.rs`).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

pub trait HistoryCell {
    /// Renders this cell's content as owned `Text`.
    fn as_text(&self) -> Text<'static>;
}

/// What the user typed. The only visual distinction this skeleton draws
/// between roles: a `> ` marker on the first line (continuation lines of
/// a multi-line message get a blank-space marker instead, so they still
/// line up under the first). Richer styling (and markdown for the
/// assistant's side) is oleada 3.
pub struct UserCell {
    pub text: String,
}

impl HistoryCell for UserCell {
    fn as_text(&self) -> Text<'static> {
        let marker_style = Style::default().add_modifier(Modifier::BOLD);
        let lines: Vec<Line<'static>> = self
            .text
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let marker = if i == 0 { "> " } else { "  " };
                Line::from(vec![
                    Span::styled(marker, marker_style),
                    Span::raw(line.to_string()),
                ])
            })
            .collect();
        Text::from(lines)
    }
}

/// A fragment of the assistant's response — plain text in this skeleton
/// (markdown-aware rendering is oleada 3). May be committed more than
/// once per turn: `app.rs` flushes newline-terminated fragments as they
/// stream in rather than waiting for the whole message, so the
/// scrollback keeps growing live instead of appearing all at once when
/// the round ends.
pub struct AssistantTextCell {
    pub text: String,
}

impl HistoryCell for AssistantTextCell {
    fn as_text(&self) -> Text<'static> {
        Text::from(self.text.clone())
    }
}

/// A turn that failed outright (backend error, non-convergence past the
/// engine's iteration cap, ...) — styled distinctly so it doesn't read as
/// part of the assistant's answer.
pub struct ErrorCell {
    pub message: String,
}

impl HistoryCell for ErrorCell {
    fn as_text(&self) -> Text<'static> {
        Text::from(Line::from(Span::styled(
            format!("error: {}", self.message),
            Style::default().fg(Color::Red),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_cell_marks_the_first_line_and_indents_continuation_lines() {
        let cell = UserCell {
            text: "primera linea\nsegunda linea".to_string(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines.len(), 2);
        assert_eq!(text.lines[0].spans[0].content, "> ");
        assert_eq!(text.lines[1].spans[0].content, "  ");
    }

    #[test]
    fn assistant_text_cell_round_trips_its_text_verbatim() {
        let cell = AssistantTextCell {
            text: "hola mundo".to_string(),
        };
        assert_eq!(cell.as_text(), Text::from("hola mundo"));
    }

    #[test]
    fn error_cell_prefixes_the_message() {
        let cell = ErrorCell {
            message: "boom".to_string(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "error: boom");
    }
}
