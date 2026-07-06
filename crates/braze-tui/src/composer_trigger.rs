//! [`detect_trigger`]: pure detection of whether the composer's cursor
//! currently sits inside an active `/command` or `@mention` token —
//! "fase TUI 2" (PLAN.md), slash commands + file mentions. Kept as a
//! standalone, character-indexed function (no `TextArea` dependency) so
//! it's testable without a real composer.

/// What the cursor is currently "inside", if anything — `app.rs`'s
/// `refresh_popup` turns this into a live `ComposerPopup` with matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerTrigger {
    /// A `/command` token — only recognized as the very first token of
    /// the whole composer (row 0, starting at column 0), matching how
    /// slash commands work in the reference TUIs this project studied
    /// (never mid-sentence). `String` is everything typed after the
    /// `/`, not including it.
    Slash(String),
    /// An `@mention` token — recognized anywhere in the text (unlike
    /// `/`, a file reference can appear mid-sentence). `String` is
    /// everything typed after the `@`, not including it.
    Mention(String),
}

/// Looks at `line` (the composer's current line, as `char`s — cursor
/// positions from `ratatui_textarea::TextArea::cursor()` are
/// character-indexed, not byte-indexed) and `col` (the cursor's column
/// within it) to find the whitespace-delimited token immediately behind
/// the cursor, then classifies it. `is_first_line` must be `cursor.0 ==
/// 0` from the caller — needed to restrict `/` to the very first token
/// of the message.
pub fn detect_trigger(line: &str, col: usize, is_first_line: bool) -> Option<ComposerTrigger> {
    let chars: Vec<char> = line.chars().collect();
    let col = col.min(chars.len());

    let mut start = col;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    if start >= col {
        return None; // cursor right after whitespace (or at column 0 with nothing typed yet)
    }

    let token: String = chars[start..col].iter().collect();

    if is_first_line
        && start == 0
        && let Some(rest) = token.strip_prefix('/')
    {
        return Some(ComposerTrigger::Slash(rest.to_string()));
    }
    if let Some(rest) = token.strip_prefix('@') {
        return Some(ComposerTrigger::Mention(rest.to_string()));
    }
    None
}

/// Length (in chars) of whatever remains typed *after* the cursor, within
/// the same whitespace-delimited token `detect_trigger` found behind it —
/// bajo (docs/AUDITORIA-2026-07-v2.md, "replace_trigger_token deja
/// residuo con el cursor a mitad de token"): `detect_trigger` only scans
/// backward from the cursor, so accepting a completion with the cursor
/// mid-token (e.g. `@fo|o.txt`, cursor at `|`) left the "o.txt" suffix
/// stranded right after the inserted replacement. Scans forward from
/// `col` (char-indexed, same convention as `detect_trigger`) to the next
/// whitespace or end of line.
pub fn token_suffix_len(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let col = col.min(chars.len());
    let mut end = col;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    end - col
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_slash_at_the_start_of_the_first_line_is_an_empty_slash_query() {
        assert_eq!(
            detect_trigger("/", 1, true),
            Some(ComposerTrigger::Slash(String::new()))
        );
    }

    #[test]
    fn a_partially_typed_command_is_the_slash_query_so_far() {
        assert_eq!(
            detect_trigger("/hel", 4, true),
            Some(ComposerTrigger::Slash("hel".to_string()))
        );
    }

    #[test]
    fn a_slash_not_at_the_start_of_the_message_is_not_a_command() {
        // "hola /help" — cursor right after "help", but the "/" isn't at
        // the very start of the whole message.
        assert_eq!(detect_trigger("hola /help", 10, true), None);
    }

    #[test]
    fn a_slash_on_a_later_line_is_never_a_command_even_at_column_zero() {
        assert_eq!(detect_trigger("/help", 5, false), None);
    }

    #[test]
    fn a_mention_is_recognized_anywhere_in_the_line() {
        assert_eq!(
            detect_trigger("revisa @src/main", 16, true),
            Some(ComposerTrigger::Mention("src/main".to_string()))
        );
        // Also fine on a later line, unlike slash commands.
        assert_eq!(
            detect_trigger("revisa @src/main", 16, false),
            Some(ComposerTrigger::Mention("src/main".to_string()))
        );
    }

    #[test]
    fn a_finished_token_with_a_trailing_space_is_not_an_active_trigger() {
        // The cursor is past the space following "/help" — the command
        // was already "completed" by whitespace, no longer being typed.
        assert_eq!(detect_trigger("/help ", 6, true), None);
    }

    #[test]
    fn plain_text_is_neither_a_command_nor_a_mention() {
        assert_eq!(detect_trigger("hola mundo", 10, true), None);
    }

    #[test]
    fn cursor_mid_word_only_sees_the_token_up_to_the_cursor() {
        // col=3 sits right after "/he" (0-based, before "lp") — only
        // "he" counts, "lp" is past the cursor and irrelevant.
        assert_eq!(
            detect_trigger("/help", 3, true),
            Some(ComposerTrigger::Slash("he".to_string()))
        );
    }

    #[test]
    fn token_suffix_len_finds_the_remainder_up_to_whitespace() {
        // "@fo|o.txt" — cursor at column 3 (after "@fo"), "o.txt" (5
        // chars) still follows before the next whitespace/end.
        assert_eq!(token_suffix_len("@foo.txt", 3), 5);
    }

    #[test]
    fn token_suffix_len_is_zero_at_the_end_of_the_token() {
        assert_eq!(token_suffix_len("@help", 5), 0);
    }

    #[test]
    fn token_suffix_len_stops_at_the_next_whitespace() {
        assert_eq!(token_suffix_len("@src/main resto", 4), 5);
    }

    #[test]
    fn token_suffix_len_is_zero_on_an_empty_line() {
        assert_eq!(token_suffix_len("", 0), 0);
    }
}
