//! `edit_file` tool: replaces exactly one occurrence of `old_string` with
//! `new_string` in a file. Fails if `old_string` doesn't appear, or
//! appears more than once — same disambiguation principle as Claude
//! Code's Edit tool: an ambiguous edit is refused rather than guessed at.
//! Guarded — treated as a write for permission purposes (there is no
//! separate `ActionDescriptor::EditFile` variant).
//!
//! ## Fuzzy application (docs/SOTA-2026-07.md, adenda Aider)
//!
//! Small models (3-7B, braze's executor target) frequently reproduce the
//! text they intend to replace with small whitespace deviations —
//! trailing spaces dropped, indentation re-emitted at a different depth.
//! Aider measured 9× fewer apply failures from tolerating exactly that
//! class of deviation. So matching runs as a ladder, strictest first,
//! each rung still requiring an *unambiguous* (exactly-one) match:
//!
//! 1. exact substring (unchanged original behavior — always wins);
//! 2. line-window match ignoring *trailing* whitespace per line;
//! 3. line-window match ignoring *leading and trailing* whitespace,
//!    with `new_string` re-indented by the offset observed between the
//!    file's first matched line and `old_string`'s first line — the
//!    file's real indentation wins, not the model's.
//!
//! Rungs 2-3 are line-window matches: `old_string`'s lines must
//! correspond to whole lines of the file (the observed failure mode is
//! "right lines, wrong whitespace", not partial-line fragments).

use std::path::PathBuf;

use serde::Deserialize;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"path": "src/lib.rs", "old_string": "foo", "new_string": "bar"}`.
#[derive(Debug, Deserialize)]
pub struct EditFileArgs {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

/// Steering appended to matching failures — the whole-file path is the
/// empirically better edit surface for small models (Aider's leaderboard
/// assigns whole-file to every small model; see the module doc comment),
/// so a model that can't reproduce the exact text gets pointed there
/// instead of retrying the same failing shape.
const WRITE_FILE_STEERING: &str = "If you cannot reproduce the exact current text, use \
     write_file with the complete updated file content instead.";

/// `Ok(summary)` on success. `Err(message)` covers I/O failures and the
/// disambiguation failures (`old_string` missing / ambiguous) — all
/// recoverable tool-level failures, see `provider.rs::wrap`.
pub async fn edit_file(args: EditFileArgs) -> Result<String, String> {
    if args.old_string.is_empty() {
        return Err("old_string must not be empty".to_string());
    }

    let path = PathBuf::from(&args.path);
    let original = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        // A model reaching for edit_file to CREATE a file is a predictable
        // mistake (the tool name reads as "change a file"); a bare OS error
        // ("No such file or directory") gives it nothing to act on, unlike
        // the matching-failure messages below, which all steer somewhere.
        // See docs/AUDITORIA-2026-07-v3.md, hallazgo A4.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "'{}' does not exist. To create a new file, use write_file with its full content.",
                path.display()
            ));
        }
        Err(err) => return Err(format!("failed to read '{}': {err}", path.display())),
    };

    let (updated, strategy) = apply_edit(&original, &args.old_string, &args.new_string)
        .map_err(|kind| kind.into_message(&path, &original, &args.old_string))?;

    tokio::fs::write(&path, updated.as_bytes())
        .await
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))?;

    Ok(match strategy {
        MatchStrategy::Exact => format!("edited {}", path.display()),
        MatchStrategy::TrailingWhitespace => format!(
            "edited {} (matched ignoring trailing whitespace)",
            path.display()
        ),
        MatchStrategy::RelativeIndentation => format!(
            "edited {} (matched ignoring indentation; the file's real indentation was preserved)",
            path.display()
        ),
    })
}

/// Which rung of the matching ladder produced the edit — surfaced in the
/// success summary so session logs (and the bench) can tell exact edits
/// apart from fuzzily-applied ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchStrategy {
    Exact,
    TrailingWhitespace,
    RelativeIndentation,
}

/// Why no rung of the ladder could apply the edit.
enum MatchFailure {
    NotFound,
    /// Occurrence count and the rung that found the ambiguity —
    /// ambiguity at ANY rung refuses the edit rather than guessing.
    Ambiguous(usize, MatchStrategy),
}

impl MatchFailure {
    fn into_message(self, path: &std::path::Path, original: &str, old_string: &str) -> String {
        match self {
            MatchFailure::NotFound => {
                let hint = find_closest_line(original, old_string)
                    .map(|(line_no, line)| {
                        format!(
                            " The closest match in the file is line {line_no}: `{}`.",
                            line.trim()
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "old_string not found in '{}' (also tried whitespace-tolerant matching).\
                     {hint} {WRITE_FILE_STEERING}",
                    path.display()
                )
            }
            MatchFailure::Ambiguous(count, strategy) => format!(
                "old_string is ambiguous in '{}': found {count} occurrences{}, expected \
                 exactly 1. Include more surrounding context in old_string to disambiguate. \
                 {WRITE_FILE_STEERING}",
                path.display(),
                match strategy {
                    MatchStrategy::Exact => "",
                    _ => " (under whitespace-tolerant matching)",
                }
            ),
        }
    }
}

/// Best-effort "did you mean" hint for a failed match: the file line with
/// the most words in common with `old_string`'s first non-blank line.
/// `None` when nothing shares even one word with it — a small model gets
/// no material to correct with a plain "not found"; this gives it a real
/// anchor line to compare against instead (docs/AUDITORIA-2026-07-v3.md,
/// hallazgo A3).
fn find_closest_line<'a>(original: &'a str, old_string: &str) -> Option<(usize, &'a str)> {
    let needle = old_string.lines().find(|l| !l.trim().is_empty())?.trim();
    let needle_words: std::collections::HashSet<&str> = needle
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .collect();
    if needle_words.is_empty() {
        return None;
    }

    original
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let overlap = line
                .split_whitespace()
                .filter(|w| needle_words.contains(w))
                .count();
            (i + 1, line, overlap)
        })
        .filter(|&(_, _, overlap)| overlap > 0)
        .max_by_key(|&(_, _, overlap)| overlap)
        .map(|(line_no, line, _)| (line_no, line))
}

/// Pure core of the tool: runs the matching ladder over `original` and
/// returns the updated content plus the strategy that matched.
fn apply_edit(
    original: &str,
    old_string: &str,
    new_string: &str,
) -> Result<(String, MatchStrategy), MatchFailure> {
    // Rung 1: exact substring — always takes precedence.
    let exact = original.matches(old_string).count();
    if exact == 1 {
        return Ok((
            original.replacen(old_string, new_string, 1),
            MatchStrategy::Exact,
        ));
    }
    if exact > 1 {
        return Err(MatchFailure::Ambiguous(exact, MatchStrategy::Exact));
    }

    // Rungs 2-3: line-window matching.
    for (strategy, line_eq) in [
        (
            MatchStrategy::TrailingWhitespace,
            (|a: &str, b: &str| a.trim_end() == b.trim_end()) as fn(&str, &str) -> bool,
        ),
        (MatchStrategy::RelativeIndentation, |a: &str, b: &str| {
            a.trim() == b.trim()
        }),
    ] {
        match find_line_window(original, old_string, line_eq) {
            Ok(Some(window_start)) => {
                return Ok((
                    replace_line_window(original, old_string, new_string, window_start, strategy),
                    strategy,
                ));
            }
            Ok(None) => {}
            Err(count) => return Err(MatchFailure::Ambiguous(count, strategy)),
        }
    }

    Err(MatchFailure::NotFound)
}

/// Finds the unique window of whole file lines whose lines are pairwise
/// `line_eq`-equal to `old_string`'s lines. `Ok(Some(start))` for exactly
/// one match (start = line index), `Ok(None)` for zero, `Err(count)` for
/// ambiguity. Blank-only `old_string` windows are rejected (nothing to
/// anchor on once whitespace is ignored).
fn find_line_window(
    original: &str,
    old_string: &str,
    line_eq: fn(&str, &str) -> bool,
) -> Result<Option<usize>, usize> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    if old_lines.is_empty() || old_lines.iter().all(|l| l.trim().is_empty()) {
        return Ok(None);
    }
    let file_lines: Vec<&str> = original.lines().collect();
    if old_lines.len() > file_lines.len() {
        return Ok(None);
    }

    let matches: Vec<usize> = (0..=file_lines.len() - old_lines.len())
        .filter(|&start| {
            old_lines
                .iter()
                .enumerate()
                .all(|(i, old)| line_eq(file_lines[start + i], old))
        })
        .collect();

    match matches.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(*only)),
        many => Err(many.len()),
    }
}

/// Rebuilds `original` with the matched line window replaced by
/// `new_string`'s lines. Under `RelativeIndentation`, every `new_string`
/// line is re-indented by the offset between the file's first matched
/// line and `old_string`'s first line, so the file's real indentation is
/// preserved even though the model emitted the block at another depth.
/// The original's trailing-newline presence is preserved.
fn replace_line_window(
    original: &str,
    old_string: &str,
    new_string: &str,
    window_start: usize,
    strategy: MatchStrategy,
) -> String {
    let file_lines: Vec<&str> = original.lines().collect();
    let window_len = old_string.lines().count();

    let new_lines: Vec<String> = match strategy {
        MatchStrategy::RelativeIndentation => {
            let file_indent = leading_whitespace(file_lines[window_start]);
            let old_indent = leading_whitespace(old_string.lines().next().unwrap_or_default());
            new_string
                .lines()
                .map(|line| reindent(line, old_indent, file_indent))
                .collect()
        }
        _ => new_string.lines().map(str::to_string).collect(),
    };

    let mut out_lines: Vec<String> = Vec::with_capacity(file_lines.len());
    out_lines.extend(file_lines[..window_start].iter().map(|l| l.to_string()));
    out_lines.extend(new_lines);
    out_lines.extend(
        file_lines[window_start + window_len..]
            .iter()
            .map(|l| l.to_string()),
    );

    let mut out = out_lines.join("\n");
    if original.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Swaps `old_indent` for `file_indent` at the start of `line`, when
/// present — lines indented *deeper* than the block's first line keep
/// their extra depth relative to the new base.
fn reindent(line: &str, old_indent: &str, file_indent: &str) -> String {
    match line.strip_prefix(old_indent) {
        Some(rest) => format!("{file_indent}{rest}"),
        // The line is shallower than the block's first line (or uses
        // different whitespace characters) — keep it untouched rather
        // than guessing.
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    async fn fixture_file(dir: &std::path::Path, contents: &str) -> PathBuf {
        tokio::fs::create_dir_all(dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("fixture.txt");
        tokio::fs::write(&file_path, contents)
            .await
            .expect("write fixture file");
        file_path
    }

    #[tokio::test]
    async fn replaces_the_single_occurrence() {
        let dir = unique_temp_dir("edit-file-happy");
        let file_path = fixture_file(&dir, "hello world").await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "world".to_string(),
            new_string: "braze".to_string(),
        })
        .await;

        assert!(result.is_ok());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "hello braze");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn old_string_not_found_is_an_error() {
        let dir = unique_temp_dir("edit-file-not-found");
        let file_path = fixture_file(&dir, "hello world").await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "missing".to_string(),
            new_string: "x".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "hello world", "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn multiple_occurrences_is_an_ambiguity_error() {
        let dir = unique_temp_dir("edit-file-ambiguous");
        let file_path = fixture_file(&dir, "foo foo foo").await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "foo".to_string(),
            new_string: "bar".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "foo foo foo", "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- fuzzy application ladder (docs/SOTA-2026-07.md, adenda Aider) ---

    /// The file has trailing whitespace the model didn't reproduce —
    /// rung 2 (trailing-whitespace-insensitive) must apply the edit.
    #[tokio::test]
    async fn trailing_whitespace_difference_still_matches() {
        let dir = unique_temp_dir("edit-file-fuzzy-trailing");
        let file_path = fixture_file(&dir, "fn main() {   \n    hola();\n}\n").await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // Note: no trailing spaces after "{", unlike the file.
            old_string: "fn main() {\n    hola();".to_string(),
            new_string: "fn main() {\n    chao();".to_string(),
        })
        .await
        .expect("fuzzy match should apply the edit");
        assert!(result.contains("trailing whitespace"), "got: {result}");

        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "fn main() {\n    chao();\n}\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The model re-emitted the block at a different indentation depth —
    /// rung 3 must match AND preserve the file's real indentation, both
    /// for same-depth lines and deeper continuation lines.
    #[tokio::test]
    async fn indentation_difference_matches_and_preserves_the_files_indentation() {
        let dir = unique_temp_dir("edit-file-fuzzy-indent");
        let original = "mod x {\n        fn f() {\n            uno();\n        }\n}\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // The model emitted the block with 4-space base indentation;
            // the file actually uses 8.
            old_string: "    fn f() {\n        uno();\n    }".to_string(),
            new_string: "    fn f() {\n        dos();\n    }".to_string(),
        })
        .await
        .expect("indentation-relative match should apply the edit");
        assert!(result.contains("indentation"), "got: {result}");

        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(
            contents, "mod x {\n        fn f() {\n            dos();\n        }\n}\n",
            "the file's 8-space indentation must win over the model's 4"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// An exact match elsewhere always beats any fuzzy candidate — the
    /// ladder never skips rung 1.
    #[tokio::test]
    async fn exact_match_takes_precedence_over_fuzzy_candidates() {
        let dir = unique_temp_dir("edit-file-exact-precedence");
        // "x();" appears exactly (line 1) and fuzzily ("  x();  ", line 2).
        let file_path = fixture_file(&dir, "x();\n  x();  \n").await;

        edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "x();".to_string(),
            new_string: "y();".to_string(),
        })
        .await
        .expect_err("ambiguous at the EXACT rung: 'x();' is a substring of both lines");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Fuzzy ambiguity refuses the edit — same disambiguation principle
    /// as the exact rung.
    #[tokio::test]
    async fn fuzzy_ambiguity_is_refused() {
        let dir = unique_temp_dir("edit-file-fuzzy-ambiguous");
        let original = "  foo()\n  bar()\n    foo()\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // Trimmed, this matches both "  foo()" and "    foo()".
            old_string: "foo()".to_string(),
            new_string: "baz()".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, original, "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Matching failures steer the model toward the whole-file path —
    /// the empirically better edit surface for small models.
    #[tokio::test]
    async fn not_found_error_steers_toward_write_file() {
        let dir = unique_temp_dir("edit-file-steering");
        let file_path = fixture_file(&dir, "hello world").await;

        let err = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "missing entirely".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail");

        assert!(err.contains("write_file"), "got: {err}");
        assert!(err.contains("whitespace-tolerant"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A blank-only old_string must not fuzzy-match everything once
    /// whitespace is ignored.
    #[tokio::test]
    async fn blank_only_old_string_never_fuzzy_matches() {
        let dir = unique_temp_dir("edit-file-blank");
        let original = "a\n\nb\n";
        let file_path = fixture_file(&dir, original).await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "   \n   ".to_string(),
            new_string: "x".to_string(),
        })
        .await;

        assert!(result.is_err());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, original, "file must be untouched");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A file without a trailing newline must not gain one through a
    /// fuzzy line-window edit. The multi-line old_string with no
    /// trailing spaces is NOT an exact substring (the file has trailing
    /// spaces on line 1), so this genuinely exercises rung 2.
    #[tokio::test]
    async fn fuzzy_edit_preserves_missing_trailing_newline() {
        let dir = unique_temp_dir("edit-file-no-trailing-nl");
        let file_path = fixture_file(&dir, "uno()   \ndos()").await;

        let result = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "uno()\ndos()".to_string(),
            new_string: "tres()\ndos()".to_string(),
        })
        .await
        .expect("fuzzy match should apply");
        assert!(result.contains("trailing whitespace"), "got: {result}");

        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "tres()\ndos()", "no trailing newline must appear");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- closest-line hint (docs/AUDITORIA-2026-07-v3.md, hallazgo A3) ---

    #[tokio::test]
    async fn not_found_error_suggests_the_closest_line_by_word_overlap() {
        let dir = unique_temp_dir("edit-file-closest-line");
        let original = "fn alpha() {}\nfn beta() {}\nfn compute_total(x: i32) -> i32 { x }\n";
        let file_path = fixture_file(&dir, original).await;

        let err = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            // Not present verbatim, but shares "compute_total" with line 3.
            old_string: "fn compute_total(y: i32) -> i32 { y }".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail: old_string isn't in the file");

        assert!(err.contains("closest match"), "got: {err}");
        assert!(err.contains("line 3"), "got: {err}");
        assert!(err.contains("compute_total"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn not_found_error_has_no_hint_when_nothing_overlaps() {
        let dir = unique_temp_dir("edit-file-no-closest-line");
        let file_path = fixture_file(&dir, "hello world").await;

        let err = edit_file(EditFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            old_string: "totally unrelated content".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail");

        assert!(!err.contains("closest match"), "got: {err}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- nonexistent file steers to write_file (hallazgo A4) ---

    #[tokio::test]
    async fn editing_a_nonexistent_file_steers_toward_write_file() {
        let dir = unique_temp_dir("edit-file-missing-file");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let missing = dir.join("does-not-exist.txt");

        let err = edit_file(EditFileArgs {
            path: missing.to_string_lossy().into_owned(),
            old_string: "anything".to_string(),
            new_string: "x".to_string(),
        })
        .await
        .expect_err("must fail: the file doesn't exist");

        assert!(err.contains("does not exist"), "got: {err}");
        assert!(err.contains("write_file"), "got: {err}");
        assert!(
            !err.contains("os error"),
            "should not leak the raw OS error: {err}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
