//! [`LocalToolsProvider`]: the single [`ToolProvider`] this crate exposes,
//! fronting all six built-in local tools. Owns a caller-supplied
//! [`PermissionGuard`] and checks it before every write/edit/shell/read
//! action. Reads (`read_file`, `grep`, `glob`) proceed silently inside the
//! `WorkdirAllowlist`, same as writes — only a read that reaches outside
//! it (e.g. `~/.ssh/id_rsa`, `/etc/shadow`) requires confirmation.
//!
//! All relative paths a tool call carries are resolved against
//! [`LocalToolsProvider::workdir`] (join if relative, used as-is if
//! absolute) *before* the permission check and *before* the actual I/O —
//! both must agree on the same resolved path. Without an explicit
//! workdir, a caller whose own process cwd doesn't match the directory
//! its `PermissionGuard`'s `WorkdirAllowlist` was scoped to (e.g.
//! `braze-bench` running tasks in a per-task sandbox while the bench
//! binary itself runs from wherever it was launched) gets a mismatch:
//! permission checks pass/fail against the sandbox, but the actual read
//! or write happens relative to the wrong directory entirely — silently
//! escaping the sandbox for writes, or failing to find files for reads.
//! See docs/AUDITORIA-2026-07.md hallazgo F1.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use braze_permissions::{ActionDescriptor, PermissionGuard};
use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
use braze_types::{ToolCall, ToolResult, ToolStub};
use serde::de::DeserializeOwned;

use crate::edit_file::{self, EditFileArgs};
use crate::glob::{self, GlobArgs};
use crate::grep::{self, GrepArgs};
use crate::read_file::{self, ReadFileArgs};
use crate::schema;
use crate::shell_exec::{self, ShellExecArgs};
use crate::write_file::{self, WriteFileArgs};

/// Stable provider id this crate advertises to `ToolRegistry` — see
/// `ToolProvider::provider_id`'s doc comment for the "local"/"mcp:..."
/// convention.
const PROVIDER_ID: &str = "local";

/// Implements [`ToolProvider`] for the six built-in local tools
/// (`read_file`, `write_file`, `edit_file`, `shell_exec`, `grep`,
/// `glob`). Does not construct its own [`PermissionGuard`] — whoever
/// instantiates this (`braze-engine` in Fase 5) decides the real
/// confirmation policy and hands a ready guard to
/// [`LocalToolsProvider::new`].
pub struct LocalToolsProvider {
    guard: PermissionGuard,
    workdir: PathBuf,
}

impl LocalToolsProvider {
    /// Uses the process's current directory as the workdir — correct for
    /// `braze-cli`, where the process cwd *is* the project the agent
    /// operates on. A caller whose process cwd doesn't match the logical
    /// working directory (e.g. `braze-bench`, one sandbox per task) must
    /// use [`LocalToolsProvider::with_workdir`] instead.
    pub fn new(guard: PermissionGuard) -> Self {
        let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self { guard, workdir }
    }

    /// Uses `workdir` as the base every relative path is resolved
    /// against. Must match the directory `guard`'s `WorkdirAllowlist` was
    /// scoped to, or the permission check and the actual I/O will
    /// disagree about what's "inside" the sandbox.
    pub fn with_workdir(guard: PermissionGuard, workdir: impl Into<PathBuf>) -> Self {
        Self {
            guard,
            workdir: workdir.into(),
        }
    }

    /// Joins `path` onto [`Self::workdir`] if relative; returns `path`
    /// unchanged if already absolute.
    fn resolve(&self, path: &str) -> String {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            path.to_string()
        } else {
            self.workdir.join(candidate).to_string_lossy().into_owned()
        }
    }

    async fn invoke_read_file(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: ReadFileArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_read(call, &args.path).await?;
        Ok(wrap(call, read_file::read_file(args).await))
    }

    async fn invoke_write_file(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: WriteFileArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_write(call, &args.path).await?;
        Ok(wrap(call, write_file::write_file(args).await))
    }

    async fn invoke_edit_file(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: EditFileArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_write(call, &args.path).await?;
        Ok(wrap(call, edit_file::edit_file(args).await))
    }

    async fn invoke_shell_exec(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let args: ShellExecArgs = parse_args(call)?;
        let action = ActionDescriptor::ShellCommand {
            command: args.command.clone(),
        };
        self.guard
            .check(&action)
            .await
            .map_err(|err| ToolError::InvocationFailed {
                name: call.name.clone(),
                message: err.to_string(),
            })?;
        Ok(wrap(
            call,
            shell_exec::shell_exec(args, &self.workdir).await,
        ))
    }

    async fn invoke_grep(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: GrepArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_read(call, &args.path).await?;
        Ok(wrap(call, grep::grep(args).await))
    }

    async fn invoke_glob(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: GlobArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_read(call, &args.path).await?;
        Ok(wrap(call, glob::glob(args).await))
    }

    /// Shared by `write_file` and `edit_file`: both are writes for
    /// permission purposes (there is no separate `ActionDescriptor` for
    /// "edit").
    async fn check_write(&self, call: &ToolCall, path: &str) -> Result<(), ToolError> {
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from(path),
        };
        self.guard
            .check(&action)
            .await
            .map_err(|err| ToolError::InvocationFailed {
                name: call.name.clone(),
                message: err.to_string(),
            })
    }

    /// Shared by `read_file`, `grep`, and `glob`: all three are reads for
    /// permission purposes. Silent inside the `WorkdirAllowlist`; a path
    /// reaching outside it (e.g. `~/.ssh/id_rsa`) requires confirmation
    /// instead of being read unconditionally.
    async fn check_read(&self, call: &ToolCall, path: &str) -> Result<(), ToolError> {
        let action = ActionDescriptor::ReadPath {
            path: PathBuf::from(path),
        };
        self.guard
            .check(&action)
            .await
            .map_err(|err| ToolError::InvocationFailed {
                name: call.name.clone(),
                message: err.to_string(),
            })
    }
}

#[async_trait]
impl ToolProvider for LocalToolsProvider {
    fn provider_id(&self) -> &str {
        PROVIDER_ID
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(schema::all_stubs(PROVIDER_ID))
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        Ok(schema::schema_for(name))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        match call.name.as_str() {
            "read_file" => self.invoke_read_file(call).await,
            "write_file" => self.invoke_write_file(call).await,
            "edit_file" => self.invoke_edit_file(call).await,
            "shell_exec" => self.invoke_shell_exec(call).await,
            "grep" => self.invoke_grep(call).await,
            "glob" => self.invoke_glob(call).await,
            other => Err(ToolError::NotFound(other.to_string())),
        }
    }
}

/// Deserializes `call.arguments` into `T`. A malformed payload is an
/// invocation-level failure (hard `Err(ToolError::InvocationFailed)`), not
/// a recoverable `ToolResult { is_error: true }` — the model asked for a
/// tool call shape this provider can't even parse.
fn parse_args<T: DeserializeOwned>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_value(call.arguments.clone()).map_err(|err| ToolError::InvocationFailed {
        name: call.name.clone(),
        message: format!("invalid arguments: {err}"),
    })
}

/// Turns a tool-fn's `Result<String, String>` into a [`ToolResult`]:
/// `Ok` -> `is_error: false`, `Err` -> `is_error: true`. Both are
/// surfaced back to the model as a normal tool result, not a hard
/// provider error — only permission denials and malformed arguments
/// short-circuit `invoke` with `Err(ToolError)`.
///
/// Content is truncated (see [`truncate_output`]) here — the single seam
/// every one of the six local tools' output passes through — rather than
/// per-tool, so a large `read_file`/`grep`/`shell_exec` output can't blow
/// the model's context budget on its own regardless of which tool
/// produced it. See docs/AUDITORIA-2026-07.md hallazgo D2.
fn wrap(call: &ToolCall, outcome: Result<String, String>) -> ToolResult {
    match outcome {
        Ok(content) => ToolResult {
            tool_call_id: call.id.clone(),
            content: truncate_output(content),
            is_error: false,
        },
        Err(content) => ToolResult {
            tool_call_id: call.id.clone(),
            content: truncate_output(content),
            is_error: true,
        },
    }
}

/// Cap on a single tool result's size. Chosen relative to
/// `OllamaBackend`'s default `num_ctx` (8192 tokens, ~4 chars/token): one
/// oversized tool result — a large file dump, a `grep -r`/`glob` over a
/// big tree — must not, on its own, be able to push the prompt past a
/// small local model's entire context window and trigger the silent
/// truncation-from-the-front that `num_ctx` already documents as
/// dangerous (loses the system prompt and tool definitions first).
const MAX_TOOL_OUTPUT_BYTES: usize = 8_000;

/// Truncates `content` to [`MAX_TOOL_OUTPUT_BYTES`] at a UTF-8-safe
/// boundary, appending an actionable trailer (not just "truncated" — a
/// small model needs to be told *what to do differently*, per
/// docs/AUDITORIA-2026-07.md's finding that terse errors get retried
/// verbatim instead of corrected).
fn truncate_output(content: String) -> String {
    if content.len() <= MAX_TOOL_OUTPUT_BYTES {
        return content;
    }
    let mut cut = MAX_TOOL_OUTPUT_BYTES;
    while !content.is_char_boundary(cut) {
        cut -= 1;
    }
    let omitted = content.len() - cut;
    format!(
        "{}\n\n[output truncated: {omitted} of {} bytes omitted. Narrow your query — a more \
         specific path/pattern, or a smaller file — instead of retrying this exact call.]",
        &content[..cut],
        content.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{allow_guard, deny_guard, unique_temp_dir};

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[tokio::test]
    async fn list_stubs_returns_all_six_tools() {
        let provider = LocalToolsProvider::new(allow_guard(std::env::temp_dir()));
        let stubs = provider
            .list_stubs()
            .await
            .expect("list_stubs should succeed");
        let names: Vec<&str> = stubs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "write_file",
                "edit_file",
                "shell_exec",
                "grep",
                "glob"
            ]
        );
        assert!(stubs.iter().all(|s| s.source == "local"));
    }

    #[tokio::test]
    async fn resolve_schema_for_unknown_tool_is_ok_none() {
        let provider = LocalToolsProvider::new(allow_guard(std::env::temp_dir()));
        let schema = provider
            .resolve_schema("does_not_exist")
            .await
            .expect("resolve_schema must not error on an unknown name");
        assert!(schema.is_none());
    }

    #[tokio::test]
    async fn resolve_schema_for_known_tool_is_some() {
        let provider = LocalToolsProvider::new(allow_guard(std::env::temp_dir()));
        let schema = provider
            .resolve_schema("read_file")
            .await
            .expect("resolve_schema should succeed")
            .expect("read_file is a known tool");
        assert_eq!(schema.name, "read_file");
    }

    #[tokio::test]
    async fn invoke_read_file_happy_path() {
        let dir = unique_temp_dir("provider-read-file");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("hello.txt");
        tokio::fs::write(&file_path, "hi")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "read_file",
                serde_json::json!({ "path": file_path.to_string_lossy() }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        assert_eq!(result.content, "hi");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for F1: a relative path must resolve against the
    /// explicit workdir passed to `with_workdir`, not the process's own
    /// cwd — this is the exact scenario `braze-bench` needs (one sandbox
    /// per task, launched from wherever the bench binary itself runs).
    #[tokio::test]
    async fn invoke_read_file_resolves_relative_path_against_explicit_workdir() {
        let dir = unique_temp_dir("provider-read-file-workdir");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("notas.txt"), "linea unica")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::with_workdir(allow_guard(&dir), &dir);
        let result = provider
            .invoke(&call(
                "read_file",
                serde_json::json!({ "path": "notas.txt" }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        assert_eq!(result.content, "linea unica");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for F1: a relative `write_file` path must land
    /// inside the explicit workdir, never wherever the process happens to
    /// be running from — the exact bug that made `braze-bench` write
    /// task-generated files into the real repo instead of the sandbox.
    #[tokio::test]
    async fn invoke_write_file_resolves_relative_path_against_explicit_workdir() {
        let dir = unique_temp_dir("provider-write-file-workdir");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");

        let provider = LocalToolsProvider::with_workdir(allow_guard(&dir), &dir);
        let result = provider
            .invoke(&call(
                "write_file",
                serde_json::json!({ "path": "saludo.txt", "content": "hola mundo" }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        let contents = tokio::fs::read_to_string(dir.join("saludo.txt"))
            .await
            .expect("the file must exist inside the workdir");
        assert_eq!(contents, "hola mundo");

        let process_cwd_path = std::env::current_dir()
            .expect("cwd should resolve")
            .join("saludo.txt");
        let leaked_into_cwd = process_cwd_path.exists();
        // Clean up defensively before asserting, so a broken fix doesn't
        // leave a stray file behind in the real working directory across
        // test runs.
        let _ = std::fs::remove_file(&process_cwd_path);
        assert!(
            !leaked_into_cwd,
            "the file must not leak into the process cwd"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for D2: a large file must not reach the model
    /// unbounded — a single oversized tool result can blow a small local
    /// model's entire context budget on its own.
    #[tokio::test]
    async fn invoke_read_file_truncates_a_large_file() {
        let dir = unique_temp_dir("provider-read-file-large");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("big.txt");
        let big_content = "x".repeat(MAX_TOOL_OUTPUT_BYTES * 2);
        tokio::fs::write(&file_path, &big_content)
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "read_file",
                serde_json::json!({ "path": file_path.to_string_lossy() }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        assert!(result.content.len() < big_content.len());
        assert!(result.content.contains("output truncated"));
        assert!(
            result.content.contains("Narrow your query"),
            "expected an actionable trailer, got: {}",
            result.content
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invoke_write_file_happy_path_inside_allowlist() {
        let dir = unique_temp_dir("provider-write-file-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "write_file",
                serde_json::json!({ "path": file_path.to_string_lossy(), "content": "payload" }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "payload");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invoke_write_file_denied_does_not_touch_disk() {
        // deny_guard's allowlist root doesn't cover `outside`, so this
        // write is classified Irreversible by DefaultClassifier; the
        // guard's prompt always answers "no".
        let allow_root = unique_temp_dir("provider-write-file-denied-root");
        let outside = unique_temp_dir("provider-write-file-denied-target").join("out.txt");
        let provider = LocalToolsProvider::new(deny_guard(&allow_root));

        let result = provider
            .invoke(&call(
                "write_file",
                serde_json::json!({ "path": outside.to_string_lossy(), "content": "payload" }),
            ))
            .await;

        assert!(result.is_err());
        assert!(!outside.exists(), "denied write must not touch disk");
    }

    #[tokio::test]
    async fn invoke_edit_file_happy_path() {
        let dir = unique_temp_dir("provider-edit-file-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("fixture.txt");
        tokio::fs::write(&file_path, "hello world")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "edit_file",
                serde_json::json!({
                    "path": file_path.to_string_lossy(),
                    "old_string": "world",
                    "new_string": "braze"
                }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "hello braze");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invoke_shell_exec_happy_path() {
        let provider = LocalToolsProvider::new(allow_guard(std::env::temp_dir()));
        let result = provider
            .invoke(&call(
                "shell_exec",
                serde_json::json!({ "command": ["echo", "hello"] }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    async fn invoke_shell_exec_denied_does_not_run_the_command() {
        let dir = unique_temp_dir("provider-shell-exec-denied");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let target = dir.join("keep-me.txt");
        tokio::fs::write(&target, "still here")
            .await
            .expect("write fixture");

        // "rm -rf" is Irreversible under DefaultClassifier regardless of
        // the allowlist; deny_guard's prompt always says no.
        let provider = LocalToolsProvider::new(deny_guard(&dir));
        let result = provider
            .invoke(&call(
                "shell_exec",
                serde_json::json!({ "command": ["rm", "-rf", target.to_string_lossy()] }),
            ))
            .await;

        assert!(result.is_err());
        assert!(target.exists(), "denied shell command must not run");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invoke_grep_happy_path() {
        let dir = unique_temp_dir("provider-grep-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("a.txt"), "needle here")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "grep",
                serde_json::json!({ "pattern": "needle", "path": dir.to_string_lossy() }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        assert!(result.content.contains("needle here"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invoke_glob_happy_path() {
        let dir = unique_temp_dir("provider-glob-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        tokio::fs::write(dir.join("keep.rs"), "// rust")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "glob",
                serde_json::json!({ "pattern": "*.rs", "path": dir.to_string_lossy() }),
            ))
            .await
            .expect("invoke should succeed");

        assert!(!result.is_error);
        assert!(result.content.contains("keep.rs"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test: a `read_file` reaching outside the
    /// `WorkdirAllowlist` (e.g. `~/.ssh/id_rsa`, `/etc/shadow`) must not
    /// happen silently — `deny_guard`'s prompt always says no, so the read
    /// must be denied rather than succeeding.
    #[tokio::test]
    async fn invoke_read_file_outside_allowlist_is_denied() {
        let allow_root = unique_temp_dir("provider-read-file-denied-root");
        let secret_dir = unique_temp_dir("provider-read-file-denied-secret");
        tokio::fs::create_dir_all(&secret_dir)
            .await
            .expect("create secret dir");
        let secret = secret_dir.join("id_rsa");
        tokio::fs::write(&secret, "-----BEGIN PRIVATE KEY-----")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(deny_guard(&allow_root));
        let result = provider
            .invoke(&call(
                "read_file",
                serde_json::json!({ "path": secret.to_string_lossy() }),
            ))
            .await;

        assert!(result.is_err(), "read outside the workdir must be gated");

        let _ = tokio::fs::remove_dir_all(&secret_dir).await;
    }

    #[tokio::test]
    async fn invoke_grep_outside_allowlist_is_denied() {
        let allow_root = unique_temp_dir("provider-grep-denied-root");
        let outside = unique_temp_dir("provider-grep-denied-outside");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("create outside dir");

        let provider = LocalToolsProvider::new(deny_guard(&allow_root));
        let result = provider
            .invoke(&call(
                "grep",
                serde_json::json!({ "pattern": "x", "path": outside.to_string_lossy() }),
            ))
            .await;

        assert!(result.is_err(), "grep outside the workdir must be gated");

        let _ = tokio::fs::remove_dir_all(&outside).await;
    }

    #[tokio::test]
    async fn invoke_glob_outside_allowlist_is_denied() {
        let allow_root = unique_temp_dir("provider-glob-denied-root");
        let outside = unique_temp_dir("provider-glob-denied-outside");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("create outside dir");

        let provider = LocalToolsProvider::new(deny_guard(&allow_root));
        let result = provider
            .invoke(&call(
                "glob",
                serde_json::json!({ "pattern": "*.rs", "path": outside.to_string_lossy() }),
            ))
            .await;

        assert!(result.is_err(), "glob outside the workdir must be gated");

        let _ = tokio::fs::remove_dir_all(&outside).await;
    }

    #[tokio::test]
    async fn invoke_unknown_tool_name_is_not_found() {
        let provider = LocalToolsProvider::new(allow_guard(std::env::temp_dir()));
        let err = provider
            .invoke(&call("does_not_exist", serde_json::json!({})))
            .await
            .expect_err("unknown tool name must error");

        assert!(matches!(err, ToolError::NotFound(name) if name == "does_not_exist"));
    }

    #[tokio::test]
    async fn invoke_malformed_arguments_is_invocation_failed() {
        let provider = LocalToolsProvider::new(allow_guard(std::env::temp_dir()));
        let err = provider
            .invoke(&call("read_file", serde_json::json!({ "wrong_field": 1 })))
            .await
            .expect_err("malformed arguments must error");

        assert!(matches!(err, ToolError::InvocationFailed { .. }));
    }

    // --- truncate_output (hallazgo D2) ---

    #[test]
    fn short_content_passes_through_unchanged() {
        let content = "hello world".to_string();
        assert_eq!(truncate_output(content.clone()), content);
    }

    #[test]
    fn content_at_exactly_the_cap_is_unchanged() {
        let content = "x".repeat(MAX_TOOL_OUTPUT_BYTES);
        assert_eq!(truncate_output(content.clone()), content);
    }

    #[test]
    fn oversized_content_is_truncated_with_an_actionable_trailer() {
        let original_len = MAX_TOOL_OUTPUT_BYTES * 3;
        let content = "x".repeat(original_len);
        let truncated = truncate_output(content);
        // Retained content is capped exactly at MAX_TOOL_OUTPUT_BYTES; the
        // trailer on top is small relative to the (much larger) original.
        assert!(truncated.len() < original_len);
        assert!(truncated.starts_with(&"x".repeat(MAX_TOOL_OUTPUT_BYTES)));
        assert!(truncated.contains("output truncated"));
        assert!(truncated.contains(&format!(
            "{} of {original_len} bytes",
            MAX_TOOL_OUTPUT_BYTES * 2
        )));
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_utf8_character() {
        // 'é' is 2 bytes; placed right at the cap so a naive byte-index
        // slice would land mid-character and panic. A `String` return
        // value is valid UTF-8 by construction, so simply not panicking
        // here is the assertion.
        let content = format!(
            "{}é{}",
            "x".repeat(MAX_TOOL_OUTPUT_BYTES - 1),
            "y".repeat(50)
        );
        let _ = truncate_output(content);
    }
}
