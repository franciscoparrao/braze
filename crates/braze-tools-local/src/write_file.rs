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

/// `Ok(summary)` on success. `Err(message)` is a recoverable tool-level
/// failure (e.g. parent directory doesn't exist) — see
/// `provider.rs::wrap`.
pub async fn write_file(args: WriteFileArgs) -> Result<String, String> {
    let path = PathBuf::from(&args.path);
    let len = args.content.len();
    tokio::fs::write(&path, args.content.as_bytes())
        .await
        .map(|_| format!("wrote {len} bytes to {}", path.display()))
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))
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
}
