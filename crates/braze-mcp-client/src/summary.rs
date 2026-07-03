//! Truncates an MCP tool's full `description` into a prompt-sized
//! [`ToolStub`](braze_types::ToolStub) `summary`.
//!
//! `list_stubs` fans this out over every tool on every turn (see PLAN.md,
//! "carga diferida de herramientas"), so the summary must stay small and
//! predictable regardless of how verbose an individual MCP server's tool
//! descriptions are.

/// Maximum length, in `char`s, of a generated summary (before an optional
/// trailing ellipsis). Chosen to comfortably fit a one-line description in
/// a prompt without needing per-tool budgeting.
const MAX_SUMMARY_CHARS: usize = 160;

/// Truncation criteria, in order:
///
/// 1. Only the first line of `description` is ever considered — anything
///    after the first `\n` is dropped outright, on the assumption that
///    MCP servers put the one-line gist first and elaborate below it.
/// 2. If that first line already fits within [`MAX_SUMMARY_CHARS`], it is
///    returned unchanged (no ellipsis).
/// 3. Otherwise, if a sentence-ending punctuation mark (`.`, `!`, `?`)
///    appears at or before [`MAX_SUMMARY_CHARS`], the summary is cut right
///    after it (keeping the punctuation, no ellipsis) — this reads as a
///    complete sentence rather than a hard cut.
/// 4. Otherwise, hard-truncate at the last whitespace boundary at or before
///    [`MAX_SUMMARY_CHARS`] (never splitting a word) and append `"…"`. If
///    there is no whitespace at all in that range (one very long word),
///    truncate exactly at [`MAX_SUMMARY_CHARS`] `char`s and append `"…"`.
/// 5. A missing/empty description summarizes to `""` — never fabricated.
pub(crate) fn summarize(description: &str) -> String {
    let first_line = description.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return String::new();
    }
    if first_line.chars().count() <= MAX_SUMMARY_CHARS {
        return first_line.to_string();
    }

    // Byte offset of the limit, on a char boundary.
    let limit_byte = first_line
        .char_indices()
        .nth(MAX_SUMMARY_CHARS)
        .map(|(idx, _)| idx)
        .unwrap_or(first_line.len());

    if let Some(sentence_end) = last_sentence_end(&first_line[..limit_byte]) {
        return first_line[..sentence_end].to_string();
    }

    let cut = last_whitespace(&first_line[..limit_byte]).unwrap_or(limit_byte);
    format!("{}…", first_line[..cut].trim_end())
}

/// Byte offset just after the last `.`/`!`/`?` in `text`, if any.
fn last_sentence_end(text: &str) -> Option<usize> {
    text.char_indices()
        .rfind(|(_, ch)| matches!(ch, '.' | '!' | '?'))
        .map(|(idx, ch)| idx + ch.len_utf8())
}

/// Byte offset of the last whitespace character in `text`, if any.
fn last_whitespace(text: &str) -> Option<usize> {
    text.char_indices()
        .rfind(|(_, ch)| ch.is_whitespace())
        .map(|(idx, _)| idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_description_summarizes_to_empty_string() {
        assert_eq!(summarize(""), "");
        assert_eq!(summarize("   \n more stuff"), "");
    }

    #[test]
    fn short_description_is_returned_unchanged() {
        assert_eq!(
            summarize("Reads a file from disk."),
            "Reads a file from disk."
        );
    }

    #[test]
    fn only_the_first_line_is_considered() {
        let description =
            "Reads a file from disk.\nSupports UTF-8 and binary modes.\nMore detail here.";
        assert_eq!(summarize(description), "Reads a file from disk.");
    }

    #[test]
    fn long_description_cuts_at_sentence_boundary_within_limit() {
        // First sentence ends well within MAX_SUMMARY_CHARS; second sentence
        // pushes the raw line past the limit, so it must be dropped and the
        // cut must land right after the first sentence's period.
        let first_sentence =
            "Performs a full recursive directory listing with size and permission metadata.";
        assert!(first_sentence.chars().count() <= 160);
        let second_sentence = " It also computes a rolling checksum for every regular file found along the way for later integrity verification.";
        let description = format!("{first_sentence}{second_sentence}");
        assert!(description.chars().count() > 160);

        let summary = summarize(&description);
        assert_eq!(summary, first_sentence);
        assert!(!summary.ends_with('…'));
    }

    #[test]
    fn long_description_with_no_sentence_boundary_truncates_at_word_boundary() {
        let description = "This tool exists purely to exercise the ToolStub summary truncation \
            logic implemented in braze-mcp-client by providing a description whose first line is \
            deliberately longer than the configured maximum summary length so the word boundary \
            truncation branch gets exercised end to end without ever hitting a period";
        assert!(description.chars().count() > 160);

        let summary = summarize(description);
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() <= 161); // 160 + ellipsis
        // Never split mid-word: the char right before the ellipsis must not
        // be adjacent to a non-whitespace char in the original string at
        // that same cut point (i.e. the truncated text re-appears verbatim
        // as a prefix followed by a word boundary in the source).
        let without_ellipsis = summary.trim_end_matches('…');
        assert!(description.starts_with(without_ellipsis.trim_end()));
    }

    #[test]
    fn description_with_no_whitespace_at_all_hard_truncates() {
        let description = "a".repeat(200);
        let summary = summarize(&description);
        assert_eq!(summary.chars().count(), 161); // 160 chars + ellipsis
        assert!(summary.starts_with(&"a".repeat(160)));
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn exactly_at_limit_is_not_truncated() {
        let description = "a".repeat(160);
        assert_eq!(summarize(&description), description);
    }

    #[test]
    fn multibyte_characters_do_not_panic_and_stay_on_char_boundaries() {
        let description = "café ".repeat(80); // well over 160 chars, multibyte
        let summary = summarize(&description);
        // Must not panic (char-boundary safe) and must be a valid String.
        assert!(summary.chars().count() <= 161);
    }
}
