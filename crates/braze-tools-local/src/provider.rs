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
    /// Gate sintáctico pre-aplicación (`syntactic_gate.rs`): rechaza una
    /// edición que introduciría un error de sintaxis en un `.rs` ANTES de
    /// escribirla, dejando el archivo siempre válido. On by default — el
    /// hallazgo Tier-1 del survey de referencia (SWE-agent
    /// reject-before-apply). `Config::disable_syntactic_edit_gate` (vía
    /// `braze-cli`) es el opt-out; complementa, no reemplaza, al
    /// `post_edit_check` (parse instantáneo antes vs `cargo check`
    /// después).
    syntactic_edit_gate: bool,
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
    /// Encierra cada `shell_exec` del modelo en un mount namespace
    /// bubblewrap (`crate::bwrap`, design doc
    /// `docs/bwrap-tool-sandbox-design-2026-08-10.md`). Off by default;
    /// `Config::enable_bwrap_tool_sandbox` / `+ablate` no aplica (es
    /// seguridad, no palanca de bench). Cuando está on pero `bwrap` no
    /// está en el PATH, se degrada corriendo sin encierro con un warning
    /// una sola vez (`bwrap_degraded_warned`).
    bwrap_sandbox: bool,
    /// Permite red del host dentro del sandbox bwrap
    /// (`Config::bwrap_allow_network`). Sin efecto si `bwrap_sandbox` es
    /// off. Off by default: un `shell_exec` del modelo no tiene por qué
    /// alcanzar la red salvo que el usuario lo habilite.
    bwrap_allow_network: bool,
    /// Spill-to-file del tool output truncado
    /// (`docs/tool-output-spill-design-2026-08-11.md`): al truncar, el
    /// output completo se guarda en `.braze/spill/<call_id>.txt` y el
    /// trailer apunta ahí para que el modelo lo recupere con `read_file`
    /// (offset/limit) en vez de re-correr el comando. On by default (es
    /// sin pérdida y el path es leíble sin fricción);
    /// `Config::enable_tool_output_spill` / `+ablate:no-spill` lo apaga.
    /// El head+tail del truncado es siempre-on, independiente de este flag.
    spill_enabled: bool,
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
            syntactic_edit_gate: true,
            output_budget: MAX_TOOL_OUTPUT_BYTES,
            output_max_lines: None,
            formatters: crate::post_edit_check::default_rust_formatters(),
            bwrap_sandbox: false,
            bwrap_allow_network: false,
            spill_enabled: true,
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
            syntactic_edit_gate: true,
            output_budget: MAX_TOOL_OUTPUT_BYTES,
            output_max_lines: None,
            formatters: crate::post_edit_check::default_rust_formatters(),
            bwrap_sandbox: false,
            bwrap_allow_network: false,
            spill_enabled: true,
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

    /// Enables/disables the pre-application syntactic gate — chainable,
    /// same shape as [`Self::with_post_edit_check`]. See
    /// `syntactic_edit_gate`'s field doc comment.
    pub fn with_syntactic_edit_gate(mut self, enabled: bool) -> Self {
        self.syntactic_edit_gate = enabled;
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

    /// Enables the bubblewrap out-of-process sandbox for `shell_exec` —
    /// chainable, same shape as [`Self::with_post_edit_check`]. See
    /// `bwrap_sandbox`'s field doc comment and
    /// `docs/bwrap-tool-sandbox-design-2026-08-10.md`.
    pub fn with_bwrap_sandbox(mut self, enabled: bool) -> Self {
        self.bwrap_sandbox = enabled;
        self
    }

    /// Allows host network inside the bwrap sandbox — chainable. No-op
    /// unless [`Self::with_bwrap_sandbox`] is also on.
    pub fn with_bwrap_allow_network(mut self, allow: bool) -> Self {
        self.bwrap_allow_network = allow;
        self
    }

    /// Enables/disables spill-to-file of truncated tool output —
    /// chainable, same shape as [`Self::with_post_edit_check`]. See
    /// `spill_enabled`'s field doc comment and
    /// `docs/tool-output-spill-design-2026-08-11.md`.
    pub fn with_tool_output_spill(mut self, enabled: bool) -> Self {
        self.spill_enabled = enabled;
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
        let result = self.wrap(
            call,
            write_file::write_file(args, self.syntactic_edit_gate).await,
        );
        Ok(self.append_post_edit_feedback(result, &path).await)
    }

    async fn invoke_edit_file(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: EditFileArgs = parse_args(call)?;
        args.path = self.resolve(&args.path);
        self.check_write(call, &args.path).await?;
        let path = args.path.clone();
        let result = self.wrap(
            call,
            edit_file::edit_file(args, self.edit_strict_mode, self.syntactic_edit_gate).await,
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
            && let Some(feedback) =
                crate::post_edit_check::post_edit_feedback(path, &self.formatters).await
        {
            result.content.push_str(&feedback);
        }
        result
    }

    /// Incidente roam #14 (2026-07-20): una denegación de permiso volvía
    /// como `"action denied: run \`cargo test\`"` y nada más — un
    /// callejón sin salida. Observado en vivo: el modelo quiso verificar
    /// con `cargo test`, el permiso se denegó (en `braze run` headless
    /// EOF = no), y sin saber qué hacer con el "no" el modelo <em>fabricó
    /// el éxito</em> ("Running cargo test should now compile… confirming
    /// both behave as expected") sobre un test que en realidad fallaba.
    /// El mensaje ahora dirige: el comando NO corrió, no inventes su
    /// salida, y si era verificación, díselo al usuario con el comando
    /// exacto. Vale para modo interactivo también: un usuario que deniega
    /// dejaba al modelo en el mismo pozo.
    fn denied_message(err: &braze_permissions::PermissionError) -> String {
        format!(
            "{err}. The command did NOT run — no output was produced. Do not assume it \
             succeeded or invent its result. If this was a verification step (tests, a \
             build, a linter), say plainly in your final answer that you could not run it, \
             and include the exact command so the user can run it themselves."
        )
    }

    async fn invoke_shell_exec(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let mut args: ShellExecArgs = parse_args(call)?;
        let action = ActionDescriptor::ShellCommand {
            command: args.command.clone(),
        };
        self.guard
            .check(&action)
            .await
            .map_err(|err| ToolError::InvocationFailed {
                name: call.name.clone(),
                message: Self::denied_message(&err),
            })?;
        // Cuarta capa de seguridad (design doc
        // bwrap-tool-sandbox-design-2026-08-10): encerrar la ejecución ya
        // autorizada en un mount namespace. Se compone DEBAJO del guard
        // (que ya decidió) reescribiendo `command` a `bwrap <argv> --
        // <cmd>`; `shell_exec` spawnea `command[0]` con el resto, así que
        // hereda gratis su timeout y `kill_on_drop` (que con
        // `--die-with-parent` mata el árbol entero).
        if self.bwrap_sandbox {
            args.command = self.maybe_wrap_in_bwrap(args.command).await;
        }
        Ok(self.wrap(call, shell_exec::shell_exec(args, &self.workdir).await))
    }

    /// Reescribe `command` para correr bajo `bwrap`, o lo devuelve intacto
    /// si `bwrap` no está disponible (degradación con warning único —
    /// design doc § "degradar con warning"). El comando ya pasó el guard.
    async fn maybe_wrap_in_bwrap(&self, command: Vec<String>) -> Vec<String> {
        if !crate::bwrap::bwrap_available() {
            // Warning una sola vez por proceso: repetirlo por comando
            // ahogaría los logs y no aporta.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "bwrap sandbox requested but `bwrap` is not on PATH; shell_exec runs \
                     WITHOUT the out-of-process sandbox"
                );
            });
            return command;
        }
        let Some((program, rest)) = command.split_first() else {
            return command;
        };
        crate::bwrap::ensure_governance_files(&self.workdir);
        let secrets = crate::bwrap::discover_secrets(&self.workdir).await;
        // Sin mask file no se pueden enmascarar secretos: se degrada a no
        // montarlos (el resto del encierro sigue), en vez de abortar.
        let mask_file = crate::bwrap::create_mask_file().unwrap_or_default();
        let spec = crate::bwrap::BwrapSpec {
            workspace: self.workdir.clone(),
            git_writable: crate::bwrap::is_git_command(program),
            secrets: if mask_file.as_os_str().is_empty() {
                Vec::new()
            } else {
                secrets
            },
            mask_file,
            allow_network: self.bwrap_allow_network,
        };
        let argv = crate::bwrap::build_bwrap_argv(&spec, program, rest);
        let mut wrapped = Vec::with_capacity(argv.len() + 1);
        wrapped.push("bwrap".to_string());
        wrapped.extend(argv);
        wrapped
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
                message: Self::denied_message(&err),
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
        let spill = self
            .spill_enabled
            .then_some((call.id.as_str(), self.workdir.as_path()));
        let (content, is_error) = match outcome {
            Ok(content) => (content, false),
            Err(content) => (content, true),
        };
        ToolResult {
            tool_call_id: call.id.clone(),
            content: truncate_output_with_spill(
                content,
                self.output_budget,
                self.output_max_lines,
                spill,
            ),
            is_error,
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
/// Atajo sin spill — solo lo usan los tests del truncado puro
/// (producción siempre pasa por [`truncate_output_with_spill`] desde
/// `wrap`).
#[cfg(test)]
fn truncate_output(content: String, budget: usize, max_lines: Option<u32>) -> String {
    truncate_output_with_spill(content, budget, max_lines, None)
}

/// Fracción del presupuesto de bytes que se reserva para el HEAD del
/// output; el resto es el TAIL (blueprint gemini-cli `headRatio = 0.2`).
/// El tail pesa más a propósito: en grep/logs/builds el resultado o el
/// error suele estar al FINAL, y el head-only anterior lo tiraba.
const HEAD_BUDGET_NUM: usize = 2;
const HEAD_BUDGET_DEN: usize = 10;

/// Como [`truncate_output`], pero conservando HEAD+TAIL (no solo head) al
/// pasar el presupuesto de bytes, y —si `spill` es `Some`— escribiendo el
/// output COMPLETO (pre-truncado) a un archivo para que el modelo lo
/// recupere con `read_file` en vez de re-correr el comando (design doc
/// `docs/tool-output-spill-design-2026-08-11.md`).
///
/// `spill = Some((call_id, workdir))`: al truncar, se escribe
/// `workdir/.braze/spill/<call_id>.txt` con el `content` original. Un
/// fallo de escritura degrada al trailer "narrow your query" sin abortar.
/// El cap de líneas se aplica ANTES del head+tail, igual que antes.
fn truncate_output_with_spill(
    content: String,
    budget: usize,
    max_lines: Option<u32>,
    spill: Option<(&str, &Path)>,
) -> String {
    let max_lines = max_lines.unwrap_or(0) as usize;
    let total_lines = content.lines().count();
    // El output ORIGINAL, antes de cualquier truncado — lo que se
    // spillea (sin pérdida) si algo se recorta.
    let original_len = content.len();

    let (working, lines_omitted) = if max_lines > 0 && total_lines > max_lines {
        let retained = content
            .lines()
            .take(max_lines)
            .collect::<Vec<_>>()
            .join("\n");
        (retained, total_lines - max_lines)
    } else {
        (content.clone(), 0)
    };

    let bytes_before_cap = working.len();
    let (rendered, bytes_omitted) = if working.len() > budget {
        (head_tail(&working, budget), bytes_before_cap.saturating_sub(budget))
    } else {
        (working, 0)
    };

    if lines_omitted == 0 && bytes_omitted == 0 {
        return rendered;
    }

    // Algo se recortó: intentar el spill del output completo.
    let recovery = match spill {
        Some((call_id, workdir)) => match write_spill_file(workdir, call_id, &content) {
            Some(rel) => format!(
                "Full output ({original_len} bytes) saved to {rel} — read specific ranges \
                 with read_file (offset/limit) instead of re-running this command."
            ),
            None => NARROW_HINT.to_string(),
        },
        None => NARROW_HINT.to_string(),
    };

    match (lines_omitted, bytes_omitted) {
        (0, 0) => unreachable!("cubierto por el early-return de arriba"),
        (lines, 0) => format!(
            "{rendered}\n\n[output truncated: {lines} of {total_lines} lines omitted. {recovery}]"
        ),
        (0, bytes) => format!(
            "{rendered}\n\n[output truncated: {bytes} of {bytes_before_cap} bytes omitted from \
             the middle (head and tail kept). {recovery}]"
        ),
        (lines, bytes) => format!(
            "{rendered}\n\n[output truncated: {lines} of {total_lines} lines omitted, then \
             {bytes} more of the retained {bytes_before_cap} bytes cut from the middle (head \
             and tail kept). {recovery}]"
        ),
    }
}

/// El consejo cuando no hay spill (o falló): el modelo debe acotar, no
/// re-intentar igual. Terse errors get retried verbatim (AUDITORIA-2026-07).
const NARROW_HINT: &str =
    "Narrow your query — a more specific path/pattern, or a smaller file — instead of \
     retrying this exact call.";

/// Conserva el HEAD (`HEAD_BUDGET_NUM/HEAD_BUDGET_DEN` del budget) y el
/// TAIL (el resto) de `s`, con un marcador `...` en el medio, todo en
/// bordes de char UTF-8. `s.len()` DEBE ser > `budget` (el caller lo
/// garantiza).
fn head_tail(s: &str, budget: usize) -> String {
    let head_budget = budget * HEAD_BUDGET_NUM / HEAD_BUDGET_DEN;
    let tail_budget = budget - head_budget;
    // Fin del head en borde de char <= head_budget.
    let mut head_end = head_budget.min(s.len());
    while head_end > 0 && !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    // Inicio del tail en borde de char >= len - tail_budget, y nunca
    // antes del fin del head (si se solaparan, no hay medio que cortar).
    let mut tail_start = s.len().saturating_sub(tail_budget).max(head_end);
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!("{}\n...\n{}", &s[..head_end], &s[tail_start..])
}

/// Escribe `content` a `workdir/.braze/spill/<call_id>.txt` y devuelve la
/// ruta RELATIVA (`.braze/spill/<id>.txt`) para el trailer — relativa
/// porque el modelo la pasa a `read_file`, que resuelve contra el mismo
/// workdir. `None` si no se pudo escribir (dir no escribible, etc.) — el
/// caller degrada a solo-truncado. Escritura directa (no vía la tool
/// `write_file`): el harness no pasa por el `PermissionGuard`, y una
/// LECTURA posterior de `.braze/` es `Reversible` (silenciosa).
fn write_spill_file(workdir: &Path, call_id: &str, content: &str) -> Option<String> {
    // Sanitiza el id por si acaso (los ids del engine son uuid/rescued-…,
    // pero un `/` o `..` en un nombre de archivo sería un escape de dir).
    let safe: String = call_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let dir = workdir.join(".braze").join("spill");
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("{safe}.txt"));
    std::fs::write(&file, content).ok()?;
    Some(format!(".braze/spill/{safe}.txt"))
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

    /// Extrae `exit_code`/`stdout`/`stderr` del JSON que `shell_exec`
    /// serializa, esté en el `Ok` o el `Err` de la tool.
    fn shell_result(result: &ToolResult) -> (i64, String) {
        let v: serde_json::Value = serde_json::from_str(&result.content).expect("json de shell");
        (
            v["exit_code"].as_i64().unwrap_or(-999),
            format!("{}{}", v["stdout"].as_str().unwrap_or(""), v["stderr"].as_str().unwrap_or("")),
        )
    }

    /// Verificación EN VIVO del sandbox bwrap (design doc §
    /// "Verificación"): con bwrap real, un `shell_exec` (a) no puede
    /// leer un `.env` plantado, (b) no puede escribir fuera del
    /// workspace, (c) sí puede escribir dentro. Gated por disponibilidad
    /// de bwrap + user namespaces: en una máquina sin ellos el test se
    /// salta (no falla — la degradación es parte del diseño).
    #[tokio::test]
    async fn bwrap_sandbox_denies_secret_read_and_outside_write() {
        if !crate::bwrap::bwrap_available() {
            eprintln!("bwrap no disponible; se salta la verificación en vivo");
            return;
        }
        let dir = unique_temp_dir("bwrap-live");
        std::fs::create_dir_all(&dir).expect("workspace");
        std::fs::write(dir.join(".env"), "SECRET=hunter2\n").expect("plantar .env");

        let provider = LocalToolsProvider::with_workdir(allow_guard(dir.clone()), dir.clone())
            .with_bwrap_sandbox(true);

        // Sanity: si el propio bwrap no puede crear el namespace (userns
        // deshabilitado), abortamos el test sin marcarlo fallido —
        // `bwrap true` sale ≠0 con un mensaje de setup, no de política.
        let probe = provider
            .invoke(&call("shell_exec", serde_json::json!({ "command": ["true"] })))
            .await;
        if let Ok(r) = &probe {
            let (_, out) = shell_result(r);
            if out.contains("namespace") || out.contains("Operation not permitted") {
                eprintln!("user namespaces deshabilitados; se salta: {out}");
                return;
            }
        }

        // (a) LEER el secreto: dentro del sandbox el .env está montado
        // sobre un archivo chmod 000 → EACCES / permission denied.
        let read = provider
            .invoke(&call(
                "shell_exec",
                serde_json::json!({ "command": ["cat", ".env"] }),
            ))
            .await;
        let read = read.expect("la tool devuelve resultado (aunque el comando falle)");
        let (code, out) = shell_result(&read);
        assert_ne!(code, 0, "cat .env NO debe tener éxito dentro del sandbox: {out}");
        assert!(
            out.to_lowercase().contains("permission denied") || out.to_lowercase().contains("denied"),
            "el fallo debe ser por permiso, no otra cosa: {out}"
        );
        assert!(!out.contains("hunter2"), "el secreto NO debe leerse: {out}");

        // (b) ESCRIBIR fuera del workspace: el FS es read-only salvo el
        // workspace, así que tocar /etc falla.
        let outside = provider
            .invoke(&call(
                "shell_exec",
                serde_json::json!({ "command": ["touch", "/etc/braze-escape-probe"] }),
            ))
            .await
            .expect("resultado");
        let (code, out) = shell_result(&outside);
        assert_ne!(code, 0, "escribir en /etc debe fallar (FS read-only): {out}");
        assert!(!std::path::Path::new("/etc/braze-escape-probe").exists());

        // (c) ESCRIBIR dentro del workspace: el bind r/w lo permite.
        let inside = provider
            .invoke(&call(
                "shell_exec",
                serde_json::json!({ "command": ["touch", "adentro.txt"] }),
            ))
            .await
            .expect("resultado");
        let (code, out) = shell_result(&inside);
        assert_eq!(code, 0, "escribir dentro del workspace debe funcionar: {out}");
        assert!(dir.join("adentro.txt").exists(), "el archivo debió crearse en el host");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Con el sandbox OFF (default), el mismo `cat .env` SÍ lee el
    /// secreto — confirma que el encierro es lo que hace la diferencia,
    /// no otra cosa del entorno.
    #[tokio::test]
    async fn without_sandbox_secret_is_readable() {
        let dir = unique_temp_dir("bwrap-off");
        std::fs::create_dir_all(&dir).expect("workspace");
        std::fs::write(dir.join(".env"), "SECRET=hunter2\n").expect("plantar .env");
        let provider = LocalToolsProvider::with_workdir(allow_guard(dir.clone()), dir.clone());
        let r = provider
            .invoke(&call(
                "shell_exec",
                serde_json::json!({ "command": ["cat", ".env"] }),
            ))
            .await
            .expect("resultado");
        let (code, out) = shell_result(&r);
        assert_eq!(code, 0);
        assert!(out.contains("hunter2"), "sin sandbox el secreto se lee normal");
        let _ = std::fs::remove_dir_all(&dir);
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

        // Spill off: este test cubre el truncado puro (con spill on, el
        // trailer sería el del spill — cubierto por su propio test).
        let provider = LocalToolsProvider::new(allow_guard(&dir)).with_tool_output_spill(false);
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

    /// Spill-to-file (docs/tool-output-spill-design-2026-08-11.md): al
    /// truncar con el spill on, el output COMPLETO queda en
    /// `.braze/spill/<id>.txt` bajo el workdir y el trailer apunta ahí.
    #[tokio::test]
    async fn spill_writes_the_full_output_and_points_to_it() {
        let dir = unique_temp_dir("spill");
        tokio::fs::create_dir_all(&dir).await.expect("workdir");
        let file_path = dir.join("big.txt");
        let big = "x".repeat(MAX_TOOL_OUTPUT_BYTES * 2);
        tokio::fs::write(&file_path, &big).await.expect("fixture");

        // `with_workdir` = el spill aterriza bajo `dir`, no bajo el cwd.
        let provider = LocalToolsProvider::with_workdir(allow_guard(&dir), dir.clone());
        let result = provider
            .invoke(&call(
                "read_file",
                serde_json::json!({ "path": file_path.to_string_lossy() }),
            ))
            .await
            .expect("invoke ok");

        assert!(result.content.contains("output truncated"));
        assert!(
            result.content.contains(".braze/spill/call-1.txt"),
            "el trailer debe apuntar al spill: {}",
            result.content
        );
        assert!(
            !result.content.contains("Narrow your query"),
            "con spill no se usa el hint de acotar"
        );
        // El archivo de spill existe y contiene el output COMPLETO (el
        // archivo leído es una sola línea gigante, sin cap de líneas).
        let spilled = tokio::fs::read_to_string(dir.join(".braze/spill/call-1.txt"))
            .await
            .expect("spill file existe");
        assert_eq!(spilled.len(), big.len(), "el spill guarda el output completo");

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

    /// Gate sintáctico end-to-end por el provider (on by default): una
    /// edición que rompería la sintaxis de un `.rs` que compilaba se
    /// rechaza SIN tocar disco — el archivo queda intacto y el resultado
    /// es un error accionable.
    #[tokio::test]
    async fn a_syntax_breaking_edit_is_rejected_and_the_file_is_unchanged() {
        let dir = unique_temp_dir("provider-syntactic-gate");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("lib.rs");
        let good = "pub fn a() -> i32 {\n    1\n}\n";
        tokio::fs::write(&file_path, good)
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir));
        let result = provider
            .invoke(&call(
                "edit_file",
                serde_json::json!({
                    "path": file_path.to_string_lossy(),
                    "old_string": "    1\n}",
                    "new_string": "    1\n" // borra la llave de cierre → sintaxis rota
                }),
            ))
            .await
            .expect("invoke devuelve Ok con un ToolResult de error, no un Err duro");

        assert!(result.is_error, "la edición que rompe sintaxis debe fallar");
        assert!(
            result.content.contains("NOT applied") && result.content.contains("syntax error"),
            "mensaje accionable, got: {}",
            result.content
        );
        let after = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(after, good, "el archivo NO debe haber cambiado en disco");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Con el gate apagado, la MISMA edición aterriza (rompe el archivo) —
    /// confirma que el opt-out realmente desactiva el gate.
    #[tokio::test]
    async fn the_syntactic_gate_can_be_disabled() {
        let dir = unique_temp_dir("provider-gate-off");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("lib.rs");
        tokio::fs::write(&file_path, "pub fn a() -> i32 {\n    1\n}\n")
            .await
            .expect("write fixture");

        let provider = LocalToolsProvider::new(allow_guard(&dir)).with_syntactic_edit_gate(false);
        let result = provider
            .invoke(&call(
                "edit_file",
                serde_json::json!({
                    "path": file_path.to_string_lossy(),
                    "old_string": "    1\n}",
                    "new_string": "    1\n"
                }),
            ))
            .await
            .expect("invoke ok");

        assert!(!result.is_error, "con el gate off la edición aplica");
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

        let err = result.expect_err("a denied command must fail");
        assert!(target.exists(), "denied shell command must not run");
        // Incidente roam #14: la denegación debe dirigir, no ser un
        // callejón sin salida que invite a fabricar el resultado.
        let msg = err.to_string();
        assert!(msg.contains("did NOT run"), "got: {msg}");
        assert!(
            msg.contains("could not run it") && msg.contains("command"),
            "the denial must steer the model to surface the command to the user: {msg}"
        );

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
        assert_eq!(
            truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, None),
            content
        );
    }

    #[test]
    fn content_at_exactly_the_cap_is_unchanged() {
        let content = "x".repeat(MAX_TOOL_OUTPUT_BYTES);
        assert_eq!(
            truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, None),
            content
        );
    }

    #[test]
    fn oversized_content_is_truncated_with_an_actionable_trailer() {
        // Head+tail: distinguir extremos para verificar que AMBOS
        // sobreviven. Head 'h', medio 'm' (lo que se corta), tail 't'.
        let head = "h".repeat(MAX_TOOL_OUTPUT_BYTES);
        let mid = "m".repeat(MAX_TOOL_OUTPUT_BYTES);
        let tail = "t".repeat(MAX_TOOL_OUTPUT_BYTES);
        let original_len = head.len() + mid.len() + tail.len();
        let content = format!("{head}{mid}{tail}");
        let truncated = truncate_output(content, MAX_TOOL_OUTPUT_BYTES, None);
        assert!(truncated.len() < original_len);
        // El body (antes del trailer) empieza con 'h' y termina con 't':
        // el tail sobrevive, que es la mejora sobre el head-only.
        let body = truncated.split("\n\n[output truncated").next().unwrap();
        assert!(body.starts_with('h'));
        assert!(body.ends_with('t'), "el tail debe sobrevivir");
        assert!(!body.contains('m'), "el medio se cortó");
        assert!(truncated.contains("output truncated"));
        assert!(truncated.contains("omitted from the middle"));
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
        // El contenido retenido (head+tail, sin el trailer) cabe en el
        // budget; el trailer va encima como siempre.
        assert!(truncated.starts_with('x'));
        assert!(truncated.contains("output truncated"));
        // 500 - 100 de budget = 400 bytes cortados del medio.
        assert!(truncated.contains("400 of 500 bytes"));
    }

    /// `max_lines: Some(N)` truncates by line count even when content fits
    /// the byte budget. This is the case that needs the separate cap:
    /// many short lines (each ~3 bytes) can fit the byte budget but blow
    /// the model's context with thousands of repetitions — `grep -r`
    /// over a big repo is the canonical example.
    #[test]
    fn truncate_output_caps_at_max_lines_independently_of_the_byte_budget() {
        let content = (0..1000)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        // 5-char lines + newline = 6 bytes/line × 1000 = 6000 bytes — well
        // below the byte cap (8000), so without `max_lines` this passes
        // through untruncated.
        assert!(content.len() < MAX_TOOL_OUTPUT_BYTES);
        let truncated = truncate_output(content.clone(), MAX_TOOL_OUTPUT_BYTES, Some(5));
        assert!(
            truncated.contains("output truncated"),
            "must trigger the trailer: {truncated}"
        );
        assert!(truncated.contains("lines"));
        // The first 5 lines are retained; "line0".."line4".
        assert!(truncated.contains("line0"));
        assert!(truncated.contains("line4"));
        assert!(
            !truncated.contains("line5"),
            "5th-line-onward must be truncated: {truncated}"
        );
    }

    /// `max_lines: None` (the default) keeps the byte-cap-only behavior —
    /// confirming the new line-cap doesn't regress the existing
    /// happy path where 1000 short lines (under the byte budget) stay
    /// untruncated.
    #[test]
    fn truncate_output_with_no_max_lines_does_not_apply_a_line_cap() {
        let content = (0..1000)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
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
        let content = (0..5)
            .map(|_| long_line.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let small_budget = 50usize;
        let truncated = truncate_output(content, small_budget, Some(3));

        let trailer_start = truncated
            .find("\n\n[output truncated")
            .expect("must be truncated");
        let retained = &truncated[..trailer_start];
        // Head+tail: el contenido retenido son los bytes del budget más el
        // marcador `\n...\n` del medio (5 bytes). Debe respetar el budget
        // salvo por ese marcador constante.
        const MIDDLE_MARKER: usize = 5; // "\n...\n"
        assert!(
            retained.len() <= small_budget + MIDDLE_MARKER,
            "retained content must respect the byte budget (plus the middle marker) even after \
             max_lines kept a few oversized lines: {} bytes retained, budget was {small_budget}",
            retained.len()
        );
        assert!(
            truncated.contains("lines omitted"),
            "must report the line truncation too: {truncated}"
        );
        assert!(
            truncated.contains("bytes"),
            "must report the byte truncation too: {truncated}"
        );
    }

    #[test]
    fn head_tail_keeps_both_ends_and_marks_the_middle() {
        let head = "A".repeat(1000);
        let tail = "Z".repeat(1000);
        let content = format!("{head}{}{tail}", "M".repeat(5000));
        let truncated = truncate_output(content, 1000, None);
        // Ambos extremos presentes; el medio ('M') NO.
        let body = truncated.split("\n\n[output truncated").next().unwrap();
        assert!(body.starts_with('A'), "head presente");
        assert!(body.ends_with('Z'), "tail presente");
        assert!(!body.contains('M'), "el medio se cortó");
        assert!(truncated.contains("head and tail kept"));
    }

    #[test]
    fn content_within_budget_passes_through_untouched() {
        let content = "pequeño".to_string();
        assert_eq!(truncate_output(content.clone(), 1000, None), content);
    }

    #[test]
    fn tail_ratio_favors_the_end_where_errors_live() {
        // Budget 100: head ~20 bytes, tail ~80. El tail (donde vive el
        // error de un build) debe ser más grande que el head.
        let head = "H".repeat(500);
        let tail = "T".repeat(500);
        let content = format!("{head}{tail}");
        let truncated = truncate_output(content, 100, None);
        let body = truncated.split("\n\n[output truncated").next().unwrap();
        let n_head = body.matches('H').count();
        let n_tail = body.matches('T').count();
        assert!(n_tail > n_head, "el tail debe pesar más: head={n_head} tail={n_tail}");
    }
}
