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
    /// Gates the post-edit `cargo check` guardrail (see
    /// `post_edit_check.rs`). On by default — the ACI evidence (arXiv
    /// 2405.15793) is that feeding breakage back in the very next
    /// observation is one of the highest-value interface features; a
    /// no-op outside Cargo projects. `Config::disable_post_edit_check`
    /// (via `braze-cli`) is the opt-out.
    post_edit_check: bool,
    /// Gates `edit_file`'s fuzzy matching ladder (rungs 2-3) — see
    /// `edit_file.rs`'s "Strict mode" doc section (E1,
    /// docs/AUDITORIA-2026-07-v3.md). `false` (the default) keeps the
    /// fuzzy ladder on, the production behavior since Aider's fix; `true`
    /// is an ablation knob for `braze-bench`, not something a real
    /// `braze` invocation has a reason to set.
    edit_strict_mode: bool,
    /// Per-tool-result byte budget before `wrap` truncates and appends
    /// an actionable "narrow your query" trailer. Defaults to
    /// [`MAX_TOOL_OUTPUT_BYTES`] (v4 P2.4 — configurable via
    /// `Config::tool_output_max_bytes` / `BRAZE_TOOL_OUTPUT_MAX_BYTES`).
    output_budget: usize,
    /// Per-tool-result line budget (v4 P2.4). `None` is **not** a cap —
    /// only `output_budget` applies. `Some(N)` additionally truncates at
    /// `N` lines if the byte cap hasn't hit yet, useful for many-short-
    /// line outputs (a `grep -r` over a thousands-file repo) where a
    /// byte-only cap can still show 100k+ lines. Configurable via
    /// `Config::tool_output_max_lines` / `BRAZE_TOOL_OUTPUT_MAX_LINES`.
    output_max_lines: Option<u32>,
    /// Per-extension formatter map for the post-edit guardrail (v4
    /// P1.6 — generalizes the previously Rust-only `cargo check`).
    /// Defaults to [`crate::post_edit_check::default_rust_formatters`]
    /// (a single Rust `cargo check` entry, equivalent to the
    /// pre-generalization hardcoded behavior). Overridable via
    /// [`LocalToolsProvider::with_formatters`] from
    /// `Config::formatters`.
    formatters: Vec<braze_config::FormatterConfig>,
}

impl LocalToolsProvider {
    /// Uses the process's current directory as the workdir — correct for
    /// `braze-cli`, where the process cwd *is* the project the agent
    /// operates on. A caller whose process cwd doesn't match the logical
    /// working directory (e.g. `braze-bench`, one sandbox per task) must
    /// use [`LocalToolsProvider::with_workdir`] instead.
    pub fn new(guard: PermissionGuard) -> Self {
        let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            guard,
            workdir,
            post_edit_check: true,
            edit_strict_mode: false,
            output_budget: MAX_TOOL_OUTPUT_BYTES,
            output_max_lines: None,
            formatters: crate::post_edit_check::default_rust_formatters(),
        }
    }

    /// Uses `workdir` as the base every relative path is resolved
    /// against. Must match the directory `guard`'s `WorkdirAllowlist` was
    /// scoped to, or the permission check and the actual I/O will
    /// disagree about what's "inside" the sandbox.
    pub fn with_workdir(guard: PermissionGuard, workdir: impl Into<PathBuf>) -> Self {
        Self {
            guard,
            workdir: workdir.into(),
            post_edit_check: true,
            edit_strict_mode: false,
            output_budget: MAX_TOOL_OUTPUT_BYTES,
            output_max_lines: None,
            formatters: crate::post_edit_check::default_rust_formatters(),
        }
    }

    /// Enables/disables the post-edit `cargo check` guardrail —
    /// chainable, same shape as the engine's `with_*` knobs.
    pub fn with_post_edit_check(mut self, enabled: bool) -> Self {
        self.post_edit_check = enabled;
        self
    }

    /// Enables/disables `edit_file`'s fuzzy matching ladder — chainable,
    /// same shape as [`Self::with_post_edit_check`]. See
    /// `edit_strict_mode`'s field doc comment.
    pub fn with_edit_strict_mode(mut self, strict: bool) -> Self {
        self.edit_strict_mode = strict;
        self
    }

    /// Overrides the per-tool-result byte budget (v4 P2.4) — see
    /// `output_budget`'s field doc comment. Chainable, same shape as
    /// [`Self::with_post_edit_check`].
    pub fn with_output_budget(mut self, budget: usize) -> Self {
        self.output_budget = budget;
        self
    }

    /// Overrides the per-tool-result line budget (v4 P2.4) — see
    /// `output_max_lines`'s field doc comment. `None` is **not** a cap.
    /// Chainable, same shape as [`Self::with_output_budget`].
    pub fn with_output_max_lines(mut self, max_lines: Option<u32>) -> Self {
        self.output_max_lines = max_lines;
        self
    }

    /// Overrides the post-edit formatter list (v4 P1.6 — generalizes
    /// Rust-only `cargo check` into a per-extension map). See
    /// `formatters`'s field doc comment. Chainable, same shape as
    /// [`Self::with_output_budget`].
    pub fn with_formatters(mut self, formatters: Vec<braze_config::FormatterConfig>) -> Self {
        self.formatters = formatters;
        self
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
        Ok(self.wrap(call, read_file::read_file(args).await))
    }

    async fn invoke_write_file(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: WriteFileArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_write(call, &args.path).await?;
        let path = args.path.clone();
        let result = self.wrap(call, write_file::write_file(args).await);
        Ok(self.append_post_edit_feedback(result, &path).await)
    }

    async fn invoke_edit_file(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: EditFileArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_write(call, &args.path).await?;
        let path = args.path.clone();
        let result = self.wrap(
            call,
            edit_file::edit_file(args, self.edit_strict_mode).await,
        );
        Ok(self.append_post_edit_feedback(result, &path).await)
    }

    /// Appends the post-edit guardrail's feedback (if any) to a
    /// *successful* write/edit result — a failed edit already carries
    /// its own error and never triggers the check (nothing new landed
    /// on disk to validate). The result stays `is_error: false` either
    /// way: the edit did apply; the feedback is the next problem to fix,
    /// not a failure of this call.
    async fn append_post_edit_feedback(&self, mut result: ToolResult, path: &str) -> ToolResult {
        if self.post_edit_check
            && !result.is_error
            && let Some(feedback) = crate::post_edit_check::post_edit_feedback(path, &self.formatters).await
        {
            result.content.push_str(&feedback);
        }
        result
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
        Ok(self.wrap(
            call,
            shell_exec::shell_exec(args, &self.workdir).await,
        ))
    }

    async fn invoke_grep(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: GrepArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_read(call, &args.path).await?;
        Ok(self.wrap(call, grep::grep(args).await))
    }

    async fn invoke_glob(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: GlobArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_read(call, &args.path).await?;
        Ok(self.wrap(call, glob::glob(args).await))
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
impl LocalToolsProvider {
    /// The single seam every one of the six local tools' output passes
    /// through (hallazgo D2, docs/AUDITORIA-2026-07.md) — now an
    /// `impl` method so it can honor this provider's configured
    /// `output_budget` / `output_max_lines` (v4 P2.4), overriding the
    /// hardcoded `MAX_TOOL_OUTPUT_BYTES` when a caller wires them via
    /// [`LocalToolsProvider::with_output_budget`] /
    /// [`LocalToolsProvider::with_output_max_lines`]. Truncation
    /// strategy: `max_lines` (if `Some`) truncates by line count first
    /// (useful for many-short-line outputs); the byte cap applies
    /// second. Either side gets an actionable trailer telling a small
    /// model *what to do differently*, never just "truncated".
    fn wrap(&self, call: &ToolCall, outcome: Result<String, String>) -> ToolResult {
        match outcome {
            Ok(content) => ToolResult {
                tool_call_id: call.id.clone(),
                content: truncate_output(content, self.output_budget, self.output_max_lines),
                is_error: false,
            },
            Err(content) => ToolResult {
                tool_call_id: call.id.clone(),
                content: truncate_output(content, self.output_budget, self.output_max_lines),
                is_error: true,
            },
        }
    }
}

/// Cap on a single tool result's size. Chosen relative to
/// `OllamaBackend`'s default `num_ctx` (8192 tokens, ~4 chars/token): one
/// oversized tool result — a large file dump, a `grep -r`/`glob` over a
/// big tree — must not, on its own, be able to push the prompt past a
/// small local model's entire context window and trigger the silent
/// truncation-from-the-front that `num_ctx` already documents as
/// dangerous (loses the system prompt and tool definitions first).
pub(crate) const MAX_TOOL_OUTPUT_BYTES: usize = 8_000;

/// Truncates `content` to `budget` bytes at a UTF-8-safe boundary,
/// appending an actionable trailer (not just "truncated" — a small
/// model needs to be told *what to do differently*, per
/// docs/AUDITORIA-2026-07.md's finding that terse errors get retried
/// verbatim instead of corrected). If `max_lines` is `Some(N)`, also
/// truncates at `N` lines first (v4 P2.4 — useful for many-short-line
/// outputs like a `grep -r` over a thousands-file repo where a
/// byte-only cap can still show 100k+ lines before triggering).
///
/// The byte cap always applies **after** the line cap, unconditionally
/// — a fix for a real bug (audit of the other-model commit `2923f63`,
/// 2026-07-09): the line-truncation branch used to `return` immediately
/// once it retained `max_lines` lines, without re-checking those
/// retained lines against `budget`. A handful of very long lines (e.g.
/// `max_lines: Some(5)` over 5 lines of 5KB each) could then blow past
/// `budget` even though this doc comment already promised the byte cap
/// applies second. Both truncations can now fire on the same call — the
/// trailer says so explicitly instead of only reporting one.
fn truncate_output(content: String, budget: usize, max_lines: Option<u32>) -> String {
    let max_lines = max_lines.unwrap_or(0) as usize;
    let total_lines = content.lines().count();

    let (mut working, lines_omitted) = if max_lines > 0 && total_lines > max_lines {
        let retained = content.lines().take(max_lines).collect::<Vec<_>>().join("\n");
        (retained, total_lines - max_lines)
    } else {
        (content, 0)
    };

    let bytes_before_cap = working.len();
    if working.len() > budget {
        let mut cut = budget;
        while !working.is_char_boundary(cut) {
            cut -= 1;
        }
        working.truncate(cut);
    }
    let bytes_omitted = bytes_before_cap - working.len();

    match (lines_omitted, bytes_omitted) {
        (0, 0) => working,
        (lines, 0) => format!(
            "{working}\n\n[output truncated: {lines} of {total_lines} lines omitted. Narrow \
             your query — a more specific path/pattern, or a smaller file — instead of \
             retrying this exact call.]"
        ),
        (0, bytes) => format!(
            "{working}\n\n[output truncated: {bytes} of {bytes_before_cap} bytes omitted. \
             Narrow your query — a more specific path/pattern, or a smaller file — instead of \
             retrying this exact call.]"
        ),
        (lines, bytes) => format!(
            "{working}\n\n[output truncated: {lines} of {total_lines} lines omitted, then \
             {bytes} more of the retained {bytes_before_cap} bytes cut to fit the output \
             budget. Narrow your query — a more specific path/pattern, or a smaller file — \
             instead of retrying this exact call.]"
        ),
    }
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

    /// Incidente roam #11: camino completo, no solo la función del
    /// guardrail — un `write_file` real sobre un crate real debe dejar
    /// la confirmación DENTRO del `ToolResult` que ve el modelo. El
    /// contrato viejo (silencio en éxito) hacía que este resultado fuera
    /// idéntico al de un archivo sin guardrail aplicable.
    #[tokio::test]
    async fn a_successful_edit_carries_the_post_edit_confirmation() {
        let dir = std::env::temp_dir().join(format!("braze-provider-pec-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).expect("crate dirs");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"pec-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");

        let provider = LocalToolsProvider::new(allow_guard(dir.clone()));
        let target = dir.join("src/main.rs");
        let result = provider
            .invoke(&call(
                "write_file",
                serde_json::json!({
                    "path": target.to_string_lossy(),
                    "content": "fn main() {}\n",
                }),
            ))
            .await
            .expect("write_file must succeed");

        assert!(!result.is_error, "the edit itself must succeed: {result:?}");
        assert!(
            result.content.contains("the code COMPILES"),
            "the model must see that the check ran and passed: {}",
            result.content
        );
        assert!(
            result.content.contains("no tests were run"),
            "…and must not be able to read it as full verification: {}",
            result.content
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, None), content);
    }

    #[test]
    fn content_at_exactly_the_cap_is_unchanged() {
        let content = "x".repeat(MAX_TOOL_OUTPUT_BYTES);
        assert_eq!(truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, None), content);
    }

    #[test]
    fn oversized_content_is_truncated_with_an_actionable_trailer() {
        let original_len = MAX_TOOL_OUTPUT_BYTES * 3;
        let content = "x".repeat(original_len);
        let truncated = truncate_output(content, MAX_TOOL_OUTPUT_BYTES, None);
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
        let _ = truncate_output(content, MAX_TOOL_OUTPUT_BYTES, None);
    }

    // --- tool_output knobs (v4 P2.4, configurable via
    // `Config::tool_output_max_bytes` / `tool_output_max_lines`) ---

    /// A reduced byte budget actually shrinks truncation, not just the
    /// default `MAX_TOOL_OUTPUT_BYTES`. Pins that `with_output_budget`
    /// is honored by `wrap` end-to-end.
    #[test]
    fn truncate_output_uses_a_caller_supplied_byte_budget() {
        let small_budget = 100usize;
        let content = "x".repeat(500);
        let truncated = truncate_output(content.clone(), small_budget, None);
        assert!(truncated.starts_with(&"x".repeat(small_budget)));
        assert!(truncated.contains("output truncated"));
        assert!(truncated.contains("400 of 500 bytes"));
    }

    /// `max_lines: Some(N)` truncates by line count even when content fits
    /// the byte budget. This is the case that needs the separate cap:
    /// many short lines (each ~3 bytes) can fit the byte budget but blow
    /// the model's context with thousands of repetitions — `grep -r`
    /// over a big repo is the canonical example.
    #[test]
    fn truncate_output_caps_at_max_lines_independently_of_the_byte_budget() {
        let content = (0..1000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        // 5-char lines + newline = 6 bytes/line × 1000 = 6000 bytes — well
        // below the byte cap (8000), so without `max_lines` this passes
        // through untruncated.
        assert!(content.len() < MAX_TOOL_OUTPUT_BYTES);
        let truncated = truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, Some(5));
        assert!(truncated.contains("output truncated"), "must trigger the trailer: {truncated}");
        assert!(truncated.contains("lines"));
        // The first 5 lines are retained; "line0".."line4".
        assert!(truncated.contains("line0"));
        assert!(truncated.contains("line4"));
        assert!(!truncated.contains("line5"), "5th-line-onward must be truncated: {truncated}");
    }

    /// `max_lines: None` (the default) keeps the byte-cap-only behavior —
    /// confirming the new line-cap doesn't regress the existing
    /// happy path where 1000 short lines (under the byte budget) stay
    /// untruncated.
    #[test]
    fn truncate_output_with_no_max_lines_does_not_apply_a_line_cap() {
        let content = (0..1000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        assert!(content.len() < MAX_TOOL_OUTPUT_BYTES);
        let pass_through = truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, None);
        assert_eq!(pass_through, content, "max_lines=None must not truncate");
    }

    /// Regression test for the truncation bug found auditing the
    /// other-model commit `2923f63` (2026-07-09): a handful of very long
    /// lines survives `max_lines` but must still be cut down to `budget`
    /// bytes afterward — the line cap retaining `max_lines` lines is not
    /// itself a guarantee those lines fit the byte budget.
    #[test]
    fn truncate_output_reapplies_the_byte_cap_after_a_few_very_long_lines_survive_max_lines() {
        let long_line = "x".repeat(100);
        // 5 lines of 100 bytes each = 500+ bytes retained, well over a
        // 50-byte budget — the exact "few long lines" shape the old code
        // never re-checked.
        let content = (0..5).map(|_| long_line.clone()).collect::<Vec<_>>().join("\n");
        let small_budget = 50usize;
        let truncated = truncate_output(content, small_budget, Some(3));

        let trailer_start = truncated.find("\n\n[output truncated").expect("must be truncated");
        let retained = &truncated[..trailer_start];
        assert!(
            retained.len() <= small_budget,
            "retained content must respect the byte budget even after max_lines kept a few \
             oversized lines: {} bytes retained, budget was {small_budget}",
            retained.len()
        );
        assert!(truncated.contains("lines omitted"), "must report the line truncation too: {truncated}");
        assert!(truncated.contains("bytes"), "must report the byte truncation too: {truncated}");
    }
}
