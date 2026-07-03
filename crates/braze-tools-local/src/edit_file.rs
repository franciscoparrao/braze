//! `edit_file` tool: replaces exactly one occurrence of `old_string` with
//! `new_string` in a file. Fails if `old_string` doesn't appear, or
//! appears more than once — same disambiguation principle as Claude
//! Code's Edit tool: an ambiguous edit is refused rather than guessed at.
//! Guarded — treated as a write for permission purposes (there is no
//! separate `ActionDescriptor::EditFile` variant).

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

/// `Ok(summary)` on success. `Err(message)` covers both I/O failures and
/// the two disambiguation failures (`old_string` missing / ambiguous) —
/// all recoverable tool-level failures, see `provider.rs::wrap`.
pub async fn edit_file(args: EditFileArgs) -> Result<String, String> {
    if args.old_string.is_empty() {
        return Err("old_string must not be empty".to_string());
    }

    let path = PathBuf::from(&args.path);
    let original = tokio::fs::read_to_string(&path)
        .await
        .map_err(|err| format!("failed to read '{}': {err}", path.display()))?;

    let occurrences = original.matches(args.old_string.as_str()).count();
    if occurrences == 0 {
        return Err(format!("old_string not found in '{}'", path.display()));
    }
    if occurrences > 1 {
        return Err(format!(
            "old_string is ambiguous in '{}': found {occurrences} occurrences, expected exactly 1",
            path.display()
        ));
    }

    let updated = original.replacen(args.old_string.as_str(), &args.new_string, 1);
    tokio::fs::write(&path, updated.as_bytes())
        .await
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))?;

    Ok(format!("edited {}", path.display()))
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
}
