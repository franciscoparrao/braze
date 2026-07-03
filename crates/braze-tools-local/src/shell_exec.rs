//! `shell_exec` tool: runs a command in argv form (`command[0]` is the
//! program — never a raw shell string, which would open the door to shell
//! injection ambiguity) via `tokio::process::Command`, capturing
//! stdout/stderr/exit code. Also hosts [`run`], the shared process-
//! spawning helper reused by [`crate::grep`] and [`crate::glob`] so all
//! three tools go through one `tokio::process::Command` code path instead
//! of duplicating spawn/capture logic.

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

/// Spawns `program` with `args` and waits for completion. `Err` only for
/// spawn-level failures (program not found, exec permission denied, ...)
/// — a nonzero exit code is a normal `CommandOutput { success: false }`,
/// not an `Err`.
pub async fn run(program: &str, args: &[String]) -> Result<CommandOutput, String> {
    let output = Command::new(program)
        .args(args)
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
pub async fn shell_exec(args: ShellExecArgs) -> Result<String, String> {
    let Some((program, rest)) = args.command.split_first() else {
        return Err("command must contain at least the program name".to_string());
    };

    let output = run(program, rest).await?;
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

    #[tokio::test]
    async fn captures_stdout_of_a_successful_command() {
        let result = shell_exec(ShellExecArgs {
            command: vec!["echo".to_string(), "hello".to_string()],
        })
        .await
        .expect("echo should succeed");

        assert!(result.contains("hello"));
        assert!(result.contains("\"exit_code\":0"));
    }

    #[tokio::test]
    async fn nonzero_exit_code_is_a_recoverable_error() {
        let result = shell_exec(ShellExecArgs {
            command: vec!["false".to_string()],
        })
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let result = shell_exec(ShellExecArgs { command: vec![] }).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn nonexistent_program_is_a_spawn_error() {
        let result = shell_exec(ShellExecArgs {
            command: vec!["this-binary-does-not-exist-anywhere".to_string()],
        })
        .await;

        assert!(result.is_err());
    }
}
