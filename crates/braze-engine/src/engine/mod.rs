//! [`Engine`]: the agentic loop. Composition root — this is the only crate
//! that talks to `braze-model`, `braze-tools-core`, `braze-session` and
//! `braze-events` at the same time (see PLAN.md, dependency graph).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use braze_events::{AgentEvent, BackgroundTask, TaskHandle, TaskNotifier, TurnObserver};
use braze_model::{CompletionEvent, CompletionRequest, ModelBackend};
use braze_session::{ContextCompactor, SessionError, SessionStore};
use braze_tools_core::ToolRegistry;
use braze_types::{ContentBlock, Message, Role, SessionId, ToolCall, ToolResult, ToolStub};

// P1.1 paso 2 (v8 § 3): reparación de huérfanos y presupuesto de
// contexto viven en `engine/context.rs` — funciones libres extraídas
// verbatim de este archivo.
mod context;
// P1.1 paso 3: dispatch de tool calls, summary fallback y ronda de
// planificación — métodos `impl Engine` extraídos verbatim (los
// submódulos acceden a los campos privados de `Engine` por ser hijos
// de este módulo; `pub(super)` marca lo que este archivo llama).
mod dispatch;
mod fallback;
mod planner;
#[cfg(test)]
mod test_support;
// P1.1 paso 4: el loop de turno, la ronda de completion y la puerta de
// persistencia/hooks — los últimos métodos grandes fuera de mod.rs.
mod hooks_dispatch;
mod round;
mod turn;

use fallback::SummaryFallbackOutcome;
use round::{RoundOutcome, RoundUsage};
use turn::TurnDispatchState;

pub use context::synthesize_orphan_repairs;
// El resto de los helpers de context.rs se consume dentro del propio
// módulo (el bloque `impl Engine` del paso 4 vive allá); solo este lo
// usa un módulo hermano (dispatch.rs, vía el glob de `use super::*`).
use context::ensure_unique_tool_call_id;

use crate::error::EngineError;
// P1.1 paso 1 (v8 § 3): la escalera de rescate vive en `crate::rescue`
// — parsers puros extraídos verbatim de este archivo.
use crate::history::build_messages_with_full_observations;
use crate::rescue::{
    EnvelopeResponse, coerce_arguments_to_schema, extract_function_xml_tool_calls,
    extract_pythonic_tool_calls, extract_tagged_tool_calls, parse_envelope_response,
    try_parse_textual_tool_call,
};

/// Default number of raw tactical events above which [`Engine::run_turn`]
/// triggers a compaction pass before building the next model request. See
/// [`Engine::new`].
pub const DEFAULT_TACTICAL_COMPACTION_THRESHOLD: usize = 40;

/// Default safety cap on model/tool-call round trips within a single
/// [`Engine::run_turn`] call, so a model that never converges on a
/// text-only response can't hang the turn forever. Now exposed as
/// [`Engine::max_turn_iterations`] (defaults to this constant) —
/// configurable via `Config::max_turn_iterations`
/// (`BRAZE_MAX_TURN_ITERATIONS`, v4 P0.2/healf rounds) because the
/// optimum depends on model capacity (a SLM converging in ~3-5 rondas
/// benefits less from a high cap than one prone to floundering).
const MAX_TURN_ITERATIONS: usize = 20;

/// How long to wait for a single background tool task to complete before
/// treating it (and only it — sibling tasks keep waiting) as failed. See
/// the doc comment on the completion-collection loop in
/// [`Engine::run_turn`] for the documented MVP limitation this implies.
const TOOL_COMPLETION_TIMEOUT: Duration = Duration::from_secs(120);

/// Minimum number of raw tactical events always rendered verbatim to the
/// model, even in the same round a compaction just ran — see
/// [`Engine::load_messages`]. Without this, a compaction discarded the
/// *entire* tactical window (including the user's message for the current
/// turn, just appended in `run_turn`), so the model's next request
/// contained no trace of what was actually being asked.
const KEEP_RAW_TAIL: usize = 6;

/// Local tool names whose successful dispatch may change filesystem/
/// environment state that another tool's result depends on — F6
/// (docs/AUDITORIA-2026-07-v3.md): the repeated-call nudge in
/// `dispatch_tool_calls` claims "the result has not changed", which is
/// false for the canonical `read_file(x) → write_file(x) → read_file(x)`
/// pattern (re-verifying an edit) — without this, the second `read_file`
/// gets nudged and blocked from actually re-running instead of returning
/// the file's real, now-different content. Read-only-by-default is the
/// safe assumption here (mirrors `history.rs`'s `NEVER_CLEAR_TOOLS`
/// starting empty) — MCP tools aren't covered; revisit with a read/write
/// annotation on `ToolStub` if that becomes a real gap.
const MUTATING_TOOL_NAMES: &[&str] = &["write_file", "edit_file", "shell_exec"];

/// Subconjunto de `MUTATING_TOOL_NAMES` que con certeza tocó el
/// workspace. `shell_exec` queda FUERA a propósito: el harness no sabe
/// si el comando escribió algo, y `cargo test` (el caso normal) no es
/// una edición. Se usa para la nota de convergencia, donde afirmar "ya
/// editaste" en falso es peor que no decir nada — ver incidente roam
/// #10 en `docs/bitacora-harness-modelo.html`.
const FILE_MUTATING_TOOL_NAMES: &[&str] = &["write_file", "edit_file"];

/// A partir de cuántas lecturas de la MISMA ruta en un turno, sin
/// edición intermedia, el harness anexa la nota de relectura
/// improductiva (`crate::engine::dispatch`). Cuatro deja pasar el uso
/// legítimo (abrir un archivo largo por trozos) y ataca el bucle
/// observado en producción.
const UNPRODUCTIVE_REREAD_THRESHOLD: u32 = 4;

/// A partir de cuántos fallos de `edit_file` sobre la MISMA ruta en un
/// turno, sin edición exitosa intermedia, el interlock duro bloquea
/// `write_file` sobre esa ruta (v9 L-10 — ver
/// `TurnDispatchState::edit_failures_by_path` por la rama de daño que
/// cierra). Dos: el primer fallo puede ser un old_string desactualizado
/// legítimo; el segundo sobre la misma ruta ya es el patrón "no puedo
/// reproducir el contenido", y la reescritura total es donde ese patrón
/// corrompe en silencio.
const EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD: u32 = 2;

/// Consecutive zero-tool-call turns before `run_turn` injects the
/// narration-without-action reminder (D5, docs/AUDITORIA-2026-07-v3.md).
/// `2`: the *third* such turn in a row gets the reminder — one narrated
/// response alone is often a legitimate conversational answer; two in a
/// row against a request that should have produced a tool call is the
/// pattern actually observed.
const NARRATION_WITHOUT_ACTION_THRESHOLD: u32 = 2;

/// Default cap on the planning round's `max_tokens` (PLAN.md § "Split
/// planificador/ejecutor") — plans are short numbered lists; letting the
/// planner spend the executor's full `max_tokens` budget would just be
/// cost with no benefit. The effective value is
/// `min(self.max_tokens, self.planner_max_tokens)`, so a caller with a
/// *smaller* overall budget is still respected. Now exposed as
/// [`Engine::planner_max_tokens`] (defaults to this constant) —
/// configurable via `Config::planner_max_tokens`
/// (`BRAZE_PLANNER_MAX_TOKENS`, v4 P0.2/healf rounds).
const PLANNER_MAX_TOKENS: u32 = 1024;

/// The agentic loop. Orchestrates model calls, tool dispatch (via
/// background tasks + push notification), differential context
/// compaction, and session persistence.
pub struct Engine {
    model: Box<dyn ModelBackend>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn SessionStore>,
    compactor: Box<dyn ContextCompactor>,
    notifier: Box<dyn TaskNotifier>,
    system_prompt: String,
    max_tokens: u32,
    tactical_compaction_threshold: usize,
    /// How many of the tactical window's most recent observations stay
    /// full instead of collapsing to one line — see
    /// `history::TACTICAL_FULL_OBSERVATIONS`'s doc comment. Defaults to
    /// that same constant; overridable via
    /// [`Engine::with_tactical_full_observations`] for `braze-bench`'s
    /// `+ablate:full-observations=N` (E1, docs/AUDITORIA-2026-07-v3.md).
    tactical_full_observations: usize,
    /// Gates the ACI collapse of old observations — `false` renders every
    /// tactical observation full regardless of age/size. See
    /// [`Engine::with_observation_collapse_enabled`] (`+ablate:no-prune`).
    observation_collapse_enabled: bool,
    /// Gates tactical compaction entirely (both the event-count and the
    /// token-budget triggers) — see
    /// [`Engine::with_compaction_enabled`] (`+ablate:no-compaction`).
    compaction_enabled: bool,
    /// Session constraints to declare durably at the start of the next
    /// `run_turn` — the explicit entry point of the SC-retention route
    /// (docs/hypothesis-2026-08-13-sc-retention.md). Each becomes an
    /// idempotent `AgentEvent::SessionConstraintDeclared` in the log
    /// (skipped if an identical one is already declared there, so
    /// `--resume` and multi-turn sessions don't re-append), which the
    /// compactor then harvests verbatim into `DurableState::constraints`
    /// on every request. Empty (the default) = route inactive, zero
    /// behavior change. See [`Engine::with_session_constraints`].
    session_constraints: Vec<String>,
    /// Cumulative per-turn token circuit breaker (v4 P0.2): once a
    /// turn's summed `input + output` across rounds exceeds this, the
    /// loop stops gracefully instead of re-sending an ever-growing
    /// history. `None` (the default) = disabled. See
    /// [`Engine::with_max_turn_total_tokens`].
    max_turn_total_tokens: Option<u64>,
    /// Presupuesto de wall-clock por turno, evaluado en el borde de cada
    /// ronda. `None` (el default) = deshabilitado. Ver
    /// [`Engine::with_max_turn_wall_clock`] — es el corte que la línea
    /// round-economics necesita para comparar configuraciones a tiempo
    /// fijo en vez de a rondas fijas.
    max_turn_wall_clock: Option<Duration>,
    /// Deadline de wall-clock por RONDA, aplicado a nivel de streaming
    /// (cubre el request inicial y cada `stream.next()`). `None` (el
    /// default) = deshabilitado. Complementa a `max_turn_wall_clock`, que
    /// evalúa en el borde de la ronda y por eso no puede acotar una ronda
    /// desbocada — el defecto de instrumento que el piloto de
    /// round-economics encontró en sus datos
    /// (`docs/round-economics-pilot-costo-2026-08-08.md` § 4.4). Ver
    /// [`Engine::with_max_round_wall_clock`].
    max_round_wall_clock: Option<Duration>,
    /// Approximate token budget for the durable+tactical portion of the
    /// prompt (i.e. excluding `system_prompt`/tool schemas, which the
    /// caller should already have reserved headroom for when computing
    /// this). `None` (the default) means compaction is triggered purely
    /// by `tactical_compaction_threshold`'s raw event count, as before —
    /// set via [`Engine::with_context_budget`] for backends with a small,
    /// known context window (e.g. Ollama's `num_ctx`), where a single
    /// large tool result can blow the budget long before the event count
    /// does. See [`Engine::load_messages`].
    context_budget_tokens: Option<u32>,
    /// Number of independent candidates per round for técnica G10
    /// (docs/AUDITORIA-2026-07.md, Best-of-n / Test-Time Scaling). `1`
    /// (the default) or `0` disable the technique entirely — the round
    /// goes through [`Engine::complete_once`] directly, exactly the same
    /// code path as before G10 existed. Only `> 1` routes the round
    /// through [`Engine::complete_with_best_of_n`].
    best_of_n: usize,
    /// Overrides [`TOOL_COMPLETION_TIMEOUT`] — see
    /// [`Engine::with_tool_completion_timeout`]. Kept configurable (rather
    /// than only ever the module constant) purely so tests can exercise
    /// the timeout-then-abort path in `dispatch_tool_calls` without
    /// actually waiting 120 real seconds.
    tool_completion_timeout: Duration,
    /// Tools dispatched inline, with NO completion timeout — J-13
    /// (docs/AUDITORIA-2026-07-v7.md): a tool that blocks on a HUMAN
    /// answer (`ask_user`) must not race the background-tool timeout.
    /// Under the 120s clock, a slow human answer was cancelled (the model
    /// got a timeout error and guessed anyway) and the answer the human
    /// then typed was consumed by the chat loop as a brand-new prompt — a
    /// garbage turn. The composition root that registers an interactive
    /// provider names it here (`braze-cli` adds `ask_user`); empty by
    /// default and in every non-interactive root (bench, `braze run`).
    untimed_tools: std::collections::HashSet<String>,
    /// Safety cap on model/tool-call round trips within a single
    /// [`Engine::run_turn`] call. Defaults to [`MAX_TURN_ITERATIONS`];
    /// overridden via [`Engine::with_max_turn_iterations`] from
    /// `Config::max_turn_iterations` (v4 P0.2/mitad rondas).
    max_turn_iterations: usize,
    /// Cap on the planning round's `max_tokens` (effective value is
    /// `min(self.max_tokens, self.planner_max_tokens)`). Defaults to
    /// [`PLANNER_MAX_TOKENS`]; overridden via
    /// [`Engine::with_planner_max_tokens`] from `Config::planner_max_tokens`
    /// (v4 P0.2/mitad rondas).
    planner_max_tokens: u32,
    /// Set for the duration of a [`Engine::run_turn`] call, cleared on
    /// every exit path via [`TurnGuard`]'s `Drop`. N-17
    /// (docs/AUDITORIA-2026-07-v2.md): two concurrent `run_turn` calls on
    /// the same `Engine` would share one `TaskNotifier`'s single
    /// completion channel — `dispatch_tool_calls`'s "stale completion"
    /// check (see its doc comment) would discard the *other* turn's real
    /// completions as stale, and that turn would eventually persist a
    /// false timeout error. Not reachable via any current caller (every
    /// caller of `Engine::run_turn` serializes turns), but was previously
    /// neither guarded against nor documented — this turns the misuse
    /// into an explicit, diagnosable error instead of silent cross-talk
    /// if that ever changes.
    turn_in_progress: std::sync::atomic::AtomicBool,
    /// How many *consecutive* turns ended with zero tool calls dispatched
    /// (a plain text final answer, never a `dispatch_tool_calls` call) —
    /// D5 (docs/AUDITORIA-2026-07-v3.md): the repeated-call nudge (A5) is
    /// intra-turn and only fires in response to an actual tool call, so
    /// it never catches this project's own documented failure mode — a
    /// model that just keeps *narrating* an intended action turn after
    /// turn without ever calling the tool for it (observed live against
    /// qwen2.5:3b, `prompt.rs`'s doc comment). Reset to 0 the moment any
    /// turn dispatches a real tool call; incremented only on the "final
    /// text answer, no tool calls" success path in `run_turn`. Persists
    /// across `run_turn` calls on the same `Engine` instance (a CLI
    /// session's whole lifetime, barring a manual model switch or
    /// process restart) — never read back from the session log.
    consecutive_turns_without_tool_calls: std::sync::atomic::AtomicU32,
    /// Gates the textual tool-call rescue in [`Engine::complete_once`] —
    /// A′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.2):
    /// whether the engine injects [`AgentEvent::HarnessNote`] warnings
    /// (turn budget at 80%, final round coming) into the conversation.
    /// `true` by default; `with_harness_notes_enabled(false)` is the
    /// `no-harness-notes` ablation.
    harness_notes_enabled: bool,
    /// Audit-only hooks (Paquete B′,
    /// docs/harness-engineering-hooks-skills-2026-07-10.md § Parte II) —
    /// dispatched after every persisted event and before every executor
    /// request, in registration order, each call bounded by
    /// `hooks::HOOK_TIMEOUT`. Empty by default; composition roots
    /// register via [`Engine::with_hook`].
    hooks: Vec<crate::hooks::RegisteredHook>,
    /// C′.1 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.3):
    /// stubs por provider sobre los cuales sus tools no se listan y
    /// quedan detrás del meta-tool `search_tools`. Ver
    /// `crate::tool_search`.
    tool_search_threshold: usize,
    /// Tools de providers diferidos que una búsqueda ya "activó" — se
    /// re-listan el resto de la sesión. `std::sync::Mutex` (no tokio):
    /// nunca se sostiene a través de un await.
    activated_deferred_tools: std::sync::Mutex<std::collections::HashSet<String>>,
    /// C′.2 (crate::task_list): expone `task_add`/`task_update` y
    /// re-inyecta el resumen compacto por ronda. OFF por default — dos
    /// tools extra son distractores potenciales para un SLM; entra al
    /// bench por su propia fila (`+ablate:task-list`).
    task_list_enabled: bool,
    /// I.7 (`crate::exploration`): expone la tool `explore` — el
    /// mini-loop hijo aislado de solo-lectura. OFF por default, mismo
    /// razonamiento que la task list (una tool extra es un distractor
    /// potencial); entra al bench por `+ablate:explore` y su adopción
    /// la decide el A/B pre-registrado
    /// (`docs/explorador-aislado-ab-design.md`).
    exploration_enabled: bool,
    /// Habilita el subagente `editor` (SWE-Edit #17, `crate::editor`) —
    /// la mitad escritora del par Viewer/Editor. Off por default;
    /// `Config::enable_editor` / `+ablate:editor`. Su A/B decide su
    /// adopción (`docs/editor-subagent-design-2026-08-10.md`).
    editor_enabled: bool,
    /// El estado de la lista (por turno, en memoria — se resetea al
    /// inicio de cada `run_turn`, J-4 docs/AUDITORIA-2026-07-v7.md: sin
    /// el reset, los planes de turnos/temas distintos se mezclaban y un
    /// pendiente abandonado re-inyectaba el resumen para siempre. Ver el
    /// module doc de `crate::task_list` sobre `--resume`).
    task_list: std::sync::Mutex<crate::task_list::TaskList>,
    /// J-3 (docs/AUDITORIA-2026-07-v7.md): los `HarnessNote` del turno EN
    /// CURSO, re-anexados como mensaje user efímero al final de cada
    /// request — el mismo patrón request-scoped del resumen de la task
    /// list. El evento se persiste igual (auditoría/bench), pero NUNCA se
    /// renderiza desde la historia: antes, un "[harness] answer now" del
    /// turno 1 seguía siendo instrucción vigente en los turnos 2..N (el
    /// bench single-turn jamás vio este modo de falla). Se limpia al
    /// inicio de cada `run_turn`.
    turn_harness_notes: std::sync::Mutex<Vec<String>>,
    /// ¿Este turno ya aplicó una edición de archivo EXITOSA
    /// (`FILE_MUTATING_TOOL_NAMES`)? Cambia el consejo de la nota de
    /// convergencia: sin edición previa, "arregla con un edit decisivo";
    /// con edición previa, "verifica y cierra". Se limpia al inicio de
    /// cada `run_turn`, igual que `turn_harness_notes`.
    turn_did_edit: std::sync::atomic::AtomicBool,
    /// ¿Este turno DESPACHÓ una edición de archivo, exitosa o no
    /// (`FILE_MUTATING_TOOL_NAMES`, sin el guard de `!is_error`)?
    /// Distingue "intentó cambiar el workspace y no lo logró" de "solo
    /// leyó". Con `turn_did_edit` desglosa el fallback de resumen
    /// (incidente roam #16): `attempted && !did` = el turno quiso editar
    /// y aterrizó cero → un resumen no-vacío es razonamiento hueco, se
    /// falla; `!attempted` = quizá un Q&A read-only legítimo cuya
    /// respuesta quedó en el canal de razonamiento, se respeta. Se limpia
    /// al inicio de cada `run_turn`.
    turn_attempted_edit: std::sync::atomic::AtomicBool,
    /// D′ (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte
    /// III): registry de skills descubiertas al arranque. `None` (el
    /// default y el bench siempre) = feature apagada.
    skill_registry: Option<std::sync::Arc<braze_skills::SkillRegistry>>,
    /// Recuris § 2.2.2 — ver [`Engine::with_call_time_skills`].
    call_time_skills_enabled: bool,
    /// Brazo U1 de Q0 — ver [`Engine::with_insistent_task_tools`].
    insistent_task_tools: bool,
    /// Skills ya cargadas esta sesión — sus addenda se re-anexan al
    /// system prompt de cada request (reconstruidos del registry, nunca
    /// persistidos como conversación).
    loaded_skills: std::sync::Mutex<Vec<braze_skills::LoadedSkill>>,
    /// Techo del walk-up de la carga JIT de AGENTS.md por subdirectorio
    /// (`docs/agents-md-jit-design-2026-08-11.md`). `Some(root)` prende la
    /// feature: cuando un tool toca un archivo bajo `root`, se descubre el
    /// `AGENTS.md` más cercano subiendo hasta `root` y se inyecta. `None`
    /// (el default, y el bench siempre) = feature apagada. Lo setea la CLI
    /// con `braze_memory::resolve_project_root(cwd)` salvo
    /// `disable_agents_md`.
    agents_md_root: Option<std::path::PathBuf>,
    /// Rutas canónicas de los `AGENTS.md` ya cargados esta sesión — dedup
    /// del descubrimiento JIT. Sembrado con el `AGENTS.md` raíz (ya en el
    /// system prompt) para no re-inyectarlo. Session-scoped como
    /// `loaded_skills` (NO se resetea por turno). El `Vec` de bodies
    /// paralelo (`loaded_agents_md_bodies`) guarda el orden de
    /// descubrimiento para la inyección; el `HashSet` es solo el dedup.
    loaded_agents_md: std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
    /// Cuerpos de los `AGENTS.md` de subdir descubiertos, en orden, para
    /// anexar al system prompt de cada request. Paralelo a
    /// `loaded_agents_md` (que dedupe por path); el raíz NO está acá (vive
    /// en `self.system_prompt`).
    loaded_agents_md_bodies: std::sync::Mutex<Vec<String>>,
    /// Cap de tokens por body inyectado (config `skills.max_body_tokens`).
    skills_max_body_tokens: usize,
    /// Cuántas skills puede cargar la mención de UN turno (config
    /// `skills.max_loaded_per_turn`).
    skills_max_loaded_per_turn: usize,
    /// see [`Engine::with_textual_rescue_enabled`]. `true` (the default)
    /// preserves the existing behavior.
    textual_rescue_enabled: bool,
    /// Parses whole-response JSON *envelopes* (`{"action": "tool_call" |
    /// "final_answer", ...}`) into tool calls / final text — the return
    /// channel of `OllamaBackend`'s prompt-tools/constrained modes
    /// (docs/constrained-decoding-ab-design.md). `false` (the default)
    /// everywhere except the `+ablate:prompt-tools`/`constrained-tools`
    /// bench rows — see [`Engine::with_envelope_parsing_enabled`].
    envelope_parsing_enabled: bool,
    /// A/B del impuesto JSON (`crate::edit_fence`,
    /// docs/hypothesis-2026-08-10-json-tax-edit-fence.md): con el lever
    /// prendido, `edit_file` sale del inventario, el system prompt lleva
    /// la gramática SEARCH/REPLACE, y los bloques del texto de cada
    /// ronda se sintetizan como calls de `edit_file` — canal primario,
    /// nunca contado como rescue. `false` (el default) es un no-op
    /// estricto; `Config::enable_edit_fence` / `+ablate:edit-fence`.
    edit_fence_enabled: bool,
    /// Optional planner backend (PLAN.md § "Split planificador/ejecutor"):
    /// a stronger model that produces a one-shot plan before the turn's
    /// first executor round, persisted as [`AgentEvent::PlanCreated`].
    /// `None` (the default) means zero behavior change — see
    /// [`Engine::with_planner`] and [`Engine::attempt_planning_round`].
    planner: Option<Box<dyn ModelBackend>>,
    /// v8 § 6 — summary-por-lead: cuando está presente, la compactación
    /// le pide a ESTE backend (típicamente una segunda instancia del
    /// modelo del `--lead`) el summary de los eventos dropeados, con
    /// fallback al digest extractivo del compactor ante cualquier fallo.
    /// `None` (el default) = comportamiento byte-idéntico al previo. Ver
    /// [`Engine::with_compaction_summarizer`] y
    /// `Engine::attempt_lead_summary` (engine/context.rs).
    summarizer: Option<Box<dyn ModelBackend>>,
    /// End-of-turn verification gate (first H2 hook,
    /// docs/verification-lever-design-2026-07-22.md). `Some` runs the
    /// configured command when the model produces a final answer after a
    /// turn that dispatched tool calls; a non-zero exit injects the
    /// captured output back as an observation and grants the model up to
    /// `max_rounds` more rounds instead of accepting the unverified
    /// claim of success (finding #15). `None` (the default) = zero
    /// behavior change. Set via [`Engine::with_verification`].
    verification: Option<VerificationConfig>,
}

/// Configuration for the end-of-turn verification gate
/// (docs/verification-lever-design-2026-07-22.md).
#[derive(Debug, Clone)]
pub struct VerificationConfig {
    /// Argv of the verification command, e.g. `["cargo", "test"]`. Run in
    /// the engine's working directory. Exit 0 = verified; non-zero =
    /// failed (its output is fed back). A missing binary or a timeout is
    /// treated as "skip" (never blocks a legitimate turn) — the same
    /// failure posture as the post-edit check.
    pub command: Vec<String>,
    /// Per-run wall-clock ceiling for the command.
    pub timeout: Duration,
    /// How many extra rounds the model gets to fix a verification failure
    /// before the turn ends anyway (marked unverified). Bounds the loop.
    pub max_rounds: usize,
    /// Directory to run the command in. `None` = the process's current
    /// directory (the interactive case: `braze` was invoked where the
    /// user wants it verified). `Some` is required in the bench, whose
    /// tasks run in throwaway sandbox dirs that are NOT the process cwd —
    /// the command must run where the model's edits actually landed.
    pub working_dir: Option<std::path::PathBuf>,
}

impl Engine {
    /// Builds an `Engine` with [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] as
    /// its compaction trigger. `tools` is wrapped internally in an `Arc` so
    /// it can be shared into the `'static` background-task futures
    /// [`TaskNotifier::spawn`] takes ownership of — `ToolRegistry` itself
    /// is not `Clone` (it owns a `Vec<Box<dyn ToolProvider>>`), so an `Arc`
    /// is the seam that lets the same registry be dispatched against from
    /// many concurrently-spawned tasks without cloning its providers.
    pub fn new(
        model: Box<dyn ModelBackend>,
        tools: ToolRegistry,
        store: Arc<dyn SessionStore>,
        compactor: Box<dyn ContextCompactor>,
        notifier: Box<dyn TaskNotifier>,
        system_prompt: String,
        max_tokens: u32,
    ) -> Self {
        Self {
            model,
            tools: Arc::new(tools),
            store,
            compactor,
            notifier,
            system_prompt,
            max_tokens,
            tactical_compaction_threshold: DEFAULT_TACTICAL_COMPACTION_THRESHOLD,
            tactical_full_observations: crate::history::TACTICAL_FULL_OBSERVATIONS,
            observation_collapse_enabled: true,
            compaction_enabled: true,
            session_constraints: Vec::new(),
            max_turn_total_tokens: None,
            max_turn_wall_clock: None,
            max_round_wall_clock: None,
            context_budget_tokens: None,
            best_of_n: 1,
            tool_completion_timeout: TOOL_COMPLETION_TIMEOUT,
            untimed_tools: std::collections::HashSet::new(),
            max_turn_iterations: MAX_TURN_ITERATIONS,
            planner_max_tokens: PLANNER_MAX_TOKENS,
            turn_in_progress: std::sync::atomic::AtomicBool::new(false),
            consecutive_turns_without_tool_calls: std::sync::atomic::AtomicU32::new(0),
            harness_notes_enabled: true,
            hooks: Vec::new(),
            tool_search_threshold: crate::tool_search::DEFAULT_TOOL_SEARCH_THRESHOLD,
            activated_deferred_tools: std::sync::Mutex::new(std::collections::HashSet::new()),
            task_list_enabled: false,
            exploration_enabled: false,
            editor_enabled: false,
            task_list: std::sync::Mutex::new(crate::task_list::TaskList::default()),
            turn_harness_notes: std::sync::Mutex::new(Vec::new()),
            turn_did_edit: std::sync::atomic::AtomicBool::new(false),
            turn_attempted_edit: std::sync::atomic::AtomicBool::new(false),
            skill_registry: None,
            call_time_skills_enabled: false,
            insistent_task_tools: false,
            loaded_skills: std::sync::Mutex::new(Vec::new()),
            agents_md_root: None,
            loaded_agents_md: std::sync::Mutex::new(std::collections::HashSet::new()),
            loaded_agents_md_bodies: std::sync::Mutex::new(Vec::new()),
            skills_max_body_tokens: 1200,
            skills_max_loaded_per_turn: 2,
            textual_rescue_enabled: true,
            envelope_parsing_enabled: false,
            edit_fence_enabled: false,
            planner: None,
            summarizer: None,
            verification: None,
        }
    }

    /// Sets an approximate token budget for the durable+tactical portion
    /// of the prompt, above which a compaction triggers regardless of raw
    /// event count — see the field's doc comment and
    /// [`Engine::load_messages`]. Chainable, e.g.
    /// `Engine::new(...).with_context_budget(6000)`.
    pub fn with_context_budget(mut self, tokens: u32) -> Self {
        self.context_budget_tokens = Some(tokens);
        self
    }

    /// Overrides [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] with a
    /// caller-supplied value (C10, docs/AUDITORIA-2026-07.md) — e.g. from
    /// `braze_config::Config::tactical_compaction_threshold`. Chainable,
    /// same shape as [`Engine::with_context_budget`].
    pub fn with_tactical_compaction_threshold(mut self, threshold: usize) -> Self {
        self.tactical_compaction_threshold = threshold;
        self
    }

    /// Overrides how many of the tactical window's most recent
    /// observations stay full instead of collapsing — see
    /// `tactical_full_observations`'s field doc comment. Chainable, same
    /// shape as [`Engine::with_context_budget`].
    pub fn with_tactical_full_observations(mut self, full_observations: usize) -> Self {
        self.tactical_full_observations = full_observations;
        self
    }

    /// Disables the ACI collapse of old observations entirely — every
    /// tactical observation renders full, no matter how old or large
    /// (opencode ítem 2 / `+ablate:no-prune`, docs/AUDITORIA-2026-07-v6.md):
    /// the collapse is a central lever of the SLM-first thesis, and its
    /// contribution can't be measured without a way to turn it OFF for a
    /// bench row. `true` (the default) keeps the existing behavior.
    /// Chainable, same shape as [`Engine::with_tactical_full_observations`].
    pub fn with_observation_collapse_enabled(mut self, enabled: bool) -> Self {
        self.observation_collapse_enabled = enabled;
        self
    }

    /// Disables tactical compaction entirely — both the event-count and
    /// the token-budget triggers (E1 / `+ablate:no-compaction`,
    /// docs/AUDITORIA-2026-07-v6.md § roadmap). A long turn can then blow
    /// the model's real context window; that's the ablation's point.
    /// `true` (the default) keeps the existing behavior. Chainable.
    pub fn with_compaction_enabled(mut self, enabled: bool) -> Self {
        self.compaction_enabled = enabled;
        self
    }

    /// Declares session constraints for the SC-retention durable route
    /// (docs/hypothesis-2026-08-13-sc-retention.md): each string is
    /// persisted at the start of the next `run_turn` as an
    /// `AgentEvent::SessionConstraintDeclared` (idempotently — an
    /// identical constraint already in the log is not re-appended), and
    /// from then on renders VERBATIM at the top of every request via
    /// `DurableState::constraints`, immune to `truncate_words`, the
    /// digest tail-cap and the summary cap. Explicit-entry only, by
    /// design: this lever honors *known* constraints; detecting them in
    /// free text is a separate claim (CompInt RQ4) it deliberately does
    /// not make. Empty (the default) = no route, byte-identical behavior.
    /// Chainable, same shape as [`Engine::with_compaction_enabled`].
    pub fn with_session_constraints(mut self, constraints: Vec<String>) -> Self {
        self.session_constraints = constraints;
        self
    }

    /// Cumulative per-turn token budget (v4 P0.2,
    /// docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3): once the turn's
    /// summed `input + output` tokens exceed `budget`, the next iteration
    /// stops the loop gracefully — same tools-free summary attempt the
    /// iteration cap gets — instead of re-sending an ever-growing history
    /// until `max_turn_iterations`. `None` (the default) disables the
    /// breaker. Token-based, not USD-based, on purpose: this crate has no
    /// pricing knowledge (that lives in config/bench), and a caller that
    /// thinks in dollars can convert with its own rates. Chainable.
    pub fn with_max_turn_total_tokens(mut self, budget: Option<u64>) -> Self {
        self.max_turn_total_tokens = budget;
        self
    }

    /// Presupuesto de wall-clock por turno (línea round-economics,
    /// `docs/hypothesis-2026-07-28-round-economics.md`): al empezar cada
    /// ronda, si el turno ya gastó más de `budget`, el loop para con
    /// [`EngineError::TurnWallClockExhausted`]. `None` (el default) lo
    /// deshabilita, que es el comportamiento histórico.
    ///
    /// Es el tercer corte del turno, y el único cuyo recurso cambia de
    /// precio con el despliegue: `max_turn_iterations` cuenta rondas y
    /// `max_turn_total_tokens` cuenta tokens — las dos son invariantes a
    /// si una ronda tarda 2 s o 90 s. Medir configuraciones de harness a
    /// *tiempo* fijo en vez de a rondas fijas es la unidad experimental
    /// que pide esa línea, y no se puede construir desde afuera: un
    /// `tokio::time::timeout` alrededor de [`Engine::run_turn`] mata la
    /// ronda en vuelo y con ella su `Usage`, así que las rondas y los
    /// tokens de toda fila cortada quedan censurados (J-21/J-10). Cortar
    /// en el borde de ronda deja la contabilidad completa y comparable.
    ///
    /// El corte NO concede la ronda de resumen sin tools que sí concede
    /// el presupuesto de tokens — ver [`EngineError::TurnWallClockExhausted`]
    /// por qué esa concesión sesgaría justo el factor bajo estudio.
    /// Chainable, misma forma que [`Engine::with_context_budget`].
    pub fn with_max_turn_wall_clock(mut self, budget: Option<Duration>) -> Self {
        self.max_turn_wall_clock = budget;
        self
    }

    /// Deadline de wall-clock por RONDA, aplicado dentro de la ronda a
    /// nivel de streaming: el reloj arranca antes del request al modelo y
    /// cada espera (`complete` inicial, cada `stream.next()`) corre
    /// contra lo que queda del deadline. Al vencerse, el stream se
    /// abandona — lo que cancela la generación en los backends que
    /// detectan al consumidor caído, como el `LocalBackend` — y la ronda
    /// falla con [`EngineError::RoundWallClockExhausted`]. `None` (el
    /// default) lo deshabilita.
    ///
    /// Existe porque [`Engine::with_max_turn_wall_clock`] evalúa en el
    /// borde de la ronda, deliberadamente — y por eso su caso peor no es
    /// "presupuesto + una ronda" sino "presupuesto + una ronda NO
    /// acotada": el piloto de round-economics midió filas de 600 s con
    /// `rounds` 0-1 (una sola ronda desbocada de generación CPU) que solo
    /// el backstop de infraestructura podía parar, censurando toda la
    /// contabilidad (`docs/round-economics-pilot-costo-2026-08-08.md`
    /// § 4.4). Este deadline acota esa ronda desde adentro: las rondas
    /// completadas del turno conservan sus eventos y su usage, y el error
    /// sale por el camino normal en vez de matar el future desde afuera.
    ///
    /// Aplica a TODA ronda que pase por `complete_once_with` — ejecutor,
    /// planner, resumen sin tools y cada candidato de best-of-n — porque
    /// cualquiera de ellas puede desbocarse por la misma causa.
    /// Chainable, misma forma que [`Engine::with_context_budget`].
    pub fn with_max_round_wall_clock(mut self, deadline: Option<Duration>) -> Self {
        self.max_round_wall_clock = deadline;
        self
    }

    /// Sets the number of independent candidates each round generates
    /// before voting on which one to use — técnica G10
    /// (docs/AUDITORIA-2026-07.md), e.g. from
    /// `braze_config::Config::best_of_n`. `n <= 1` is a no-op (the round
    /// loop already treats that as "disabled"). Chainable, same shape as
    /// [`Engine::with_context_budget`].
    pub fn with_best_of_n(mut self, n: usize) -> Self {
        self.best_of_n = n;
        self
    }

    /// Overrides [`TOOL_COMPLETION_TIMEOUT`] with a caller-supplied value.
    /// Chainable, same shape as [`Engine::with_context_budget`].
    pub fn with_tool_completion_timeout(mut self, timeout: Duration) -> Self {
        self.tool_completion_timeout = timeout;
        self
    }

    /// Marks a tool as interactive: dispatched inline and exempt from
    /// [`Engine::with_tool_completion_timeout`] — see the
    /// `untimed_tools` field doc (J-13). Chainable, once per tool.
    pub fn with_untimed_tool(mut self, name: impl Into<String>) -> Self {
        self.untimed_tools.insert(name.into());
        self
    }

    /// Overrides [`MAX_TURN_ITERATIONS`] — the safety cap on
    /// model/tool-call round trips within a single [`Engine::run_turn`]
    /// call. Configurable via `Config::max_turn_iterations`
    /// (`BRAZE_MAX_TURN_ITERATIONS`, v4 P0.2/mitad rondas). Chainable,
    /// same shape as [`Engine::with_context_budget`].
    pub fn with_max_turn_iterations(mut self, cap: usize) -> Self {
        self.max_turn_iterations = cap;
        self
    }

    /// Overrides [`PLANNER_MAX_TOKENS`] — the cap on the planning
    /// round's `max_tokens` (effective `min(self.max_tokens, self.planner_max_tokens)`).
    /// Configurable via `Config::planner_max_tokens`
    /// (`BRAZE_PLANNER_MAX_TOKENS`, v4 P0.2/mitad rondas). Chainable,
    /// same shape as [`Engine::with_context_budget`].
    pub fn with_planner_max_tokens(mut self, cap: u32) -> Self {
        self.planner_max_tokens = cap;
        self
    }

    /// Disables (`enabled: false`) the textual tool-call rescue (B5,
    /// docs/AUDITORIA-2026-07.md) — N-15 (docs/AUDITORIA-2026-07-v2.md):
    /// the rescue is purely syntactic (any response that's entirely a
    /// `{"name":..., "arguments":...}` JSON blob gets dispatched as a
    /// real tool call), so a user literally asking to see the JSON for a
    /// *real* tool name (e.g. "muéstrame el JSON para invocar
    /// write_file") gets that example executed for real. Chainable, same
    /// shape as [`Engine::with_context_budget`].
    pub fn with_textual_rescue_enabled(mut self, enabled: bool) -> Self {
        self.textual_rescue_enabled = enabled;
        self
    }

    /// Enables the end-of-turn verification gate (H2,
    /// docs/verification-lever-design-2026-07-22.md). Chainable, same
    /// shape as [`Engine::with_context_budget`]; `None` semantics are the
    /// default (no gate).
    pub fn with_verification(mut self, config: VerificationConfig) -> Self {
        self.verification = Some(config);
        self
    }

    /// Enables (`enabled: true`) whole-response envelope parsing — the
    /// return channel of the prompt-tools/constrained-decoding A/B
    /// (docs/constrained-decoding-ab-design.md): a response that is
    /// entirely `{"action": "tool_call", "name": ..., "arguments": ...}`
    /// becomes a tool call (its optional `reasoning` stays as the round's
    /// text), and `{"action": "final_answer", "text": ...}` becomes the
    /// final text. Deliberately NOT a rung of the rescue ladder and never
    /// counted as a rescue: in this mode the envelope is the *primary*
    /// parse channel (the backend instructed the model to emit it), and
    /// the A/B's mechanism check is precisely `rescues ≈ 0` on the
    /// constrained arm. What doesn't parse falls through to the normal
    /// ladder. `false` (the default) is a strict no-op. Chainable.
    pub fn with_envelope_parsing_enabled(mut self, enabled: bool) -> Self {
        self.envelope_parsing_enabled = enabled;
        self
    }

    /// Enables (`enabled: true`) the edit-fence channel — the A/B del
    /// impuesto JSON (docs/hypothesis-2026-08-10-json-tax-edit-fence.md):
    /// `edit_file` leaves the request's tool inventory, the system
    /// prompt carries the SEARCH/REPLACE grammar
    /// (`crate::edit_fence::EDIT_FENCE_ADDENDUM`), and well-formed
    /// blocks in a round's text are synthesized into `edit_file` calls.
    /// Like the envelope, deliberately NOT a rung of the rescue ladder
    /// and never counted as a rescue — the fence is the *instructed*
    /// channel here, and the A/B needs `rescued_tool_calls` clean as a
    /// mechanism check. `false` (the default) is a strict no-op.
    /// Chainable.
    pub fn with_edit_fence_enabled(mut self, enabled: bool) -> Self {
        self.edit_fence_enabled = enabled;
        self
    }

    /// Gates the A′.2 harness notes (budget/iteration-cap warnings the
    /// model sees mid-turn) — `true` by default; the `no-harness-notes`
    /// ablation is how braze-bench measures whether announcing a
    /// deadline actually converts aborted turns into converged ones.
    /// Chainable, same shape as [`Engine::with_textual_rescue_enabled`].
    pub fn with_harness_notes_enabled(mut self, enabled: bool) -> Self {
        self.harness_notes_enabled = enabled;
        self
    }

    /// Registers an audit-only hook (Paquete B′,
    /// docs/harness-engineering-hooks-skills-2026-07-10.md § Parte II) —
    /// dispatched in registration order. Chainable, same shape as the
    /// other builders.
    pub fn with_hook(mut self, hook: std::sync::Arc<dyn crate::hooks::EngineHook>) -> Self {
        self.hooks.push(crate::hooks::RegisteredHook::new(hook));
        self
    }

    /// Overrides the per-provider stub count over which a provider's
    /// tools hide behind `search_tools` (C′.1, `crate::tool_search`) —
    /// `Config::tool_search_threshold` / `+ablate:tool-search-threshold=N`.
    /// Chainable.
    pub fn with_tool_search_threshold(mut self, threshold: usize) -> Self {
        self.tool_search_threshold = threshold;
        self
    }

    /// Enables the C′.2 typed task list (`crate::task_list`) — the two
    /// harness-owned tools plus the per-round compact summary
    /// reinjection. Off by default; `Config::enable_task_list` /
    /// `+ablate:task-list`. Chainable.
    pub fn with_task_list_enabled(mut self, enabled: bool) -> Self {
        self.task_list_enabled = enabled;
        self
    }

    /// Exige evidencia de ejecución para cerrar una tarea de la lista
    /// C′.2: `task_update(id, "done")` se rechaza salvo que alguna tool
    /// call del registry haya terminado sin error desde el último `done`
    /// aceptado.
    ///
    /// Es la traducción barata de los *checkers* de Recuris
    /// (arXiv:2608.24876, § 2.2.3), donde un goal solo avanza cuando la
    /// observación del entorno sostiene el cambio de estado en vez del
    /// claim del modelo. El síntoma que ataca ya está documentado acá:
    /// v8 K-6 registró que un 3B re-marca `done` con frecuencia, y la
    /// métrica dual (`[RouteMiss]`, 2026-08-12) mide la versión de esto
    /// que sí llega al resultado.
    ///
    /// Off by default; `Config::enable_task_evidence` /
    /// `+ablate:task-evidence`. Sin `with_task_list_enabled` no hace
    /// nada — no hay lista que cerrar. Chainable.
    pub fn with_task_evidence_required(self, required: bool) -> Self {
        if let Ok(mut list) = self.task_list.lock() {
            list.set_require_evidence(required);
        }
        self
    }

    /// Invocación *call-time* de skills (Recuris, arXiv:2608.24876
    /// § 2.2.2): cuando el modelo redacta una tool call para la que
    /// alguna skill se declara guía (frontmatter `tools:`), la call **no
    /// se ejecuta**. Vuelve un resultado sintético de no-ejecución, la
    /// skill entra al system prompt, y el modelo re-emite la acción ya
    /// con la guía delante.
    ///
    /// El cambio respecto de D′ no es *qué* se inyecta sino *cuándo*: la
    /// guía llega antes de que la acción ocurra en vez de después de que
    /// falle, y solo para las herramientas que el turno realmente usa —
    /// una skill que nadie invoca nunca se paga. Es el lado barato de la
    /// condición de amortización del Paper 2: la Tabla 12 de Recuris
    /// mide que tener la biblioteca entera en contexto cuesta 3.111
    /// tokens más en el primer call, rinde 18 puntos peor y sale 46% más
    /// caro por éxito que este esquema.
    ///
    /// Cada skill intercepta **una sola vez por sesión**: una vez
    /// cargada, las llamadas siguientes a esa herramienta se ejecutan
    /// normalmente. Off by default; `Config::enable_call_time_skills` /
    /// `+ablate:call-time-skills`. Sin registro de skills no hace nada.
    /// Chainable.
    pub fn with_call_time_skills(mut self, enabled: bool) -> Self {
        self.call_time_skills_enabled = enabled;
        self
    }

    /// Brazo **U1** de Q0 (`docs/hypothesis-2026-08-28-task-evidence-gate.md`):
    /// las descripciones de `task_add`/`task_update` piden el uso
    /// explícitamente en vez de ofrecerlo.
    ///
    /// Existe para medir si la tasa de uso del 2,2 % es un problema de
    /// redacción o de capacidad. Sin `with_task_list_enabled` no hace
    /// nada. Off by default; `+ablate:task-tools-insistent`. Chainable.
    pub fn with_insistent_task_tools(mut self, insistent: bool) -> Self {
        self.insistent_task_tools = insistent;
        self
    }

    /// Enables the I.7 isolated exploration child loop
    /// (`crate::exploration`) — the harness-owned `explore` tool. Off by
    /// default; `Config::enable_exploration` / `+ablate:explore`.
    /// Chainable.
    pub fn with_exploration_enabled(mut self, enabled: bool) -> Self {
        self.exploration_enabled = enabled;
        self
    }

    /// Enables the SWE-Edit `editor` child loop (`crate::editor`) — the
    /// harness-owned `editor` tool. Off by default;
    /// `Config::enable_editor` / `+ablate:editor`. Chainable.
    pub fn with_editor_enabled(mut self, enabled: bool) -> Self {
        self.editor_enabled = enabled;
        self
    }

    /// Attaches a skill registry (D′, `braze-skills`) with its two caps —
    /// `$name` mentions in a turn's user input load bodies as system
    /// prompt addenda. `None` registry (the default; braze-bench always)
    /// keeps the feature fully off. Chainable.
    pub fn with_skills(
        mut self,
        registry: std::sync::Arc<braze_skills::SkillRegistry>,
        max_body_tokens: usize,
        max_loaded_per_turn: usize,
    ) -> Self {
        self.skill_registry = Some(registry);
        self.skills_max_body_tokens = max_body_tokens;
        self.skills_max_loaded_per_turn = max_loaded_per_turn.max(1);
        self
    }

    /// Enables just-in-time discovery of subdirectory `AGENTS.md` files
    /// (`docs/agents-md-jit-design-2026-08-11.md`): `root` is the ceiling
    /// of the walk-up (typically `braze_memory::resolve_project_root(cwd)`)
    /// and `root_agents_md` the canonical path of the root `AGENTS.md`
    /// already baked into the system prompt — seeded into the loaded set
    /// so the walk never re-injects it. When a tool touches a file under
    /// `root`, the nearest `AGENTS.md` up to `root` is loaded and appended
    /// to the system prompt for the rest of the session. Not calling this
    /// (the default; the bench always) keeps the feature fully off.
    /// Chainable.
    pub fn with_agents_md_jit(
        mut self,
        root: std::path::PathBuf,
        root_agents_md: Option<std::path::PathBuf>,
    ) -> Self {
        if let Some(root_md) = root_agents_md {
            let canonical = std::fs::canonicalize(&root_md).unwrap_or(root_md);
            self.loaded_agents_md.lock().unwrap().insert(canonical);
        }
        self.agents_md_root = Some(root);
        self
    }

    /// Enables the planner/executor split (PLAN.md § "Split
    /// planificador/ejecutor"): `planner` — typically a stronger/cloud
    /// model — produces a one-shot plan at the start of every turn, which
    /// the executor (`self.model`) then follows. Purely additive: without
    /// this call the turn loop is byte-identical to before the feature
    /// existed. Chainable, same shape as [`Engine::with_context_budget`].
    pub fn with_planner(mut self, planner: Box<dyn ModelBackend>) -> Self {
        self.planner = Some(planner);
        self
    }

    /// v8 § 6 — summary-por-lead: la compactación le pide el summary de
    /// los eventos dropeados a `summarizer` (una llamada tools-free con
    /// cap de tokens y timeout) en vez de usar solo el digest extractivo;
    /// ante cualquier fallo cae al digest — nunca peor que sin esto.
    /// Purely additive, mismo contrato que [`Engine::with_planner`].
    /// Chainable.
    pub fn with_compaction_summarizer(mut self, summarizer: Box<dyn ModelBackend>) -> Self {
        self.summarizer = Some(summarizer);
        self
    }
}

// P1.1 / v9 L-5 COMPLETO (2026-08-18): el `mod tests` de este archivo
// se repartió entero a los módulos dueños de cada cluster — escalera de
// parsers en `crate::rescue`; fallback, contexto/huérfanos, hooks,
// explorador, task list, search_tools y schema-repair en sus
// `engine/{fallback,context,hooks_dispatch,dispatch}.rs`; skills y
// run_turn/summary en `engine/turn.rs`; edit-fence, envelope y
// best-of-n en `engine/round.rs`; fixtures compartidas en
// `engine/test_support.rs`. Este archivo queda como composition root
// (struct + builders) sin tests propios.
