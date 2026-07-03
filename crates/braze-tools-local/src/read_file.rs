//! `read_file` tool: reads the full contents of a text file at a given
//! path. Never goes through the permission guard — PLAN.md only requires
//! confirmation for writes/deletes/irreversible commands, not reads.

use std::path::PathBuf;

use serde::Deserialize;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"path": "src/main.rs"}`.
#[derive(Debug, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
}

/// `Ok(contents)` on success. `Err(message)` is a recoverable tool-level
/// failure (e.g. file not found) meant to become a `ToolResult` with
/// `is_error: true`, not a hard `ToolError` — see `provider.rs::wrap`.
pub async fn read_file(args: ReadFileArgs) -> Result<String, String> {
    let path = PathBuf::from(&args.path);
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|err| format!("failed to read '{}': {err}", path.display()))
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
        })
        .await;

        assert!(result.is_err());
    }
}
