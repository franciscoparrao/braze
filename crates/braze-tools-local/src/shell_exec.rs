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
/// `{"command": ["ls", "-la", "/tmp"]}`, optionally with
/// `"timeout": <secs>`.
///
/// `timeout` exists because models trained on other agent harnesses
/// (Codex, Claude Code) keep sending it whether we accept it or not:
/// the memory-distillation sweeps of 2026-07-16 (gpt-oss:20b, 14 schema
/// validation failures across 15 tasks) showed each rejected call
/// burning a full round on "Additional properties are not allowed" —
/// pure harness friction. Accepting it as a real, honored parameter is
/// strictly better than rejecting it (and better than silently
/// stripping it: the model asked for a bound, so enforce the bound).
#[derive(Debug, Deserialize)]
pub struct ShellExecArgs {
    pub command: Vec<String>,
    pub timeout: Option<u64>,
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
        // N-33 (docs/AUDITORIA-2026-07-v2.md): without this, dropping the
        // future that's awaiting `.output()` (e.g. an aborted
        // `braze_events::TaskNotifier` task, itself dropped when a caller
        // like `braze-bench` gives up on a hung turn after its wall-clock
        // timeout) leaves the child process running — it keeps consuming
        // CPU/RAM independently of whatever this call site decided to do.
        // `kill_on_drop(true)` makes tokio send it a kill signal instead.
        .kill_on_drop(true)
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

    let output = match args.timeout {
        Some(secs) => {
            // Clamped, not trusted: 0 would kill every command instantly
            // and a model that meant milliseconds (120000) lands on the
            // cap instead of an hour-long wait. `run`'s `kill_on_drop`
            // makes dropping the timed-out future actually kill the
            // child, not just stop waiting on it (N-33).
            let secs = secs.clamp(1, 3600);
            match tokio::time::timeout(
                std::time::Duration::from_secs(secs),
                run(program, rest, workdir),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    return Err(format!("command timed out after {secs}s and was killed"));
                }
            }
        }
        None => run(program, rest, workdir).await?,
    };
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
    use std::time::Duration;

    fn cwd() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    #[tokio::test]
    async fn captures_stdout_of_a_successful_command() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["echo".to_string(), "hello".to_string()],
                timeout: None,
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
                timeout: None,
            },
            &cwd(),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec![],
                timeout: None,
            },
            &cwd(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn nonexistent_program_is_a_spawn_error() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["this-binary-does-not-exist-anywhere".to_string()],
                timeout: None,
            },
            &cwd(),
        )
        .await;

        assert!(result.is_err());
    }

    /// A command that outlives its model-requested `timeout` is killed
    /// and reported as a recoverable error naming the bound — not left
    /// running, not surfaced as a spawn failure.
    #[tokio::test]
    async fn a_command_exceeding_its_requested_timeout_is_killed_and_reported() {
        let started = std::time::Instant::now();
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["sleep".to_string(), "30".to_string()],
                timeout: Some(1),
            },
            &cwd(),
        )
        .await;

        let err = result.expect_err("sleep 30 must not survive a 1s timeout");
        assert!(
            err.contains("timed out after 1s"),
            "error should name the timeout, got: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the call must return promptly after the timeout, not wait out the sleep"
        );
    }

    /// The clamp floor: `timeout: 0` (a model hallucination, or "no
    /// limit" in some other harness's dialect) must not kill every
    /// command instantly — it behaves as the 1s floor.
    #[tokio::test]
    async fn a_zero_timeout_is_clamped_not_instant_death() {
        let result = shell_exec(
            ShellExecArgs {
                command: vec!["echo".to_string(), "hola".to_string()],
                timeout: Some(0),
            },
            &cwd(),
        )
        .await
        .expect("echo must survive a clamped zero timeout");

        assert!(result.contains("hola"));
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
                timeout: None,
            },
            &dir,
        )
        .await
        .expect("cat should find marker.txt via the workdir, not the process cwd");

        assert!(result.contains("hi"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-33 (docs/AUDITORIA-2026-07-v2.md): aborting
    /// the task that's awaiting `run()` must actually kill the child
    /// process, not just stop *this* code from waiting on it. Proven
    /// indirectly (there's no portable way to inspect the OS process table
    /// for a `tokio::test`) via a shell command that only writes a marker
    /// file *after* a delay: if `kill_on_drop` weren't set, the process
    /// would keep running to completion in the background after abort and
    /// the marker would still show up.
    #[tokio::test]
    async fn aborting_the_awaiting_task_kills_the_child_process() {
        let dir = crate::test_support::unique_temp_dir("shell-exec-kill-on-drop");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let marker = dir.join("marker");

        let workdir = dir.clone();
        let marker_arg = marker.to_string_lossy().into_owned();
        let handle = tokio::spawn(async move {
            let _ = run(
                "sh",
                &["-c".to_string(), format!("sleep 1 && touch '{marker_arg}'")],
                &workdir,
            )
            .await;
        });

        // Let the shell actually start before pulling the rug out.
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();
        let _ = handle.await;

        // Longer than the shell's own `sleep 1` — if the process were
        // still alive, the marker would exist by the time this returns.
        tokio::time::sleep(Duration::from_millis(1400)).await;
        assert!(
            !marker.exists(),
            "child process kept running after its owning task was aborted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
