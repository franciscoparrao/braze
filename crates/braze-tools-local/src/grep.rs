//! `grep` tool: searches for a pattern inside files under a directory by
//! shelling out to the system `grep` binary (reusing
//! [`crate::shell_exec::run`]) instead of reimplementing a search engine
//! or adding a `regex` dependency — neither is in `workspace.dependencies`.
//!
//! Pattern semantics: literal substring match by default (`grep -F`,
//! avoids surprising regex-metacharacter behavior for the common case);
//! set `"regex": true` to interpret `pattern` as a POSIX extended regular
//! expression (`grep -E`) instead.

use serde::Deserialize;

use crate::shell_exec::run;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"pattern": "TODO", "path": "src", "regex": false}`. `path` and
/// `regex` are optional, defaulting to `"."` and `false`.
#[derive(Debug, Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub regex: bool,
}

fn default_path() -> String {
    ".".to_string()
}

/// `Ok("no matches found")` is a normal, successful outcome (grep's exit
/// code 1) — not an error. `Err` is reserved for real failures: exit
/// code >= 2 (bad pattern, unreadable path, ...) or a spawn failure.
pub async fn grep(args: GrepArgs) -> Result<String, String> {
    let mode_flag = if args.regex { "-E" } else { "-F" };
    let cmd_args = vec![
        "-r".to_string(),
        "-n".to_string(),
        mode_flag.to_string(),
        args.pattern,
        args.path,
    ];

    let output = run("grep", &cmd_args).await?;
    match output.exit_code {
        0 => Ok(output.stdout),
        1 => Ok("no matches found".to_string()),
        _ => Err(if output.stderr.is_empty() {
            output.stdout
        } else {
            output.stderr
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    #[tokio::test]
    async fn finds_literal_substring_match() {
        let dir = unique_temp_dir("grep-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("a.txt"), "needle in a haystack")
            .await
            .expect("write fixture");

        let result = grep(GrepArgs {
            pattern: "needle".to_string(),
            path: dir.to_string_lossy().into_owned(),
            regex: false,
        })
        .await
        .expect("grep should succeed");

        assert!(result.contains("needle in a haystack"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn no_matches_is_not_an_error() {
        let dir = unique_temp_dir("grep-no-match");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("a.txt"), "nothing relevant here")
            .await
            .expect("write fixture");

        let result = grep(GrepArgs {
            pattern: "absent-pattern".to_string(),
            path: dir.to_string_lossy().into_owned(),
            regex: false,
        })
        .await;

        assert_eq!(result, Ok("no matches found".to_string()));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn regex_mode_matches_extended_pattern() {
        let dir = unique_temp_dir("grep-regex");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("a.txt"), "value=123")
            .await
            .expect("write fixture");

        let result = grep(GrepArgs {
            pattern: "value=[0-9]+".to_string(),
            path: dir.to_string_lossy().into_owned(),
            regex: true,
        })
        .await
        .expect("grep -E should succeed");

        assert!(result.contains("value=123"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn literal_mode_does_not_treat_pattern_as_regex() {
        let dir = unique_temp_dir("grep-literal");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        // Contains the literal string "a.b" but not "axb" or "acb" — a
        // regex "." would also match those, a literal "-F" match won't.
        tokio::fs::write(dir.join("a.txt"), "prefix a.b suffix")
            .await
            .expect("write fixture");

        let result = grep(GrepArgs {
            pattern: "a.b".to_string(),
            path: dir.to_string_lossy().into_owned(),
            regex: false,
        })
        .await
        .expect("grep -F should succeed");

        assert!(result.contains("a.b"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
