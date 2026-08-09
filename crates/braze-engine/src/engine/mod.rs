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
            max_turn_total_tokens: None,
            max_turn_wall_clock: None,
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
            task_list: std::sync::Mutex::new(crate::task_list::TaskList::default()),
            turn_harness_notes: std::sync::Mutex::new(Vec::new()),
            turn_did_edit: std::sync::atomic::AtomicBool::new(false),
            turn_attempted_edit: std::sync::atomic::AtomicBool::new(false),
            skill_registry: None,
            loaded_skills: std::sync::Mutex::new(Vec::new()),
            skills_max_body_tokens: 1200,
            skills_max_loaded_per_turn: 2,
            textual_rescue_enabled: true,
            envelope_parsing_enabled: false,
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
    // P1.1 paso 1: los tests unitarios de la escalera siguen aquí (ver
    // el module doc de `crate::rescue`); estos dos parsers internos solo
    // los referencian tests, no la producción de este archivo.
    use crate::rescue::{parse_function_xml_tool_call, parse_glm_arg_tag_tool_call};
    // P1.1 pasos 5-6: los tests de context/planner/compactación viven en
    // sus módulos; queda el helper de fallback que los tests de este
    // archivo aún referencian.
    use super::fallback::strip_leaked_tool_call_shapes;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use braze_events::{NoopObserver, TextDeltaObserver};
    use braze_model::ModelError;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
    use braze_types::{ContentBlock, ToolStub};
    use futures::Stream;
    use tokio::sync::Mutex as AsyncMutex;

    use super::test_support::*;

    /// Regression test for A3/B4: a stream that fails mid-round (after
    /// delivering partial text) must propagate as an error from
    /// `run_turn`, and the partial text must never be persisted as if it
    /// were a complete `AssistantText` response.
    #[tokio::test]
    async fn a_mid_stream_error_propagates_and_does_not_persist_partial_text() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let engine = Engine::new(
            Box::new(ErroringModel),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::Model(_))),
            "expected the stream error to propagate as EngineError::Model, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "the partial text from the failed round must never be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-17 (docs/AUDITORIA-2026-07-v2.md): a second
    /// `run_turn` call on the same `Engine` while a first one is still in
    /// flight must be rejected explicitly instead of racing it over the
    /// shared `TaskNotifier` completion channel.
    #[tokio::test]
    async fn a_second_concurrent_run_turn_is_rejected_while_the_first_is_in_flight() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = SlowModel {
            delay: Duration::from_millis(200),
            round: vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        };

        let engine = Arc::new(Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        ));

        let engine_a = Arc::clone(&engine);
        let first = tokio::spawn(async move {
            let mut observer = NoopObserver;
            engine_a.run_turn(&session, "hola", &mut observer).await
        });

        // Give the first call time to acquire the guard before the second
        // one starts.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut observer_b = NoopObserver;
        let result_b = engine
            .run_turn(&session, "hola de nuevo", &mut observer_b)
            .await;
        assert!(
            matches!(result_b, Err(EngineError::ConcurrentTurn)),
            "expected the second call to be rejected, got {result_b:?}"
        );

        let result_a = first.await.expect("first turn's task should not panic");
        assert!(result_a.is_ok(), "the first turn should still succeed");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-24 (docs/AUDITORIA-2026-07-v2.md): a round
    /// with no tool calls whose `stop_reason` reports token-budget
    /// truncation must not be persisted as a normal converged answer.
    #[tokio::test]
    async fn a_truncated_final_response_is_reported_as_an_error_not_a_silent_success() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("partial answer cut off mid".to_string()),
            CompletionEvent::Usage {
                input_tokens: 10,
                output_tokens: 100,
                stop_reason: Some("max_tokens".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
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
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::TruncatedFinalResponse)),
            "expected a truncated final response to be reported as an error, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "the truncated text must never be persisted as a normal final answer"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Una ronda vacía no siempre es el modelo rindiéndose: muchas veces
    /// gastó la ronda entera en un canal que el harness no expone (el
    /// `analysis` de Harmony) y cerró sin emitir nada mapeable. Medido con
    /// gpt-oss:20b en la suite discriminante (2026-07-26), era la fuente
    /// DOMINANTE del ruido de medición — dos de las tres tareas cuyo
    /// resultado oscilaba entre corridas idénticas fallaban así.
    ///
    /// El modelo no puede corregir lo que no sabe: sin la nota, su
    /// siguiente request es idéntico y repetir la ronda es lo esperable.
    #[tokio::test]
    async fn una_ronda_vacia_se_nudgea_y_el_modelo_puede_recuperarse() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Primera ronda: nada mapeable (todo se fue al canal oculto).
            vec![CompletionEvent::Done],
            // Tras el nudge, el modelo responde de verdad.
            vec![
                CompletionEvent::TextDelta("ahora sí, listo".to_string()),
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
        );

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("el turno debe recuperarse tras el nudge, no morir en la ronda vacía");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        // La nota se persiste, para que el turno sea auditable: sin ella,
        // el log mostraría una ronda que desaparece sin explicación.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::HarnessNote { kind, .. } if kind == "empty_round"
            )),
            "debe quedar registro de por qué se repitió la ronda"
        );
        // Y el turno termina con la respuesta real del modelo.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("ahora sí")
            )),
            "la respuesta posterior al nudge debe ser la que cierra el turno"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the "una completion vacía termina el turno
    /// como éxito silencioso" bajo (docs/AUDITORIA-2026-07-v2.md): a
    /// round with no text and no tool calls at all must not be treated as
    /// a legitimate, silent convergence.
    #[tokio::test]
    async fn an_empty_completion_is_reported_as_an_error_not_a_silent_success() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![CompletionEvent::Done]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "expected an empty completion to be reported as an error, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for U-1 (docs/usability-log-template.md, hallado en
    /// vivo 2026-07-07 contra qwen3.5-coder/Nitro): a turn that already
    /// dispatched a successful tool call, then gets a completely empty
    /// round (no text, no tool calls) — the exact shape a real session
    /// hit right after `write_file` succeeded — must recover via the
    /// tools-free summary round instead of discarding that already-done
    /// work behind a hard `EmptyModelResponse` error.
    #[tokio::test]
    async fn an_empty_round_after_a_dispatched_tool_call_recovers_via_the_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Done,
            ],
            // The round right after the tool call comes back with
            // nothing at all — no text, no further tool calls. Ahora el
            // engine responde con un nudge y le devuelve la ronda hasta
            // MAX_EMPTY_ROUND_RETRIES veces; este modelo insiste, que es
            // lo que lleva a la ruta de fallback que el test verifica.
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            // The tools-free summary attempt — reports Usage like any
            // real model round would (H-4: it used to be dropped).
            vec![
                CompletionEvent::TextDelta("listo, ya lo hice".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 900,
                    output_tokens: 15,
                    stop_reason: Some("end_turn".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
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

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected the turn to recover via the summary round, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "listo, ya lo hice"
            )),
            "expected the summary round's text to be persisted, got: {events:?}"
        );
        // The already-dispatched tool call's own result must still be
        // there too — this is the actual work the original bug threw away.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        );
        // H-3 (docs/AUDITORIA-2026-07-v5.md): the fallback being *reached
        // for* is now persisted, independent of whether it went on to
        // produce usable text.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "expected SummaryFallbackAttempted to be persisted, got: {events:?}"
        );
        // H-4 (docs/AUDITORIA-2026-07-v5.md): the fallback's own Usage is
        // persisted too — it re-sends the whole history as its prompt, so
        // dropping it under-reported cost exactly on degraded turns. The
        // fallback round is the only scripted round that reports Usage in
        // this test, so exactly one must land, with its numbers.
        let usage_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => Some((*input_tokens, *output_tokens)),
                _ => None,
            })
            .collect();
        assert_eq!(
            usage_events,
            vec![(900, 15)],
            "the summary fallback's Usage must be persisted, got: {events:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Same shape as the recovery test above, but the tools-free summary
    /// attempt *also* comes back empty. This used to surface
    /// `EmptyModelResponse` — until the memory-distillation smoke
    /// (2026-07-16, gpt-oss:20b/Nitro) showed reasoning models can close
    /// both the round AND the fallback with an empty content channel
    /// while the actual fix is already on disk. The turn must now end Ok
    /// with the tool results preserved and NO fabricated final answer —
    /// while a fallback whose *call itself* dies keeps failing (next
    /// test), so real backend errors don't hide behind this tolerance.
    ///
    /// El tool call es `write_file`, no `echo` (incidente roam #13): la
    /// tolerancia se justifica por trabajo REAL en disco, y con un
    /// stand-in no-mutante el test afirmaba algo más amplio de lo que el
    /// caso motivador sostiene — precisamente el hueco por el que un
    /// turno sin una sola mutación exitosa terminaba en Ok silencioso.
    #[tokio::test]
    async fn an_empty_summary_round_after_a_dispatched_tool_call_ends_the_turn_without_failing() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            // The tools-free summary attempt is ALSO empty.
            vec![CompletionEvent::Done],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected the turn to end Ok with its tool results preserved, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        // The real work the old hard failure threw away must be there.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. })),
            "expected the dispatched tool call's result to be persisted, got: {events:?}"
        );
        // The fallback attempt stays on record (H-3) …
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "expected SummaryFallbackAttempted to be persisted, got: {events:?}"
        );
        // … but nothing may be invented as a final answer: tolerating the
        // empty summary must not fabricate an `AssistantText`.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "an empty summary must not persist a fabricated final answer, got: {events:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Incidente roam #13 (2026-07-20): el reverso del test de arriba.
    /// Un turno que despachó tool calls pero nunca aterrizó una mutación
    /// —observado en vivo: sólo lecturas, dos `edit_file` rechazados—
    /// no tiene nada que preservar. Terminar en `Ok` le dejaba al usuario
    /// una pantalla en blanco: ni respuesta ni error. Ahora sale el
    /// error honesto, que además reporta los tokens generados.
    #[tokio::test]
    async fn an_empty_summary_round_without_any_successful_edit_fails_instead_of_ending_silently() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    // Sólo lectura: el turno "hizo cosas" sin cambiar nada.
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "a turn that changed nothing and said nothing must not report success, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Incidente roam #16 (2026-07-20): el caso que el fix de #13 NO
    /// cubría. El turno intenta una edición que FALLA, aterriza cero, y
    /// el fallback de resumen devuelve texto NO-vacío (razonamiento
    /// disfrazado de respuesta). #13 solo fallaba con resumen vacío; con
    /// texto, el turno terminaba en Ok con un resultado hueco que
    /// envenenaba al siguiente. Ahora `attempted && !landed` lo falla.
    #[tokio::test]
    async fn a_nonempty_summary_after_a_failed_edit_and_no_landed_edit_fails() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Ronda 0: intenta write_file (el provider lo devuelve is_error).
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            // Ronda 1: sin texto ni tool calls → gatilla el fallback.
            // El engine ahora nudgea y devuelve la ronda hasta
            // MAX_EMPTY_ROUND_RETRIES veces; el modelo insiste.
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            // Fallback de resumen: texto NO-vacío (el razonamiento hueco).
            vec![
                CompletionEvent::TextDelta(
                    "We need to add the method. Let's insert it before the tests.".to_string(),
                ),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(FailingWriteToolProvider)]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "a turn that attempted an edit, landed none, and only produced salvaged \
             reasoning must fail, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El contrapeso de regresión de #16: un turno READ-ONLY legítimo que
    /// nunca intenta editar (solo lee) y cuya respuesta llega vía el
    /// fallback de resumen DEBE seguir terminando en Ok. Es exactamente
    /// el caso que un `!turn_did_edit` a secas habría roto — de ahí que
    /// el guard exija `attempted && !landed`, no solo `!landed`.
    #[tokio::test]
    async fn a_nonempty_summary_after_a_read_only_turn_still_ends_ok() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Ronda 0: solo lectura — nunca intenta editar.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            // Fallback: una respuesta real a una pregunta read-only.
            vec![
                CompletionEvent::TextDelta(
                    "The function computes the haversine distance in meters.".to_string(),
                ),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "a read-only turn that answered via the summary fallback must not be failed \
             by the #16 guard, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regresión de U-1: un turno que SÍ aterrizó una edición y luego
    /// cierra vía fallback de resumen con texto sigue en Ok — el guard de
    /// #16 exige que NO haya aterrizado ninguna edición, así que un edit
    /// exitoso lo desactiva por completo.
    #[tokio::test]
    async fn a_nonempty_summary_after_a_successful_edit_still_ends_ok() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            vec![
                CompletionEvent::TextDelta("Added the method and it compiles.".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            // ReadWriteToolProvider's write_file succeeds → turn_did_edit.
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "a turn with a successful edit must stay Ok even if it closed via the summary \
             fallback, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A `ModelBackend` scripted per-call like `ScriptedModel`, except its
    /// N-th call (0-indexed) fails outright at the request level — used to
    /// prove the empty-summary tolerance above does NOT extend to a
    /// fallback whose own model call dies: that shape may be a real
    /// backend failure (auth, network, rate limit) and must keep
    /// surfacing as an error.
    struct ScriptedModelFailingOnCall {
        rounds: AsyncMutex<std::collections::VecDeque<Vec<CompletionEvent>>>,
        fail_on_attempt: u32,
        calls: AtomicU32,
    }

    #[async_trait]
    impl ModelBackend for ScriptedModelFailingOnCall {
        fn name(&self) -> &str {
            "scripted-failing-on-call"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            if attempt == self.fail_on_attempt {
                return Err(ModelError::Request(
                    "simulated backend failure on the summary fallback".to_string(),
                ));
            }
            let mut rounds = self.rounds.lock().await;
            let round = rounds
                .pop_front()
                .unwrap_or_else(|| vec![CompletionEvent::Done]);
            Ok(Box::pin(futures::stream::iter(round.into_iter().map(Ok))))
        }
    }

    /// Same shape again, but the summary fallback's model call itself
    /// fails instead of returning empty — the turn must still surface
    /// `EmptyModelResponse`, not ride the empty-summary tolerance.
    #[tokio::test]
    async fn an_empty_round_still_fails_if_the_summary_rounds_call_itself_dies() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModelFailingOnCall {
            rounds: AsyncMutex::new(
                vec![
                    vec![
                        CompletionEvent::ToolCallRequested {
                            id: "call-1".to_string(),
                            name: "echo".to_string(),
                            arguments: serde_json::json!({ "text": "hola" }),
                        },
                        CompletionEvent::Done,
                    ],
                    // El engine ahora nudgea y devuelve la ronda hasta
                    // MAX_EMPTY_ROUND_RETRIES veces; el modelo insiste.
                    vec![CompletionEvent::Done],
                    vec![CompletionEvent::Done],
                    vec![CompletionEvent::Done],
                ]
                .into_iter()
                .collect(),
            ),
            // Call 0: ronda con tool call. Call 1: ronda vacia. Calls 2 y
            // 3: los dos reintentos con nudge, que el modelo tambien
            // devuelve vacios. Call 4: el summary fallback — muere a nivel
            // de request, que es lo que este test verifica.
            fail_on_attempt: 4,
            calls: AtomicU32::new(0),
        };
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

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "expected a dead fallback call to keep surfacing as an error, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_with_no_tool_calls_streams_text_and_persists_it() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("Hola ".to_string()),
            CompletionEvent::TextDelta("mundo".to_string()),
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
        );

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "Hola mundo");

        // Re-open the same on-disk store to verify persistence directly.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_persists_usage_reported_by_the_backend() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Usage {
                input_tokens: 42,
                output_tokens: 7,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: Some(30),
                cache_write_tokens: Some(5),
                escalation_trigger: None,
            },
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
        );

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        match &events[1] {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                assert_eq!(*input_tokens, 42);
                assert_eq!(*output_tokens, 7);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(*cache_read_tokens, Some(30));
                assert_eq!(*cache_write_tokens, Some(5));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(events[2], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_with_a_tool_call_round_trips_end_to_end() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
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
        );

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "please echo hi",
                &mut TextDeltaObserver(|chunk: &str| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "done");
        // Valid arguments: unchanged behavior — the real tool actually ran,
        // exactly once.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantToolCall { .. }));
        assert!(matches!(events[2], AgentEvent::ToolCallStarted { .. }));
        match &events[3] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.content, "echoed: hi");
                assert!(!result.is_error);
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }
        assert!(matches!(events[4], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Smoke test for `ProtocolValidatingModel`: the exact same scenario as
    /// `run_turn_with_a_tool_call_round_trips_end_to_end` above, but with
    /// the model wrapped in the validator — proves the harness itself is
    /// usable (doesn't false-positive on a normal, well-formed exchange)
    /// before it's relied on by the regression tests below.
    #[tokio::test]
    async fn run_turn_with_a_tool_call_passes_protocol_validation() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]));

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
            .expect("turn should succeed and every request should pass protocol validation");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-4.
    ///
    /// `load_messages_repairs_an_orphaned_tool_use_with_no_result` (below)
    /// proves the repair *happens*, but it seeds the orphan and calls
    /// `load_messages` directly with nothing in between — so the repaired
    /// `ToolCallCompleted` lands right after the orphaned `AssistantToolCall`
    /// and the sequence is trivially valid. That's not what actually
    /// happens in production: `run_turn` appends the turn's new
    /// `UserMessage` *before* calling `load_messages` (see `run_turn`'s
    /// first few lines), so the repair — which only runs inside
    /// `load_messages` — ends up appended *after* that `UserMessage`
    /// instead of immediately after the tool_use it repairs. The resulting
    /// log order (`tool_use`, unrelated `user` text, `tool_result`) is
    /// exactly what `ProtocolValidatingModel` is built to catch, and it
    /// persists to disk — every future resume repeats it.
    ///
    /// Fixed: `run_turn` now calls `repair_session` (which repairs and
    /// persists, discarding the loaded events) *before* appending the
    /// turn's `UserMessage` — see `Engine::run_turn`/`Engine::repair_session`.
    #[tokio::test]
    async fn resuming_after_a_crash_with_an_orphaned_tool_call_stays_protocol_valid() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Simulate a process that crashed between persisting the tool_use
        // and receiving its result: an `AssistantToolCall` with no
        // matching `ToolCallCompleted` anywhere in the log yet.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "please echo hi".to_string(),
                },
            )
            .await
            .expect("seed the original user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
            )
            .await
            .expect("seed an orphaned tool_use — process 'crashed' right here");

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("done".to_string()),
            CompletionEvent::Done,
        ]]));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        // Resuming the session and sending a new message must repair the
        // orphan into a protocol-valid sequence — `ProtocolValidatingModel`
        // panics inside `complete()` if it instead produces the
        // tool_use/user-text/tool_result order the bug creates.
        engine
            .run_turn(&session, "are you still there?", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- oleada 2: Engine::with_planner (PLAN.md § "Split planificador/ejecutor") ---

    /// Degradation rule 3 (espíritu N-24): a plan truncated by the token
    /// budget is discarded — a cut-off plan can mislead mid-step — but
    /// its `Usage` is still persisted: the cost was real.
    #[tokio::test]
    async fn a_truncated_plan_is_discarded_but_its_usage_is_persisted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. paso cortado a mit".to_string()),
            CompletionEvent::Usage {
                input_tokens: 40,
                output_tokens: 1024,
                stop_reason: Some("max_tokens".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the turn must survive a truncated plan");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "a truncated plan must be discarded"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Usage {
                    input_tokens: 40,
                    ..
                }
            )),
            "the planner's Usage must be persisted even when its plan is discarded"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A single-step plan is discarded, not persisted — the executor's
    /// first round covers a trivial request without paying the
    /// plan-in-prompt cost (and without the degeneration artifact the
    /// matrix sweep measured on exactly those tasks).
    #[tokio::test]
    async fn a_single_step_plan_is_discarded_and_the_turn_proceeds_without_it() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. responder al usuario".to_string()),
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed without the plan");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "a single-step plan must not be persisted"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { text } if text == "listo")),
            "the executor must still answer normally"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Records everything the engine mirrors into it, for asserting the
    /// live `TurnObserver` seam (PLAN.md § "Fase TUI — diseño", oleada 1)
    /// sees exactly what gets persisted, in the same order.
    struct RecordingObserver {
        deltas: Vec<String>,
        events: Vec<AgentEvent>,
    }

    impl TurnObserver for RecordingObserver {
        fn on_text_delta(&mut self, delta: &str) {
            self.deltas.push(delta.to_string());
        }
        fn on_event(&mut self, event: &AgentEvent) {
            self.events.push(event.clone());
        }
    }

    /// Variant name only — the mirror test cares about kind and order,
    /// not payload equality.
    fn event_kind(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::UserMessage { .. } => "UserMessage",
            AgentEvent::AssistantText { .. } => "AssistantText",
            AgentEvent::AssistantToolCall { .. } => "AssistantToolCall",
            AgentEvent::ToolCallStarted { .. } => "ToolCallStarted",
            AgentEvent::ToolCallCompleted { .. } => "ToolCallCompleted",
            AgentEvent::CompactionOccurred { .. } => "CompactionOccurred",
            AgentEvent::Usage { .. } => "Usage",
            _ => "Other",
        }
    }

    /// The observer must receive a live mirror of every event the turn
    /// persists, in persistence order, plus the raw text deltas — the
    /// contract `braze-tui` will render from.
    #[tokio::test]
    async fn the_observer_mirrors_every_persisted_event_in_order() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("do".to_string()),
                CompletionEvent::TextDelta("ne".to_string()),
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

        let mut observer = RecordingObserver {
            deltas: Vec::new(),
            events: Vec::new(),
        };
        engine
            .run_turn(&session, "please echo hi", &mut observer)
            .await
            .expect("turn should succeed");

        assert_eq!(observer.deltas, vec!["do", "ne"]);

        // The mirrored sequence must match the persisted log exactly —
        // same kinds, same order, nothing extra and nothing missing.
        let verify_store = FileSessionStore::new(dir.clone());
        let persisted = verify_store.load(&session).await.expect("load events");
        assert_eq!(
            observer.events.iter().map(event_kind).collect::<Vec<_>>(),
            persisted.iter().map(event_kind).collect::<Vec<_>>(),
        );
        assert_eq!(
            observer.events.iter().map(event_kind).collect::<Vec<_>>(),
            vec![
                "UserMessage",
                "AssistantToolCall",
                "ToolCallStarted",
                "ToolCallCompleted",
                "AssistantText",
            ],
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A4: a completion delivered for a handle that
    /// isn't part of the current round's `pending` set (simulating a task
    /// that finally finished after an earlier round already gave up on it
    /// via timeout) must be discarded, not persisted as a second
    /// `ToolCallCompleted` — which would otherwise corrupt the session
    /// with two `tool_result`s for a single `tool_use_id`.
    #[tokio::test]
    async fn stale_completion_from_an_earlier_round_is_discarded_not_persisted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
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
        let notifier = TestNotifier::new();
        // Queued before dispatch even starts, so it is guaranteed to be
        // the first completion `next_completed` yields — exactly the
        // ordering a stale, previously-timed-out task's late delivery
        // would produce.
        notifier.inject_stale_completion("call-1");

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(notifier),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let completions: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
            .collect();
        // Exactly one `ToolCallCompleted` for call-1 — the real one — not
        // two.
        assert_eq!(completions.len(), 1);
        match completions[0] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invalid_args_get_one_round_of_schema_repair_context_then_the_retry_succeeds() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                // First attempt: missing the required `text` field.
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Second attempt (scripted as if the model read the repair
                // context and corrected itself): valid arguments.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
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
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantToolCall { .. }));

        // The rejected call never gets a `ToolCallStarted` (it never
        // reaches dispatch) — its `ToolCallCompleted` follows the
        // `AssistantToolCall` directly, and carries the resolved schema so
        // the model has something concrete to correct itself with.
        match &events[2] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
                // "properties" only appears in the serialized schema dump,
                // never in `jsonschema`'s own error text (which reads
                // along the lines of `"text" is a required property`,
                // singular) — a reliable signal the schema was included.
                assert!(result.content.contains("properties"));
                assert!(result.content.contains("text"));
                // The real tool must never have run for the rejected call.
                assert_ne!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted for call-1, got {other:?}"),
        }

        assert!(matches!(events[3], AgentEvent::AssistantToolCall { .. }));
        assert!(matches!(events[4], AgentEvent::ToolCallStarted { .. }));
        match &events[5] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-2");
                assert!(!result.is_error);
                assert_eq!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted for call-2, got {other:?}"),
        }

        // `invoke` ran exactly once: only for the corrected, valid call.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_second_invalid_call_to_the_same_tool_in_one_turn_gets_no_more_schema_context() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same tool, still invalid — the model didn't correct
                // itself this time.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "wrong_field": 1 }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("giving up".to_string()),
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

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let first_message = match &events[2] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
                assert!(result.content.contains("properties"));
                result.content.clone()
            }
            other => panic!("expected ToolCallCompleted for call-1, got {other:?}"),
        };

        match &events[4] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-2");
                assert!(result.is_error);
                // Second failure of the same tool name this turn: no
                // schema dump this time, and a visibly shorter/different
                // message than the first repair-context one.
                assert!(!result.content.contains("properties"));
                assert_ne!(result.content, first_message);
                assert!(result.content.len() < first_message.len());
            }
            other => panic!("expected ToolCallCompleted for call-2, got {other:?}"),
        }

        // Both calls were rejected before dispatch — `invoke` never ran.
        assert_eq!(invocations.load(Ordering::SeqCst), 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A5: the model repeating an identical
    /// (name, arguments) tool call within the same turn must be nudged
    /// instead of re-dispatched — the dominant non-convergence pattern for
    /// small/local models, which otherwise burn a round (and, in Ollama's
    /// case, real CPU time) re-running a call whose result can't change.
    #[tokio::test]
    async fn an_identical_repeated_tool_call_is_served_from_cache_not_re_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same tool, same arguments, different id — a small model
                // re-issuing the identical call instead of using the
                // result it already has.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
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
        );

        engine
            .run_turn(&session, "please echo hi twice", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        // La invariante que de verdad protege esta palanca: la tool REAL
        // corrió una sola vez. La repetición no se re-despacha (sin efectos
        // secundarios, sin costo repetido).
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let first = match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-1"))
            .expect("expected a ToolCallCompleted for call-1")
        {
            AgentEvent::ToolCallCompleted { result, .. } => result.content.clone(),
            _ => unreachable!(),
        };
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-2"))
            .expect("expected a ToolCallCompleted for call-2")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                // La repetición se responde CON el resultado anterior, no con
                // una negativa. Negarse dejaba al modelo pidiendo algo que el
                // colapso ACI ya le había borrado del contexto: medido contra
                // roam (2026-07-26), gastó 4 llamadas y abandonó el turno.
                assert!(
                    !result.is_error,
                    "servir el resultado cacheado no es un error"
                );
                assert!(
                    result.content.contains(&first),
                    "la repetición debe traer el contenido del resultado original"
                );
                assert!(
                    result.content.contains("caché"),
                    "y debe decir que viene de caché, para que el modelo no crea que re-ejecutó"
                );
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F6 (docs/AUDITORIA-2026-07-v3.md): `read_file(x)` → `write_file(x)`
    /// → `read_file(x)` again is a legitimate re-verification pattern —
    /// the second `read_file` must actually re-run (the write may have
    /// changed what it returns), not get nudged with a now-false "the
    /// result has not changed" claim.
    #[tokio::test]
    async fn a_repeated_read_after_a_mutating_call_actually_redispatches() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let read_args = serde_json::json!({ "path": "x.txt" });
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: read_args.clone(),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "x.txt", "content": "new" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same (name, arguments) as call-1 — but a write happened
                // in between, so this must actually re-run.
                CompletionEvent::ToolCallRequested {
                    id: "call-3".to_string(),
                    name: "read_file".to_string(),
                    arguments: read_args,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let read_invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::clone(
                &read_invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "read x, write x, read x again", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            read_invocations.load(Ordering::SeqCst),
            2,
            "the second read_file, after an intervening write_file, must actually re-run"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-3"))
            .expect("expected a ToolCallCompleted for call-3")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(!result.is_error, "must not be nudged: {result:?}");
                assert_eq!(result.content, "contenido");
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- D5 (docs/AUDITORIA-2026-07-v3.md): narration-without-action
    // across several turns ---

    /// The nudge that already exists (A5) only fires in response to a
    /// *tool call* being repeated — it never catches a model that just
    /// keeps narrating across turns without ever calling a tool. After
    /// `NARRATION_WITHOUT_ACTION_THRESHOLD` (2) such turns, the 3rd turn
    /// must carry an extra reminder alongside the user's own message.
    #[tokio::test]
    async fn narration_without_action_across_several_turns_injects_a_reminder() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("ok, entiendo".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("voy a hacerlo".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("dale, ahora si".to_string()),
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
        );

        engine
            .run_turn(&session, "guarda el archivo", &mut NoopObserver)
            .await
            .expect("turn 1 should succeed");
        engine
            .run_turn(&session, "hazlo ahora", &mut NoopObserver)
            .await
            .expect("turn 2 should succeed");
        engine
            .run_turn(&session, "por favor hazlo", &mut NoopObserver)
            .await
            .expect("turn 3 should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let user_texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::UserMessage { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec![
                "guarda el archivo",
                "hazlo ahora",
                "por favor hazlo",
                "[Reminder] Your last few responses described an intended action without \
                 actually calling the tool for it. If you're being asked to do something, \
                 call the appropriate tool now instead of describing or restating the plan.",
            ],
            "the reminder must appear exactly once, appended right after the 3rd turn's \
             own user message (2 prior narration-only turns crossed the threshold)"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A turn that dispatches a real tool call resets the streak to 0,
    /// even though the model still converges with a plain-text final
    /// answer *in that same turn* (a later round with no tool calls must
    /// not, on its own, count that whole turn as "narration only" — see
    /// `any_tool_calls_this_turn`'s doc comment). Sequence: 1 narration
    /// turn (streak→1), 1 turn that calls a tool then answers in text
    /// (streak must reset, not reach 2), then 2 more narration turns —
    /// without the reset, the 4th turn here would already be the 3rd
    /// *consecutive* narration-only turn and would wrongly carry the
    /// reminder.
    #[tokio::test]
    async fn a_dispatched_tool_call_resets_the_narration_streak() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let invocations = Arc::new(AtomicU32::new(0));
        let model = ScriptedModel::new(vec![
            // Turn 1: narration only (streak 0 -> 1).
            vec![
                CompletionEvent::TextDelta("ok".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 2: dispatches a tool call...
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            // ...then converges with a plain-text final answer in the
            // very same turn — must still reset the streak to 0, not
            // leave/advance it based on this last round alone.
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 3: narration only (streak 0 -> 1, thanks to turn 2's reset).
            vec![
                CompletionEvent::TextDelta("ok".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 4: narration only (streak 1 -> 2) — still below
            // threshold, so this turn must NOT carry the reminder.
            vec![
                CompletionEvent::TextDelta("ok again".to_string()),
                CompletionEvent::Done,
            ],
        ]);

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
            .run_turn(&session, "a", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "b", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "c", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "d", &mut NoopObserver)
            .await
            .unwrap();

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "turn 2's tool call must have actually dispatched"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events.iter().any(
                |e| matches!(e, AgentEvent::UserMessage { text } if text.contains("[Reminder]"))
            ),
            "turn 2's dispatched tool call must have reset the streak — only 2 \
             consecutive narration-only turns (3, 4) followed it, below the threshold"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A7: a hallucinated tool name must not be
    /// dispatched, and the error the model sees must list the tools that
    /// actually exist so a small model has something concrete to
    /// self-correct with, instead of a bare "tool not found".
    #[tokio::test]
    async fn a_hallucinated_tool_name_is_not_dispatched_and_lists_valid_tools() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_files".to_string(), // hallucinated; only "echo" exists
                    arguments: serde_json::json!({}),
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
        );

        engine
            .run_turn(&session, "please read a file", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        // The hallucinated call never reached `invoke`.
        assert_eq!(invocations.load(Ordering::SeqCst), 0);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-1"))
            .expect("expected a ToolCallCompleted for call-1")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(result.is_error);
                assert!(result.content.contains("read_files"));
                assert!(
                    result.content.contains("echo"),
                    "expected the available tool name to be listed, got: {}",
                    result.content
                );
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A6: a turn that never converges within
    /// `MAX_TURN_ITERATIONS` must degrade gracefully — one final
    /// tools-free round asking the model to summarize — instead of
    /// failing outright with nothing to show for it.
    #[tokio::test]
    async fn a_turn_that_never_converges_gets_a_final_tools_free_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let mut rounds: Vec<Vec<CompletionEvent>> = (0..MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("attempt {i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        // One round beyond the cap: the tools-free summary attempt.
        rounds.push(vec![
            CompletionEvent::TextDelta("aqui esta lo que encontre".to_string()),
            CompletionEvent::Done,
        ]);

        let model = ScriptedModel::new(rounds);
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

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully instead of erroring");

        assert_eq!(streamed, "aqui esta lo que encontre");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text == "aqui esta lo que encontre"
        )));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `Engine::with_max_turn_iterations` (v4 P0.2/mitad rondas,
    /// `Config::max_turn_iterations` / `BRAZE_MAX_TURN_ITERATIONS`):
    /// reducing the cap from the default 20 to 2 must make `run_turn`
    /// abort after exactly 2 non-converging rounds and emit the
    /// tools-free summary fallback — instead of the default cap's
    /// 20 rounds. The scripted model below has only 2 tool-call rounds
    /// + 1 fallback round, so if the cap weren't honored the engine
    /// would exhaust its script and panic; that's the failure mode the
    /// test catches by construction.
    #[tokio::test]
    async fn a_lower_max_turn_iterations_caps_the_loop_instead_of_20() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Two tool-call rounds (the new cap), then one fallback summary
        // round — exactly the three rounds a correctly-capped engine
        // needs. Were `max_turn_iterations` still hardcoded at 20, the
        // engine would keep pulling from an exhausted script.
        let rounds: Vec<Vec<CompletionEvent>> = vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-0".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "r0"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "r1"}),
                },
                CompletionEvent::Done,
            ],
            // After the cap: the tools-free summary round that degrades
            // gracefully instead of failing outright.
            vec![
                CompletionEvent::TextDelta("fallback summary".to_string()),
                CompletionEvent::Done,
            ],
        ];

        let model = ScriptedModel::new(rounds);
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
        .with_max_turn_iterations(2);

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully to a summary");

        assert_eq!(streamed, "fallback summary");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        // Two tool-call rounds produced two `AssistantToolCall`s — the
        // cap must have been exactly 2, not the default 20, or else
        //ScriptedModel would have panicked pulling from an exhausted
        // vec and this assertion point would be unreachable.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::AssistantToolCall { .. }))
                .count(),
            2,
            "the cap must stop the loop after exactly 2 tool-call rounds"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text == "fallback summary"
        )));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for U-16 (docs/usability-log-2026-07-07-si2.md):
    /// `z-ai/glm-5.2` kept emitting its native (malformed) `<tool_call>`
    /// syntax even in the tools-free summary round, which explicitly tells
    /// the model no tool is available — before the fix, that leaked block
    /// was persisted verbatim as the turn's final answer instead of being
    /// stripped like it is in every other round.
    #[tokio::test]
    async fn a_leaked_tool_call_in_the_summary_round_is_stripped_not_shown_verbatim() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let mut rounds: Vec<Vec<CompletionEvent>> = (0..MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("attempt {i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta(
                "Esto es lo que encontré.\n<tool_call>read_file<arg_key>path</arg_key>\
                 <arg_value>x.txt</arg_value></tool_call>"
                    .to_string(),
            ),
            CompletionEvent::Done,
        ]);

        let model = ScriptedModel::new(rounds);
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

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully instead of erroring");

        assert!(
            !streamed.contains("<tool_call>"),
            "the leaked block must never reach the user verbatim: {streamed}"
        );
        assert_eq!(streamed, "Esto es lo que encontré.");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C4: an `AssistantToolCall` with no matching
    /// `ToolCallCompleted` anywhere in the log (what an interrupted
    /// process leaves behind — see `dispatch_tool_calls`, which persists
    /// the `tool_use` before dispatch) must be repaired with a synthetic
    /// error result on the next `load_messages`, not left dangling —
    /// otherwise Anthropic rejects every future request against this
    /// session with a permanent 400.
    #[tokio::test]
    async fn load_messages_repairs_an_orphaned_tool_use_with_no_result() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "lee el archivo".to_string(),
                },
            )
            .await
            .expect("seed user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-orphan".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "x.txt"}),
                },
            )
            .await
            .expect("seed orphaned tool_use — process 'crashed' right here");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let messages = engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        // The reconstructed history must be a valid tool_use/tool_result
        // pair, not a dangling tool_use — otherwise Anthropic's API
        // rejects the request outright.
        assert!(messages.iter().any(|m| matches!(
            &m.content[0],
            ContentBlock::ToolResult { tool_use_id, is_error, .. }
                if tool_use_id == "call-orphan" && *is_error
        )));

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let completions: Vec<_> = events
            .iter()
            .filter(
                |e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-orphan"),
            )
            .collect();
        assert_eq!(
            completions.len(),
            1,
            "expected exactly one synthetic repair to be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The repair must be idempotent: calling `load_messages` again after
    /// the first repair must not persist a second `ToolCallCompleted` for
    /// the same id (it now has one, so it's no longer an orphan).
    #[tokio::test]
    async fn repairing_an_orphaned_tool_use_twice_only_persists_one_completion() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-orphan".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "x.txt"}),
                },
            )
            .await
            .expect("seed orphaned tool_use");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("first load_messages should repair the orphan");
        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("second load_messages should be a no-op repair-wise");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let completions = events
            .iter()
            .filter(
                |e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-orphan"),
            )
            .count();
        assert_eq!(completions, 1, "the second call must not repair again");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-6.
    ///
    /// Once a large tool result has settled into `durable_events` (past
    /// the compactor's tactical window), the token-budget estimate must
    /// reflect the *cleared* render actually sent to the model — a short
    /// "[tool result cleared: N chars removed...]" placeholder — not the
    /// raw, uncleared payload. Before this fix, `estimate_prompt_tokens`
    /// measured `durable_events` via `format!("{event:?}")` over the raw
    /// event, so a 5000-char tool result kept counting as ~5000 chars
    /// forever: since compacting only ever folds `tactical` (never
    /// shrinks `durable_events`), `over_token_budget` would stay `true` on
    /// every single `load_messages` call no matter how many times it
    /// compacted — a `CompactionOccurred` appended on every round,
    /// indefinitely, for content that was already small once rendered.
    #[tokio::test]
    async fn a_large_settled_tool_result_does_not_permanently_blow_the_token_budget() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        for event in [
            AgentEvent::UserMessage {
                text: "lee el archivo grande".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "big.txt" }),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "x".repeat(5_000),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "ok, lo leí".to_string(),
            },
            AgentEvent::UserMessage {
                text: "gracias".to_string(),
            },
            AgentEvent::AssistantText {
                text: "de nada".to_string(),
            },
        ] {
            store.append(&session, &event).await.expect("seed event");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            // A small window (2) settles the tool call pair into
            // `durable_events` immediately, exactly like a real session
            // long past its first ~20 events.
            Box::new(SimpleContextCompactor::new(2)),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        // Far below the 5000-char raw content, comfortably above the
        // cleared render (a short placeholder plus a handful of small
        // events) — this is the whole point: the *rendered* size is what
        // must be compared against the budget.
        .with_context_budget(100);

        for _ in 0..3 {
            engine
                .load_messages(&session, &mut NoopObserver)
                .await
                .expect("load_messages should succeed");
        }

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "the cleared render of the settled tool result is well under budget — \
             compaction must not trigger (let alone repeatedly) just because the \
             raw, already-settled payload is large"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- full_observations_byte_budget (hallazgo U-17,
    // docs/usability-log-2026-07-07-si2.md) ---

    // --- tactical_cap_scale (I-2, docs/AUDITORIA-2026-07-v6.md): the
    // caps scale with the budget's VALUE, not its mere presence ---

    /// v4 P0.2 (docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3): a turn
    /// whose cumulative tokens blow the budget stops at the top of the
    /// next iteration — gracefully when the tools-free summary produces
    /// text, as here.
    #[tokio::test]
    async fn a_turn_over_its_token_budget_stops_gracefully_via_the_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let expensive_usage = || CompletionEvent::Usage {
            input_tokens: 90_000,
            output_tokens: 500,
            stop_reason: Some("tool_use".to_string()),
            cache_read_tokens: None,
            cache_write_tokens: None,
            escalation_trigger: None,
        };
        let model = ScriptedModel::new(vec![
            // Round 1: a tool call whose usage alone blows the 50k budget.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                expensive_usage(),
                CompletionEvent::Done,
            ],
            // The tools-free summary attempt (round 2 never runs as a
            // normal round — the breaker fires first).
            vec![
                CompletionEvent::TextDelta("resumen de lo hecho".to_string()),
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
        .with_max_turn_total_tokens(Some(50_000));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected graceful summary recovery, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "resumen de lo hecho"
            )),
            "the summary text must be persisted, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "the breaker goes through the same instrumented fallback path"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Same breaker, but the summary attempt comes back empty — the turn
    /// surfaces `TurnBudgetExhausted` with the real numbers instead of
    /// pretending it converged.
    #[tokio::test]
    async fn a_turn_over_its_token_budget_with_an_empty_summary_errors_with_the_spent_count() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 90_000,
                    output_tokens: 500,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // Empty summary attempt.
            vec![CompletionEvent::Done],
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
        .with_max_turn_total_tokens(Some(50_000));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        match result {
            Err(EngineError::TurnBudgetExhausted {
                budget_tokens,
                spent_tokens,
            }) => {
                assert_eq!(budget_tokens, 50_000);
                assert_eq!(spent_tokens, 90_500);
            }
            other => panic!("expected TurnBudgetExhausted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// round-economics (docs/hypothesis-2026-07-28-round-economics.md):
    /// el presupuesto de wall-clock corta en el BORDE de la ronda, no
    /// abortando la que está en vuelo. La ronda 0 pide un tool que tarda
    /// más que el presupuesto entero; el corte tiene que llegar recién al
    /// intentar la ronda 1, con `rounds_completed = 1` y el tool ya
    /// ejecutado — que es justo lo que un `tokio::time::timeout` de
    /// afuera NO puede dar (mata la ronda en vuelo y pierde su `Usage`,
    /// J-21/J-10).
    #[tokio::test]
    async fn a_turn_over_its_wall_clock_budget_stops_at_the_next_round_boundary() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // La ronda 2 existe en el guion pero no debe llegar a correr:
            // el presupuesto ya se agotó mientras el tool dormía.
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(
                super::test_support::SlowEchoToolProvider::new(
                    Arc::clone(&invocations),
                    Duration::from_millis(80),
                ),
            )]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_wall_clock(Some(Duration::from_millis(30)));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        match result {
            Err(EngineError::TurnWallClockExhausted {
                budget_ms,
                elapsed_ms,
                rounds_completed,
            }) => {
                assert_eq!(budget_ms, 30);
                assert!(
                    elapsed_ms >= 80,
                    "el turno tiene que haber gastado al menos lo que durmió el tool, \
                     got {elapsed_ms} ms"
                );
                assert_eq!(
                    rounds_completed, 1,
                    "la ronda 0 completó entera — el corte es en el borde, no un abort"
                );
            }
            other => panic!("expected TurnWallClockExhausted, got {other:?}"),
        }
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "el tool de la ronda 0 corrió completo antes del corte"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El presupuesto no muerde a un turno que converge dentro de él: sin
    /// esto, el corte sería indistinguible de "toda corrida falla" y el
    /// brazo experimental no mediría nada.
    #[tokio::test]
    async fn a_turn_that_converges_within_its_wall_clock_budget_is_untouched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("listo".to_string()),
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
        .with_max_turn_wall_clock(Some(Duration::from_secs(60)));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("un turno dentro del presupuesto converge normal");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

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

    // --- Paquete B′: hooks audit-only
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte II) ---

    /// A recording hook: appends `(hook_id, what_it_saw)` to a shared
    /// vec — the ordering fixture for the stable-order test.
    struct RecordingHook {
        id: &'static str,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::hooks::EngineHook for RecordingHook {
        fn id(&self) -> &str {
            self.id
        }
        async fn on_event(&self, event: &AgentEvent) -> Result<(), String> {
            if matches!(event, AgentEvent::UserMessage { .. }) {
                self.log.lock().unwrap().push(format!("{}:user", self.id));
            }
            Ok(())
        }
        async fn before_model_request(
            &self,
            _request: &braze_model::CompletionRequest,
        ) -> Result<(), String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:request", self.id));
            Ok(())
        }
    }

    /// A hook that always fails — the degradation fixture.
    struct AlwaysFailingHook;

    #[async_trait::async_trait]
    impl crate::hooks::EngineHook for AlwaysFailingHook {
        fn id(&self) -> &str {
            "always-failing"
        }
        async fn on_event(&self, _event: &AgentEvent) -> Result<(), String> {
            Err("boom".to_string())
        }
    }

    /// Two hooks, registration order — both see the same points, in the
    /// order they were registered (acceptance criterion "orden de hooks
    /// estable y testeado"). Audit-only: the turn's outcome is untouched.
    #[tokio::test]
    async fn hooks_run_in_registration_order_and_see_events_and_requests() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola!".to_string()),
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
        .with_hook(Arc::new(RecordingHook {
            id: "alpha",
            log: Arc::clone(&log),
        }))
        .with_hook(Arc::new(RecordingHook {
            id: "beta",
            log: Arc::clone(&log),
        }));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must succeed with hooks attached");

        let seen = log.lock().unwrap().clone();
        // The user message lands first (persisted before the round), the
        // request dispatch after — each point in registration order.
        assert_eq!(
            seen,
            vec!["alpha:user", "beta:user", "alpha:request", "beta:request"],
            "stable registration order at every point"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Acceptance criteria "hook que falla con warn_and_continue no mata
    /// el turno y emite evento": the turn succeeds, and the failing hook
    /// is disabled after its third consecutive failure with exactly one
    /// persisted `HookErrored`.
    #[tokio::test]
    async fn a_persistently_failing_hook_is_disabled_with_one_event_and_the_turn_survives() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Three persisted events (UserMessage, Usage, AssistantText) —
        // exactly enough on_event failures to cross the disable
        // threshold of 3 within one turn.
        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola!".to_string()),
            CompletionEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
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
        .with_hook(Arc::new(AlwaysFailingHook));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("a failing audit-only hook must never kill the turn");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let hook_errors: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::HookErrored { .. }))
            .collect();
        assert_eq!(
            hook_errors.len(),
            1,
            "exactly one HookErrored — at the disable crossing, not per failure: {events:#?}"
        );
        match hook_errors[0] {
            AgentEvent::HookErrored { id, reason, .. } => {
                assert_eq!(id, "always-failing");
                assert_eq!(reason, "boom");
            }
            other => panic!("expected HookErrored, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `PromptBudgetAuditHook` on a real turn: pure smoke — read-only by
    /// construction, must never fail or alter the outcome.
    #[tokio::test]
    async fn the_prompt_budget_audit_hook_is_inert_on_a_normal_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola!".to_string()),
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
        .with_hook(Arc::new(crate::hooks::PromptBudgetAuditHook));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("audit hook must be inert");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::HookErrored { .. })),
            "the audit hook must never error"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.2):
    /// crossing 80% of the turn budget injects ONE `HarnessNote` into the
    /// conversation — the announced deadline a small model needs to
    /// converge instead of exploring until the breaker kills the turn.
    #[tokio::test]
    async fn crossing_80_percent_of_the_token_budget_emits_one_harness_note() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Round 1: a tool call + usage at 85% of the budget.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 80_000,
                    output_tokens: 5_000,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // Round 2: converges with text, still under the budget.
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
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
        .with_max_turn_total_tokens(Some(100_000));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge normally");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let notes: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::HarnessNote { .. }))
            .collect();
        assert_eq!(notes.len(), 1, "exactly one note, not one per round");
        match notes[0] {
            AgentEvent::HarnessNote { kind, text } => {
                assert_eq!(kind, "turn_budget");
                assert!(text.contains("85000"), "carries the real numbers: {text}");
                assert!(text.contains("100000"), "carries the budget: {text}");
            }
            other => panic!("expected HarnessNote, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The `no-harness-notes` ablation: same turn shape, no note — the
    /// A/B braze-bench runs to attribute any pass-rate delta to the
    /// deadline having been announced.
    #[tokio::test]
    async fn the_harness_notes_ablation_silences_the_budget_note() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 80_000,
                    output_tokens: 5_000,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
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
        .with_max_turn_total_tokens(Some(100_000))
        .with_harness_notes_enabled(false);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge normally");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::HarnessNote { .. })),
            "ablated engine must emit no notes"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Minimal `read_file` stand-in for the re-read nudge test: always
    /// succeeds, so the nudge is the only thing distinguishing the
    /// fourth result from the first three.
    struct StubReadFileProvider;

    #[async_trait]
    impl ToolProvider for StubReadFileProvider {
        fn provider_id(&self) -> &str {
            "stub-read"
        }
        async fn list_stubs(&self) -> Result<Vec<braze_types::ToolStub>, ToolError> {
            Ok(vec![braze_types::ToolStub {
                name: "read_file".to_string(),
                summary: "read a file".to_string(),
                source: "stub".to_string(),
                input_schema: None,
            }])
        }
        async fn resolve_schema(
            &self,
            name: &str,
        ) -> Result<Option<braze_tools_core::ToolSchema>, ToolError> {
            Ok((name == "read_file").then(|| braze_tools_core::ToolSchema {
                name: "read_file".to_string(),
                description: "read a file".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }))
        }
        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "linea a\nlinea b\n".to_string(),
                is_error: false,
            })
        }
    }

    /// Regression test for the roam #6 incident (2026-07-20): a model
    /// that re-reads the same file with slightly different windows
    /// evades the exact-args repeated-call guard. Observed twice in
    /// production: 5 and 10 reads of one 103-line file, zero edits,
    /// until the turn's cap killed it. The nudge must ride the FOURTH
    /// read's successful result — appended, never blocking (chunked
    /// reads of a big file are legitimate).
    #[tokio::test]
    async fn re_reading_one_file_without_editing_appends_a_nudge() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Four reads of the same path with DIFFERENT windows — exactly
        // the shape that slips past `seen_calls`.
        let mut rounds: Vec<Vec<CompletionEvent>> = (0..4)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({
                            "path": "notas.txt",
                            "offset": i * 3 + 1,
                            "limit": 10
                        }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]);

        let engine = Engine::new(
            Box::new(ScriptedModel::new(rounds)),
            ToolRegistry::new(vec![Box::new(StubReadFileProvider)]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "revisa notas.txt", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let nudged: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallCompleted { result, .. }
                    if result.content.contains("without editing it") =>
                {
                    Some(&result.content)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            nudged.len(),
            1,
            "exactly the fourth read must carry the nudge: {events:#?}"
        );
        assert!(
            nudged[0].contains("notas.txt") && !nudged[0].starts_with("[harness]"),
            "the nudge is appended to the real content, not a replacement: {}",
            nudged[0]
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the roam #5 incident (2026-07-20): with a
    /// realistic cap, a convergence note must arrive with REAL runway
    /// (past 70% of the cap), not only the one-round warning at the
    /// edge. In production a 20B model burned 17 rounds re-reading a
    /// file it had broken, got the last-round note at round 19 of 20,
    /// and spent it on a repeated grep.
    #[tokio::test]
    async fn a_convergence_note_arrives_with_runway_before_the_last_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // 10 tool-calling rounds then text: the cap is 10, so the
        // convergence note must fire at round 7 (70%) — three rounds of
        // runway — and the last-round note still at round 9.
        let mut rounds: Vec<Vec<CompletionEvent>> = (0..9)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("r{i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]);
        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(ScriptedModel::new(rounds)),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_iterations(10);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let kinds: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::HarnessNote { kind, .. } => Some(kind.clone()),
                _ => None,
            })
            .collect();
        assert!(
            kinds.iter().any(|k| k == "iteration_converge"),
            "expected a convergence note with runway, got: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| *k == "iteration_converge").count(),
            1,
            "the convergence note must fire exactly once"
        );
        // Y sigue llegando el aviso de última ronda: son dos funciones
        // distintas, no un reemplazo.
        assert!(
            kinds.iter().any(|k| k == "iteration_cap"),
            "the last-round note must still fire: {kinds:?}"
        );
        // Sin edición previa en el turno (echo no es mutante), el consejo
        // es el original: arregla con un edit decisivo.
        let convergence_text = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::HarnessNote { kind, text } if kind == "iteration_converge" => {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("convergence note present");
        assert!(
            convergence_text.contains("fix it with one decisive edit"),
            "a turn that never edited must get the decisive-edit advice: {convergence_text}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the roam #10 incident (2026-07-20, tarea 2 del
    /// testbed): when the turn has ALREADY landed a successful edit, the
    /// convergence note must not tell the model to "fix it with one
    /// decisive edit" — that advice sent gpt-oss:20b back to re-read a
    /// file it had already fixed, after an `old_string not found` error
    /// that only meant the change was already applied. With a prior edit
    /// the advice inverts: verify once and answer.
    #[tokio::test]
    async fn the_convergence_note_says_verify_when_the_turn_already_edited() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Round 0 writes (successful mutation), rounds 1..8 read, round 9
        // answers. Cap 10 ⇒ the convergence note fires at round 7, well
        // after the edit landed.
        let mut rounds: Vec<Vec<CompletionEvent>> = vec![vec![
            CompletionEvent::ToolCallRequested {
                id: "call-w".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": "a.rs" }),
            },
            CompletionEvent::Done,
        ]];
        rounds.extend((0..8).map(|i| {
            vec![
                CompletionEvent::ToolCallRequested {
                    id: format!("call-r{i}"),
                    name: "read_file".to_string(),
                    // Rutas distintas: aísla este test de la nota de
                    // relectura improductiva, que es otra palanca.
                    arguments: serde_json::json!({ "path": format!("f{i}.rs") }),
                },
                CompletionEvent::Done,
            ]
        }));
        rounds.push(vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]);

        let engine = Engine::new(
            Box::new(ScriptedModel::new(rounds)),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_iterations(10);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let convergence_text = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::HarnessNote { kind, text } if kind == "iteration_converge" => {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("convergence note present");
        assert!(
            convergence_text.contains("already applied at least one successful edit"),
            "a turn that edited must get the verify-and-close advice: {convergence_text}"
        );
        assert!(
            !convergence_text.contains("fix it with one decisive edit"),
            "the decisive-edit advice must NOT survive a landed edit: {convergence_text}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The iteration-cap note fires exactly once, right before the final
    /// round — "round N of N is your last" — so a model that would blow
    /// the cap gets one explicit chance to answer instead.
    #[tokio::test]
    async fn the_penultimate_round_emits_the_iteration_cap_note() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
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
        .with_max_turn_iterations(2);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge in round 2");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let notes: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::HarnessNote { .. }))
            .collect();
        assert_eq!(notes.len(), 1);
        match notes[0] {
            AgentEvent::HarnessNote { kind, text } => {
                assert_eq!(kind, "iteration_cap");
                assert!(text.contains("round 2 of 2"), "got: {text}");
            }
            other => panic!("expected HarnessNote, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `true` when any message in `req` carries a Text block containing
    /// `needle` — for the J-3/J-4 request-scoping assertions below.
    fn any_message_text_contains(req: &CompletionRequest, needle: &str) -> bool {
        req.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains(needle)))
        })
    }

    /// J-3 (docs/AUDITORIA-2026-07-v7.md): a harness note reaches the
    /// remaining rounds of ITS OWN turn (as an ephemeral trailing user
    /// message) but is gone from the next turn's requests — before this,
    /// the persisted event rendered from history and a stale "answer now,
    /// stop calling tools" from turn 1 stayed a live instruction for the
    /// whole session (the single-turn bench never saw the failure mode).
    #[tokio::test]
    async fn a_harness_note_is_scoped_to_its_own_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                // Turn 1, round 1: a tool call + usage at 85% of budget →
                // the budget note fires.
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": "hola" }),
                    },
                    CompletionEvent::Usage {
                        input_tokens: 80_000,
                        output_tokens: 5_000,
                        stop_reason: Some("tool_use".to_string()),
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        escalation_trigger: None,
                    },
                    CompletionEvent::Done,
                ],
                // Turn 1, round 2: converges.
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
                // Turn 2, round 1: converges immediately.
                vec![
                    CompletionEvent::TextDelta("listo de nuevo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };
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
        .with_max_turn_total_tokens(Some(100_000));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn 1 must converge");
        engine
            .run_turn(&session, "otra cosa", &mut NoopObserver)
            .await
            .expect("turn 2 must converge");

        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 3, "2 rounds in turn 1 + 1 in turn 2");
            assert!(
                any_message_text_contains(&requests[1], "[harness]"),
                "turn 1's round 2 must still see its own note (the A\u{2032}.2 mechanism)"
            );
            assert!(
                !any_message_text_contains(&requests[2], "[harness]"),
                "turn 2 must NOT see turn 1's stale note (J-3)"
            );
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// J-4 (docs/AUDITORIA-2026-07-v7.md): the task list is turn state —
    /// an entry left `pending` when its turn ends must not re-inject the
    /// summary into the next turn's requests (before this, plans of
    /// unrelated turns mixed and per-round cost grew monotonically with
    /// the session).
    #[tokio::test]
    async fn a_pending_task_does_not_leak_into_the_next_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                // Turn 1, round 1: adds a task (stays pending forever).
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "task_add".to_string(),
                        arguments: serde_json::json!({ "description": "leer el archivo" }),
                    },
                    CompletionEvent::Done,
                ],
                // Turn 1, round 2: converges without finishing the task.
                vec![
                    CompletionEvent::TextDelta("me rindo".to_string()),
                    CompletionEvent::Done,
                ],
                // Turn 2, round 1: converges immediately.
                vec![
                    CompletionEvent::TextDelta("tema nuevo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };
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
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn 1 must converge");
        engine
            .run_turn(&session, "otra cosa", &mut NoopObserver)
            .await
            .expect("turn 2 must converge");

        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 3, "2 rounds in turn 1 + 1 in turn 2");
            assert!(
                any_message_text_contains(&requests[1], "Task list:"),
                "turn 1's round 2 must see its own open task"
            );
            assert!(
                !any_message_text_contains(&requests[2], "Task list:"),
                "turn 2 must NOT inherit turn 1's abandoned pending task (J-4)"
            );
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `None` (the default) never trips, no matter how much a turn spends
    /// — zero behavior change for existing callers.
    #[tokio::test]
    async fn without_a_token_budget_an_expensive_turn_completes_normally() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("respuesta".to_string()),
            CompletionEvent::Usage {
                input_tokens: 5_000_000,
                output_tokens: 100,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
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
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(result.is_ok(), "got {result:?}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `+ablate:no-prune` (opencode ítem 2): with the collapse disabled,
    /// an old observation far beyond the full-observations window renders
    /// FULL — no "[old observation collapsed:" marker anywhere.
    #[tokio::test]
    async fn with_collapse_disabled_old_observations_render_full() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // 8 tool-call pairs (16 events) — with TACTICAL_FULL_OBSERVATIONS
        // = 5, the oldest 3 observations would normally collapse. Each
        // observation is large enough that collapsing saves space (the
        // collapse is skipped for tiny contents).
        for i in 0..8 {
            let id = format!("call-{i}");
            store
                .append(
                    &session,
                    &AgentEvent::AssistantToolCall {
                        id: id.clone(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": format!("f{i}.txt")}),
                    },
                )
                .await
                .expect("seed call");
            store
                .append(
                    &session,
                    &AgentEvent::ToolCallCompleted {
                        id: id.clone(),
                        result: braze_types::ToolResult {
                            tool_call_id: id,
                            content: format!("contenido {i}\n{}", "línea\n".repeat(100)),
                            is_error: false,
                        },
                    },
                )
                .await
                .expect("seed result");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_observation_collapse_enabled(false);

        let messages = engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let rendered: String = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                braze_types::ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("[old observation collapsed:"),
            "collapse disabled: every observation must render full"
        );
        assert!(
            rendered.contains("contenido 0"),
            "the oldest observation's real content must be present"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Confirms the generic permissive schema `braze-model` sends to the
    /// model today (`{"type":"object","additionalProperties":true}`) would
    /// not itself reject arbitrary arguments if it were ever validated
    /// against — the new validation in `dispatch_tool_calls` is exactly as
    /// strict as the real resolved schema says and no stricter, it doesn't
    /// introduce false rejections for that permissive case.
    #[test]
    fn generic_permissive_schema_accepts_arbitrary_arguments() {
        let schema = serde_json::json!({"type": "object", "additionalProperties": true});
        let instance = serde_json::json!({"cualquier_cosa": 123});
        assert!(jsonschema::validate(&schema, &instance).is_ok());
    }

    // --- coerce_arguments_to_schema (hallazgo F2) ---

    fn limit_and_flag_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "integer"},
                "ratio": {"type": "number"},
                "recursive": {"type": "boolean"},
            },
        })
    }

    #[test]
    fn coerces_a_stringified_integer_to_a_number() {
        let mut args = serde_json::json!({"path": "x", "limit": "50"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["limit"], serde_json::json!(50));
    }

    #[test]
    fn coerces_a_stringified_float_to_a_number() {
        let mut args = serde_json::json!({"ratio": "0.5"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["ratio"], serde_json::json!(0.5));
    }

    #[test]
    fn coerces_stringified_booleans() {
        let mut args = serde_json::json!({"recursive": "true"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["recursive"], serde_json::json!(true));

        let mut args = serde_json::json!({"recursive": "false"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["recursive"], serde_json::json!(false));
    }

    #[test]
    fn an_unparseable_string_is_left_untouched_for_validation_to_reject() {
        let mut args = serde_json::json!({"limit": "not a number"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["limit"], serde_json::json!("not a number"));
    }

    #[test]
    fn a_json_object_is_re_serialized_to_a_string_when_the_schema_wants_one() {
        // The mirror-image mistake: `<parameter=path>{"a":1}</parameter>`
        // parses as a JSON object because the XML grammar treats any
        // value starting with `{`/`[` as structured — but the schema says
        // `path` is a string.
        let mut args = serde_json::json!({"path": {"a": 1}});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["path"], serde_json::json!(r#"{"a":1}"#));
    }

    #[test]
    fn already_correctly_typed_arguments_are_left_alone() {
        // The common case (wire-sourced calls): coercion must be a no-op.
        let mut args = serde_json::json!({
            "path": "x", "limit": 50, "ratio": 0.5, "recursive": true
        });
        let before = args.clone();
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args, before);
    }

    #[test]
    fn a_string_value_for_a_string_field_is_left_alone() {
        let mut args = serde_json::json!({"path": "src/main.rs"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["path"], serde_json::json!("src/main.rs"));
    }

    #[test]
    fn a_non_object_schema_or_arguments_is_a_no_op() {
        let mut args = serde_json::json!("not an object");
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args, serde_json::json!("not an object"));

        let mut args = serde_json::json!({"limit": "50"});
        coerce_arguments_to_schema(&mut args, &serde_json::json!({"type": "object"}));
        assert_eq!(args["limit"], serde_json::json!("50"));
    }

    // --- try_parse_textual_tool_call (hallazgo B5) ---

    #[test]
    fn parses_a_bare_json_tool_call() {
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "read_file", "arguments": {"path": "x.txt"}}"#)
                .expect("should parse");
        assert_eq!(rescued.name, "read_file");
        assert_eq!(rescued.arguments, serde_json::json!({"path": "x.txt"}));
    }

    #[test]
    fn parses_a_tool_call_fenced_in_json_code_block() {
        let text = "```json\n{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}\n```";
        let rescued = try_parse_textual_tool_call(text).expect("should parse");
        assert_eq!(rescued.name, "echo");
    }

    #[test]
    fn parses_a_tool_call_fenced_in_a_bare_code_block() {
        let text = "```\n{\"name\": \"echo\", \"arguments\": {}}\n```";
        let rescued = try_parse_textual_tool_call(text).expect("should parse");
        assert_eq!(rescued.name, "echo");
    }

    #[test]
    fn accepts_parameters_as_a_synonym_for_arguments() {
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "echo", "parameters": {"text": "hi"}}"#)
                .expect("should parse");
        assert_eq!(rescued.arguments, serde_json::json!({"text": "hi"}));
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_a_tool_call() {
        assert!(try_parse_textual_tool_call("El archivo tiene 3 lineas.").is_none());
    }

    #[test]
    fn json_without_a_name_field_is_not_a_tool_call() {
        assert!(try_parse_textual_tool_call(r#"{"arguments": {"path": "x.txt"}}"#).is_none());
    }

    #[test]
    fn non_object_arguments_are_rejected() {
        assert!(
            try_parse_textual_tool_call(r#"{"name": "echo", "arguments": "just a string"}"#)
                .is_none()
        );
    }

    // --- F1 (docs/AUDITORIA-2026-07-v3.md): reject OpenAI-style tool
    // *definitions* masquerading as a call via `parameters` ---

    #[test]
    fn an_openai_style_tool_definition_is_not_mistaken_for_a_call() {
        let text = r#"{"name": "get_weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}"#;
        assert!(
            try_parse_textual_tool_call(text).is_none(),
            "a JSON-Schema-shaped `parameters` must not be treated as arguments"
        );
    }

    #[test]
    fn a_genuine_object_typed_argument_named_type_is_still_accepted() {
        // Must not over-trigger: a real argument object that happens to
        // have a `type` field but no `properties` is not a schema.
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "set_status", "arguments": {"type": "busy"}}"#)
                .expect("should still parse — no `properties` key present");
        assert_eq!(rescued.arguments, serde_json::json!({"type": "busy"}));
    }

    // --- F1: fenced examples are not real leaked tool calls ---

    #[test]
    fn a_tagged_call_inside_a_markdown_fence_is_not_executed() {
        let text = "Así es como Qwen emite tool calls:\n```\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"/etc/shadow\"}}\n</tool_call>\n```\n";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_bare_function_xml_inside_a_markdown_fence_is_not_executed() {
        let text = "Ejemplo:\n```\n<function=read_file>\n<parameter=path>\n/etc/shadow\n</parameter>\n</function>\n```\n";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    /// J-8 (docs/AUDITORIA-2026-07-v7.md): the pythonic rung was the only
    /// one missing the F1 fence check — a fenced `[func(...)]` example
    /// ("así emite Llama sus tool calls") got extracted and dispatched
    /// for real.
    #[test]
    fn a_pythonic_call_inside_a_markdown_fence_is_not_executed() {
        let text = "Así emite Llama sus tool calls:\n```\n[get_weather(city=\"SF\")]\n```\n";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unfenced_pythonic_call_after_fenced_prose_is_still_rescued() {
        let text = "Ejemplo:\n```\nsolo texto\n```\n[echo(text=\"hi\")]";
        let (calls, _) = extract_pythonic_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "the real call after the fence must still rescue"
        );
    }

    #[test]
    fn an_unfenced_tagged_call_after_fenced_prose_is_still_rescued() {
        // The fence-toggle logic must correctly track state across
        // multiple fences, not just detect "any fence exists somewhere".
        let text = "Aquí un ejemplo:\n```\nesto es solo texto\n```\n<tool_call>\n{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}\n</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "the real call after the fence must still rescue"
        );
    }

    #[test]
    fn each_rescued_call_gets_a_distinct_id() {
        let a = try_parse_textual_tool_call(r#"{"name": "echo", "arguments": {}}"#).unwrap();
        let b = try_parse_textual_tool_call(r#"{"name": "echo", "arguments": {}}"#).unwrap();
        assert_ne!(a.id, b.id);
    }

    // --- extract_tagged_tool_calls (formato nativo Qwen/Hermes, ítem 2
    // del backlog 2026-07-06) ---

    #[test]
    fn extracts_a_single_qwen_tagged_tool_call() {
        let text = "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"x.txt\"}}\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert!(remaining.is_empty());
    }

    #[test]
    fn extracts_several_tagged_calls_from_one_response() {
        // Qwen emits one pair of tags per call for parallel calls.
        let text = concat!(
            "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a\"}}\n</tool_call>\n",
            "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"b\"}}\n</tool_call>",
        );
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "a"}));
        assert_eq!(calls[1].arguments, serde_json::json!({"path": "b"}));
        assert!(remaining.is_empty());
        assert_ne!(calls[0].id, calls[1].id);
    }

    #[test]
    fn prose_around_a_tagged_call_is_preserved_as_the_round_text() {
        let text = "Voy a leer el archivo.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"x\"}}\n</tool_call>\nListo.";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "Voy a leer el archivo.\n\nListo.");
    }

    #[test]
    fn a_fenced_json_inside_the_tags_still_parses() {
        let text = "<tool_call>```json\n{\"name\": \"echo\", \"arguments\": {}}\n```</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn a_malformed_tagged_block_stays_in_the_text_instead_of_being_swallowed() {
        let text = "<tool_call>\n{\"name\": \"echo\", \"arguments\": no-es-json}\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_malformed_block_next_to_a_valid_one_keeps_only_the_malformed_text() {
        let text = concat!(
            "<tool_call>{broken</tool_call>",
            "<tool_call>{\"name\": \"echo\", \"arguments\": {}}</tool_call>",
        );
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "<tool_call>{broken</tool_call>");
    }

    #[test]
    fn an_unclosed_tag_rescues_nothing_and_keeps_the_text_intact() {
        // E.g. a round cut off mid-block: better visible than lost.
        let text = "algo de texto <tool_call>\n{\"name\": \"echo\"";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_valid_block_followed_by_an_unclosed_tag_keeps_the_dangling_tail() {
        let text =
            "<tool_call>{\"name\": \"echo\", \"arguments\": {}}</tool_call> y <tool_call>{\"na";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "y <tool_call>{\"na");
    }

    #[test]
    fn plain_prose_without_tags_is_not_rescued_by_the_tagged_extractor() {
        let (calls, remaining) = extract_tagged_tool_calls("El archivo tiene 3 lineas.");
        assert!(calls.is_empty());
        assert_eq!(remaining, "El archivo tiene 3 lineas.");
    }

    #[test]
    fn tagged_extraction_accepts_parameters_as_a_synonym_for_arguments() {
        let text =
            "<tool_call>{\"name\": \"echo\", \"parameters\": {\"text\": \"hi\"}}</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"text": "hi"}));
    }

    // --- gramática XML <function=...> de qwen3-coder (extensión del
    // ítem 2, destrancada 2026-07-06 al haber qwen3.5-coder en Nitro) ---

    /// The exact shape qwen3-coder's chat template documents: XML-ish
    /// tags, parameter values on their own lines, wrapped in the same
    /// `<tool_call>` tags qwen2.5 uses around JSON.
    #[test]
    fn function_xml_inside_tool_call_wrapper_parses() {
        let text = "<tool_call>\n<function=read_file>\n<parameter=path>\nx.txt\n</parameter>\n</function>\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert!(remaining.is_empty());
    }

    #[test]
    fn bare_function_xml_with_prose_around_parses_and_preserves_the_prose() {
        let text = "Voy a leerlo.\n<function=read_file>\n<parameter=path>\nx.txt\n</parameter>\n</function>\ndespués te cuento";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert_eq!(remaining, "Voy a leerlo.\n\ndespués te cuento");
    }

    #[test]
    fn function_xml_with_several_parameters_collects_them_all() {
        let text = "<function=edit_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n<parameter=old_string>\nlet x = 1;\n</parameter>\n<parameter=new_string>\nlet x = 2;\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(call.name, "edit_file");
        assert_eq!(
            call.arguments,
            serde_json::json!({
                "path": "src/main.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;",
            })
        );
    }

    /// The whole point of the XML grammar: code-carrying values need no
    /// JSON escaping — inner quotes/braces arrive verbatim as a string.
    #[test]
    fn function_xml_keeps_code_carrying_values_as_verbatim_strings() {
        let text = "<function=write_file>\n<parameter=path>\na.json\n</parameter>\n<parameter=content>\nfn main() { println!(\"{:?}\", vec![1]); }\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(
            call.arguments["content"],
            serde_json::json!("fn main() { println!(\"{:?}\", vec![1]); }")
        );
    }

    /// Scalar-looking values stay strings ("42" must not become 42 —
    /// a `path: String` schema downstream would reject the number),
    /// while a clearly structured value (`{...}`) is parsed.
    #[test]
    fn function_xml_coerces_only_clearly_structured_values() {
        let text = "<function=echo>\n<parameter=text>\n42\n</parameter>\n<parameter=options>\n{\"deep\": true}\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(call.arguments["text"], serde_json::json!("42"));
        assert_eq!(call.arguments["options"], serde_json::json!({"deep": true}));
    }

    #[test]
    fn malformed_function_xml_stays_in_the_text() {
        // Missing </parameter> close.
        let text = "<function=echo>\n<parameter=text>\nhola\n</function>";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn function_xml_without_parameters_is_a_zero_argument_call() {
        let call =
            parse_function_xml_tool_call("<function=list_tools>\n</function>").expect("parses");
        assert_eq!(call.name, "list_tools");
        assert_eq!(call.arguments, serde_json::json!({}));
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_function_xml() {
        let (calls, remaining) =
            extract_function_xml_tool_calls("la función f(x) = x + 1 es creciente");
        assert!(calls.is_empty());
        assert_eq!(remaining, "la función f(x) = x + 1 es creciente");
    }

    // --- gramática <arg_key>/<arg_value> de z-ai/glm-5.2 (hallazgo U-15,
    // docs/usability-log-2026-07-07-si2.md — observada 2026-07-07 vía
    // OpenRouter) ---

    /// The exact shape observed leaking from `z-ai/glm-5.2`: no
    /// `<function=...>` wrapper, just the bare name followed by
    /// `<arg_key>`/`<arg_value>` pairs, all inside the same `<tool_call>`
    /// tags qwen2.5/qwen3-coder use.
    #[test]
    fn glm_arg_tags_inside_tool_call_wrapper_parses() {
        let text = "<tool_call>read_file<arg_key>limit</arg_key><arg_value>120</arg_value><arg_key>offset</arg_key><arg_value>63</arg_value><arg_key>path</arg_key><arg_value>crates/braze-bench/src/backend_spec.rs</arg_value></tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({
                "limit": "120",
                "offset": "63",
                "path": "crates/braze-bench/src/backend_spec.rs",
            })
        );
        assert!(remaining.is_empty());
    }

    #[test]
    fn glm_arg_tags_with_prose_around_are_preserved() {
        let text = "Voy a leerlo.\n<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>\ndespués te cuento";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert_eq!(remaining, "Voy a leerlo.\n\ndespués te cuento");
    }

    /// Scalar-looking values stay strings, same rule as the qwen3-coder
    /// XML rescue — a `path: String` schema downstream must not receive a
    /// JSON number just because the raw text looked numeric.
    #[test]
    fn glm_arg_tags_coerce_only_clearly_structured_values() {
        let text = "<tool_call>echo<arg_key>text</arg_key><arg_value>42</arg_value><arg_key>options</arg_key><arg_value>{\"deep\": true}</arg_value></tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("42"));
        assert_eq!(
            calls[0].arguments["options"],
            serde_json::json!({"deep": true})
        );
    }

    #[test]
    fn glm_arg_tags_without_any_arg_key_are_not_mistaken_for_the_grammar() {
        // No `<arg_key>` at all: indistinguishable from prose that merely
        // mentions a tool by name — must fall through unrescued rather
        // than being guessed at as a zero-argument call.
        assert!(parse_glm_arg_tag_tool_call("read_file").is_none());
    }

    #[test]
    fn malformed_glm_arg_tags_stay_in_the_text() {
        // Missing the closing </arg_value>.
        let text = "<tool_call>echo<arg_key>text</arg_key><arg_value>hola</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    // --- extract_pythonic_tool_calls (hallazgo C2, Llama's native format) ---

    #[test]
    fn parses_a_single_pythonic_call() {
        let (calls, remaining) =
            extract_pythonic_tool_calls(r#"[get_weather(city="SF", metric="celsius")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"city": "SF", "metric": "celsius"})
        );
        assert_eq!(remaining, "");
    }

    #[test]
    fn pythonic_call_preserves_surrounding_prose() {
        let text = r#"Claro, reviso el clima.[get_weather(city="SF")]Listo."#;
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "Claro, reviso el clima.Listo.");
    }

    #[test]
    fn pythonic_call_parses_numbers_and_booleans() {
        let (calls, _) =
            extract_pythonic_tool_calls("[read_file(path=\"a.txt\", offset=5, recursive=true)]");
        assert_eq!(calls[0].arguments["offset"], serde_json::json!(5));
        assert_eq!(calls[0].arguments["recursive"], serde_json::json!(true));
    }

    #[test]
    fn pythonic_call_parses_floats() {
        let (calls, _) = extract_pythonic_tool_calls("[set_ratio(value=0.5)]");
        assert_eq!(calls[0].arguments["value"], serde_json::json!(0.5));
    }

    #[test]
    fn several_pythonic_calls_in_one_bracket_are_all_parsed() {
        let (calls, remaining) = extract_pythonic_tool_calls(r#"[echo(text="a"), echo(text="b")]"#);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a"));
        assert_eq!(calls[1].arguments["text"], serde_json::json!("b"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn pythonic_call_without_arguments_is_a_zero_argument_call() {
        let (calls, _) = extract_pythonic_tool_calls("[list_tools()]");
        assert_eq!(calls[0].name, "list_tools");
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn a_comma_inside_a_quoted_argument_does_not_split_the_call() {
        let (calls, _) = extract_pythonic_tool_calls(r#"[echo(text="a, b, c")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a, b, c"));
    }

    #[test]
    fn a_bracket_inside_a_quoted_argument_does_not_close_the_block_early() {
        let (calls, remaining) = extract_pythonic_tool_calls(r#"[echo(text="a] b")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a] b"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_a_pythonic_call() {
        // No literal brackets around the call — must not match (this is
        // exactly the ambiguity the bracket-wrapper requirement avoids).
        let text = "puedes llamar a leer(archivo) para revisar el contenido";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_ordinary_list_literal_is_not_mistaken_for_a_call() {
        // `[1, 2, 3]` has no `identifier(` right after the `[`.
        let text = "los valores son [1, 2, 3] en ese orden";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unrecognized_argument_shape_leaves_the_whole_block_in_the_text() {
        // A nested list value isn't in scope (string/number/bool only) —
        // the whole call must be left untouched, not partially rescued.
        let text = "[echo(items=[1, 2])]";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unclosed_pythonic_bracket_stays_in_the_text() {
        let text = "[get_weather(city=\"SF\"";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    // --- strip_leaked_tool_call_shapes (hallazgo U-16,
    // docs/usability-log-2026-07-07-si2.md: attempt_tools_free_summary_round
    // had no rescue logic at all, so a leaked tool-call block there used
    // to get persisted verbatim as if it were the model's real answer) ---

    #[test]
    fn a_leaked_tagged_call_with_no_other_text_strips_to_empty() {
        let text =
            "<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>";
        assert_eq!(strip_leaked_tool_call_shapes(text), "");
    }

    #[test]
    fn a_leaked_call_alongside_real_prose_keeps_only_the_prose() {
        let text = "Basado en lo que leí hasta ahora, el fix consiste en...\n<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>";
        assert_eq!(
            strip_leaked_tool_call_shapes(text),
            "Basado en lo que leí hasta ahora, el fix consiste en..."
        );
    }

    #[test]
    fn plain_prose_with_no_leaked_call_is_returned_unchanged() {
        let text = "No hay nada raro acá, solo texto normal.";
        assert_eq!(strip_leaked_tool_call_shapes(text), text);
    }

    /// Regression test for the rescue escalera's ordering: a `<tool_call>`
    /// tagged block must win even if the response also happens to contain
    /// bracketed text that looks pythonic-shaped elsewhere.
    #[tokio::test]
    async fn a_llama_pythonic_call_is_rescued_end_to_end_when_no_structured_call_arrives() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Voy a revisar.[echo(text=\"hi\")]".to_string()),
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

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the pythonic call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Voy a revisar.")
            )),
            "the surrounding prose must be persisted as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("echo(")
            )),
            "the bracketed call must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- synthesize_orphan_repairs (N-26, docs/AUDITORIA-2026-07-v2.md) ---

    #[test]
    fn synthesize_orphan_repairs_finds_a_tool_use_with_no_matching_result() {
        let events = vec![
            AgentEvent::UserMessage {
                text: "please echo hi".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": "hi" }),
            },
        ];

        let repairs = synthesize_orphan_repairs(&events);
        assert_eq!(repairs.len(), 1);
        match &repairs[0] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_orphan_repairs_is_a_no_op_when_every_call_already_has_a_result() {
        let events = vec![
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "echoed: hi".to_string(),
                    is_error: false,
                },
            },
        ];

        assert!(synthesize_orphan_repairs(&events).is_empty());
    }

    /// Regression test for B5: a model that emits the tool call as plain
    /// text (no structured `tool_calls` entry — the failure mode for
    /// small/local models or templates without native tool-call support)
    /// must still have the tool actually run, and the raw JSON must not
    /// be persisted as if it were a normal conversational reply.
    #[tokio::test]
    async fn a_tool_call_emitted_as_plain_text_is_rescued_and_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
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

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the rescued call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the raw JSON must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Ítem 2 del backlog (2026-07-06): a model emitting its native
    /// Qwen/Hermes `<tool_call>{json}</tool_call>` tagged format — with
    /// prose around it — must have the call dispatched, the prose kept
    /// as the round's text, and the tags/JSON stripped from what's
    /// persisted as conversation.
    #[tokio::test]
    async fn a_qwen_tagged_tool_call_with_surrounding_prose_is_rescued_and_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Voy a usar echo.\n<tool_call>\n".to_string()),
                CompletionEvent::TextDelta(
                    r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
                ),
                CompletionEvent::TextDelta("\n</tool_call>".to_string()),
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

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the tagged call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        // The prose survives as round text; the tags and JSON don't.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Voy a usar echo.")
            )),
            "the surrounding prose must be persisted as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("<tool_call>") || text.contains("\"name\"")
            )),
            "the tagged block must not be persisted as conversational text"
        );
        // H-3 (docs/AUDITORIA-2026-07-v5.md): the rescue actually
        // happening was already visible via `tracing::info!` before this
        // — this pins that it's also persisted, bench-countable.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::TextualRescueApplied { parser } if parser.contains("Qwen/Hermes")
            )),
            "a rescued <tool_call> block must persist TextualRescueApplied naming that rung"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
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
