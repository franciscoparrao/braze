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

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::theme::Theme;

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
    theme: Theme,
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
    pub fn running(name: String, theme: Theme) -> Self {
        Self {
            name,
            state: ToolCallOutcome::Running,
            theme,
        }
    }

    pub fn done(name: String, is_error: bool, content: &str, theme: Theme) -> Self {
        Self {
            name,
            state: ToolCallOutcome::Done {
                is_error,
                summary: summarize_tool_output(content),
            },
            theme,
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
                Span::styled("▶ ", Style::default().fg(self.theme.warning)),
                Span::raw(self.name.clone()),
            ])),
            ToolCallOutcome::Done { is_error, summary } => {
                let (glyph, color) = if *is_error {
                    ("✗ ", self.theme.error)
                } else {
                    ("✓ ", self.theme.success)
                };
                let mut spans = vec![
                    Span::styled(glyph, Style::default().fg(color)),
                    Span::raw(self.name.clone()),
                ];
                if !summary.is_empty() {
                    spans.push(Span::raw(": "));
                    spans.push(Span::styled(
                        summary.clone(),
                        Style::default().fg(self.theme.muted),
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
    pub theme: Theme,
}

impl HistoryCell for ErrorCell {
    fn as_text(&self) -> Text<'_> {
        Text::from(Line::from(Span::styled(
            format!("error: {}", self.message),
            Style::default().fg(self.theme.error),
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
    pub theme: Theme,
}

impl HistoryCell for PermissionCell {
    fn as_text(&self) -> Text<'_> {
        let (glyph, color, verb) = if self.allowed {
            ("✓ ", self.theme.success, "allowed")
        } else {
            ("✗ ", self.theme.error, "denied")
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
    pub theme: Theme,
}

impl HistoryCell for NoticeCell {
    fn as_text(&self) -> Text<'_> {
        Text::from(Line::from(Span::styled(
            self.message.clone(),
            Style::default().fg(self.theme.warning),
        )))
    }
}

/// The `/help` command's output — "fase TUI 2" (PLAN.md). Static
/// (doesn't reflect current app state, e.g. `turn_running`) — a
/// deliberate simplification for a command whose whole purpose is
/// "remind me what's available", not a live status readout.
pub struct HelpCell;

impl HistoryCell for HelpCell {
    fn as_text(&self) -> Text<'_> {
        let heading_style = Style::default().add_modifier(Modifier::BOLD);
        Text::from(vec![
            Line::from(Span::styled("Atajos", heading_style)),
            Line::from("Enter enviar · Ctrl+J salto de línea"),
            Line::from("Esc interrumpe el turno en curso (o deniega una aprobación pendiente)"),
            Line::from("Ctrl+T ver el output completo de la última tool call"),
            Line::from("Ctrl+C / Ctrl+D (composer vacío) salir"),
            Line::from(""),
            Line::from(Span::styled("Comandos", heading_style)),
            Line::from("/help  este mensaje"),
            Line::from("/quit, /exit  salir de braze"),
            Line::from(""),
            Line::from(Span::styled("Menciones", heading_style)),
            Line::from("@ seguido de parte de un nombre de archivo abre un buscador"),
        ])
    }
}

/// The full, untruncated content of a completed tool call — Ctrl+T
/// ("fase TUI 2", PLAN.md), for when `ToolCallCell`'s ~80-char summary
/// isn't enough. Read fresh from the session store when requested (see
/// `app.rs`'s `expand_last_tool_call`), never from a TUI-side cache —
/// the rollout log is already the single source of truth for this
/// content; duplicating it in `App` state would just be another copy to
/// keep in sync for no benefit.
pub struct ExpandedToolOutputCell {
    pub name: String,
    pub is_error: bool,
    pub content: String,
    pub theme: Theme,
}

/// Longest content this cell shows before truncating with a note — much
/// more generous than `ToolCallCell`'s ~80-char summary (this *is* the
/// "give me the full thing" view), but still bounded: an extreme tool
/// output (a huge file read) dumped unbounded into the scrollback would
/// be unwieldy to scroll past, and risks `insert_before`'s known
/// `u16::MAX`-row ceiling (ratatui#1426) on top of that.
const EXPANDED_TOOL_OUTPUT_MAX_LINES: usize = 200;

impl HistoryCell for ExpandedToolOutputCell {
    fn as_text(&self) -> Text<'_> {
        let (glyph, color) = if self.is_error {
            ("✗ ", self.theme.error)
        } else {
            ("✓ ", self.theme.success)
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(glyph, Style::default().fg(color)),
            Span::styled(
                format!("{} (completo)", self.name),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])];

        let content_lines: Vec<&str> = self.content.lines().collect();
        lines.extend(
            content_lines
                .iter()
                .take(EXPANDED_TOOL_OUTPUT_MAX_LINES)
                .map(|line| Line::from((*line).to_string())),
        );
        if content_lines.len() > EXPANDED_TOOL_OUTPUT_MAX_LINES {
            lines.push(Line::from(Span::styled(
                format!(
                    "… (+{} líneas más — ver el rollout log completo de la sesión)",
                    content_lines.len() - EXPANDED_TOOL_OUTPUT_MAX_LINES
                ),
                Style::default().fg(self.theme.muted),
            )));
        }

        Text::from(lines)
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
        let cell = ToolCallCell::running("read_file".to_string(), Theme::dark());
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "▶ ");
        assert_eq!(text.lines[0].spans[1].content, "read_file");
    }

    #[test]
    fn tool_call_cell_done_success_uses_a_check_glyph_and_summarizes_output() {
        let cell = ToolCallCell::done("echo".to_string(), false, "echoed: hi", Theme::dark());
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✓ ");
        assert_eq!(text.lines[0].spans[1].content, "echo");
        assert!(text.lines[0].spans[3].content.contains("echoed: hi"));
    }

    #[test]
    fn tool_call_cell_done_error_uses_a_cross_glyph() {
        let cell = ToolCallCell::done("read_file".to_string(), true, "file not found", Theme::dark());
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
            theme: Theme::dark(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "error: boom");
    }

    #[test]
    fn permission_cell_allowed_uses_a_check_glyph() {
        let cell = PermissionCell {
            description: "run `rm -rf /tmp/x`".to_string(),
            allowed: true,
            theme: Theme::dark(),
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
            theme: Theme::dark(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✗ ");
        assert!(text.lines[0].spans[1].content.contains("denied"));
    }

    #[test]
    fn notice_cell_renders_the_message_verbatim() {
        let cell = NoticeCell {
            message: "interrupted by user".to_string(),
            theme: Theme::dark(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "interrupted by user");
    }

    #[test]
    fn help_cell_lists_commands_and_keybindings() {
        let text = HelpCell.as_text();
        let rendered: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("/help"));
        assert!(rendered.contains("/quit"));
        assert!(rendered.contains("Ctrl+C"));
        assert!(rendered.contains("Ctrl+T"));
        assert!(rendered.contains('@'));
    }

    #[test]
    fn expanded_tool_output_cell_shows_the_full_content_untruncated() {
        let long_content = (0..10)
            .map(|i| format!("linea {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cell = ExpandedToolOutputCell {
            name: "read_file".to_string(),
            is_error: false,
            content: long_content.clone(),
            theme: Theme::dark(),
        };
        let text = cell.as_text();
        let rendered: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("read_file"));
        assert!(rendered.contains("linea 0"));
        assert!(rendered.contains("linea 9"));
        assert!(!rendered.contains("líneas más"));
    }

    #[test]
    fn expanded_tool_output_cell_truncates_past_the_line_cap_with_a_note() {
        let content = (0..(EXPANDED_TOOL_OUTPUT_MAX_LINES + 5))
            .map(|i| format!("linea {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cell = ExpandedToolOutputCell {
            name: "grep".to_string(),
            is_error: false,
            content,
            theme: Theme::dark(),
        };
        let text = cell.as_text();
        let rendered: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("+5 líneas más"));
        assert!(!rendered.contains(&format!("linea {}", EXPANDED_TOOL_OUTPUT_MAX_LINES + 4)));
    }

    #[test]
    fn expanded_tool_output_cell_error_uses_a_cross_glyph() {
        let cell = ExpandedToolOutputCell {
            name: "shell_exec".to_string(),
            is_error: true,
            content: "boom".to_string(),
            theme: Theme::dark(),
        };
        let text = cell.as_text();
        assert_eq!(text.lines[0].spans[0].content, "✗ ");
    }
}

/// Snapshot tests (PLAN.md § "Fase TUI — diseño", oleada 5): each cell
/// rendered to a fixed-width `Buffer` and snapshotted via `Buffer`'s own
/// `Debug` impl — the idiomatic ratatui format, showing both content
/// (as quoted per-row strings) and every styled run (fg/bg/modifier).
/// Catches regressions in wrapping, glyphs, and styling together that a
/// plain `as_text()` assertion on spans wouldn't (e.g. a cell that wraps
/// one row too many, or loses a color, at a specific terminal width).
#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    /// Mirrors `app.rs`'s `commit_cell`: wrap to `width`, size the area
    /// to exactly the wrapped line count, render.
    fn render_to_buffer(cell: &dyn HistoryCell, width: u16) -> Buffer {
        let paragraph = Paragraph::new(cell.as_text()).wrap(Wrap { trim: false });
        let height = paragraph.line_count(width).max(1) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        paragraph.render(area, &mut buffer);
        buffer
    }

    #[test]
    fn user_cell_multiline() {
        let cell = UserCell {
            text: "primera linea\nsegunda linea mas larga que la primera".to_string(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn assistant_markdown_cell_with_heading_list_and_code_block() {
        let cell = AssistantMarkdownCell {
            markdown: "# Titulo\n\n- uno\n- dos\n\n```rust\nfn main() {}\n```\n".to_string(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn tool_call_cell_running() {
        let cell = ToolCallCell::running("read_file".to_string(), Theme::dark());
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn tool_call_cell_done_success() {
        let cell = ToolCallCell::done("echo".to_string(), false, "echoed: hi", Theme::dark());
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn tool_call_cell_done_error() {
        let cell = ToolCallCell::done("read_file".to_string(), true, "file not found", Theme::dark());
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn permission_cell_allowed() {
        let cell = PermissionCell {
            description: "run `rm -rf /tmp/x`".to_string(),
            allowed: true,
            theme: Theme::dark(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn permission_cell_denied() {
        let cell = PermissionCell {
            description: "run `rm -rf /tmp/x`".to_string(),
            allowed: false,
            theme: Theme::dark(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn error_cell() {
        let cell = ErrorCell {
            message: "backend unreachable".to_string(),
            theme: Theme::dark(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn notice_cell() {
        let cell = NoticeCell {
            message: "⏸ interrupted by user".to_string(),
            theme: Theme::dark(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }

    #[test]
    fn help_cell() {
        insta::assert_debug_snapshot!(render_to_buffer(&HelpCell, 50));
    }

    #[test]
    fn expanded_tool_output_cell() {
        let cell = ExpandedToolOutputCell {
            name: "read_file".to_string(),
            is_error: false,
            content: "linea uno\nlinea dos".to_string(),
            theme: Theme::dark(),
        };
        insta::assert_debug_snapshot!(render_to_buffer(&cell, 40));
    }
}
