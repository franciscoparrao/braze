//! Runs one task against one backend, end to end, through the real
//! `braze_engine::Engine` — same composition `braze-cli` does at
//! startup, minus MCP servers (determinism: no dependency on what
//! happens to be configured/reachable) and with permission confirmation
//! replaced by [`BenchPrompt`] (see its doc on why, and PLAN.md's
//! "Hallazgo de diseño no anticipado").

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use braze_config::Config;
use braze_permissions::{
    ActionDescriptor, ConfirmationPrompt, DefaultClassifier, PermissionGuard, WorkdirAllowlist,
};
use braze_session::{FileSessionStore, SessionStore, SimpleContextCompactor};
use braze_types::SessionId;

use crate::backend_spec::{BackendSpec, SamplingSpec};
use crate::error::BenchError;
use crate::metrics::{MemoryRunMetrics, RunOutcome, TaskResult, compute_metrics};
use crate::preserve;
use crate::sandbox::TaskSandbox;
use crate::task::TaskDef;

/// Wall-clock budget for a single task attempt. A model stuck in a
/// non-convergence loop on CPU-only Ollama has been observed taking
/// upwards of 20 minutes to exhaust `MAX_TURN_ITERATIONS` on its own —
/// without an independent timeout here, one such task can stall an entire
/// sweep instead of being recorded as the (diagnostically useful) failure
/// it is. See docs/AUDITORIA-2026-07.md hallazgo F2.
pub const DEFAULT_TASK_TIMEOUT: Duration = Duration::from_secs(180);

/// Decides irreversible-flagged actions without a human, persisting the
/// same `PermissionRequested`/`PermissionDecided` pair the real
/// confirmation prompts do (see `braze-cli::TerminalConfirmationPrompt`,
/// `braze-tui::approval`) — without this, a denial never shows up in the
/// session log, so `metrics::compute_metrics`'s `permission_denials`
/// count stays stuck at 0 and the denial gets miscounted as a
/// `tool_execution_failures` instead (the engine already appended
/// `ToolCallStarted` before the tool's own dispatch path rejects it).
/// See N-35, docs/AUDITORIA-2026-07-v2.md.
///
/// The decision is deny-everything with ONE carve-out:
/// [`is_benchable_cargo`] (`cargo check`/`build`/`test`, no
/// config-injection flags). `DefaultClassifier` rightly refuses to
/// blanket-allow cargo interactively — `cargo check` runs an arbitrary
/// `build.rs` — but in the bench that ship has sailed: the sandbox
/// project is model-authored and the post-edit guardrail already runs
/// `cargo check` on it after every edit without asking anyone. What the
/// blanket denial actually measured (memory-distillation sweep
/// 2026-07-16, gpt-oss:20b: 12 denials across 15 tasks) was harness
/// friction — a human at the interactive prompt would answer "yes" to
/// every one of these — and it taxed the conditions that verify more,
/// exactly the contrast that suite exists to measure.
///
/// Combined with a `WorkdirAllowlist` scoped to a throwaway sandbox
/// directory, this means: safe/reversible actions (reads, writes inside
/// the sandbox, allowlisted shell commands) proceed exactly as they would
/// interactively, cargo's build/verify subcommands proceed as a human
/// supervisor would have approved, and everything else the classifier
/// flags `Irreversible` — a hallucinated `dd`/`curl`/`mv`, a write
/// outside the sandbox — is refused before it ever runs for real.
struct BenchPrompt {
    session: SessionId,
    store: Arc<dyn SessionStore>,
    /// Prefijos de comando del "motor" declarados por la tarea
    /// (`TaskDef::sandbox_commands`) — segundo carve-out del
    /// deny-everything, mismo contrato que cargo: ver
    /// [`is_declared_sandbox_command`].
    sandbox_commands: Vec<String>,
}

/// `cargo check`/`cargo build`/`cargo test` only, and only without the
/// flags that turn "compile the sandbox project" into "execute something
/// else": `--manifest-path` retargets an arbitrary project (whose
/// `build.rs` then runs), `--config` can inject `rustc-wrapper`/runner
/// executables inline, `-Z` unlocks unstable behavior. `cargo run` stays
/// denied outright — it exists to execute the produced binary.
///
/// Accepted both as a direct argv (`["cargo", "check"]`) and wrapped in
/// the `["bash", "-lc", "cargo check"]` shape — the preserved-session
/// diagnosis of 2026-07-16 showed gpt-oss:20b sends EVERY shell command
/// through `bash -lc`, so a carve-out that only matched `command[0] ==
/// "cargo"` never fired once across a whole sweep. The unwrapping is
/// deliberately strict (see [`unwrap_single_shell_script`]): a script
/// with any shell metacharacter is not "a cargo command in a wrapper",
/// it's a shell program, and stays denied.
pub(crate) fn is_benchable_cargo(action: &ActionDescriptor) -> bool {
    let ActionDescriptor::ShellCommand { command } = action else {
        return false;
    };
    match unwrap_single_shell_script(command) {
        Some(tokens) => is_plain_cargo_verify(&tokens),
        None => {
            let tokens: Vec<&str> = command.iter().map(String::as_str).collect();
            is_plain_cargo_verify(&tokens)
        }
    }
}

/// `["bash"|"sh", <only -c/-l style flags, at least one c>, <script>]` →
/// the script's whitespace-split tokens, and `None` for anything else —
/// including any script character outside `[A-Za-z0-9 _.=/-]`. The
/// whitelist is the security boundary: it excludes every shell
/// metacharacter (`;`, `|`, `&`, `$`, backticks, quotes, redirection,
/// globs, `~`), so a `Some` result is guaranteed to be a single plain
/// command, not a composite/expanding shell program wearing one's shape.
fn unwrap_single_shell_script(command: &[String]) -> Option<Vec<&str>> {
    let (program, rest) = command.split_first()?;
    if program != "bash" && program != "sh" {
        return None;
    }
    let (script, flags) = rest.split_last()?;
    if flags.is_empty()
        || !flags.iter().all(|f| {
            f.len() >= 2 && f.starts_with('-') && f[1..].chars().all(|c| c == 'c' || c == 'l')
        })
        || !flags.iter().any(|f| f.contains('c'))
    {
        return None;
    }
    if script.is_empty()
        || !script
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '=' | '/' | '-'))
    {
        return None;
    }
    Some(script.split_whitespace().collect())
}

/// ¿`action` matchea alguno de los prefijos de motor declarados por la
/// tarea (`TaskDef::sandbox_commands`)? Mismo contrato de seguridad que
/// [`is_benchable_cargo`]: el comando se acepta SOLO como argv plano —
/// directo, o desenvuelto de `bash -lc` por
/// [`unwrap_single_shell_script`], cuya whitelist de caracteres excluye
/// todo metacaracter de shell — y el prefijo declarado debe coincidir
/// token a token desde el inicio. Los argumentos posteriores al prefijo
/// quedan cubiertos por la misma whitelist (sin `;`/`|`/`$`/comillas no
/// hay composición posible), y las escrituras del script caen dentro del
/// sandbox desechable que `WorkdirAllowlist` ya acota.
pub(crate) fn is_declared_sandbox_command(
    action: &ActionDescriptor,
    declared: &[String],
) -> bool {
    if declared.is_empty() {
        return false;
    }
    let ActionDescriptor::ShellCommand { command } = action else {
        return false;
    };
    let tokens: Vec<&str> = match unwrap_single_shell_script(command) {
        Some(tokens) => tokens,
        None => {
            // Argv directo (sin wrapper de shell): se exige la MISMA
            // whitelist de caracteres por token que el unwrap impone al
            // script — sin ella, un argumento podría cargar
            // metacaracteres que el shell de la tool sí expandiría.
            if !command.iter().all(|t| {
                !t.is_empty()
                    && t.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '=' | '/' | '-'))
            }) {
                return false;
            }
            command.iter().map(String::as_str).collect()
        }
    };
    declared.iter().any(|prefix| {
        let prefix_tokens: Vec<&str> = prefix.split_whitespace().collect();
        !prefix_tokens.is_empty()
            && prefix_tokens.len() <= tokens.len()
            && prefix_tokens.iter().zip(&tokens).all(|(a, b)| a == b)
    })
}

/// The cargo rule itself, over an already-plain argv — see
/// [`is_benchable_cargo`] for what's allowed and why.
fn is_plain_cargo_verify(tokens: &[&str]) -> bool {
    tokens.first() == Some(&"cargo")
        && matches!(tokens.get(1), Some(&"check" | &"build" | &"test"))
        && !tokens[2..].iter().any(|arg| {
            arg.starts_with("--manifest-path")
                || arg.starts_with("--config")
                || arg.starts_with("-Z")
        })
}

#[async_trait]
impl ConfirmationPrompt for BenchPrompt {
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        let allowed = is_benchable_cargo(action)
            || is_declared_sandbox_command(action, &self.sandbox_commands);
        let key = braze_permissions::derive_permission_key(action);

        // Best-effort, same as the real prompts: a session-store hiccup
        // here must not change the decision itself.
        let _ = self
            .store
            .append(
                &self.session,
                &braze_events::AgentEvent::PermissionRequested {
                    action: action.to_string(),
                    reversible: false,
                    key: key.clone(),
                },
            )
            .await;

        let _ = self
            .store
            .append(
                &self.session,
                &braze_events::AgentEvent::PermissionDecided {
                    action: action.to_string(),
                    allowed,
                    key,
                },
            )
            .await;

        allowed
    }
}

/// Synthesizes the `.braze/memory.json` a previous session would have
/// left, for `+ablate:project-memory-seeded` — see the call site for the
/// full rationale. Deterministic by construction: touched files are the
/// task's own `setup_files` (sorted; `TaskSandbox::new` literally just
/// wrote them), the tool is the one that would have written them, and
/// the timestamp is a fixed epoch string (the field is never rendered;
/// `docs/decision-*`: no clocks in evidence paths). Tasks with no
/// `setup_files` produce an empty memory, which renders as no section —
/// those cells degenerate to the plain `project-memory` arm by design,
/// and the pre-registration counts them as null cells, not signal.
///
/// Hard error (not best-effort) on I/O failure: a silently missing seed
/// would turn the injection arm into the empty arm and invalidate the
/// experiment without anyone noticing — the same reasoning as the
/// grader's fail-fast, opposite of the production hook's own
/// best-effort posture.
fn seed_project_memory_from_setup_files(
    task: &crate::task::TaskDef,
    sandbox_root: &std::path::Path,
) -> Result<(), BenchError> {
    let mut memory = braze_memory::ProjectMemory::new(sandbox_root.display().to_string());
    let mut paths: Vec<&String> = task.setup_files.keys().collect();
    paths.sort();
    for path in paths {
        memory.record_touched_file(path.clone(), "write_file", "1785000000");
    }
    let memory_path = braze_memory::default_memory_path(sandbox_root);
    if let Some(parent) = memory_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            BenchError::Startup(format!("project-memory-seeded: mkdir {parent:?}: {e}"))
        })?;
    }
    let json = serde_json::to_string_pretty(&memory)
        .map_err(|e| BenchError::Startup(format!("project-memory-seeded: serialize: {e}")))?;
    std::fs::write(&memory_path, json).map_err(|e| {
        BenchError::Startup(format!("project-memory-seeded: write {memory_path:?}: {e}"))
    })?;
    Ok(())
}

fn join_memory_sections(project_memory: Option<&str>, task_memory: Option<&str>) -> Option<String> {
    match (project_memory, task_memory) {
        (Some(project), Some(task)) => {
            Some(format!("{project}\n\nBenchmark procedural memory:\n{task}"))
        }
        (Some(project), None) => Some(project.to_string()),
        (None, Some(task)) => Some(format!("Benchmark procedural memory:\n{task}")),
        (None, None) => None,
    }
}

/// Runs `task` against the backend `spec` builds, and returns the
/// resulting metrics. Never propagates a model/engine error up to the
/// caller — a failed run still produces a `TaskResult` (with
/// `converged: false` and `run_error: Some(..)`) so one bad task doesn't
/// abort the whole suite.
///
/// `preserve_root`: when `Some`, this run's sandbox (final workdir state)
/// and session transcript (JSONL rollout) are copied there before their
/// temp copies are deleted as usual — see `preserve.rs`'s module doc. `None`
/// (the default; `BRAZE_BENCH_KEEP_SESSIONS` unset) means zero behavior
/// change from before this parameter existed.
/// `wall_clock_budget`: presupuesto de wall-clock del TURNO
/// (round-economics). Llega por parámetro y no leído de `config` porque
/// una unidad TTC lo REPARTE entre sus rollouts — ver
/// [`run_task_ttc`]. `None` = sin presupuesto de tiempo, el
/// comportamiento histórico.
#[allow(clippy::too_many_arguments)] // ya estaba en el límite; el budget es un parámetro más del brazo
pub async fn run_task(
    spec: &BackendSpec,
    config: &Config,
    task: &TaskDef,
    repetition: u32,
    timeout: Duration,
    sampling: SamplingSpec,
    preserve_root: Option<&Path>,
    wall_clock_budget: Option<Duration>,
) -> Result<TaskResult, BenchError> {
    let sandbox = TaskSandbox::new(task)?;

    let session_dir = std::env::temp_dir().join(format!(
        "braze-bench-session-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    let store: Arc<dyn SessionStore> = Arc::new(FileSessionStore::new(session_dir.clone()));
    let session = SessionId::new();

    // E1 (docs/AUDITORIA-2026-07-v3.md): a `+ablate:` suffix on `spec`
    // overrides whichever of these knobs it names; everything else falls
    // through to `config`'s own value, same as a plain (unablated) spec.
    let ablation = spec.ablation();
    let post_edit_check_enabled =
        !config.disable_post_edit_check && !ablation.disable_post_edit_check;
    let syntactic_gate_enabled =
        !config.disable_syntactic_edit_gate && !ablation.disable_syntactic_edit_gate;
    let textual_rescue_enabled =
        !config.disable_textual_tool_call_rescue && !ablation.disable_textual_rescue;
    let tactical_window = ablation.tactical_window.unwrap_or(config.tactical_window);
    let tactical_compaction_threshold = ablation
        .tactical_compaction_threshold
        .unwrap_or(config.tactical_compaction_threshold);
    let best_of_n = ablation.best_of_n.unwrap_or(config.best_of_n);
    // round-economics: el tope de rondas es la mitad "avara vs
    // derrochadora" del factorial, así que tiene que ser por fila y no
    // solo por `Config` — si no, las dos configuraciones no caben en el
    // mismo sweep y se pierde el pareo (tarea, repetición) de McNemar.
    let max_turn_iterations = ablation
        .max_turn_iterations
        .unwrap_or(config.max_turn_iterations as usize);

    let allowlist = WorkdirAllowlist::new(sandbox.path());
    let classifier = DefaultClassifier::new(WorkdirAllowlist::new(sandbox.path()));
    let prompt = BenchPrompt {
        session,
        store: Arc::clone(&store),
        sandbox_commands: task.sandbox_commands.clone(),
    };
    let guard = PermissionGuard::new(allowlist, Box::new(classifier), Box::new(prompt));
    // `with_workdir`, not `new`: the bench binary's own process cwd is
    // wherever it happened to be launched from, not this task's sandbox —
    // using `new` (which defaults to the process cwd) would silently
    // decouple the guard's `WorkdirAllowlist` (scoped to the sandbox
    // above) from where the tools' actual I/O lands. See
    // docs/AUDITORIA-2026-07.md hallazgo F1.
    //
    // `with_post_edit_check` here (E1c, docs/AUDITORIA-2026-07-v3.md): this
    // call was previously missing entirely, so every bench run had the
    // guardrail permanently ON regardless of `Config::disable_post_edit_check`
    // — a real `braze` invocation with that flag set measured a different
    // harness than the one the bench reported on.
    let tools_provider = braze_tools_local::LocalToolsProvider::with_workdir(guard, sandbox.path())
        .with_post_edit_check(post_edit_check_enabled)
        .with_syntactic_edit_gate(syntactic_gate_enabled)
        .with_edit_strict_mode(ablation.edit_strict_mode)
        // Spill-to-file on por default; `+ablate:no-spill` lo apaga para
        // el A/B de si el truncado sin pérdida ayuda de verdad.
        .with_tool_output_spill(!ablation.disable_tool_output_spill);
    // C′.1: el fixture del A/B de search_tools — un catálogo sintético
    // de ruido junto a las tools locales reales, solo cuando la tarea lo
    // pide (`noise_tools > 0`); las suites existentes no cambian.
    let mut providers: Vec<Box<dyn braze_tools_core::ToolProvider>> =
        vec![Box::new(tools_provider)];
    if task.noise_tools > 0 {
        providers.push(Box::new(crate::noise::NoiseToolsProvider::new(
            task.noise_tools,
        )));
    }
    // Ancla BFCL: tools objetivo declaradas por la tarea, con schema real
    // y resultado enlatado (docs/bfcl-anchor-design-2026-07-18.md).
    if !task.synthetic_tools.is_empty() {
        providers.push(Box::new(crate::synthetic::SyntheticToolsProvider::new(
            task.synthetic_tools.clone(),
        )));
    }
    let tools = braze_tools_core::ToolRegistry::new(providers);

    let model = spec.build_agent_model(config, sampling)?;
    // N-36 (docs/AUDITORIA-2026-07-v2.md): the exact same anti-loop system
    // prompt `braze chat`/`braze run` build by default — a bare one-line
    // prompt with no tool-use guidance measured a different (worse)
    // system than the one users actually run. D1
    // (docs/AUDITORIA-2026-07-v3.md): mirrors production's model-family
    // hint. I-4 (docs/AUDITORIA-2026-07-v6.md): the hint is name-based
    // now — every executor passes its model name and
    // `ModelFamily::from_model_name` decides (Generic = no hint), same
    // ungating braze-cli got; an OpenRouter-served GLM/Qwen needs its
    // native-template hint exactly like an Ollama-served one.
    let model_hint = Some(spec.executor_model_name(config));
    // docs/project-memory-design.md: `+ablate:project-memory` mide que el
    // mecanismo del hook dispara dentro de un turno — un sandbox de
    // braze-bench es fresco por repetición (`TaskSandbox::new` más
    // arriba), así que nunca hay memoria de una sesión previa para
    // cargar acá; el hook igual se construye y registra de verdad
    // (`.braze/memory.json` dentro del sandbox), fiel al wiring de
    // producción, para que el mecanismo sea verificable aunque su valor
    // cross-sesión necesite el suite multi-turno que el roadmap v7 ya
    // anota como pendiente.
    // `+ablate:project-memory-seeded` (pre-registro:
    // docs/hypothesis-2026-08-04-project-memory-ab.md): sintetiza la
    // memoria que una sesión previa habría dejado, ANTES de construir el
    // hook (que carga en el constructor). Contenido derivado
    // determinísticamente de los `setup_files` de la tarea — cero texto
    // escrito por el experimentador — y `project_key` = el sandbox real,
    // así que K-7 queda intacto: nada fuera del bench puede fabricar una
    // memoria que pase la validación.
    if ablation.seed_project_memory {
        seed_project_memory_from_setup_files(task, sandbox.path())?;
    }
    let project_memory_hook = if ablation.enable_project_memory {
        let memory_path = braze_memory::default_memory_path(sandbox.path());
        let store: std::sync::Arc<dyn braze_memory::ProjectMemoryStore> =
            std::sync::Arc::new(braze_memory::FileProjectMemoryStore::new(memory_path));
        let project_key = sandbox.path().display().to_string();
        Some(std::sync::Arc::new(
            braze_engine::ProjectMemoryHook::new(store, project_key).await,
        ))
    } else {
        None
    };
    let project_memory_snapshot: Option<String> = project_memory_hook.as_ref().and_then(|hook| {
        braze_memory::render_project_memory_section(
            &hook.snapshot(),
            braze_memory::DEFAULT_PROJECT_MEMORY_BUDGET_TOKENS,
        )
    });
    let task_memory = crate::memory::render_task_memory(task)?;
    let combined_memory_snapshot = join_memory_sections(
        project_memory_snapshot.as_deref(),
        task_memory.as_ref().map(|memory| memory.section.as_str()),
    );

    // No references (opencode-10): the bench sandbox is hermetic by
    // design — a user's reference dirs leaking into the measured system
    // prompt would make pass rates depend on local config.
    // Sin environment block (E′ I.6): el sandbox no es un repo git y el
    // bench mide el prompt default de producción (environment_block es
    // off por default — si algún día se promueve, N-36 exige seguirlo).
    // Sin AGENTS.md, mismo argumento que las reference dirs de arriba:
    // el bench mide el prompt default de producción, y un context file
    // del sandbox sería contenido de la tarea filtrado al prompt.
    let system_prompt = braze_config::default_system_prompt(
        sandbox.path(),
        model_hint.as_deref(),
        &[],
        None,
        combined_memory_snapshot.as_deref(),
        None,
    );

    // N-36: mirrors `braze-cli::main.rs`'s own Ollama-only context budget
    // — without it, a bench pass rate for an Ollama backend measured a
    // context-management regime production never actually uses. Keyed on
    // the *executor* being Ollama (`ollama_models` also reports a local
    // planner, but the budget protects the executor's context window —
    // production keys it on `default_backend` the same way). Computed
    // here, before `tools`/`system_prompt` move into `Engine::new` below
    // (hallazgo B4, docs/AUDITORIA-2026-07-v3.md: the margin needs the
    // real system prompt length plus the size of every advertised tool
    // stub, not a fixed constant).
    // C′.1: umbral de deferral de tools — ablation > config. Computed
    // up here because the context budget below must see the same value
    // the engine will run with.
    let tool_search_threshold = ablation
        .tool_search_threshold
        .unwrap_or(config.tool_search_threshold);
    let ollama_budget = if spec.executor_is_ollama() {
        // J-17 (docs/AUDITORIA-2026-07-v7.md): measure the stubs the
        // model actually SEES after deferral (visible providers +
        // `search_tools` meta-stub), not the full pre-deferral catalog —
        // with `noise_tools` in play, budgeting on the whole catalog
        // shrank the budget exactly for the deferral arm of the A/B,
        // making it compact/collapse earlier than its real prompt
        // required. Slightly conservative in the other direction once
        // the model activates hidden tools mid-task (activated stubs
        // join the prompt without re-budgeting), which is the safe side.
        let visible_stubs = braze_engine::initially_visible_stubs(
            tools.all_stubs_lossy().await,
            tool_search_threshold,
        );
        let tool_definitions_bytes = braze_tools_core::tool_stub_definition_bytes(&visible_stubs);
        Some(braze_config::ollama_context_budget_tokens(
            config.ollama_num_ctx,
            config.max_tokens,
            &system_prompt,
            tool_definitions_bytes,
        ))
    } else {
        None
    };

    // C10 (docs/AUDITORIA-2026-07.md): mirrors braze-cli's wiring, so a
    // bench run measures the same tactical window/threshold/best_of_n
    // behavior a real `braze` invocation with this config would use —
    // modulo whatever `ablation` overrides (E1).
    let mut engine = braze_engine::Engine::new(
        model,
        tools,
        Arc::clone(&store),
        Box::new(SimpleContextCompactor::new(tactical_window)),
        Box::new(braze_events::ChannelTaskNotifier::new()),
        system_prompt,
        config.max_tokens,
    )
    .with_tactical_compaction_threshold(tactical_compaction_threshold)
    .with_best_of_n(best_of_n)
    .with_textual_rescue_enabled(textual_rescue_enabled)
    // E1 + opencode ítem 2 (docs/AUDITORIA-2026-07-v6.md § roadmap
    // Paquete 1): the two levers the paper's ablation matrix couldn't
    // turn off before — the ACI collapse (`+ablate:no-prune`) and
    // tactical compaction (`+ablate:no-compaction`).
    .with_observation_collapse_enabled(!ablation.disable_observation_collapse)
    .with_compaction_enabled(!ablation.disable_compaction)
    // A′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.2):
    // the announced-deadline notes, ablatable so their effect on
    // TurnBudgetExhausted/iteration-cap aborts is measurable.
    .with_harness_notes_enabled(!ablation.disable_harness_notes)
    // C′.1: umbral de deferral de tools — ablation > config (computed
    // above, shared with the context budget).
    .with_tool_search_threshold(tool_search_threshold)
    // C′.2: la fila puede prender la task list aunque config la tenga
    // off (default) — el brazo planner→tasks del A/B pre-registrado.
    .with_task_list_enabled(config.enable_task_list || ablation.enable_task_list)
    // Gate de evidencia para cerrar tareas (checkers de Recuris). Sin la
    // lista encendida no tiene efecto — ver la doc de la clave.
    .with_task_evidence_required(config.enable_task_evidence || ablation.enable_task_evidence)
    .with_call_time_skills(config.enable_call_time_skills || ablation.enable_call_time_skills)
    .with_exploration_enabled(config.enable_exploration || ablation.enable_exploration)
    .with_editor_enabled(config.enable_editor || ablation.enable_editor)
    // A/B del impuesto JSON (docs/hypothesis-2026-08-10-json-tax-edit-fence.md):
    // el brazo `+ablate:edit-fence` — la edición como SEARCH/REPLACE
    // textual en vez de tool-arg JSON. Off por default, igual que en
    // todos los composition roots de producción.
    .with_edit_fence_enabled(config.enable_edit_fence || ablation.enable_edit_fence)
    // A/B constrained decoding (docs/constrained-decoding-ab-design.md):
    // el canal de vuelta de los brazos `+ablate:prompt-tools`/
    // `constrained-tools` — el envelope se parsea como canal primario
    // (NO cuenta como rescue; la verificación del mecanismo es
    // `rescues ≈ 0` en el brazo C). Off por default, igual que en todos
    // los composition roots de producción.
    .with_envelope_parsing_enabled(ablation.prompt_tools_active())
    // B′ (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte
    // II): audit-only — logs the prompt-budget breakdown per request
    // under `RUST_LOG=braze_engine=info`, the same channel the other
    // lever activations already use. Zero effect on results by
    // construction (read-only hook).
    .with_hook(std::sync::Arc::new(braze_engine::PromptBudgetAuditHook))
    // C10: mirrors braze-cli's wiring. `max_turn_iterations`/
    // `planner_max_tokens` were a pre-existing mirror gap (wired in
    // production since opencode ítem 1 but never here — a bench run
    // measured the hardcoded defaults regardless of config);
    // `max_turn_total_tokens` is the v4 P0.2 breaker, new in Paquete 3.
    .with_max_turn_iterations(max_turn_iterations)
    .with_planner_max_tokens(config.planner_max_tokens)
    .with_max_turn_total_tokens(config.max_turn_total_tokens)
    // round-economics: el presupuesto de wall-clock del turno
    // (`--turn-wall-clock-secs` / `BRAZE_MAX_TURN_WALL_CLOCK_SECS`). Es
    // la unidad experimental de esa línea, y NO reemplaza al `timeout`
    // de abajo: aquel sigue de backstop de infraestructura, este corta
    // en el borde de ronda con la contabilidad intacta.
    .with_max_turn_wall_clock(wall_clock_budget)
    // Deadline de streaming por ronda (`--round-wall-clock-secs`): acota
    // la ronda desbocada que el corte en borde de ronda no puede, con
    // las rondas completadas del turno intactas. Viaja por `Config` como
    // el resto de los knobs del engine.
    .with_max_round_wall_clock(
        config
            .max_round_wall_clock_secs
            .map(std::time::Duration::from_secs),
    );

    if let Some(full_observations) = ablation.tactical_full_observations {
        engine = engine.with_tactical_full_observations(full_observations);
    }

    // Verification gate (H2, docs/verification-lever-design-2026-07-22.md):
    // the treatment arm (`+ablate:verify-gate`) runs `cargo check` at
    // turn end on tasks that declare `expect_cargo_check` (the same
    // command the grader uses post-run), so the model gets a round to fix
    // what it claimed was done. Tasks without a cargo check have nothing
    // to verify — the gate is a no-op there even in the treatment arm.
    if let Some(max_rounds) = ablation.verify_gate
        && task.expect_cargo_check
    {
        engine = engine.with_verification(braze_engine::VerificationConfig {
            command: vec!["cargo".to_string(), "check".to_string()],
            timeout: std::time::Duration::from_secs(300),
            max_rounds,
            working_dir: Some(sandbox.path().to_path_buf()),
        });
    }

    if let Some(budget) = ollama_budget {
        engine = engine.with_context_budget(budget);
    }

    // PLAN.md § "Split planificador/ejecutor", oleada 4: a spec with a
    // `+plan:` suffix runs the same engine with the planner attached —
    // baseline and planned variant differ in exactly one thing.
    // `+ablate:no-planner` (E1) runs the row WITHOUT attaching it while
    // keeping the `+plan:` display identity, so the pair lines up in the
    // report as the exact same config minus one lever.
    if !ablation.disable_planner
        && let Some(planner) = spec.build_planner(config, sampling)?
    {
        engine = engine.with_planner(planner);
    }

    // v8 § 6 — summary-por-lead: segunda instancia del backend del
    // `+lead:` de esta fila como summarizer de compactación. Enabling
    // key (`+ablate:lead-summary`); sin `+lead:` en la fila,
    // `build_lead` es None y la key no tiene efecto.
    if ablation.enable_lead_summary
        && let Some(summarizer) = spec.build_lead(config, sampling)?
    {
        engine = engine.with_compaction_summarizer(summarizer);
    }

    // docs/project-memory-design.md: registrado como hook audit-only,
    // mismo patrón que `PromptBudgetAuditHook` arriba. Se conserva el
    // handle para el flush post-turno (v8 K-8).
    if let Some(hook) = &project_memory_hook {
        engine = engine.with_hook(hook.clone());
    }

    // SC-retention (docs/hypothesis-2026-08-13-sc-retention.md): el
    // constraint declarado por la tarea se antepone VERBATIM al prompt
    // en AMBOS brazos (una sola fuente — el modelo ve exactamente el
    // mismo texto brazo a brazo), y solo el brazo con la ruta activa
    // (sin `+ablate:no-sc-route`) lo declara además al engine, que es
    // la única diferencia que el A/B mide.
    let prompt = match &task.session_constraint {
        Some(constraint) => format!("{constraint}\n\n{}", task.prompt),
        None => task.prompt.clone(),
    };
    if let Some(constraint) = &task.session_constraint
        && !ablation.disable_sc_route
    {
        engine = engine.with_session_constraints(vec![constraint.clone()]);
    }

    let started = Instant::now();
    let run_outcome = match tokio::time::timeout(
        timeout,
        engine.run_turn(&session, &prompt, &mut braze_events::NoopObserver),
    )
    .await
    {
        Ok(Ok(())) => RunOutcome::Converged,
        Ok(Err(err)) => RunOutcome::Failed(err),
        // The elapsed-timer future itself carries no useful information
        // (just "it didn't finish in time") — the interesting bit is
        // already captured by `RunOutcome::TimedOut`.
        Err(_elapsed) => RunOutcome::TimedOut,
    };
    let wall_time = started.elapsed();

    // v8 K-8: los saves de la memoria van a una task en background — la
    // PRÓXIMA tarea construye un hook fresco que carga del store, así
    // que hay que drenar los saves de esta antes de seguir, o la
    // condición "memory" del bench leería estado desactualizado.
    // Después de `wall_time`: el drenaje es bookkeeping del harness, no
    // parte de la medición.
    if let Some(hook) = &project_memory_hook {
        hook.flush().await;
    }

    let events = match store.load(&session).await {
        Ok(events) => events,
        // A session that never got a single event persisted (e.g. the
        // model call failed before anything was appended) — treat as
        // an empty log rather than a harness-level failure.
        Err(braze_session::SessionError::NotFound(_)) => Vec::new(),
        Err(err) => return Err(err.into()),
    };

    let display_name = spec.display_name(config);

    // Opt-in transcript preservation (`BRAZE_BENCH_KEEP_SESSIONS`,
    // `preserve.rs`) — copy BEFORE the usual deletion below, so the default
    // (no env var set, `preserve_root: None`) is byte-for-byte the old
    // behavior. Best-effort: a copy failure is logged, never fails the run
    // itself — preservation is diagnostics, not part of the measurement.
    if let Some(root) = preserve_root {
        let dest = preserve::preserved_run_dir(root, &display_name, &task.id, repetition);
        if let Err(err) = preserve::copy_dir_recursive(&session_dir, &dest.join("session")) {
            eprintln!(
                "braze-bench: no se pudo preservar la sesión de '{}' :: '{display_name}' (rep {repetition}): {err}",
                task.id
            );
        }
        if let Err(err) = preserve::copy_dir_recursive(sandbox.path(), &dest.join("sandbox")) {
            eprintln!(
                "braze-bench: no se pudo preservar el sandbox de '{}' :: '{display_name}' (rep {repetition}): {err}",
                task.id
            );
        }
    }

    let _ = tokio::fs::remove_dir_all(&session_dir).await;

    // Checked before the sandbox drops at the end of this function (its
    // `Drop` removes the directory) — this is what makes a write/edit
    // task's pass/fail track the real filesystem outcome instead of only
    // "was some tool called" (see docs/AUDITORIA-2026-07.md hallazgo F4).
    let expected_files_found = if task.expect_file_contains.is_empty() {
        None
    } else {
        Some(
            task.expect_file_contains
                .iter()
                .all(|(relative_path, expected_substrings)| {
                    let Ok(contents) = std::fs::read_to_string(sandbox.path().join(relative_path))
                    else {
                        return false;
                    };
                    // Every expected substring must match as a bounded
                    // token — one miss fails the whole file, matching
                    // the AND semantics the field's doc comment pins.
                    expected_substrings.iter().all(|needle| {
                        crate::metrics::contains_as_a_bounded_token(&contents, needle)
                    })
                }),
        )
    };

    // v8 K-9: semantic grading — `cargo check` in the sandbox, after the
    // run, only when the task declares it. Like `expected_files_found`
    // above, this must happen before the sandbox's `Drop` removes the
    // directory. The engine's own post-edit guardrail already ran cargo
    // during the turn, so the target dir is warm and this is cheap.
    let expected_cargo_check_passed = if task.expect_cargo_check {
        Some(run_cargo_check_in_sandbox(sandbox.path()).await)
    } else {
        None
    };

    // v8 § 6.15 (TTC): huella del artefacto de archivos — contenido de
    // los paths de expect_file_contains en orden estable, con marca para
    // archivos faltantes. También antes del Drop del sandbox.
    let file_outcome_fingerprint = if task.expect_file_contains.is_empty() {
        None
    } else {
        let mut paths: Vec<&String> = task.expect_file_contains.keys().collect();
        paths.sort();
        let mut buffer = Vec::new();
        for path in paths {
            buffer.extend_from_slice(path.as_bytes());
            buffer.push(0);
            match std::fs::read(sandbox.path().join(path)) {
                Ok(bytes) => buffer.extend_from_slice(&bytes),
                Err(_) => buffer.extend_from_slice(b"<missing>"),
            }
            buffer.push(0);
        }
        Some(crate::metrics::fnv1a_hex(&buffer))
    };

    Ok(compute_metrics(
        &display_name,
        task,
        repetition,
        &events,
        wall_time,
        run_outcome,
        expected_files_found,
        expected_cargo_check_passed,
        file_outcome_fingerprint,
        MemoryRunMetrics {
            memory_tokens: task_memory
                .as_ref()
                .map(|memory| memory.tokens_estimate)
                .unwrap_or(0),
        },
        // Paquete 3: `None` when the spec's models aren't priced (or a
        // composite bills at mixed rates) — the row reports no cost
        // estimate rather than a guessed one.
        spec.resolve_pricing(config),
    ))
}

/// v8 § 6.15 — TTC local (`+ablate:ttc=N`): corre `rollouts` rollouts
/// COMPLETOS e independientes de la tarea (sandbox y sesión frescos
/// cada uno, seeds decorrelacionados) y devuelve UNA fila: el ganador
/// de [`select_ttc_winner`], con los campos de costo sumados sobre
/// todos los rollouts. Secuencial a propósito: el nodo de inferencia
/// del proyecto (Nitro) es una sola máquina CPU — paralelizar rollouts
/// ahí contamina la latencia por contención, el modo de falla
/// documentado en CLAUDE.md.
///
/// Un rollout que falla a nivel harness no mata la unidad TTC (los
/// demás siguen); solo si TODOS fallan se propaga el último error.
/// `wall_clock_budget` (round-economics) es del TASK, no del rollout:
/// la unidad experimental de esa línea es "una tarea bajo un presupuesto
/// fijo de tiempo", así que N rollouts tienen que caber DENTRO del
/// mismo presupuesto. Darle el presupuesto entero a cada rollout — que
/// es lo que salía gratis de reusar `run_task` tal cual — le regalaba N×
/// el tiempo al brazo derrochador contra el avaro, o sea confundía el
/// tratamiento con el recurso que el experimento mantiene fijo.
///
/// El reparto es un DEADLINE de tarea, no una división en N partes
/// iguales: cada rollout recibe lo que quede, y los rollouts que ya no
/// entran simplemente no corren. Eso es lo que de verdad hace un harness
/// derrochador contra un reloj — y que quepan más rollouts cuando la
/// ronda se abarata es el mecanismo mismo que la hipótesis predice.
#[allow(clippy::too_many_arguments)] // espejo de `run_task` (ya en el límite) + rollouts
pub async fn run_task_ttc(
    spec: &BackendSpec,
    config: &Config,
    task: &TaskDef,
    repetition: u32,
    timeout: Duration,
    sampling: SamplingSpec,
    preserve_root: Option<&Path>,
    rollouts: u32,
    wall_clock_budget: Option<Duration>,
) -> Result<TaskResult, BenchError> {
    let mut candidates = Vec::new();
    let mut last_error = None;
    let unit_started = Instant::now();
    for rollout in 0..rollouts {
        // Presupuesto restante de la unidad. Sin presupuesto (`None`) la
        // rama entera es un no-op y los N rollouts corren como siempre.
        let rollout_budget = match wall_clock_budget {
            None => None,
            Some(total) => {
                let Some(remaining) = total.checked_sub(unit_started.elapsed()) else {
                    eprintln!(
                        "braze-bench: TTC '{}' rollout {}/{rollouts}: presupuesto de wall-clock \
                         de la unidad agotado — los rollouts restantes no corren",
                        task.id,
                        rollout + 1
                    );
                    break;
                };
                Some(remaining)
            }
        };
        // Auto-consistencia REQUIERE variación entre rollouts: mismo
        // seed daría n copias idénticas y el voto sería un no-op. El
        // offset primo grande no colisiona con el offset por repetición
        // (+1 por rep) en ningún sweep realista.
        let mut rollout_sampling = sampling;
        rollout_sampling.seed = sampling
            .seed
            .map(|s| s.wrapping_add(u64::from(rollout).wrapping_mul(1_000_003)));
        match run_task(
            spec,
            config,
            task,
            repetition,
            timeout,
            rollout_sampling,
            preserve_root,
            rollout_budget,
        )
        .await
        {
            Ok(result) => candidates.push(result),
            Err(err) => {
                eprintln!(
                    "braze-bench: rollout TTC {}/{rollouts} de '{}' falló a nivel harness: {err}",
                    rollout + 1,
                    task.id
                );
                last_error = Some(err);
            }
        }
    }
    match (candidates.is_empty(), last_error) {
        (true, Some(err)) => Err(err),
        (true, None) => unreachable!("rollouts >= 1 garantiza candidato o error"),
        (false, _) => Ok(select_ttc_winner(candidates, rollouts)),
    }
}

/// La selección TTC — auto-consistencia sobre el ARTEFACTO, jamás sobre
/// el veredicto: el selector solo lee `converged`, la huella del
/// resultado (`outcome_fingerprint`), los fallos de ejecución y el
/// costo. Orden: (1) convergió; (2) su huella tiene la mayoría entre
/// los convergidos (self-consistency: si 2 de 3 rollouts produjeron el
/// mismo archivo final, ese resultado gana); (3) menos
/// `tool_execution_failures`; (4) menos tokens; (5) el más temprano.
/// El ganador sale con `ttc_rollouts = Some(n)` y el costo agregado de
/// TODOS los rollouts — la fila nunca esconde lo que costó la técnica.
fn select_ttc_winner(mut candidates: Vec<TaskResult>, rollouts: u32) -> TaskResult {
    let mut fingerprint_votes: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for candidate in candidates.iter().filter(|c| c.converged) {
        if let Some(fingerprint) = &candidate.outcome_fingerprint {
            *fingerprint_votes.entry(fingerprint.clone()).or_insert(0) += 1;
        }
    }

    let mut best_index = 0;
    let mut best_key = (false, 0u32, i64::MIN, i64::MIN);
    for (index, candidate) in candidates.iter().enumerate() {
        let votes = candidate
            .outcome_fingerprint
            .as_ref()
            .filter(|_| candidate.converged)
            .and_then(|f| fingerprint_votes.get(f))
            .copied()
            .unwrap_or(0);
        let key = (
            candidate.converged,
            votes,
            -i64::from(candidate.tool_execution_failures),
            -(i64::from(candidate.input_tokens) + i64::from(candidate.output_tokens)),
        );
        // Estrictamente mayor: en empate total gana el más temprano.
        if key > best_key {
            best_key = key;
            best_index = index;
        }
    }

    let total_wall: u128 = candidates.iter().map(|c| c.wall_time_ms).sum();
    let total_input = candidates
        .iter()
        .fold(0u32, |acc, c| acc.saturating_add(c.input_tokens));
    let total_output = candidates
        .iter()
        .fold(0u32, |acc, c| acc.saturating_add(c.output_tokens));
    let total_cost = {
        let costs: Vec<f64> = candidates
            .iter()
            .filter_map(|c| c.estimated_cost_usd)
            .collect();
        (!costs.is_empty()).then(|| costs.iter().sum())
    };

    let mut winner = candidates.swap_remove(best_index);
    winner.ttc_rollouts = Some(rollouts);
    winner.wall_time_ms = total_wall;
    winner.input_tokens = total_input;
    winner.output_tokens = total_output;
    winner.estimated_cost_usd = total_cost;
    winner
}

/// Ceiling for the post-run `cargo check` (v8 K-9). The sandbox projects
/// are dependency-free single-file libs and the engine's post-edit
/// guardrail already warmed the target dir during the turn, so a healthy
/// check takes ~1s; two minutes means something is genuinely wedged.
const CARGO_CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// `cargo check` in `dir`, `true` iff it exits 0 (v8 K-9's semantic
/// grade). A missing `cargo` binary, an execution error, or a timeout
/// all grade `false` with a stderr note — a declared `expect_cargo_check`
/// must never silently pass because the checker itself couldn't run.
async fn run_cargo_check_in_sandbox(dir: &std::path::Path) -> bool {
    let check = tokio::time::timeout(
        CARGO_CHECK_TIMEOUT,
        tokio::process::Command::new("cargo")
            .arg("check")
            .arg("--quiet")
            .current_dir(dir)
            .output(),
    )
    .await;
    match check {
        Ok(Ok(output)) => output.status.success(),
        Ok(Err(err)) => {
            eprintln!(
                "braze-bench: no se pudo ejecutar 'cargo check' de grading en {}: {err}",
                dir.display()
            );
            false
        }
        Err(_elapsed) => {
            eprintln!(
                "braze-bench: 'cargo check' de grading excedió {}s en {}",
                CARGO_CHECK_TIMEOUT.as_secs(),
                dir.display()
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v8 § 6.15 — el selector TTC: auto-consistencia sobre el artefacto.
    /// La mayoría de huella entre convergidos gana aunque el solitario
    /// tenga menos tokens; el costo del ganador es la SUMA de todos.
    #[test]
    fn ttc_winner_is_the_fingerprint_majority_with_summed_costs() {
        fn candidate(
            fingerprint: &str,
            converged: bool,
            tokens: u32,
        ) -> crate::metrics::TaskResult {
            let mut r = crate::metrics::harness_error_result(
                "ollama:x",
                &crate::task::TaskDef {
                    session_constraint: None,
                    id: "t".to_string(),
                    prompt: "p".to_string(),
                    setup_files: Default::default(),
                    expect_tool_call: None,
            accept_tool_calls: Vec::new(),
                    expect_no_tool_call: false,
                    expect_text_contains: None,
                    expect_file_contains: Default::default(),
                    expect_cargo_check: false,
            sandbox_commands: Vec::new(),
                    skill: None,
                    expect_max_rounds: None,
                    expect_max_tokens: None,
                    expect_max_cost_usd: None,
                    noise_tools: 0,
                    synthetic_tools: Vec::new(),
                    memory_condition: None,
                    memory_file: None,
                    memory_budget_tokens: None,
                },
                0,
                &crate::error::BenchError::Startup("plantilla".to_string()),
            );
            r.failure_cause = None;
            r.converged = converged;
            r.outcome_fingerprint = Some(fingerprint.to_string());
            r.input_tokens = tokens;
            r.output_tokens = 0;
            r.wall_time_ms = 100;
            r.estimated_cost_usd = Some(0.01);
            r
        }

        // Dos rollouts coinciden en el artefacto "aaa"; el tercero
        // produjo "bbb" más barato. Gana la mayoría.
        let winner = select_ttc_winner(
            vec![
                candidate("aaa", true, 500),
                candidate("bbb", true, 10),
                candidate("aaa", true, 600),
            ],
            3,
        );
        assert_eq!(winner.outcome_fingerprint.as_deref(), Some("aaa"));
        assert_eq!(winner.ttc_rollouts, Some(3));
        assert_eq!(winner.input_tokens, 1110, "tokens sumados sobre los 3");
        assert_eq!(winner.wall_time_ms, 300, "walltime sumado");
        assert!((winner.estimated_cost_usd.unwrap() - 0.03).abs() < 1e-9);

        // Un no-convergido jamás le gana a un convergido, tenga la
        // huella que tenga.
        let winner = select_ttc_winner(
            vec![candidate("ccc", false, 10), candidate("ddd", true, 900)],
            2,
        );
        assert_eq!(winner.outcome_fingerprint.as_deref(), Some("ddd"));
        assert!(winner.converged);
    }

    /// Pre-registro project-memory A/B: el ciclo completo del brazo
    /// seeded — sintetizar desde `setup_files` → el hook CARGA el seed
    /// (o sea pasa la validación K-7, porque el `project_key` es el
    /// sandbox real) → el render produce una sección no vacía con las
    /// rutas. Si cualquiera de esos eslabones se rompe, el brazo
    /// degenera en el brazo vacío y el experimento mide nada — este
    /// test es el que lo delataría.
    #[tokio::test]
    async fn seeded_project_memory_survives_k7_and_renders() {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-seed-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let mut setup_files = std::collections::HashMap::new();
        setup_files.insert("src/lib.rs".to_string(), "pub fn f() {}".to_string());
        setup_files.insert("notas.txt".to_string(), "hola".to_string());
        let task = crate::task::TaskDef {
            session_constraint: None,
            id: "seed-test".to_string(),
            prompt: "p".to_string(),
            setup_files,
            expect_tool_call: None,
            accept_tool_calls: Vec::new(),
            expect_no_tool_call: false,
            expect_text_contains: None,
            expect_file_contains: Default::default(),
            expect_cargo_check: false,
            sandbox_commands: Vec::new(),
            skill: None,
            expect_max_rounds: None,
            expect_max_tokens: None,
            expect_max_cost_usd: None,
            noise_tools: 0,
            synthetic_tools: Vec::new(),
            memory_condition: None,
            memory_file: None,
            memory_budget_tokens: None,
        };

        seed_project_memory_from_setup_files(&task, &dir).expect("seed must write");

        // El mismo wiring que el runner usa: si K-7 descartara el seed,
        // el snapshot saldría vacío y el render None.
        let store: std::sync::Arc<dyn braze_memory::ProjectMemoryStore> = std::sync::Arc::new(
            braze_memory::FileProjectMemoryStore::new(braze_memory::default_memory_path(&dir)),
        );
        let hook = braze_engine::ProjectMemoryHook::new(store, dir.display().to_string()).await;
        let section = braze_memory::render_project_memory_section(
            &hook.snapshot(),
            braze_memory::DEFAULT_PROJECT_MEMORY_BUDGET_TOKENS,
        )
        .expect("seeded memory must render a non-empty section");
        assert!(section.contains("src/lib.rs"), "got: {section}");
        assert!(section.contains("notas.txt"), "got: {section}");

        // Y el contrafactual K-7: un hook con OTRA raíz descarta el seed.
        let other_store: std::sync::Arc<dyn braze_memory::ProjectMemoryStore> = std::sync::Arc::new(
            braze_memory::FileProjectMemoryStore::new(braze_memory::default_memory_path(&dir)),
        );
        let foreign =
            braze_engine::ProjectMemoryHook::new(other_store, "/otra/raiz".to_string()).await;
        assert!(
            braze_memory::render_project_memory_section(
                &foreign.snapshot(),
                braze_memory::DEFAULT_PROJECT_MEMORY_BUDGET_TOKENS,
            )
            .is_none(),
            "K-7 must discard a seed whose project_key doesn't match"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// v8 K-9, end-to-end con cargo REAL: el grading semántico devuelve
    /// `false` para el setup buggy de la suite memory-distillation
    /// (E0382 use-of-moved-value) y `true` para su fix canónico — lo que
    /// de paso prueba que el fixture de K-10 compila de verdad, no solo
    /// que matchea needles.
    #[tokio::test]
    async fn cargo_check_grading_discriminates_buggy_from_fixed() {
        const CARGO_TOML: &str = "[package]\nname = \"move_pilot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n";
        const BUGGY: &str = "pub struct Batch {\n    items: Vec<String>,\n}\n\nimpl Batch {\n    pub fn new(items: Vec<String>) -> Self {\n        Self { items }\n    }\n\n    pub fn total_chars(&self) -> usize {\n        self.items.iter().map(|s| s.len()).sum()\n    }\n\n    pub fn consume_and_count(self) -> (usize, usize) {\n        let total = self.total_chars();\n        let mut owned_items = self.items;\n        owned_items.sort();\n        (total, self.items.len())\n    }\n}\n";
        const FIXED: &str = "pub struct Batch {\n    items: Vec<String>,\n}\n\nimpl Batch {\n    pub fn new(items: Vec<String>) -> Self {\n        Self { items }\n    }\n\n    pub fn total_chars(&self) -> usize {\n        self.items.iter().map(|s| s.len()).sum()\n    }\n\n    pub fn consume_and_count(self) -> (usize, usize) {\n        let total = self.total_chars();\n        let mut owned_items = self.items;\n        owned_items.sort();\n        (total, owned_items.len())\n    }\n}\n";

        let dir = std::env::temp_dir().join(format!(
            "braze-bench-cargo-grading-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        tokio::fs::create_dir_all(dir.join("src")).await.unwrap();
        tokio::fs::write(dir.join("Cargo.toml"), CARGO_TOML)
            .await
            .unwrap();

        tokio::fs::write(dir.join("src/lib.rs"), BUGGY)
            .await
            .unwrap();
        assert!(
            !run_cargo_check_in_sandbox(&dir).await,
            "el setup buggy (E0382) debe graduar false"
        );

        tokio::fs::write(dir.join("src/lib.rs"), FIXED)
            .await
            .unwrap();
        assert!(
            run_cargo_check_in_sandbox(&dir).await,
            "el fix canónico debe graduar true"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
    use braze_events::AgentEvent;
    use braze_permissions::ActionDescriptor;

    fn temp_store() -> (Arc<dyn SessionStore>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-runner-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        (Arc::new(FileSessionStore::new(dir.clone())), dir)
    }

    /// Regression test for N-35 (docs/AUDITORIA-2026-07-v2.md):
    /// `BenchPrompt` must persist the same `PermissionRequested`/
    /// `PermissionDecided` pair the real confirmation prompts do —
    /// otherwise a bench run's denials never show up in the session log,
    /// and `metrics::compute_metrics`'s `permission_denials` count (which
    /// scans exactly those events) is stuck at 0 no matter how many
    /// actions actually got refused.
    #[tokio::test]
    async fn bench_prompt_persists_the_denial_before_returning_false() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let prompt = BenchPrompt {
            session,
            store: Arc::clone(&store),
            sandbox_commands: Vec::new(),
        };

        let action = ActionDescriptor::DeleteFile {
            path: std::path::PathBuf::from("/tmp/x"),
        };
        let allowed = prompt.confirm(&action).await;
        assert!(!allowed);

        let events = store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::PermissionRequested { .. }));
        match &events[1] {
            AgentEvent::PermissionDecided { allowed, .. } => assert!(!allowed),
            other => panic!("expected PermissionDecided, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The cargo carve-out: `cargo check`/`build`/`test` are approved
    /// (and the approval is persisted, so the session log tells the
    /// truth), while everything else — including the config-injection
    /// flags that turn cargo into an exec primitive, and `cargo run` —
    /// stays denied.
    #[tokio::test]
    async fn bench_prompt_approves_plain_cargo_verify_subcommands_and_persists_the_approval() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let prompt = BenchPrompt {
            session,
            store: Arc::clone(&store),
            sandbox_commands: Vec::new(),
        };

        let action = ActionDescriptor::ShellCommand {
            command: vec!["cargo".to_string(), "check".to_string()],
        };
        assert!(prompt.confirm(&action).await);

        let events = store.load(&session).await.expect("load events");
        match &events[1] {
            AgentEvent::PermissionDecided { allowed, .. } => assert!(allowed),
            other => panic!("expected PermissionDecided, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El carve-out declarado por la suite (experto-por-motor,
    /// 2026-08-14): el prefijo de motor matchea en las dos formas de
    /// envío (argv directo y `bash -lc`), los argumentos posteriores
    /// quedan cubiertos por la whitelist de caracteres, y NINGÚN
    /// metacaracter pasa aunque el prefijo coincida — el piloto
    /// pizzeria midió 12-25 denegaciones por brazo sin esto (fricción
    /// del harness, no capacidad; misma clase que el carve-out cargo).
    #[test]
    fn declared_sandbox_command_matches_plain_forms_and_rejects_metacharacters() {
        let declared = vec!["python3 pizzeria.py".to_string()];
        let shell = |parts: &[&str]| ActionDescriptor::ShellCommand {
            command: parts.iter().map(|s| s.to_string()).collect(),
        };

        // Argv directo y bash -lc, con y sin argumentos extra.
        assert!(is_declared_sandbox_command(
            &shell(&["python3", "pizzeria.py", "menu"]),
            &declared
        ));
        assert!(is_declared_sandbox_command(
            &shell(&["bash", "-lc", "python3 pizzeria.py agregar napolitana familiar"]),
            &declared
        ));

        // Prefijo distinto, denegado.
        assert!(!is_declared_sandbox_command(
            &shell(&["python3", "otro.py", "menu"]),
            &declared
        ));
        // Metacaracteres: denegados aunque el prefijo coincida.
        assert!(!is_declared_sandbox_command(
            &shell(&["bash", "-lc", "python3 pizzeria.py menu; rm -rf /"]),
            &declared
        ));
        assert!(!is_declared_sandbox_command(
            &shell(&["python3", "pizzeria.py", "$(id)"]),
            &declared
        ));
        // Sin declaración, nada matchea.
        assert!(!is_declared_sandbox_command(
            &shell(&["python3", "pizzeria.py", "menu"]),
            &[]
        ));
        // Un prefijo declarado vacío no es un comodín.
        assert!(!is_declared_sandbox_command(
            &shell(&["python3", "pizzeria.py", "menu"]),
            &["   ".to_string()]
        ));
    }

    #[test]
    fn benchable_cargo_covers_verify_subcommands_only() {
        let shell = |parts: &[&str]| ActionDescriptor::ShellCommand {
            command: parts.iter().map(|s| s.to_string()).collect(),
        };
        for parts in [
            &["cargo", "check"][..],
            &["cargo", "build"][..],
            &["cargo", "test"][..],
            &["cargo", "check", "--offline"][..],
            &["cargo", "test", "--workspace"][..],
        ] {
            assert!(
                is_benchable_cargo(&shell(parts)),
                "expected {parts:?} allowed"
            );
        }
        for parts in [
            &["cargo", "run"][..],
            &["cargo", "install", "x"][..],
            &["cargo", "publish"][..],
            &["cargo"][..],
            &["cargo", "check", "--manifest-path", "/otro/Cargo.toml"][..],
            &["cargo", "check", "--manifest-path=/otro/Cargo.toml"][..],
            &["cargo", "build", "--config", "build.rustc-wrapper='sh'"][..],
            &["cargo", "test", "-Zunstable-options"][..],
            &["rustc", "main.rs"][..],
        ] {
            assert!(
                !is_benchable_cargo(&shell(parts)),
                "expected {parts:?} denied"
            );
        }
        assert!(!is_benchable_cargo(&ActionDescriptor::DeleteFile {
            path: std::path::PathBuf::from("/tmp/x"),
        }));
    }

    /// The `bash -lc` wrapper shape gpt-oss:20b sends every shell command
    /// through (preserved-session diagnosis, 2026-07-16): the plain-cargo
    /// scripts unwrap and pass, while anything with shell metacharacters,
    /// a non-cargo script, or extra wrapper args stays denied.
    #[test]
    fn benchable_cargo_unwraps_the_bash_lc_shape_strictly() {
        let shell = |parts: &[&str]| ActionDescriptor::ShellCommand {
            command: parts.iter().map(|s| s.to_string()).collect(),
        };
        for parts in [
            &["bash", "-lc", "cargo check"][..],
            &["bash", "-lc", "cargo check --quiet"][..],
            &["bash", "-c", "cargo test --workspace"][..],
            &["sh", "-c", "cargo build"][..],
            &["bash", "-l", "-c", "cargo check"][..],
        ] {
            assert!(
                is_benchable_cargo(&shell(parts)),
                "expected {parts:?} allowed"
            );
        }
        for parts in [
            // Composite/expanding shell programs — the metacharacter
            // whitelist is the boundary being proven here.
            &["bash", "-lc", "cargo check; rm -rf /"][..],
            &["bash", "-lc", "cargo check && curl http://x"][..],
            &["bash", "-lc", "cargo check | tee /tmp/x"][..],
            &["bash", "-lc", "cargo check > out.txt"][..],
            &["bash", "-lc", "cargo check $(rm -rf /)"][..],
            &["bash", "-lc", "cargo check `id`"][..],
            // Non-cargo scripts in the wrapper.
            &["bash", "-lc", "rm -rf /tmp/x"][..],
            &["bash", "-lc", "ls"][..],
            // The cargo flag rules still apply through the wrapper.
            &["bash", "-lc", "cargo run"][..],
            &[
                "bash",
                "-lc",
                "cargo check --manifest-path=/otro/Cargo.toml",
            ][..],
            // Malformed wrappers: no script, no -c, extra non-flag args,
            // or a different program.
            &["bash", "-lc"][..],
            &["bash", "cargo check"][..],
            &["bash", "-l", "cargo check"][..],
            &["zsh", "-c", "cargo check"][..],
            &["bash", "-lc", "cargo check", "extra"][..],
        ] {
            assert!(
                !is_benchable_cargo(&shell(parts)),
                "expected {parts:?} denied"
            );
        }
    }
}
