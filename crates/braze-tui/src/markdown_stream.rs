//! [`MarkdownStreamCollector`]: newline-gated commit boundaries for
//! streaming markdown — the oleada-3 upgrade to oleada 2's plain
//! `drain_ready_lines`. Complete lines are still committed as soon as
//! they arrive (ordinary prose stays exactly as live as it was in
//! oleada 2), but a line that opens a fenced code block (` ``` `) holds
//! back every subsequent line — the whole fence only becomes safe to
//! commit, as one atomic chunk, once its closing ` ``` ` arrives.
//!
//! Why holding back matters: each commit gets rendered independently by
//! `tui_markdown::from_str` (see `history_cell::AssistantMarkdownCell`)
//! and never re-rendered. Splitting a fence across two such independent
//! calls would render its closing half as plain text instead of code —
//! the parser has no memory of the still-open fence from the previous
//! chunk. This adapts Gemini CLI's `findLastSafeSplitPoint`
//! (`docs/TUI-INVESTIGACION-2026-07.md`'s anexo), which gates on
//! paragraph breaks, to line-level granularity instead — matching
//! oleada 2's more responsive per-line UX for ordinary prose.
//!
//! Recognizes only the common ` ``` ` fence marker (not `~~~`) — a
//! deliberate simplification consistent with this codebase's other
//! best-effort text heuristics (e.g. `engine::try_parse_textual_tool_call`).

#[derive(Default)]
pub struct MarkdownStreamCollector {
    buffer: String,
    /// Byte offset into `buffer` up to which content has already been
    /// returned by `commit_ready`/`finish`. Everything at or after this
    /// offset is invariant-guaranteed to start *outside* an open fence
    /// (see `commit_ready`'s doc comment) — that's what lets
    /// `safe_commit_boundary` rescan just the tail on every call instead
    /// of the whole buffer, keeping this O(new content) per call rather
    /// than O(total response length).
    committed_len: usize,
}

impl MarkdownStreamCollector {
    pub fn push(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    /// The still-uncommitted tail — what oleada 2 called `active_text`.
    /// Rendered as plain text in the live preview area (`app.rs`'s
    /// `ACTIVE_ROWS`), deliberately not markdown-rendered: incremental
    /// rendering of *unstable, still-changing* markdown is a much larger
    /// problem than gating commit boundaries, and out of scope here —
    /// only committed, final chunks get `tui-markdown` treatment.
    pub fn pending(&self) -> &str {
        &self.buffer[self.committed_len..]
    }

    /// Returns markdown source newly safe to commit (render once,
    /// permanently), if any new content has crossed a safe boundary
    /// since the last call — see this module's doc comment for what
    /// "safe" means. `None` while still inside an open fence, or while
    /// the newest line hasn't been newline-terminated yet.
    pub fn commit_ready(&mut self) -> Option<String> {
        let boundary = self.committed_len + safe_commit_boundary(self.pending())?;
        if boundary <= self.committed_len {
            return None;
        }
        let chunk = self.buffer[self.committed_len..boundary].to_string();
        self.committed_len = boundary;
        Some(chunk)
    }

    /// Flushes whatever remains unconditionally, fence or not. Called
    /// once the round's text is fully persisted (`AgentEvent::AssistantText`)
    /// — at that point there is no more text coming for an unclosed
    /// fence to ever close, so rendering it as-is (a fence missing its
    /// closing marker just renders as a fence running to the end, which
    /// is the correct reading here) beats holding it forever.
    pub fn finish(&mut self) -> Option<String> {
        if self.committed_len >= self.buffer.len() {
            return None;
        }
        let chunk = self.buffer[self.committed_len..].to_string();
        self.committed_len = self.buffer.len();
        Some(chunk)
    }
}

/// The backtick-fence run length a line opens or could close with, if
/// any — `None` if the line isn't a fence delimiter at all. Strips a
/// leading blockquote marker (bajo, docs/AUDITORIA-2026-07-v2.md,
/// "heurística de fences falla en fences anidadas/citadas": a fence
/// nested inside a blockquote, e.g. `"> ```"`, previously never matched
/// `starts_with("```")` at all, so `in_fence` never toggled for it) and
/// any amount of leading whitespace before counting the run of
/// backticks, mirroring CommonMark's own fence-marker rule (a run of 3+
/// backticks, optionally indented/quoted).
fn fence_marker_len(line_without_newline: &str) -> Option<usize> {
    let stripped = line_without_newline.trim_start();
    let stripped = stripped
        .strip_prefix('>')
        .map(str::trim_start)
        .unwrap_or(stripped);
    let backticks = stripped.chars().take_while(|&c| c == '`').count();
    (backticks >= 3).then_some(backticks)
}

/// Byte offset (relative to the start of `text`) up to which `text` can
/// be safely committed: the end of the last complete line that is not
/// inside an open code fence. `text` is assumed to start outside any
/// fence (an invariant `MarkdownStreamCollector` maintains — see its
/// `committed_len` doc comment).
///
/// Tracks the *opening* fence's backtick-run length and only treats a
/// later line as the matching close if its own run is at least as long
/// (bajo, docs/AUDITORIA-2026-07-v2.md, "heurística de fences falla en
/// fences anidadas"), same as CommonMark itself — otherwise a fence
/// whose content demonstrates markdown syntax (a literal ` ``` ` line
/// inside it) would close the outer fence prematurely on that inner
/// line instead of treating it as ordinary fenced content.
fn safe_commit_boundary(text: &str) -> Option<usize> {
    let mut open_fence_len: Option<usize> = None;
    let mut offset = 0;
    let mut last_safe: Option<usize> = None;

    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break; // trailing partial line — never safe on its own
        }
        offset += line.len();
        let marker_len = fence_marker_len(&line[..line.len() - 1]);

        match (open_fence_len, marker_len) {
            (None, Some(len)) => {
                // Opens a new fence: deliberately do NOT mark this
                // boundary safe — everything from here onward stays
                // held back until the matching close.
                open_fence_len = Some(len);
            }
            (Some(opening_len), Some(closing_len)) if closing_len >= opening_len => {
                // Closes the currently-open fence — the whole fence
                // (including this closing line) is now safe as one
                // atomic chunk.
                open_fence_len = None;
                last_safe = Some(offset);
            }
            _ => {
                // Either a plain line, or a shorter backtick run inside
                // an open fence (ordinary fenced content, not a close).
                if open_fence_len.is_none() {
                    last_safe = Some(offset);
                }
            }
        }
    }

    last_safe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_complete_lines_progressively_outside_a_fence() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("primera linea\n");
        assert_eq!(
            collector.commit_ready(),
            Some("primera linea\n".to_string())
        );

        collector.push("segunda ");
        assert_eq!(collector.commit_ready(), None, "no newline yet — not safe");

        collector.push("linea\n");
        assert_eq!(
            collector.commit_ready(),
            Some("segunda linea\n".to_string())
        );
    }

    #[test]
    fn holds_back_an_open_fence_until_it_closes_then_flushes_it_as_one_chunk() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("antes del bloque\n```rust\n");
        // The prose line committed; the fence-open line did not.
        assert_eq!(
            collector.commit_ready(),
            Some("antes del bloque\n".to_string())
        );

        collector.push("let x = 1;\n");
        assert_eq!(
            collector.commit_ready(),
            None,
            "a complete line inside an open fence must not be committed on its own"
        );

        collector.push("```\n");
        assert_eq!(
            collector.commit_ready(),
            Some("```rust\nlet x = 1;\n```\n".to_string()),
            "the whole fence becomes safe as one chunk only once it closes"
        );
    }

    #[test]
    fn finish_flushes_an_unclosed_trailing_fence() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("texto\n```python\nprint(1)\n");
        assert_eq!(collector.commit_ready(), Some("texto\n".to_string()));
        assert_eq!(
            collector.commit_ready(),
            None,
            "fence never closed — nothing new is safe"
        );
        assert_eq!(
            collector.finish(),
            Some("```python\nprint(1)\n".to_string())
        );
        assert_eq!(collector.finish(), None, "nothing left to flush twice");
    }

    #[test]
    fn finish_flushes_a_trailing_partial_line_with_no_newline() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("linea completa\nsin salto final");
        assert_eq!(
            collector.commit_ready(),
            Some("linea completa\n".to_string())
        );
        assert_eq!(collector.finish(), Some("sin salto final".to_string()));
    }

    #[test]
    fn pending_reflects_only_the_uncommitted_tail() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("lista\nparcial sin cerrar");
        collector.commit_ready();
        assert_eq!(collector.pending(), "parcial sin cerrar");
    }

    /// However finely the same content is chunked across `push` calls,
    /// concatenating every `commit_ready` result plus the final
    /// `finish` must reconstruct the original text exactly — the
    /// invariant the whole design leans on.
    #[test]
    fn commit_ready_plus_finish_always_reconstructs_the_original_text_regardless_of_chunking() {
        let full = "linea uno\nlinea dos\n```json\n{\"a\": 1}\n```\nlinea final sin salto";

        // Byte-by-byte streaming: the most adversarial chunking.
        let mut collector = MarkdownStreamCollector::default();
        let mut reconstructed = String::new();
        for byte in full.as_bytes() {
            collector.push(std::str::from_utf8(std::slice::from_ref(byte)).unwrap());
            if let Some(chunk) = collector.commit_ready() {
                reconstructed.push_str(&chunk);
            }
        }
        if let Some(tail) = collector.finish() {
            reconstructed.push_str(&tail);
        }
        assert_eq!(reconstructed, full);
    }

    /// Regression test for the "heurística de fences falla en fences
    /// anidadas/citadas" bajo (docs/AUDITORIA-2026-07-v2.md): a fence
    /// nested inside a blockquote (`"> ```"`) must still be recognized
    /// as a delimiter — previously `trim_start()` alone never stripped
    /// the leading `>`, so `in_fence` never toggled and the quoted
    /// fence's content committed line-by-line as ordinary prose instead
    /// of being held back as one atomic chunk.
    #[test]
    fn a_blockquoted_fence_is_recognized_and_held_back_until_it_closes() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("> ```\n> codigo\n");
        assert_eq!(
            collector.commit_ready(),
            None,
            "the blockquoted fence must still be open, holding back its content"
        );
        collector.push("> ```\n");
        assert_eq!(
            collector.commit_ready(),
            Some("> ```\n> codigo\n> ```\n".to_string()),
            "the whole blockquoted fence must commit atomically once closed"
        );
    }

    /// Regression test for the same bajo: an outer fence using a longer
    /// backtick run (4) can contain a literal 3-backtick fence as
    /// ordinary content (e.g. documentation demonstrating markdown
    /// syntax) without that inner line prematurely closing the outer
    /// fence — matching CommonMark's own length-based fence matching.
    #[test]
    fn a_shorter_backtick_run_inside_a_longer_fence_does_not_close_it() {
        let mut collector = MarkdownStreamCollector::default();
        collector.push("````\nejemplo:\n```\ncodigo\n```\n");
        assert_eq!(
            collector.commit_ready(),
            None,
            "the inner 3-backtick lines must not close the outer 4-backtick fence"
        );
        collector.push("````\n");
        assert_eq!(
            collector.commit_ready(),
            Some("````\nejemplo:\n```\ncodigo\n```\n````\n".to_string())
        );
    }
}
