//! `shell_exec` tool: runs a command in argv form (`command[0]` is the
//! program — never a raw shell string, which would open the door to shell
//! injection ambiguity) via `tokio::process::Command`, capturing
//! stdout/stderr/exit code. Also hosts [`run`], the shared process-
//! spawning helper reused by [`crate::grep`] and [`crate::glob`] so all
//! three tools go through one `tokio::process::Command` code path instead
//! of duplicating spawn/capture logic.

use std::path::Path;

use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"command": ["ls", "-la", "/tmp"]}`.
#[derive(Debug, Deserialize)]
pub struct ShellExecArgs {
    pub command: Vec<String>,
}

/// Result of spawning one process, program-agnostic.
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
}

/// Spawns `program` with `args` in `workdir` and waits for completion.
/// `Err` only for spawn-level failures (program not found, exec
/// permission denied, ...) — a nonzero exit code is a normal
/// `CommandOutput { success: false }`, not an `Err`.
///
/// `workdir` matters here specifically (unlike `grep`/`glob`, whose sole
/// path argument `LocalToolsProvider` already resolves to absolute before
/// calling this): an arbitrary shell command's own arguments can
/// reference relative paths braze has no way to rewrite generically
/// (`cat notes.txt`, `ls subdir`), so the child process's actual cwd has
/// to be right.
pub async fn run(program: &str, args: &[String], workdir: &Path) -> Result<CommandOutput, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(workdir)
        .output()
        .await
        .map_err(|err| format!("failed to spawn '{program}': {err}"))?;

    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
        success: output.status.success(),
    })
}

/// `Ok(json_summary)` when the command exits 0, `Err(json_summary)`
/// otherwise — either way the JSON payload carries `exit_code`, `stdout`
/// and `stderr` so the model can see exactly what happened. Spawn-level
/// failures (bad `command[0]`) also come back as `Err`.
pub async fn shell_exec(args: ShellExecArgs, workdir: &Path) -> Result<String, String> {
    let Some((program, rest)) = args.command.split_first() else {
        return Err("command must contain at least the program name".to_string());
    };

    let output = run(program, rest, workdir).await?;
    let summary = json!({
        "exit_code": output.exit_code,
        "stdout": output.stdout,
        "stderr": output.stderr,
    })
    .to_string();

    if output.success {
        Ok(summary)
    } else {
        Err(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    #[tokio::test]
    async fn captures_stdout_of_a_successful_command() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["echo".to_string(), "hello".to_string()],
            },
            &cwd(),
        )
        .await
        .expect("echo should succeed");

        assert!(result.contains("hello"));
        assert!(result.contains("\"exit_code\":0"));
    }

    #[tokio::test]
    async fn nonzero_exit_code_is_a_recoverable_error() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["false".to_string()],
            },
            &cwd(),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let result = shell_exec(ShellExecArgs { command: vec![] }, &cwd()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn nonexistent_program_is_a_spawn_error() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["this-binary-does-not-exist-anywhere".to_string()],
            },
            &cwd(),
        )
        .await;

        assert!(result.is_err());
    }

    /// Regression test for F1: the command must actually run inside
    /// `workdir`, not the process's own cwd — otherwise a relative path
    /// in the command's own arguments (which braze has no generic way to
    /// rewrite) resolves against the wrong directory.
    #[tokio::test]
    async fn command_runs_inside_the_given_workdir_not_the_process_cwd() {
        let dir = crate::test_support::unique_temp_dir("shell-exec-workdir");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("marker.txt"), "hi")
            .await
            .expect("write fixture");

        let result = shell_exec(
            ShellExecArgs {
                command: vec!["cat".to_string(), "marker.txt".to_string()],
            },
            &dir,
        )
        .await
        .expect("cat should find marker.txt via the workdir, not the process cwd");

        assert!(result.contains("hi"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
