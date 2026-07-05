//! [`HistoryCell`]: what gets committed to the terminal's native
//! scrollback (see `app.rs`'s `commit_cell`, and
//! `docs/TUI-INVESTIGACION-2026-07.md`'s convergence #1 — finalized
//! content is written once to the scrollback, never re-rendered).
//!
//! A trait rather than an enum so later oleadas (permission overlay,
//! turn summary — see PLAN.md § "Fase TUI — diseño") can add cell kinds
//! without every existing match arm needing an update. `as_text` borrows
//! from `&self` rather than requiring an owned `'static` `Text` —
//! `AssistantMarkdownCell` renders straight from its stored markdown
//! source via `tui_markdown::from_str`, which itself borrows.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

pub trait HistoryCell {
    fn as_text(&self) -> Text<'_>;
}

/// What the user typed. The only visual distinction this skeleton draws
/// between roles: a `> ` marker on the first line (continuation lines of
/// a multi-line message get a blank-space marker instead, so they still
/// line up under the first). Deliberately not markdown-rendered — unlike
/// the assistant's side, literal user input shouldn't be reinterpreted.
pub struct UserCell {
    pub text: String,
}

impl HistoryCell for UserCell {
    fn as_text(&self) -> Text<'_> {
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

/// A fragment of the assistant's response, rendered as markdown via
/// `tui_markdown` — bold/headers/lists/code blocks with syntax
/// highlighting. May be committed more than once per turn:
/// `app.rs`'s `MarkdownStreamCollector` flushes safe chunks as they
/// stream in rather than waiting for the whole message, so the
/// scrollback keeps growing live instead of appearing all at once when
/// the round ends.
pub struct AssistantMarkdownCell {
    pub markdown: String,
}

impl HistoryCell for AssistantMarkdownCell {
    fn as_text(&self) -> Text<'_> {
        tui_markdown::from_str(&self.markdown)
    }
}

/// A tool call's outcome, committed once per state transition this
/// skeleton renders — not mutated in place (scrollback commits are
/// append-only, like a real terminal's), so a call that both starts and
/// finishes produces two separate lines in the transcript: one at
/// `ToolCallStarted`, one at `ToolCallCompleted`. A call rejected before
/// ever running (bad schema, unknown tool, ...) skips straight to
/// `Done` — the engine never emits `ToolCallStarted` for those (see
/// `braze_engine::Engine::dispatch_tool_calls`), so no misleading
/// "running" line appears for something that never ran.
pub struct ToolCallCell {
    name: String,
    state: ToolCallOutcome,
}

enum ToolCallOutcome {
    Running,
    Done { is_error: bool, summary: String },
}

/// Longest single line of tool output shown in a `Done` cell's summary
/// before truncating — mirrors the "keep the transcript scannable"
/// truncation `codex-rs/tui` applies to its own exec/tool cells (see
/// `docs/TUI-INVESTIGACION-2026-07.md`, informe 1 § 4). The full,
/// untruncated result isn't lost — it's still in the session's rollout
/// log — this skeleton just doesn't have a pager overlay to view it
/// (that's "fase TUI 2", per PLAN.md's diferidos).
const TOOL_SUMMARY_MAX_CHARS: usize = 80;

impl ToolCallCell {
    pub fn running(name: String) -> Self {
        Self {
            name,
            state: ToolCallOutcome::Running,
        }
    }

    pub fn done(name: String, is_error: bool, content: &str) -> Self {
        Self {
            name,
            state: ToolCallOutcome::Done {
                is_error,
                summary: summarize_tool_output(content),
            },
        }
    }
}

fn summarize_tool_output(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or("").trim();
    let truncated = first_line.chars().count() > TOOL_SUMMARY_MAX_CHARS;
    let mut summary: String = first_line.chars().take(TOOL_SUMMARY_MAX_CHARS).collect();
    if truncated {
        summary.push('…');
    }
    let remaining_lines = content.lines().count().saturating_sub(1);
    if remaining_lines > 0 {
        summary.push_str(&format!(" (+{remaining_lines} more lines)"));
    }
    summary
}

impl HistoryCell for ToolCallCell {
    fn as_text(&self) -> Text<'_> {
        match &self.state {
            ToolCallOutcome::Running => Text::from(Line::from(vec![
                Span::styled("▶ ", Style::default().fg(Color::Yellow)),
                Span::raw(self.name.clone()),
            ])),
            ToolCallOutcome::Done { is_error, summary } => {
                let (glyph, color) = if *is_error {
                    ("✗ ", Color::Red)
                } else {
                    ("✓ ", Color::Green)
                };
                let mut spans = vec![
                    Span::styled(glyph, Style::default().fg(color)),
                    Span::raw(self.name.clone()),
                ];
                if !summary.is_empty() {
                    spans.push(Span::raw(": "));
                    spans.push(Span::styled(
                        summary.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                Text::from(Line::from(spans))
            }
        }
    }
}

/// A turn that failed outright (backend error, non-convergence past the
/// engine's iteration cap, ...) — styled distinctly so it doesn't read as
/// part of the assistant's answer.
pub struct ErrorCell {
    pub message: String,
}

impl HistoryCell for ErrorCell {
    fn as_text(&self) -> Text<'_> {
        Text::from(Line::from(Span::styled(
            format!("error: {}", self.message),
            Style::default().fg(Color::Red),
        )))
    }
}

/// A permission decision, recorded once it's been answered (`app.rs`'s
/// `answer_pending_approval`) — the audit trail of *what got approved or
/// denied* belongs in the transcript, not just in the ephemeral overlay
/// that asked. `description` is `ActionDescriptor`'s `Display` output,
/// same string persisted in the session's `PermissionRequested`/
/// `PermissionDecided` events (see `approval::ChannelConfirmationPrompt`).
pub struct PermissionCell {
    pub description: String,
    pub allowed: bool,
}

impl HistoryCell for PermissionCell {
    fn as_text(&self) -> Text<'_> {
        let (glyph, color, verb) = if self.allowed {
            ("✓ ", Color::Green, "allowed")
        } else {
            ("✗ ", Color::Red, "denied")
        };
        Text::from(Line::from(vec![
            Span::styled(glyph, Style::default().fg(color)),
            Span::raw(format!("{verb}: {}", self.description)),
        ]))
    }
}

/// A neutral, informational note — distinct from `ErrorCell`: a turn the
/// *user* chose to interrupt (Esc, `app.rs`'s `interrupt_turn`) isn't a
/// failure, so it shouldn't read as one.
pub struct NoticeCell {
    pub message: String,
}

impl HistoryCell for NoticeCell {
    fn as_text(&self) -> Text<'_> {
        Text::from(Line::from(Span::styled(
            self.message.clone(),
            Style::default().fg(Color::Yellow),
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
    fn assistant_markdown_cell_renders_bold_as_a_styled_span() {
        let cell = AssistantMarkdownCell {
            markdown: "**hola**".to_string(),
        };
        let text = cell.as_text();
        assert!(
            text.lines[0]
                .spans
                .iter()
                .any(|span| span.content.contains("hola")
                    && span.style.add_modifier.contains(Modifier::BOLD)),
            "expected a bold span containing 'hola', got: {text:?}"
        );
    }

    #[test]
    fn tool_call_cell_running_shows_the_name_with_a_running_glyph() {
        let cell = ToolCallCell::running("read_file".to_string());
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "▶ ");
        assert_eq!(text.lines[0].spans[1].content, "read_file");
    }

    #[test]
    fn tool_call_cell_done_success_uses_a_check_glyph_and_summarizes_output() {
        let cell = ToolCallCell::done("echo".to_string(), false, "echoed: hi");
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✓ ");
        assert_eq!(text.lines[0].spans[1].content, "echo");
        assert!(text.lines[0].spans[3].content.contains("echoed: hi"));
    }

    #[test]
    fn tool_call_cell_done_error_uses_a_cross_glyph() {
        let cell = ToolCallCell::done("read_file".to_string(), true, "file not found");
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✗ ");
    }

    #[test]
    fn summarize_tool_output_truncates_long_first_lines_and_counts_remaining() {
        let content = format!("{}\nsegunda\ntercera", "x".repeat(200));
        let summary = summarize_tool_output(&content);
        assert!(summary.contains('…'), "expected an ellipsis, got: {summary}");
        assert!(summary.contains("(+2 more lines)"));
        assert!(summary.chars().count() < content.chars().count());
    }

    #[test]
    fn summarize_tool_output_of_a_single_short_line_is_left_untouched() {
        assert_eq!(summarize_tool_output("echoed: hi"), "echoed: hi");
    }

    #[test]
    fn error_cell_prefixes_the_message() {
        let cell = ErrorCell {
            message: "boom".to_string(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "error: boom");
    }

    #[test]
    fn permission_cell_allowed_uses_a_check_glyph() {
        let cell = PermissionCell {
            description: "run `rm -rf /tmp/x`".to_string(),
            allowed: true,
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✓ ");
        assert!(text.lines[0].spans[1].content.contains("allowed"));
        assert!(text.lines[0].spans[1].content.contains("rm -rf"));
    }

    #[test]
    fn permission_cell_denied_uses_a_cross_glyph() {
        let cell = PermissionCell {
            description: "run `rm -rf /tmp/x`".to_string(),
            allowed: false,
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✗ ");
        assert!(text.lines[0].spans[1].content.contains("denied"));
    }

    #[test]
    fn notice_cell_renders_the_message_verbatim() {
        let cell = NoticeCell {
            message: "interrupted by user".to_string(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "interrupted by user");
    }
}
