//! `write_file` tool: creates or overwrites a file with the given content.
//! Guarded — `LocalToolsProvider::invoke` checks
//! `ActionDescriptor::WriteFile` before calling [`write_file`].

use std::path::PathBuf;

use serde::Deserialize;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"path": "notes.txt", "content": "..."}`.
#[derive(Debug, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

/// An overwrite is flagged as a likely accidental shrink when the previous
/// file was at least this many bytes larger than the new content — small
/// enough to catch "wrote back a paginated read_file page instead of the
/// whole file" (docs/AUDITORIA-2026-07-v3.md, hallazgo A2), large enough
/// to stay quiet on ordinary edits that legitimately shrink a file a bit.
const SHRINK_WARNING_THRESHOLD_BYTES: usize = 500;

/// `Ok(summary)` on success. `Err(message)` is a recoverable tool-level
/// failure (e.g. parent directory doesn't exist) — see
/// `provider.rs::wrap`.
pub async fn write_file(args: WriteFileArgs) -> Result<String, String> {
    let path = PathBuf::from(&args.path);
    let len = args.content.len();

    // Read the previous size *before* writing — this is the only point
    // where "what was there before" is still observable. A model that
    // only saw a truncated/paginated `read_file` page and obeyed the
    // steering to "write the complete file" can silently discard
    // everything past what it saw; this doesn't block the write (the
    // model may well be shrinking the file on purpose), but it does
    // surface the size delta so the model — or the user reading the
    // transcript — can catch the accident immediately instead of only
    // noticing it later.
    let previous_size = tokio::fs::metadata(&path)
        .await
        .ok()
        .map(|m| m.len() as usize);

    tokio::fs::write(&path, args.content.as_bytes())
        .await
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))?;

    let mut summary = format!("wrote {len} bytes to {}", path.display());
    if let Some(previous_size) = previous_size
        && previous_size > len
        && previous_size - len >= SHRINK_WARNING_THRESHOLD_BYTES
    {
        summary.push_str(&format!(
            "\n\nWARNING: '{}' was {previous_size} bytes before this write and is now {len} \
             bytes. If you only saw a truncated or paginated read_file page rather than the \
             complete previous content, you may have just discarded the rest of the file. \
             Use read_file with offset to check what remains, or edit_file for a targeted \
             change instead of rewriting the whole file next time.",
            path.display()
        ));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    #[tokio::test]
    async fn creates_file_with_given_content() {
        let dir = unique_temp_dir("write-file-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "payload".to_string(),
        })
        .await;

        assert!(result.is_ok());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back written file");
        assert_eq!(contents, "payload");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn write_failure_is_a_recoverable_error() {
        // Parent directory doesn't exist -> the write fails.
        let dir = unique_temp_dir("write-file-missing-parent");
        let file_path = dir.join("nested").join("out.txt");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "payload".to_string(),
        })
        .await;

        assert!(result.is_err());
    }

    // --- shrink warning (docs/AUDITORIA-2026-07-v3.md, hallazgo A2) ---

    #[tokio::test]
    async fn overwriting_a_much_larger_file_warns_about_the_shrink() {
        let dir = unique_temp_dir("write-file-shrink-warn");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");
        let original = "x".repeat(SHRINK_WARNING_THRESHOLD_BYTES * 4);
        tokio::fs::write(&file_path, &original)
            .await
            .expect("write fixture");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "short".to_string(),
        })
        .await
        .expect("write should still succeed");

        assert!(result.contains("WARNING"), "got: {result}");
        assert!(result.contains("discarded the rest"), "got: {result}");
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "short", "the write itself must still apply");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn overwriting_with_a_similar_size_does_not_warn() {
        let dir = unique_temp_dir("write-file-no-shrink-warn");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");
        tokio::fs::write(&file_path, "original content")
            .await
            .expect("write fixture");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "updated content".to_string(),
        })
        .await
        .expect("write should succeed");

        assert!(!result.contains("WARNING"), "got: {result}");
    }

    #[tokio::test]
    async fn creating_a_new_file_does_not_warn() {
        let dir = unique_temp_dir("write-file-new-no-warn");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("brand-new.txt");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "hello".to_string(),
        })
        .await
        .expect("write should succeed");

        assert!(!result.contains("WARNING"), "got: {result}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
