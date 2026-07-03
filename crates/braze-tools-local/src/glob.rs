//! `glob` tool: lists files matching a shell glob pattern under a
//! directory by shelling out to the system `find` binary (reusing
//! [`crate::shell_exec::run`]) instead of adding a `glob`/`walkdir`
//! dependency — neither is in `workspace.dependencies`.

use serde::Deserialize;

use crate::shell_exec::run;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"pattern": "*.rs", "path": "src"}`. `path` is optional, defaulting
/// to `"."`.
#[derive(Debug, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    #[serde(default = "default_path")]
    pub path: String,
}

fn default_path() -> String {
    ".".to_string()
}

/// `Ok("no files matched")` is a normal, successful outcome — not an
/// error. `Err` is reserved for real `find` failures (unreadable path,
/// ...) or a spawn failure.
pub async fn glob(args: GlobArgs) -> Result<String, String> {
    let cmd_args = vec![
        args.path,
        "-type".to_string(),
        "f".to_string(),
        "-name".to_string(),
        args.pattern,
    ];

    let output = run("find", &cmd_args).await?;
    if !output.success {
        return Err(if output.stderr.is_empty() {
            output.stdout
        } else {
            output.stderr
        });
    }

    if output.stdout.trim().is_empty() {
        Ok("no files matched".to_string())
    } else {
        Ok(output.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    #[tokio::test]
    async fn lists_files_matching_pattern() {
        let dir = unique_temp_dir("glob-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("keep.rs"), "// rust")
            .await
            .expect("write fixture");
        tokio::fs::write(dir.join("skip.txt"), "text")
            .await
            .expect("write fixture");

        let result = glob(GlobArgs {
            pattern: "*.rs".to_string(),
            path: dir.to_string_lossy().into_owned(),
        })
        .await
        .expect("find should succeed");

        assert!(result.contains("keep.rs"));
        assert!(!result.contains("skip.txt"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn no_matches_is_not_an_error() {
        let dir = unique_temp_dir("glob-no-match");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");

        let result = glob(GlobArgs {
            pattern: "*.absent".to_string(),
            path: dir.to_string_lossy().into_owned(),
        })
        .await;

        assert_eq!(result, Ok("no files matched".to_string()));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
