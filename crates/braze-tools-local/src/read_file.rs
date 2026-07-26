//! `read_file` tool: reads the text contents of a file at a given path,
//! paginated by line. The permission check (silent inside the
//! `WorkdirAllowlist`, confirmation required outside it) lives in
//! `provider.rs`, not here — this function only does the actual read.
//!
//! ## Pagination (docs/AUDITORIA-2026-07-v3.md, hallazgo A1)
//!
//! Without `offset`/`limit`, a file larger than [`DEFAULT_PAGE_LINES`]
//! used to come back silently truncated at a fixed byte cap
//! (`provider.rs::MAX_TOOL_OUTPUT_BYTES`) with no way to reach the rest —
//! a small model that needed to edit past that cut was structurally stuck:
//! `edit_file` can only match text it actually saw, and the "use
//! write_file with the complete content" steering assumed a complete
//! content the model never received. `offset`/`limit` (1-indexed line
//! numbers) let the model page through the rest; the trailer on a
//! truncated page states the next `offset` to continue from.

use std::path::PathBuf;

use serde::Deserialize;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"path": "src/main.rs"}`, optionally with `offset`/`limit` (1-indexed
/// line numbers) to page through a file larger than one default page.
#[derive(Debug, Default, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Default page size in lines when the caller omits `offset`/`limit` and
/// the file is larger than this — close to SWE-agent/ACI's 100-line
/// viewer default (arXiv 2405.15793), sized up a bit since braze's cap is
/// bytes, not lines, and most source lines are well under 40 bytes.
const DEFAULT_PAGE_LINES: usize = 200;

/// `Ok(contents)` on success. `Err(message)` is a recoverable tool-level
/// failure (file not found, or a page request past the end of the file)
/// meant to become a `ToolResult` with `is_error: true`, not a hard
/// `ToolError` — see `provider.rs::wrap`.
pub async fn read_file(args: ReadFileArgs) -> Result<String, String> {
    let path = PathBuf::from(&args.path);
    let contents = tokio::fs::read_to_string(&path)
        .await
        .map_err(|err| format!("failed to read '{}': {err}", path.display()))?;

    if args.offset == Some(0) {
        return Err("offset is 1-indexed (the first line is offset=1), not 0".to_string());
    }

    let ends_with_newline = contents.ends_with('\n');
    let lines: Vec<&str> = contents.lines().collect();
    let total_lines = lines.len();

    // No pagination requested and the file already fits one default page
    // — return it verbatim, unchanged from the pre-pagination behavior.
    if args.offset.is_none() && args.limit.is_none() && total_lines <= DEFAULT_PAGE_LINES {
        return Ok(contents);
    }

    let start_line = args.offset.map(|o| o - 1).unwrap_or(0);
    if start_line >= total_lines {
        return Err(format!(
            "offset {} is past the end of '{}' ({total_lines} lines total).",
            args.offset.unwrap_or(1),
            path.display()
        ));
    }

    let requested_page_size = args.limit.unwrap_or(DEFAULT_PAGE_LINES).max(1);
    let requested_end = start_line
        .saturating_add(requested_page_size)
        .min(total_lines);
    let end_line = clamp_to_output_budget(&lines, start_line, requested_end);
    let page = &lines[start_line..end_line];

    let mut out = format!("[lines {}-{end_line} of {total_lines}]\n", start_line + 1);
    out.push_str(&page.join("\n"));
    if end_line == total_lines && ends_with_newline {
        out.push('\n');
    }
    if end_line < total_lines {
        out.push_str(&format!(
            "\n[{} more lines below — call read_file again with offset={} to continue]",
            total_lines - end_line,
            end_line + 1
        ));
    }
    Ok(out)
}

/// Shrinks a requested `[start_line, requested_end)` page so its formatted
/// body fits under `provider.rs::wrap`'s per-tool-result byte cap
/// (docs/usability-log-2026-07-07-si2.md, hallazgo U-6 and its repeats —
/// five different models stuck in the same overlapping-reread thrash on
/// real source files). Without this, a caller-requested `limit` big
/// enough to blow past that cap makes `end_line` land on `requested_end ==
/// total_lines` — looks like the whole request fit — which skips this
/// function's own "more lines below, use offset=X" trailer entirely;
/// `wrap`'s *generic* byte-cap truncation fires instead, and its "narrow
/// your query" trailer is correct advice for a grep/glob dump but wrong
/// here: the actual fix is paging forward with `offset`, not narrowing
/// anything. Always keeps at least one line, even if that line alone
/// exceeds the budget — an empty page is a worse failure than one
/// oversized line.
fn clamp_to_output_budget(lines: &[&str], start_line: usize, requested_end: usize) -> usize {
    // Headroom for the `[lines X-Y of Z]` header and the continuation
    // trailer this function wraps around the page body.
    const HEADROOM_BYTES: usize = 200;
    let budget = crate::provider::MAX_TOOL_OUTPUT_BYTES.saturating_sub(HEADROOM_BYTES);

    let mut used = 0usize;
    let mut end_line = start_line;
    for line in &lines[start_line..requested_end] {
        let cost = line.len() + 1; // +1 for the newline `join` re-adds
        if end_line > start_line && used + cost > budget {
            break;
        }
        used += cost;
        end_line += 1;
    }
    end_line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    #[tokio::test]
    async fn reads_existing_file_contents() {
        let dir = unique_temp_dir("read-file-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("hello.txt");
        tokio::fs::write(&file_path, "hello world")
            .await
            .expect("write fixture file");

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .await;

        assert_eq!(result, Ok("hello world".to_string()));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn missing_file_is_a_recoverable_error() {
        let dir = unique_temp_dir("read-file-missing");
        let missing = dir.join("does-not-exist.txt");

        let result = read_file(ReadFileArgs {
            path: missing.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .await;

        assert!(result.is_err());
    }

    // --- pagination (docs/AUDITORIA-2026-07-v3.md, hallazgo A1) ---

    async fn write_numbered_lines(dir: &std::path::Path, name: &str, count: usize) -> PathBuf {
        tokio::fs::create_dir_all(dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join(name);
        let content: String = (1..=count).map(|n| format!("line {n}\n")).collect();
        tokio::fs::write(&file_path, content)
            .await
            .expect("write fixture file");
        file_path
    }

    #[tokio::test]
    async fn a_file_within_one_default_page_is_returned_verbatim() {
        let dir = unique_temp_dir("read-file-small-page");
        let file_path = write_numbered_lines(&dir, "small.txt", DEFAULT_PAGE_LINES).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .await
        .expect("read should succeed");

        assert!(
            !result.contains("[lines"),
            "must not be paginated: {result}"
        );
        assert!(result.starts_with("line 1\n"));
        assert!(result.contains(&format!("line {DEFAULT_PAGE_LINES}")));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_large_file_with_no_offset_returns_the_first_page_and_a_continuation_hint() {
        let dir = unique_temp_dir("read-file-large-default");
        let total = DEFAULT_PAGE_LINES * 3;
        let file_path = write_numbered_lines(&dir, "big.txt", total).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .await
        .expect("read should succeed");

        assert!(result.contains(&format!("[lines 1-{DEFAULT_PAGE_LINES} of {total}]")));
        assert!(result.contains("line 1\n"));
        assert!(result.contains(&format!("line {DEFAULT_PAGE_LINES}")));
        assert!(
            !result.contains(&format!("line {}", DEFAULT_PAGE_LINES + 1)),
            "must not include lines past the first page: {result}"
        );
        assert!(result.contains(&format!("offset={}", DEFAULT_PAGE_LINES + 1)));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn offset_and_limit_page_to_an_arbitrary_slice() {
        let dir = unique_temp_dir("read-file-explicit-page");
        let file_path = write_numbered_lines(&dir, "big.txt", 500).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(201),
            limit: Some(50),
        })
        .await
        .expect("read should succeed");

        assert!(result.contains("[lines 201-250 of 500]"));
        assert!(result.contains("line 201\n"));
        assert!(result.contains("line 250"));
        assert!(
            !result.contains("line 200 "),
            "must not include line before the window"
        );
        assert!(
            !result.contains("line 251"),
            "must not include line after the window"
        );
        assert!(result.contains("offset=251"), "should hint the next offset");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn the_last_page_carries_no_continuation_hint() {
        let dir = unique_temp_dir("read-file-last-page");
        let file_path = write_numbered_lines(&dir, "big.txt", 500).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(451),
            limit: Some(100),
        })
        .await
        .expect("read should succeed");

        assert!(result.contains("[lines 451-500 of 500]"));
        assert!(!result.contains("more lines below"), "got: {result}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- output-budget clamping (docs/usability-log-2026-07-07-si2.md,
    // hallazgo U-6 and its repeats: an oversized `limit` used to make
    // `end_line` land on `total_lines`, skipping this file's own
    // continuation trailer and letting `provider.rs::wrap`'s generic
    // "narrow your query" truncation fire instead — actively wrong advice
    // when the real fix is paging forward with `offset`) ---

    #[tokio::test]
    async fn a_limit_larger_than_the_output_budget_is_clamped_and_hints_a_continuation() {
        let dir = unique_temp_dir("read-file-oversized-limit");
        let total = 2000;
        let file_path = write_numbered_lines(&dir, "big.txt", total).await;

        // Exactly the shape a model reaches for when it asks for "the
        // rest of the file in one shot" — offset=1, limit=total.
        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(1),
            limit: Some(total),
        })
        .await
        .expect("read should succeed");

        assert!(
            result.len() < crate::provider::MAX_TOOL_OUTPUT_BYTES,
            "clamped page should fit the tool-output budget: {} bytes",
            result.len()
        );
        assert!(
            result.contains("more lines below"),
            "a clamped page must still hint how to continue: {result}"
        );
        assert!(result.starts_with("[lines 1-"));
        assert!(
            !result.contains(&format!("line {total}")),
            "must not have silently returned the whole file"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_page_that_genuinely_reaches_the_end_of_the_file_is_not_clamped() {
        let dir = unique_temp_dir("read-file-small-explicit-limit");
        let file_path = write_numbered_lines(&dir, "small.txt", 20).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(1),
            limit: Some(20),
        })
        .await
        .expect("read should succeed");

        assert!(result.contains("[lines 1-20 of 20]"));
        assert!(!result.contains("more lines below"), "got: {result}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn one_line_that_alone_exceeds_the_budget_is_still_returned_whole() {
        let dir = unique_temp_dir("read-file-oversized-single-line");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("huge-line.txt");
        let huge_line = "x".repeat(crate::provider::MAX_TOOL_OUTPUT_BYTES * 2);
        let content = format!("{huge_line}\nsecond line\n");
        tokio::fs::write(&file_path, &content)
            .await
            .expect("write fixture file");

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(1),
            limit: Some(2),
        })
        .await
        .expect("read should succeed");

        assert!(
            result.contains(&huge_line),
            "the one oversized line must still come back whole"
        );
        assert!(result.contains("more lines below"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn offset_past_the_end_of_the_file_is_a_recoverable_error() {
        let dir = unique_temp_dir("read-file-offset-oob");
        let file_path = write_numbered_lines(&dir, "small.txt", 10).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(100),
            limit: None,
        })
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("past the end"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn offset_zero_is_rejected_as_not_one_indexed() {
        let dir = unique_temp_dir("read-file-offset-zero");
        let file_path = write_numbered_lines(&dir, "small.txt", 10).await;

        let result = read_file(ReadFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            offset: Some(0),
            limit: None,
        })
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("1-indexed"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
