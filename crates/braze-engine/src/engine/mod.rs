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

#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 resto (v9 L-5, 2026-08-18): los tests unitarios de la
    // escalera viven ahora en el `mod tests` de `crate::rescue`.
    // P1.1 pasos 5-6: los tests de context/planner/compactación viven
    // en sus módulos; los de fallback, en engine/fallback.rs (v9 L-5).
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use braze_events::{NoopObserver, TextDeltaObserver};
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
    use braze_types::{ContentBlock, ToolStub};

    use super::test_support::*;

    // P1.1 resto (v9 L-5): el cluster run_turn_*/summary-round vive en
    // turn.rs (con sus vecinos de planner en planner.rs y los de
    // schema-repair/repeated-call en dispatch.rs); RecordingObserver en
    // test_support.rs. Aquí quedan los clusters con módulo propio
    // pendiente (skills, exploración, task list, search_tools, hooks,
    // notas de harness, parsers de rescate, envelope, best-of-n).

    // --- D′: skills locales explicit-only
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte III) ---

    fn temp_skills_dir(label: &str, skills: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-engine-skills-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, body) in skills {
            let skill_dir = dir.join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: guidance for {name}\n---\n\n{body}"),
            )
            .unwrap();
        }
        dir
    }

    /// The explicit-mention path end to end: `$testing` in the user's
    /// input loads that skill's body into the request's system prompt,
    /// persists `SkillLoaded`, and leaves unmentioned skills out.
    #[tokio::test]
    async fn a_skill_mention_loads_its_body_into_the_system_prompt() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let skills_dir = temp_skills_dir(
            "mention",
            &[
                ("testing", "Always run cargo test before claiming success."),
                ("review", "Check invariants first."),
            ],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ]]),
            requests: Arc::clone(&requests),
        };
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);

        engine
            .run_turn(&session, "usa $testing para esto", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let requests = requests.lock().unwrap().clone();
        assert!(
            requests[0].system_prompt.contains("Loaded skill: testing"),
            "got: {}",
            requests[0].system_prompt
        );
        assert!(
            requests[0]
                .system_prompt
                .contains("Always run cargo test before claiming success."),
            "the body itself must be injected"
        );
        assert!(
            !requests[0].system_prompt.contains("Loaded skill: review"),
            "unmentioned skills stay out"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::SkillLoaded { name, trigger, .. }
                    if name == "testing" && trigger == "explicit_mention"
            )),
            "the load must persist as the rollout log's trace"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessage { text } if text.contains("Always run cargo test")
            )),
            "the body is request-scoped, never persisted as conversation"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The per-turn cap: mentioning three skills with a cap of 2 loads
    /// two and persists a `SkillLoadSkipped` for the third — bounded
    /// context growth, visible in the log.
    #[tokio::test]
    async fn the_per_turn_cap_skips_the_excess_mention_with_an_event() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let skills_dir = temp_skills_dir(
            "cap",
            &[
                ("uno", "body uno"),
                ("dos", "body dos"),
                ("tres", "body tres"),
            ],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);

        engine
            .run_turn(&session, "usa $uno $dos $tres", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let loaded: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SkillLoaded { .. }))
            .collect();
        assert_eq!(loaded.len(), 2, "cap of 2: {events:#?}");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::SkillLoadSkipped { name, reason }
                    if name == "tres" && reason.contains("per-turn cap")
            )),
            "the third mention must be visibly skipped"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for J-12 (docs/AUDITORIA-2026-07-v7.md): a FRESH
    /// engine over the same session store — the `--resume` restart, or a
    /// `/model` rebuild — must re-load the bodies the log's `SkillLoaded`
    /// events record, without appending new `SkillLoaded` events (which
    /// would double the bench's counts on every resumed turn).
    #[tokio::test]
    async fn a_fresh_engine_rehydrates_previously_loaded_skills_from_the_log() {
        let (store, dir) = temp_store();
        let store = Arc::new(store);
        let session = SessionId::new();
        let skills_dir = temp_skills_dir(
            "rehydrate",
            &[("testing", "Always run cargo test before claiming success.")],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        // Session 1: the mention loads the skill and persists SkillLoaded.
        let first_engine = Engine::new(
            Box::new(ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ]])),
            ToolRegistry::new(vec![]),
            Arc::clone(&store) as Arc<dyn SessionStore>,
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(std::sync::Arc::clone(&registry), 1200, 2);
        first_engine
            .run_turn(&session, "usa $testing para esto", &mut NoopObserver)
            .await
            .expect("first turn must converge");
        drop(first_engine); // the restart: in-memory loaded_skills is gone

        // Session 2 (same store, fresh engine): no mention this turn —
        // the body must come back from the log alone.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let second_engine = Engine::new(
            Box::new(RequestCapturingModel {
                inner: ScriptedModel::new(vec![vec![
                    CompletionEvent::TextDelta("sigo".to_string()),
                    CompletionEvent::Done,
                ]]),
                requests: Arc::clone(&requests),
            }),
            ToolRegistry::new(vec![]),
            Arc::clone(&store) as Arc<dyn SessionStore>,
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);
        second_engine
            .run_turn(&session, "continúa con lo anterior", &mut NoopObserver)
            .await
            .expect("resumed turn must converge");

        let requests = requests.lock().unwrap().clone();
        assert!(
            requests[0]
                .system_prompt
                .contains("Always run cargo test before claiming success."),
            "the rehydrated body must reach the resumed turn's system prompt, got: {}",
            requests[0].system_prompt
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let loaded_count = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SkillLoaded { .. }))
            .count();
        assert_eq!(
            loaded_count, 1,
            "rehydration must NOT append a second SkillLoaded event: {events:#?}"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- I.7: explorador de contexto aislado
    // (docs/explorador-aislado-ab-design.md) ---

    /// The delegation round-trip: the parent calls `explore`, the child
    /// loop (same scripted backend) answers in one tools-free round, the
    /// conclusion comes back as the explore call's tool result, and the
    /// rollout log records the audit event + aggregate child usage —
    /// but NONE of the child's own transcript (isolation is the lever).
    #[tokio::test]
    async fn an_exploration_delegation_round_trips_and_stays_isolated() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Script order: parent round 1 (calls explore) → child round
        // (answers, no tools) → parent round 2 (final answer).
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-explore".to_string(),
                    name: "explore".to_string(),
                    arguments: serde_json::json!({
                        "question": "which file defines parse_header?"
                    }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("parse_header is defined in src/header.rs.".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 120,
                    output_tokens: 30,
                    stop_reason: Some("stop".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("Está en src/header.rs.".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_exploration_enabled(true);

        engine
            .run_turn(
                &session,
                "¿dónde se define parse_header?",
                &mut NoopObserver,
            )
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        // The audit event carries the question and the child's cost.
        let delegated = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ExplorationDelegated {
                    question,
                    child_rounds,
                    child_tokens,
                } => Some((question.clone(), *child_rounds, *child_tokens)),
                _ => None,
            })
            .expect("ExplorationDelegated must be persisted");
        assert_eq!(delegated.0, "which file defines parse_header?");
        assert_eq!(delegated.1, 1, "the child answered in one round");
        assert_eq!(delegated.2, 150, "child input+output tokens summed");

        // The conclusion is the explore call's tool result…
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallCompleted { result, .. }
                    if !result.is_error
                        && result.content.contains("src/header.rs")
            )),
            "the child's conclusion must come back as the tool result"
        );
        // …and the child's transcript is NOT in the parent's log: the
        // only AssistantText is the parent's final answer.
        let assistant_texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            assistant_texts,
            vec!["Está en src/header.rs."],
            "isolation: the child's own text must never enter the parent log"
        );
        // The aggregate child usage rides as a Usage event so every
        // existing token accounting counts it.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Usage { stop_reason: Some(reason), input_tokens: 120, output_tokens: 30, .. }
                    if reason == "exploration_child"
            )),
            "aggregate child usage must be persisted: {events:#?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The lever must never kill the turn: a child that produces nothing
    /// usable degrades to a recoverable tool error the parent can act on.
    #[tokio::test]
    async fn a_failed_exploration_degrades_to_a_recoverable_tool_error() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-explore".to_string(),
                    name: "explore".to_string(),
                    arguments: serde_json::json!({ "question": "¿algo?" }),
                },
                CompletionEvent::Done,
            ],
            // Child round: empty text, no tool calls → exploration fails.
            vec![CompletionEvent::Done],
            vec![
                CompletionEvent::TextDelta("Exploro yo mismo.".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_exploration_enabled(true);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the failed exploration must not kill the turn");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallCompleted { result, .. }
                    if result.is_error && result.content.contains("exploration failed")
            )),
            "expected the recoverable error result: {events:#?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- C′.2: task list tipada
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § I.4) ---

    /// With the lever ON: the two task tools join the inventory, an add
    /// + update round-trips through the harness-owned handler, and the
    /// compact summary rides the NEXT round's request as an ephemeral
    /// user message (never persisted).
    #[tokio::test]
    async fn task_tools_round_trip_and_the_summary_rides_the_next_request() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "task_add".to_string(),
                        arguments: serde_json::json!({ "description": "leer notas.txt" }),
                    },
                    CompletionEvent::ToolCallRequested {
                        id: "call-2".to_string(),
                        name: "task_add".to_string(),
                        arguments: serde_json::json!({ "description": "responder" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-3".to_string(),
                        name: "task_update".to_string(),
                        arguments: serde_json::json!({ "id": 1, "status": "done" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "haz dos cosas", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let requests = requests.lock().unwrap().clone();
        // Round 1: tools listed, no summary yet (empty list).
        let round1_names: Vec<&str> = requests[0]
            .tool_stubs
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(round1_names.contains(&"task_add"));
        assert!(round1_names.contains(&"task_update"));
        let round1_text: String = requests[0]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !round1_text.contains("Task list:"),
            "no summary before any task exists"
        );

        // Round 2: the summary reflects the adds.
        let round2_text: String = requests[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            round2_text.contains("1 [pending] leer notas.txt"),
            "got: {round2_text}"
        );
        assert!(round2_text.contains("2 [pending] responder"));

        // Round 3: the update shows.
        let round3_text: String = requests[2]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            round3_text.contains("1 [done] leer notas.txt"),
            "got: {round3_text}"
        );

        // The ephemeral summary must NOT be persisted as events.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessage { text } if text.contains("Task list:")
            )),
            "the summary is request-scoped, never persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// With the lever OFF (the default): no task tools in the inventory
    /// — existing measurements see zero change.
    #[tokio::test]
    async fn the_task_list_is_absent_by_default() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Done,
            ]]),
            requests: Arc::clone(&requests),
        };

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let requests = requests.lock().unwrap().clone();
        assert!(
            !requests[0]
                .tool_stubs
                .iter()
                .any(|s| s.name == "task_add" || s.name == "task_update"),
            "off by default"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The planner bridge (the pre-registered A/B's planner→tasks arm):
    /// with the task list on, an accepted plan seeds tasks instead of
    /// persisting `PlanCreated` prose, and the summary rides the
    /// executor's first request.
    #[tokio::test]
    async fn an_accepted_plan_seeds_the_task_list_instead_of_prose() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. leer notas.txt\n2. responder".to_string()),
            CompletionEvent::Done,
        ]]);
        let executor = RequestCapturingModel {
            inner: ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ]]),
            requests: Arc::clone(&requests),
        };

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner))
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "haz dos cosas", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "planner→tasks: no prose plan in the history"
        );

        let requests = requests.lock().unwrap().clone();
        let round1_text: String = requests[0]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            round1_text.contains("1 [pending] leer notas.txt"),
            "the seeded list rides the first executor request: {round1_text}"
        );
        assert!(round1_text.contains("2 [pending] responder"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Marking a task `done` via `task_update` must persist
    /// `AgentEvent::TaskCompleted` with that task's description — the
    /// durable signal `braze-memory`'s `ProjectMemoryHook` depends on,
    /// since the task list itself is in-memory only (J-4).
    #[tokio::test]
    async fn marking_a_task_done_persists_task_completed() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "task_add".to_string(),
                    arguments: serde_json::json!({"description": "leer notas.txt"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "task_update".to_string(),
                    arguments: serde_json::json!({"id": 1, "status": "done"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "leé el archivo", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::TaskCompleted { description } if description == "leer notas.txt"
            )),
            "task_update to done must persist TaskCompleted with the task's description"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The mirror case: `task_add` alone, or a transition to
    /// `in_progress`, must NOT persist `TaskCompleted` — only an actual
    /// completion should.
    #[tokio::test]
    async fn adding_or_progressing_a_task_does_not_persist_task_completed() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "task_add".to_string(),
                    arguments: serde_json::json!({"description": "leer notas.txt"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "task_update".to_string(),
                    arguments: serde_json::json!({"id": 1, "status": "in_progress"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("trabajando".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "leé el archivo", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TaskCompleted { .. })),
            "task_add and in_progress transitions must never persist TaskCompleted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- C′.1: search_tools — herramientas diferidas en dos niveles
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § I.3) ---

    /// A provider with many stubs — the "gateway grande" fixture. Every
    /// tool resolves a permissive schema and invokes successfully.
    struct NoisyToolsProvider {
        count: usize,
        invocations: Arc<AtomicU32>,
    }

    #[async_trait]
    impl ToolProvider for NoisyToolsProvider {
        fn provider_id(&self) -> &str {
            "test:noisy"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            let mut stubs: Vec<ToolStub> = (0..self.count)
                .map(|i| ToolStub {
                    name: format!("noise_tool_{i}"),
                    summary: "an unrelated operation".to_string(),
                    source: "test:noisy".to_string(),
                    input_schema: None,
                })
                .collect();
            stubs.push(ToolStub {
                name: "frobnicate_target".to_string(),
                summary: "frobnicates the target dataset".to_string(),
                source: "test:noisy".to_string(),
                input_schema: None,
            });
            Ok(stubs)
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name.starts_with("noise_tool_") || name == "frobnicate_target" {
                Ok(Some(ToolSchema {
                    name: name.to_string(),
                    description: "test tool".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "frobnicated".to_string(),
                is_error: false,
            })
        }
    }

    /// The full two-level loop: a big provider hides behind
    /// `search_tools`; the model searches, the hit activates, the next
    /// round's inventory lists it, and the call dispatches to the real
    /// provider. The small provider stays visible throughout.
    #[tokio::test]
    async fn search_tools_hides_a_big_provider_and_activation_makes_the_hit_invocable() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invocations = Arc::new(AtomicU32::new(0));
        let echo_invocations = Arc::new(AtomicU32::new(0));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                // Round 1: the model searches the hidden catalog.
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "search_tools".to_string(),
                        arguments: serde_json::json!({ "query": "frobnicate dataset" }),
                    },
                    CompletionEvent::Done,
                ],
                // Round 2: calls the tool the search surfaced.
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-2".to_string(),
                        name: "frobnicate_target".to_string(),
                        arguments: serde_json::json!({}),
                    },
                    CompletionEvent::Done,
                ],
                // Round 3: converges.
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![
                Box::new(EchoToolProvider::new(Arc::clone(&echo_invocations))),
                Box::new(NoisyToolsProvider {
                    count: 50,
                    invocations: Arc::clone(&invocations),
                }),
            ]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_search_threshold(40);

        engine
            .run_turn(&session, "frobnica el dataset", &mut NoopObserver)
            .await
            .expect("turn must converge");

        // Cloned out so no MutexGuard lives across the await below.
        let requests = requests.lock().unwrap().clone();
        // Round 1's inventory: echo (small provider, visible), NO noise
        // tools, and the search meta-tool.
        let round1_names: Vec<&str> = requests[0]
            .tool_stubs
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(round1_names.contains(&"echo"));
        assert!(round1_names.contains(&"search_tools"));
        assert!(
            !round1_names.iter().any(|n| n.starts_with("noise_tool_")),
            "the big provider's tools must be hidden: {round1_names:?}"
        );
        assert!(
            !round1_names.contains(&"frobnicate_target"),
            "the target starts hidden too"
        );

        // Round 2's inventory: the search hit is now listed.
        let round2_names: Vec<&str> = requests[1]
            .tool_stubs
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            round2_names.contains(&"frobnicate_target"),
            "the activated hit must resurface: {round2_names:?}"
        );

        // And the real provider was actually invoked.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        // The search result itself reached the model as a tool result.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallCompleted { result, .. }
                    if result.content.contains("frobnicate_target") && !result.is_error
            )),
            "the search must answer with the matching tools"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// J-9 (docs/AUDITORIA-2026-07-v7.md): naming a hidden tool directly
    /// — without activating it via `search_tools` — must NOT dispatch it.
    /// The model gets a recoverable, actionable error instead; after a
    /// real search activates the tool, the same direct call works. This
    /// is what makes "the model can only use what's listed or searched
    /// for" literally true for the search_tools A/B.
    #[tokio::test]
    async fn a_deferred_tool_called_without_activation_is_rejected_not_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let invocations = Arc::new(AtomicU32::new(0));
        let echo_invocations = Arc::new(AtomicU32::new(0));

        let model = ScriptedModel::new(vec![
            // Round 1: guesses the hidden tool's name directly.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "frobnicate_target".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            // Round 2: does what the error told it to do.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "search_tools".to_string(),
                    arguments: serde_json::json!({ "query": "frobnicate" }),
                },
                CompletionEvent::Done,
            ],
            // Round 3: the activated tool now dispatches for real.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-3".to_string(),
                    name: "frobnicate_target".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            // Round 4: converges.
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![
                Box::new(EchoToolProvider::new(Arc::clone(&echo_invocations))),
                Box::new(NoisyToolsProvider {
                    count: 50,
                    invocations: Arc::clone(&invocations),
                }),
            ]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_search_threshold(40);

        engine
            .run_turn(&session, "frobnica el dataset", &mut NoopObserver)
            .await
            .expect("turn must converge");

        // The provider ran exactly once — for round 3, never for the
        // unactivated round-1 call.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let round1_result = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { id, result } if id == "call-1" => Some(result),
                _ => None,
            })
            .expect("the blocked call must still complete its event pair");
        assert!(round1_result.is_error);
        assert!(
            round1_result.content.contains("search_tools"),
            "the error must tell the model the way in: {}",
            round1_result.content
        );
        let round3_result = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { id, result } if id == "call-3" => Some(result),
                _ => None,
            })
            .expect("the post-activation call must complete");
        assert!(!round3_result.is_error);
        assert_eq!(round3_result.content, "frobnicated");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- Edit-fence (A/B del impuesto JSON,
    //     docs/hypothesis-2026-08-10-json-tax-edit-fence.md) ---

    /// El camino completo del brazo fence: el modelo emite prosa + un
    /// bloque SEARCH/REPLACE, el parser lo sintetiza como `edit_file`,
    /// dispatch lo ejecuta contra el provider real (schema-válido), y
    /// queda el rastro contable (`EditFenceApplied`, NUNCA
    /// `TextualRescueApplied` — la separación es el mecanismo del A/B).
    #[tokio::test]
    async fn an_edit_fence_block_is_parsed_dispatched_and_counted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Fixing the constant.\n\nsrc/lib.rs\n".to_string()),
                CompletionEvent::TextDelta(
                    "<<<<<<< SEARCH\nlet x = 1;\n=======\nlet x = 2;\n>>>>>>> REPLACE\n".to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EditRecordingToolProvider::new(Arc::clone(
                &calls,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_edit_fence_enabled(true);

        engine
            .run_turn(&session, "fix the constant", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "the fence edit must reach the tool");
        assert_eq!(
            recorded[0],
            serde_json::json!({
                "path": "src/lib.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;",
            }),
            "the block's sections must arrive verbatim as edit_file args"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::EditFenceApplied { blocks: 1 })),
            "the fence channel must persist its own bench-countable event"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextualRescueApplied { .. })),
            "the instructed fence channel must NOT count as a rescue"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Fixing the constant.")
            )),
            "the surrounding prose must survive as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("SEARCH")
            )),
            "the consumed block must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// La otra mitad del brazo: con el lever ON, `edit_file` no aparece
    /// en el inventario del request y el system prompt lleva la
    /// gramática del fence; con el lever OFF (default), ni lo uno ni lo
    /// otro — no-op estricto.
    #[tokio::test]
    async fn edit_fence_lever_hides_the_stub_and_injects_the_addendum() {
        for lever_on in [true, false] {
            let (store, dir) = temp_store();
            let session = SessionId::new();

            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let model = RequestCapturingModel {
                inner: ScriptedModel::new(vec![vec![
                    CompletionEvent::TextDelta("ok".to_string()),
                    CompletionEvent::Done,
                ]]),
                requests: Arc::clone(&requests),
            };

            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let engine = Engine::new(
                Box::new(model),
                ToolRegistry::new(vec![Box::new(EditRecordingToolProvider::new(calls))]),
                Arc::new(store),
                Box::new(SimpleContextCompactor::default()),
                Box::new(TestNotifier::new()),
                "system prompt".to_string(),
                1024,
            )
            .with_edit_fence_enabled(lever_on);

            engine
                .run_turn(&session, "hola", &mut NoopObserver)
                .await
                .expect("turn should succeed");

            let captured = requests.lock().unwrap().clone();
            assert!(!captured.is_empty());
            let req = &captured[0];
            let has_edit_stub = req.tool_stubs.iter().any(|s| s.name == "edit_file");
            let has_addendum = req.system_prompt.contains("<<<<<<< SEARCH");
            if lever_on {
                assert!(!has_edit_stub, "lever ON must hide the edit_file stub");
                assert!(has_addendum, "lever ON must inject the fence grammar");
            } else {
                assert!(has_edit_stub, "lever OFF must keep the edit_file stub");
                assert!(!has_addendum, "lever OFF must not touch the system prompt");
            }

            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    // --- Envelope parsing (A/B constrained decoding,
    //     docs/constrained-decoding-ab-design.md) ---

    #[test]
    fn parse_envelope_response_extracts_a_tool_call_with_its_reasoning() {
        let text = r#"{"action": "tool_call", "reasoning": "need the file",
                       "name": "read_file", "arguments": {"path": "x.txt"}}"#;
        match parse_envelope_response(text) {
            Some(EnvelopeResponse::ToolCall { call, reasoning }) => {
                assert_eq!(call.name, "read_file");
                assert_eq!(call.arguments, serde_json::json!({"path": "x.txt"}));
                assert!(call.id.starts_with("envelope-"));
                assert_eq!(reasoning.as_deref(), Some("need the file"));
            }
            other => panic!("expected a tool call, got {}", envelope_kind(&other)),
        }
    }

    #[test]
    fn parse_envelope_response_defaults_missing_arguments_to_an_empty_object() {
        let text = r#"{"action": "tool_call", "name": "list_dir"}"#;
        match parse_envelope_response(text) {
            Some(EnvelopeResponse::ToolCall { call, reasoning }) => {
                assert_eq!(call.arguments, serde_json::json!({}));
                assert_eq!(reasoning, None);
            }
            other => panic!("expected a tool call, got {}", envelope_kind(&other)),
        }
    }

    #[test]
    fn parse_envelope_response_rejects_non_object_arguments() {
        let text = r#"{"action": "tool_call", "name": "read_file", "arguments": "x.txt"}"#;
        assert!(parse_envelope_response(text).is_none());
    }

    #[test]
    fn parse_envelope_response_extracts_a_final_answer_and_drops_reasoning() {
        let text = r#"{"action": "final_answer", "reasoning": "done thinking", "text": "42"}"#;
        match parse_envelope_response(text) {
            Some(EnvelopeResponse::FinalAnswer { text }) => assert_eq!(text, "42"),
            other => panic!("expected a final answer, got {}", envelope_kind(&other)),
        }
    }

    #[test]
    fn parse_envelope_response_accepts_a_json_fenced_envelope() {
        let text = "```json\n{\"action\": \"final_answer\", \"text\": \"42\"}\n```";
        assert!(matches!(
            parse_envelope_response(text),
            Some(EnvelopeResponse::FinalAnswer { .. })
        ));
    }

    /// Non-envelope shapes must fall through untouched so the rescue
    /// ladder stays the owner of every other textual format: bare
    /// rescue-shape JSON (no `action`), an unknown action, and prose.
    #[test]
    fn parse_envelope_response_ignores_non_envelope_shapes() {
        assert!(
            parse_envelope_response(r#"{"name": "read_file", "arguments": {"path": "x"}}"#)
                .is_none()
        );
        assert!(
            parse_envelope_response(r#"{"action": "run", "name": "x", "arguments": {}}"#).is_none()
        );
        assert!(parse_envelope_response("I read the file and it says 42.").is_none());
        assert!(parse_envelope_response(r#"{"action": "final_answer"}"#).is_none());
    }

    fn envelope_kind(envelope: &Option<EnvelopeResponse>) -> &'static str {
        match envelope {
            Some(EnvelopeResponse::ToolCall { .. }) => "a tool call",
            Some(EnvelopeResponse::FinalAnswer { .. }) => "a final answer",
            None => "none",
        }
    }

    /// The envelope is the *primary* parse channel of prompt-tools mode,
    /// not a rescue: the call must dispatch, the `reasoning` must survive
    /// as the round's text, and — the A/B's mechanism check depends on
    /// this — NO `TextualRescueApplied` may be persisted for it.
    #[tokio::test]
    async fn an_envelope_tool_call_dispatches_without_counting_as_a_rescue() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    r#"{"action": "tool_call", "reasoning": "I will echo hi",
                       "name": "echo", "arguments": {"text": "hi"}}"#
                        .to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta(
                    r#"{"action": "final_answer", "text": "done"}"#.to_string(),
                ),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_envelope_parsing_enabled(true);

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "I will echo hi"
            )),
            "the envelope's reasoning must survive as the round's text"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "done"
            )),
            "the final_answer's inner text must be the turn's final text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"action\"")
            )),
            "the raw envelope JSON must never be persisted as conversational text"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextualRescueApplied { .. })),
            "an envelope parse must NOT count as a textual rescue — the \
             A/B's mechanism check is `rescues ≈ 0` on the constrained arm"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A `final_answer` envelope whose inner text happens to look like a
    /// bare-JSON tool call must stay text: the model explicitly declared
    /// it final, so the rescue ladder is suppressed for that round.
    #[tokio::test]
    async fn an_envelope_final_answer_is_never_reinterpreted_by_the_rescue_ladder() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let inner = r#"{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}"#;
        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(format!(
                r#"{{"action": "final_answer", "text": "{inner}"}}"#
            )),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_envelope_parsing_enabled(true);

        engine
            .run_turn(
                &session,
                "show me the JSON for an echo call",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "a declared-final answer must not be dispatched as a tool call"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the inner text must be persisted verbatim as the answer"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Default-off is a strict no-op: without
    /// `with_envelope_parsing_enabled(true)` an envelope-shaped response
    /// takes the pre-existing path — the bare-JSON rescue fires on its
    /// `name`/`arguments` fields and counts as a rescue, exactly as it
    /// did before this lever existed.
    #[tokio::test]
    async fn envelope_parsing_disabled_leaves_the_pre_existing_rescue_path_intact() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    r#"{"action": "tool_call", "name": "echo", "arguments": {"text": "hi"}}"#
                        .to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextualRescueApplied { .. })),
            "with the lever off, the bare-JSON rescue owns this shape and must count as a rescue"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F2 (docs/AUDITORIA-2026-07-v3.md): qwen3-coder's bare `<function=>`
    /// XML grammar has no native number type, so a `limit: integer`
    /// parameter comes back from the rescue as the string `"5"` — without
    /// schema-guided coercion this fails validation deterministically
    /// (every call to a tool with a numeric param, rescued via this
    /// format, would burn a repair round it can't even fix, since the
    /// XML grammar has no way to emit a JSON number). With the fix, the
    /// call dispatches on the first attempt and the tool receives a real
    /// JSON number.
    #[tokio::test]
    async fn qwen3_coder_xml_with_a_stringified_integer_param_gets_coerced_before_dispatch() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    "<function=echo_limit>\n<parameter=text>\nhi\n</parameter>\n\
                     <parameter=limit>\n5\n</parameter>\n</function>"
                        .to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let received_limit = Arc::new(std::sync::Mutex::new(None));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoWithLimitToolProvider::new(
                Arc::clone(&invocations),
                Arc::clone(&received_limit),
            ))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi with limit 5", &mut NoopObserver)
            .await
            .expect("turn should succeed — coercion must let validation pass on the first try");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "must dispatch exactly once — no schema-repair retry round needed"
        );
        assert_eq!(
            received_limit.lock().unwrap().clone(),
            Some(serde_json::json!(5)),
            "the tool must receive a real JSON number, not the string \"5\""
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-15 (docs/AUDITORIA-2026-07-v2.md):
    /// `with_textual_rescue_enabled(false)` must stop the rescue from
    /// dispatching a real tool a user only asked to see the JSON for —
    /// the raw text is persisted as ordinary conversational text instead.
    #[tokio::test]
    async fn textual_rescue_can_be_disabled() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(
                r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
            ),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_textual_rescue_enabled(false);

        engine
            .run_turn(
                &session,
                "muéstrame el JSON para invocar echo",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the tool must never actually be invoked when the rescue is disabled"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the raw JSON must be persisted as ordinary text instead of dispatched"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- técnica G10: best-of-n / test-time scaling (docs/AUDITORIA-2026-07.md) ---

    /// Regression test for G10's core value proposition: a 2-vote
    /// majority ("hi") beats a 1-vote dissenter ("wrong") among 3
    /// candidates, and only the winning call is ever dispatched.
    #[tokio::test]
    async fn best_of_n_dispatches_the_majority_tool_call_signature() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-a".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-b".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "wrong" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-c".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(3);

        engine
            .run_turn(
                &session,
                "please echo hi (with a dissenting distractor)",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "only the winning candidate's call should ever reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        {
            Some(AgentEvent::ToolCallCompleted { result, .. }) => {
                assert_eq!(
                    result.content, "echoed: hi",
                    "the 2-vote majority ('hi') must win over the 1-vote dissenter ('wrong')"
                );
            }
            other => panic!("expected a ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A 1-vs-1 tie must resolve deterministically to the
    /// earliest-generated candidate — never `Iterator::max_by_key`'s
    /// "last wins" default, which would make the outcome depend on
    /// implementation details of the vote-counting loop.
    #[tokio::test]
    async fn best_of_n_breaks_ties_by_keeping_the_earliest_candidate() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-a".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "first" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-b".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "second" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(2);

        engine
            .run_turn(&session, "please echo something", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        {
            Some(AgentEvent::ToolCallCompleted { result, .. }) => {
                assert_eq!(
                    result.content, "echoed: first",
                    "a 1-vs-1 tie must keep the earliest-generated candidate"
                );
            }
            other => panic!("expected a ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `self.best_of_n` real model calls happen per round — the
    /// persisted `Usage` must reflect the *summed* cost across every
    /// candidate, not just the winner's, or token/cost accounting
    /// silently under-reports by every discarded candidate's share.
    /// `stop_reason` is taken from the winning candidate specifically.
    #[tokio::test]
    async fn best_of_n_sums_usage_across_candidates_and_keeps_the_winners_stop_reason() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Both candidates answer with the same plain text (no tool call
        // — same "no tool call" signature, so it's a 1-vs-1 tie and
        // candidate 0 wins per the tie-break rule tested above).
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    stop_reason: Some("end_turn".to_string()),
                    // Deliberately mismatched Some/None across candidates
                    // — exercises `sum_optional_u32`'s "at least one
                    // candidate reported it" rule, not just the trivial
                    // both-Some or both-None cases.
                    cache_read_tokens: Some(6),
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 20,
                    output_tokens: 8,
                    stop_reason: Some("stop_sequence".to_string()),
                    cache_read_tokens: Some(4),
                    cache_write_tokens: Some(2),
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(2);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::Usage { .. }))
        {
            Some(AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            }) => {
                assert_eq!(
                    *input_tokens, 30,
                    "usage must sum every candidate's cost, not just the winner's"
                );
                assert_eq!(*output_tokens, 13);
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("end_turn"),
                    "stop_reason must reflect the winning candidate specifically"
                );
                assert_eq!(
                    *cache_read_tokens,
                    Some(10),
                    "cache_read_tokens must sum across candidates like input/output do"
                );
                assert_eq!(
                    *cache_write_tokens,
                    Some(2),
                    "one candidate's None must not zero out the other's reported value"
                );
            }
            other => panic!("expected a Usage event, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `best_of_n: 0` (e.g. from a misconfigured env var) must degrade
    /// gracefully to the same single-call path as the default (`1`),
    /// not panic on an empty candidate vec.
    #[tokio::test]
    async fn best_of_n_set_to_zero_behaves_like_disabled_not_a_panic() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(0);

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk: &str| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "hola");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Deltas from individual best-of-n candidates never reach the
    /// observer live (there's no single "the" answer to show until the
    /// vote resolves one) — only the winner's full text arrives, as one
    /// delta, right after voting.
    #[tokio::test]
    async fn best_of_n_suppresses_live_deltas_but_delivers_the_winners_full_text_once() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("respuesta ".to_string()),
                CompletionEvent::TextDelta("candidata".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("otra ".to_string()),
                CompletionEvent::TextDelta("respuesta".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(2);

        let mut observer = RecordingObserver {
            deltas: Vec::new(),
            events: Vec::new(),
        };
        engine
            .run_turn(&session, "hola", &mut observer)
            .await
            .expect("turn should succeed");

        // Neither candidate's individual deltas streamed live — exactly
        // one delta arrives, carrying the (tied, so earliest-kept)
        // winner's whole text in one shot.
        assert_eq!(observer.deltas, vec!["respuesta candidata".to_string()]);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
