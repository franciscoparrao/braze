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

/// `find` treats a first non-option argument starting with `-` as part of
/// its *expression*, not the path list (which then defaults to `.`). A
/// `path` of e.g. `-delete` would make `find` delete everything under the
/// cwd instead of listing it. Anchoring any relative path with `./`
/// guarantees it can never be mistaken for a flag.
fn anchor_path(path: String) -> String {
    if path.starts_with('/') || path.starts_with("./") {
        path
    } else {
        format!("./{path}")
    }
}

/// `Ok("no files matched")` is a normal, successful outcome — not an
/// error. `Err` is reserved for real `find` failures (unreadable path,
/// ...) or a spawn failure.
pub async fn glob(args: GlobArgs) -> Result<String, String> {
    let cmd_args = vec![
        anchor_path(args.path),
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

    /// Regression test for a `find` argv-injection: `find -delete ...`
    /// (what `find` sees if a bare `-delete` path reached it unanchored)
    /// would recursively delete the cwd. `anchor_path` must always
    /// neutralize a leading `-` so it can never be mistaken for a flag.
    #[test]
    fn dangerous_paths_are_anchored_so_find_never_sees_a_leading_dash() {
        assert_eq!(anchor_path("-delete".to_string()), "./-delete");
        assert_eq!(anchor_path("-exec".to_string()), "./-exec");
        assert_eq!(anchor_path("src".to_string()), "./src");
        assert_eq!(anchor_path(".".to_string()), "./.");
        // Already-anchored or absolute paths pass through unchanged.
        assert_eq!(anchor_path("./src".to_string()), "./src");
        assert_eq!(anchor_path("/tmp/x".to_string()), "/tmp/x");
    }

    /// End-to-end: a `-delete`-like path must resolve to a literal,
    /// nonexistent subdirectory (an error from `find`), never to the
    /// `-delete` primary being executed against the cwd.
    #[tokio::test]
    async fn dash_path_is_treated_as_a_literal_path_not_a_find_flag() {
        let result = glob(GlobArgs {
            pattern: "*.rs".to_string(),
            path: "-delete".to_string(),
        })
        .await;

        // `find ./-delete -type f -name '*.rs'` fails because `./-delete`
        // does not exist — it must NOT succeed by having deleted files.
        assert!(
            result.is_err(),
            "expected a 'no such file' error, got: {result:?}"
        );
    }
}
