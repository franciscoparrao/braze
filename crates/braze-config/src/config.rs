use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api_key::ApiKey;
use crate::error::ConfigError;
use crate::file;
use crate::overrides::ConfigOverrides;
use crate::paths;

/// Minimal description of an MCP server to connect to by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfigStub {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Runtime command for `post_edit_check` to run after a matching file is
/// edited (v4 P1.6: generalizes the Rust-only `cargo check` guardrail
/// into a declarative map of `extensions → command`, so any stack gets
/// the same feedback loop — `ruff check`, `prettier --check`,
/// `tsc --noEmit`, etc.). Each command runs from the edited file's
/// parent directory (`cargo` walks ancestors looking for `Cargo.toml`;
/// `ruff`/`prettier`/`tsc` likewise walk up to their project roots), so
/// no separate `cwd` strategy is required. Failure posture preserved:
/// the guardrail only ever *adds* feedback — missing binary, timeout,
/// non-zero exit with no `error:` lines all silently skip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatterConfig {
    /// Command + args to run after a matching file is edited. Executed
    /// with `cwd = edited_file.parent()`. First element is the program
    /// (resolved via `PATH`); rest are args.
    pub command: Vec<String>,
    /// File extensions (with leading dot, lowercase — matching is
    /// case-insensitive on the extension) that this formatter applies
    /// to. `[".rs"]` for Rust; `[".py", ".pyi"]` extends to `.pyi`
    /// stubs too.
    pub extensions: Vec<String>,
    /// Timeout in seconds — same semantics as `CHECK_TIMEOUT` (original
    /// hardcoded const), silent skip past this. Defaults to 60.
    #[serde(default = "default_formatter_timeout_secs")]
    pub timeout_secs: u64,
    /// `true` skips this entry (preserves the opt-out behavior of
    /// `Config::disable_post_edit_check` granularly per-entry instead of
    /// blanket-off).
    #[serde(default)]
    pub disabled: bool,
}

fn default_formatter_timeout_secs() -> u64 {
    60
}

/// Precio de API para un (backend, familia de modelos) — la pieza que
/// faltaba para computar `estimated_cost_usd` en braze-bench (E5/v4
/// "métricas nuevas", docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3)
/// y para enforcear `expect_max_cost_usd` (v4 P0.4, parseado sin
/// enforcement hasta ahora).
///
/// El matching es por backend exacto + prefijo de modelo (ver
/// [`Config::pricing_for`]): `model_prefix: ""` es el catch-all de un
/// backend (útil para Ollama, donde TODO modelo local factura $0 en
/// términos de API), y una entrada más específica
/// (`"deepseek/deepseek-v4-flash"`) le gana al catch-all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// `"ollama"`, `"anthropic"` o `"openrouter"` — debe coincidir exacto
    /// con el nombre de backend del spec.
    pub backend: String,
    /// Prefijo del nombre de modelo que esta entrada cubre. `""` matchea
    /// todos los modelos del backend; entre varias entradas que
    /// matchean, gana la de prefijo más largo.
    #[serde(default)]
    pub model_prefix: String,
    /// USD por millón de tokens de input (sin cachear).
    pub input_usd_per_mtok: f64,
    /// USD por millón de tokens de output.
    pub output_usd_per_mtok: f64,
    /// USD por millón de tokens de input servidos desde cache — `None`
    /// cuando el proveedor no reporta caching o el precio no se conoce
    /// (esos tokens se facturan como input normal en la estimación).
    #[serde(default)]
    pub cache_read_usd_per_mtok: Option<f64>,
    /// USD por millón de tokens escritos a cache (premium sobre input).
    /// Mismo contrato `None` que `cache_read_usd_per_mtok`.
    #[serde(default)]
    pub cache_write_usd_per_mtok: Option<f64>,
}

/// Un directorio de referencia fuera del working directory
/// (opencode-10, docs/opencode-a-braze.md § 10) — el equivalente braze
/// de las `references` de OpenCode, en su versión mínima:
///
/// - `path` se agrega como raíz extra del `WorkdirAllowlist`
///   ([`braze_permissions::WorkdirAllowlist::with_extra_root`] es el
///   seam que existía para exactamente esto), así el clasificador trata
///   acciones ahí como dentro del workdir en vez de pedir confirmación
///   por cada lectura.
/// - `description` (si está) se anuncia en el system prompt: "un SLM no
///   sabe dónde buscar" — decirle qué hay en ese directorio es steering
///   barato. Una referencia SIN descripción queda permitida pero no
///   anunciada — el equivalente funcional del `hidden: true` de
///   OpenCode, que por eso no se replica como campo aparte.
///
/// Un `path` relativo se resuelve contra el cwd de la sesión (misma
/// regla que el resto del allowlist) — para un config global como
/// `~/.config/braze/config.json`, conviene usar rutas absolutas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceConfig {
    pub path: PathBuf,
    #[serde(default)]
    pub description: Option<String>,
}

/// Configuración de skills locales (D′,
/// docs/harness-engineering-hooks-skills-2026-07-10.md § Parte III):
/// `paths` es una ALLOWLIST deliberada de directorios con `SKILL.md` —
/// vacía (el default) la feature queda apagada; NO se apuntan acá los
/// directorios de skills de un entorno frontier tal cual (en un 3B son
/// distractores — el estudio pide cuerpos SLM-native). Los caps existen
/// porque el contexto es presupuesto: `max_body_tokens` corta cada body
/// inyectado, `max_loaded_per_turn` acota cuántas skills puede cargar la
/// mención de un solo turno.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default = "default_skills_max_body_tokens")]
    pub max_body_tokens: usize,
    #[serde(default = "default_skills_max_loaded_per_turn")]
    pub max_loaded_per_turn: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            max_body_tokens: default_skills_max_body_tokens(),
            max_loaded_per_turn: default_skills_max_loaded_per_turn(),
        }
    }
}

/// El cap del estudio (§ config propuesta): ~1200 tokens por body —
/// una skill más larga está escrita para un frontier, no para un SLM.
fn default_skills_max_body_tokens() -> usize {
    1200
}

fn default_skills_max_loaded_per_turn() -> usize {
    2
}

/// Tabla default, fechada **2026-07-09** — los precios de API envejecen;
/// al agregar un modelo nuevo a los sweeps, agregar su entrada acá (o en
/// el config file) con el precio vigente. Un modelo sin entrada produce
/// `estimated_cost_usd: None` (sin estimación), nunca un precio
/// inventado.
fn default_model_pricing() -> Vec<ModelPricing> {
    vec![
        // Inferencia local: $0 de API por definición (el costo real es
        // hardware/energía, fuera del alcance de esta métrica).
        ModelPricing {
            backend: "ollama".to_string(),
            model_prefix: String::new(),
            input_usd_per_mtok: 0.0,
            output_usd_per_mtok: 0.0,
            cache_read_usd_per_mtok: None,
            cache_write_usd_per_mtok: None,
        },
        // Verificado contra openrouter.ai/deepseek/deepseek-v4-flash el
        // 2026-07-09 — el modelo OpenRouter recomendado del proyecto
        // (CLAUDE.md § "Modelo recomendado vía OpenRouter").
        ModelPricing {
            backend: "openrouter".to_string(),
            model_prefix: "deepseek/deepseek-v4-flash".to_string(),
            input_usd_per_mtok: 0.09,
            output_usd_per_mtok: 0.18,
            cache_read_usd_per_mtok: None,
            cache_write_usd_per_mtok: None,
        },
    ]
}

/// `pub` (not `pub(crate)`) so `braze-tools-local` — which already
/// depends on this crate for `FormatterConfig` itself — can build its
/// own `LocalToolsProvider`-level default from the same single
/// definition instead of hardcoding a second copy of the `cargo check`
/// command that could drift out of sync (found duplicated in the
/// other-model commit `2923f63`, audited 2026-07-09). See
/// `braze_tools_local::post_edit_check::default_rust_formatters`.
pub fn default_formatters() -> Vec<FormatterConfig> {
    vec![FormatterConfig {
        command: vec![
            "cargo".to_string(),
            "check".to_string(),
            "--quiet".to_string(),
            "--message-format=short".to_string(),
        ],
        extensions: vec![".rs".to_string()],
        timeout_secs: default_formatter_timeout_secs(),
        disabled: false,
    }]
}

/// `#[serde(default = "...")]` needs a named function, not a literal —
/// used by fields (like `enable_prompt_caching`) whose sane default is
/// `true`, unlike the `disable_*`-style flags elsewhere in this struct
/// that default to `false` via the plain `#[serde(default)]` shorthand.
fn default_true() -> bool {
    true
}

/// Fully-resolved `braze` configuration.
///
/// Built by layering, in increasing priority: hardcoded defaults ([`Config::default`]),
/// the on-disk config file, `BRAZE_*` environment variables, and finally
/// explicit overrides applied via [`Config::apply_overrides`] (used by
/// `braze-cli` for parsed CLI flags, from Fase 5 onward).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Which `ModelBackend` to use by default: `"anthropic"` or `"ollama"`.
    pub default_backend: String,
    /// Anthropic API key. Never hardcoded — comes from the config file or
    /// `BRAZE_ANTHROPIC_API_KEY`. `ApiKey` (N-39,
    /// docs/AUDITORIA-2026-07-v2.md), not a plain `String`, so this
    /// struct's `derive(Debug, Serialize)` can never leak the raw key.
    #[serde(default)]
    pub anthropic_api_key: Option<ApiKey>,
    /// Anthropic model name (e.g. `"claude-opus-4-6-20260805"`). No
    /// default: if the user selects `default_backend = "anthropic"` and
    /// never configures this, that is a clear startup error, not a guessed
    /// value.
    #[serde(default)]
    pub anthropic_model: Option<String>,
    /// Base URL for a local Ollama instance.
    pub ollama_base_url: String,
    /// Ollama model name.
    pub ollama_model: String,
    /// Context window requested from Ollama via `options.num_ctx`. Without
    /// an explicit value, Ollama falls back to its Modelfile default
    /// (commonly 2048-4096) and silently truncates an over-budget prompt
    /// from the front — no error, just a model that "forgot" its system
    /// prompt and tools mid-turn. See `braze-model::OllamaBackend`.
    pub ollama_num_ctx: u32,
    /// Send the tool inventory in the system prompt and parse tool calls
    /// client-side (the textual rescue ladder) instead of Ollama's native
    /// `tools` field. `false` (default) uses native tool-calling. `true`
    /// sidesteps Ollama's server-side tool-call parser, whose
    /// harmony-channel handling on gpt-oss returns HTTP 500 ("error
    /// parsing tool call") under long multi-turn contexts on Ollama
    /// < 0.32.1 (incidente roam #1). No effect on non-Ollama backends.
    pub ollama_prompt_tools: bool,
    /// Sampling temperature for the Ollama backend (`options.temperature`).
    /// `None` (the default) leaves `OllamaBackend`'s own default (0.2,
    /// biased toward well-formed tool calls) in place. These five sampling
    /// fields existed as `OllamaBackend::with_*` setters and as
    /// `braze-bench` CLI flags, but were never wired into a real `braze
    /// chat`/`braze run` invocation — so a sampling regime a bench sweep
    /// found better for a given model (e.g. Qwen's own recommended temp
    /// 0.7/top_p 0.8/top_k 20/repeat_penalty 1.05) could be *measured* but
    /// never actually *used* in production (docs/AUDITORIA-2026-07-v3.md,
    /// hallazgo D2).
    #[serde(default)]
    pub ollama_temperature: Option<f32>,
    /// Ollama `options.seed`, for reproducible sampling — see
    /// `ollama_temperature`.
    #[serde(default)]
    pub ollama_seed: Option<u64>,
    /// Ollama `options.top_p` — see `ollama_temperature`.
    #[serde(default)]
    pub ollama_top_p: Option<f32>,
    /// Ollama `options.top_k` — see `ollama_temperature`.
    #[serde(default)]
    pub ollama_top_k: Option<u32>,
    /// Ollama `options.repeat_penalty` — see `ollama_temperature`.
    #[serde(default)]
    pub ollama_repeat_penalty: Option<f32>,
    /// OpenRouter API key. Never hardcoded — comes from the config file or
    /// `BRAZE_OPENROUTER_API_KEY`. `ApiKey`, same rationale as
    /// `anthropic_api_key`.
    #[serde(default)]
    pub openrouter_api_key: Option<ApiKey>,
    /// OpenRouter model identifier (e.g.
    /// `"anthropic/claude-3.5-sonnet"`). No default: if the user selects
    /// `default_backend = "openrouter"` and never configures this, that is
    /// a clear startup error, not a guessed value.
    #[serde(default)]
    pub openrouter_model: Option<String>,
    /// Base URL for the OpenRouter API. Configurable (unlike Anthropic's
    /// hardcoded endpoint) so this backend can also target a self-hosted
    /// OpenAI-compatible gateway or a corporate mirror. NOTE: unlike
    /// Ollama, `braze` does not budget context per OpenRouter model —
    /// OpenRouter routes to models with widely varying context windows,
    /// and consuming its `/models` catalog to size that budget dynamically
    /// is out of scope for now. A model with a small context window may
    /// truncate or fail server-side without a client-side warning.
    pub openrouter_base_url: String,
    /// Default max tokens for a model completion request.
    pub max_tokens: u32,
    /// System prompt sent with every request. `None` (the default) means
    /// `braze-cli` uses its own built-in default, which includes anti-loop
    /// guidance and the working directory — see
    /// `braze-cli::default_system_prompt`. Overridable so a user can
    /// tailor it (e.g. add domain-specific instructions, or work around a
    /// particular small model's quirks) without recompiling.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Directory where `braze-session` writes its rollout logs.
    pub session_dir: PathBuf,
    /// Number of raw tactical events `SimpleContextCompactor` always keeps
    /// verbatim in the live conversational window — see
    /// `braze_session::SimpleContextCompactor::new`. Previously hardcoded
    /// (C10, docs/AUDITORIA-2026-07.md); the right size depends on the
    /// backend's context window (Anthropic's large context can afford a
    /// wider raw window than Ollama's small, fixed `num_ctx`), so it's
    /// configurable rather than a single constant for every backend.
    pub tactical_window: usize,
    /// Number of raw tactical events above which `Engine::run_turn`
    /// triggers a compaction pass — see
    /// `braze_engine::DEFAULT_TACTICAL_COMPACTION_THRESHOLD`'s doc
    /// comment. Previously hardcoded (C10, same rationale as
    /// `tactical_window`).
    pub tactical_compaction_threshold: usize,
    /// MCP servers to connect to by default (consumed by `braze-mcp-client`, Fase 4).
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfigStub>,
    /// Independent candidates `Engine` generates per round before voting
    /// on which one to use — técnica G10, docs/AUDITORIA-2026-07.md
    /// (Best-of-n / Test-Time Scaling). `1` (the default) disables it:
    /// the round takes the exact single-call path that existed before
    /// G10. See `braze_engine::Engine::with_best_of_n`.
    pub best_of_n: usize,
    /// Color preset for `braze chat --tui`: `"dark"`, `"light"`, or
    /// `"high-contrast"` — see `braze_tui::Theme`. Not validated at this
    /// layer (`braze-config` doesn't depend on `braze-tui`) — `braze-cli`
    /// resolves it via `Theme::from_name` and errors at startup on an
    /// unrecognized name, same as `default_backend`.
    pub tui_theme: String,
    /// Disables `Engine`'s textual tool-call rescue (B5,
    /// docs/AUDITORIA-2026-07.md) — N-15 (docs/AUDITORIA-2026-07-v2.md):
    /// the rescue is purely syntactic, so a user literally asking to see
    /// the JSON for a *real* tool name gets that example dispatched for
    /// real. `false` (the default) preserves the existing behavior. See
    /// `braze_engine::Engine::with_textual_rescue_enabled`.
    #[serde(default)]
    pub disable_textual_tool_call_rescue: bool,
    /// Whether OpenRouter requests carry explicit `cache_control` markers
    /// for models that need one to cache at all (Anthropic/Qwen — see
    /// `braze_model::openrouter_wire::model_supports_explicit_caching`).
    /// `true` by default: almost every `braze` turn is multi-round
    /// tool-calling, so the ~25% premium on the first request that
    /// establishes a cache entry is recovered many times over across the
    /// rest of the turn (docs/usability-log-2026-07-07-si2.md,
    /// prompt-caching design — measured live: 481,714 cumulative input
    /// tokens across one 40-round investigation turn, with zero caching).
    /// Every other provider OpenRouter routes to (OpenAI, DeepSeek,
    /// Moonshot/Kimi, Grok, Gemini 2.5) caches automatically server-side
    /// regardless of this flag — it only changes the request bytes sent
    /// for Anthropic/Qwen models. See
    /// `braze_model::openrouter::OpenRouterBackend::with_prompt_caching_enabled`.
    #[serde(default = "default_true")]
    pub enable_prompt_caching: bool,
    /// Disables the post-edit `cargo check` guardrail
    /// (`braze-tools-local`, ítem 5 del backlog 2026-07-06): after a
    /// successful `write_file`/`edit_file` on a `.rs` file inside a
    /// Cargo project, compile errors are fed back to the model in the
    /// same tool result (ACI, arXiv 2405.15793: -3.0 pp without it).
    /// `false` (the default) keeps the guardrail on; set this when the
    /// project's `cargo check` is too slow to run per-edit.
    #[serde(default)]
    pub disable_post_edit_check: bool,
    /// Backend for the optional planner model (PLAN.md § "Split
    /// planificador/ejecutor"): `"anthropic"`, `"ollama"` or
    /// `"openrouter"`. `None` (the default) disables the split entirely.
    /// Like `default_backend`, the name is validated by `braze-cli` at
    /// startup, not at this layer.
    #[serde(default)]
    pub planner_backend: Option<String>,
    /// Model name for the planner backend. `None` falls back to the same
    /// per-backend model resolution the primary backend uses
    /// (`anthropic_model`/`ollama_model`/`openrouter_model`) — so
    /// `planner_backend = "openrouter"` alone plans with the configured
    /// OpenRouter model.
    #[serde(default)]
    pub planner_model: Option<String>,
    /// Backend for the reactive lead/worker escalation (estilo Goose,
    /// ítem 6 del backlog 2026-07-06): the lead opens the session and
    /// returns while the primary backend strings failed observations
    /// together. `None` (the default) disables the decorator. Same
    /// validation posture as `planner_backend`.
    #[serde(default)]
    pub lead_backend: Option<String>,
    /// Model name for the lead backend — same fallback semantics as
    /// `planner_model`.
    #[serde(default)]
    pub lead_model: Option<String>,
    /// How many opening calls of a session the lead handles proactively
    /// before the worker takes over (`EscalatingBackend::with_lead_turns`,
    /// Goose's `GOOSE_LEAD_TURNS`). `None` (the default) uses the
    /// decorator's own default (3) — the value lives in
    /// `braze-model::escalation`, not duplicated here. **`Some(0)` is the
    /// purely-reactive mode**: the lead only ever enters when the worker
    /// visibly flounders. Exposed per I-1 (docs/AUDITORIA-2026-07-v6.md):
    /// with no way to set this, every `+lead:` A/B ran the proactive
    /// 3-turn opening while claiming to measure reactive escalation.
    #[serde(default)]
    pub lead_turns: Option<usize>,
    /// Consecutive failed observations that trigger a reactive escalation
    /// to the lead (`with_failure_threshold`; the decorator clamps 0 up
    /// to 1). `None` uses the decorator's default (2). Same I-1 rationale
    /// as `lead_turns`.
    #[serde(default)]
    pub lead_failure_threshold: Option<usize>,
    /// How many calls the lead handles per escalation episode before the
    /// worker resumes (`with_escalation_turns`; clamped to at least 1).
    /// `None` uses the decorator's default (3). Same I-1 rationale.
    #[serde(default)]
    pub lead_escalation_turns: Option<usize>,
    /// Tope de iteraciones agentic por turno antes de forzar una respuesta
    /// text-only (`Engine::run_turn`'s `MAX_TURN_ITERATIONS`). Default 20,
    /// el histórico valor hardcoded; acá expuesto (v4 P0.2/mitad rondas)
    /// porque el óptimo depende de la capacidad del modelo — un SLM que
    /// converge en ~3-5 rondas para `single_tool` pero se enreda en 15-20
    /// benefit del tope más alto, mientras `distactor_selection` con 20
    /// ya evidence floundering. Override por backend/familia en `Engine`
    /// vía `EngineBuilder::with_max_turn_iterations`.
    #[serde(default = "default_max_turn_iterations")]
    pub max_turn_iterations: u32,
    /// Tope de tokens output para una ronda del planner
    /// (`Engine::attempt_planning_round`'s `PLANNER_MAX_TOKENS`). Default
    /// 1024 — el valor hardcoded previo. Acá expuesto para apretar/aflojar
    /// el presupuesto del plan sin recompilar, mismo razonamiento que
    /// `max_turn_iterations`.
    #[serde(default = "default_planner_max_tokens")]
    pub planner_max_tokens: u32,
    /// Circuit breaker por consumo acumulado por turno (v4 P0.2,
    /// docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3): tope de tokens
    /// totales (input + output sumados entre rondas) que un solo turno
    /// puede gastar antes de que el engine corte con un resumen graceful
    /// (`Engine::with_max_turn_total_tokens`). `None` (el default) =
    /// deshabilitado. `max_turn_iterations` corta por RONDAS; esto corta
    /// por TOKENS — un turno de pocas rondas puede acumular cientos de
    /// miles de tokens de input re-enviando una historia creciente (caso
    /// real: 481K en 40 rondas, sesión ccd4621b).
    #[serde(default)]
    pub max_turn_total_tokens: Option<u64>,
    /// Per-tool-result byte budget before `LocalToolsProvider::wrap`
    /// truncates and appends an actionable "narrow your query" trailer
    /// (v4 P2.4). Default 8000 — the historical `MAX_TOOL_OUTPUT_BYTES`
    /// const, tuned for Ollama's `num_ctx=8192`. A larger context window
    /// or a paper sweep that needs more output per round can override
    /// here (`BRAZE_TOOL_OUTPUT_MAX_BYTES`).
    #[serde(default = "default_tool_output_max_bytes")]
    pub tool_output_max_bytes: u32,
    /// Per-tool-result line budget (v4 P2.4). `None` (the default) is
    /// **not** a cap — only the byte cap applies. Setting this makes
    /// `truncate_output` additionally truncate at `max_lines` lines if
    /// the byte cap hasn't already hit; useful for outputs that are
    /// many short lines (a `grep -r` over thousands of files) where a
    /// byte-only cap can still show 100k+ lines before triggering.
    #[serde(default)]
    pub tool_output_max_lines: Option<u32>,
    /// Post-edit command map: per file extension, the command to run
    /// after `write_file`/`edit_file` lands (v4 P1.6 — generalizes
    /// `post_edit_check.rs`'s previously Rust-only `cargo check`).
    /// Defaults to the bare Rust `cargo check --quiet
    /// --message-format=short` entry — equivalent to the hardcoded
    /// behavior before this field existed. `disable_post_edit_check`
    /// (still honored) blanket-skips every entry when true.
    #[serde(default = "default_formatters")]
    pub formatters: Vec<FormatterConfig>,
    /// Precios de API por (backend, prefijo de modelo) — ver
    /// [`ModelPricing`] y [`Config::pricing_for`]. Defaults en
    /// [`default_model_pricing`] (fechados; los precios envejecen).
    #[serde(default = "default_model_pricing")]
    pub model_pricing: Vec<ModelPricing>,
    /// Directorios de referencia fuera del working directory
    /// (opencode-10, docs/opencode-a-braze.md § 10): cada `path` entra al
    /// `WorkdirAllowlist` (lecturas/escrituras ahí clasifican como dentro
    /// del workdir), y los que traen `description` se anuncian en el
    /// system prompt — el steering "aquí hay docs del API en ../docs"
    /// que un SLM no puede inferir solo. Ver [`ReferenceConfig`].
    #[serde(default)]
    pub references: Vec<ReferenceConfig>,
    /// C′.1 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.3):
    /// stubs por provider sobre los cuales sus tools no se listan al
    /// modelo y quedan detrás del meta-tool `search_tools` del engine.
    /// El caso objetivo son gateways MCP grandes (cientos-miles de
    /// tools) contra un `num_ctx` local chico; las 6 tools locales nunca
    /// se difieren con el default.
    #[serde(default = "default_tool_search_threshold")]
    pub tool_search_threshold: usize,
    /// C′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.4):
    /// expone las tools `task_add`/`task_update` y re-inyecta el resumen
    /// compacto de la lista por ronda; con planner activo, el plan
    /// siembra la lista en vez de persistirse como prosa. OFF por
    /// default — dos tools extra son distractores potenciales para un
    /// SLM; se promueve solo si su A/B lo valida.
    #[serde(default)]
    pub enable_task_list: bool,
    /// I.7 — explorador de contexto aislado (tool `explore`,
    /// `docs/explorador-aislado-ab-design.md`). OFF por default, mismo
    /// posicionamiento que `enable_task_list`: la palanca entra apagada
    /// y se promueve solo si su A/B pre-registrado la valida.
    #[serde(default)]
    pub enable_exploration: bool,
    /// E′ I.6 (docs/harness-engineering-hooks-skills-2026-07-10.md):
    /// anexa al system prompt un snapshot del entorno (branch + git
    /// status recortado + fecha + OS) generado por el composition root —
    /// el modelo no gasta rondas de shell_exec en orientarse. OFF por
    /// default: el contexto es presupuesto (con num_ctx chico cada línea
    /// compite), y el bench lo deja siempre off (sandbox sin git; N-36
    /// exige que el bench siga al default de producción).
    #[serde(default)]
    pub environment_block: bool,
    /// docs/project-memory-design.md: anexa al system prompt un resumen
    /// determinístico entre sesiones (archivos tocados, tareas
    /// completadas vía la lista tipada) persistido en
    /// `.braze/memory.json` bajo la raíz del proyecto (git toplevel, o
    /// `cwd` si no hay repo). OFF por default — mismo posicionamiento
    /// que `enable_task_list`: una palanca nueva entra apagada y se
    /// promueve solo si su propio A/B (`+ablate:project-memory`) la
    /// valida, no por asunción.
    #[serde(default)]
    pub enable_project_memory: bool,
    /// v8 § 6 (docs/AUDITORIA-2026-07-v8.md): summary-por-lead — cuando
    /// hay `--lead` configurado, la compactación le pide el summary de
    /// los eventos dropeados al modelo del lead (una llamada tools-free
    /// con cap de tokens) en vez de usar solo el digest extractivo
    /// determinístico; ante cualquier fallo (error, timeout, texto
    /// vacío) cae al digest — nunca peor que hoy. OFF por default —
    /// mismo posicionamiento que `enable_task_list`: la palanca entra
    /// apagada y se promueve solo si su propia fila del bench
    /// (`+ablate:lead-summary`) la valida.
    #[serde(default)]
    pub enable_lead_summary: bool,
    /// D′ — skills locales explicit-only; ver [`SkillsConfig`]. Solo
    /// desde el config file (estructurado, como `references`).
    #[serde(default)]
    pub skills: SkillsConfig,
}

/// Espejo de `braze_engine::tool_search::DEFAULT_TOOL_SEARCH_THRESHOLD`
/// (braze-config no depende de braze-engine — misma convención que los
/// demás defaults espejados).
fn default_tool_search_threshold() -> usize {
    40
}

/// Default helper for `Config::tool_output_max_bytes` — matches the
/// hardcoded const `MAX_TOOL_OUTPUT_BYTES` in `braze-tools-local/provider.rs`.
fn default_tool_output_max_bytes() -> u32 {
    8_000
}

/// Default helper for `Config::max_turn_iterations` — named so the
/// `#[serde(default = "...")]` attribute can reference it. Matches the
/// historical `MAX_TURN_ITERATIONS` const in `braze-engine/src/engine.rs`.
fn default_max_turn_iterations() -> u32 {
    20
}

/// Default helper for `Config::planner_max_tokens` — same shape as
/// `default_max_turn_iterations`. Matches the historical `PLANNER_MAX_TOKENS`
/// const in `braze-engine/src/engine.rs`.
fn default_planner_max_tokens() -> u32 {
    1024
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Default to the free local backend so experimentation never
            // burns API credit unless the user opts in explicitly.
            default_backend: "ollama".to_string(),
            anthropic_api_key: None,
            anthropic_model: None,
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.1".to_string(),
            ollama_num_ctx: 8192,
            ollama_prompt_tools: false,
            ollama_temperature: None,
            ollama_seed: None,
            ollama_top_p: None,
            ollama_top_k: None,
            ollama_repeat_penalty: None,
            openrouter_api_key: None,
            openrouter_model: None,
            openrouter_base_url: "https://openrouter.ai/api/v1".to_string(),
            max_tokens: 4096,
            system_prompt: None,
            session_dir: paths::default_session_dir(),
            // Mirrors `SimpleContextCompactor::DEFAULT_TACTICAL_WINDOW` /
            // `braze_engine::DEFAULT_TACTICAL_COMPACTION_THRESHOLD` —
            // this is the historical hardcoded value, now just the
            // default a caller can override.
            tactical_window: 20,
            tactical_compaction_threshold: 40,
            mcp_servers: Vec::new(),
            best_of_n: 1,
            tui_theme: "dark".to_string(),
            disable_textual_tool_call_rescue: false,
            enable_prompt_caching: true,
            disable_post_edit_check: false,
            planner_backend: None,
            planner_model: None,
            lead_backend: None,
            lead_model: None,
            lead_turns: None,
            lead_failure_threshold: None,
            lead_escalation_turns: None,
            max_turn_iterations: default_max_turn_iterations(),
            planner_max_tokens: default_planner_max_tokens(),
            max_turn_total_tokens: None,
            tool_output_max_bytes: default_tool_output_max_bytes(),
            tool_output_max_lines: None,
            formatters: default_formatters(),
            model_pricing: default_model_pricing(),
            references: Vec::new(),
            tool_search_threshold: default_tool_search_threshold(),
            enable_task_list: false,
            enable_exploration: false,
            environment_block: false,
            enable_project_memory: false,
            enable_lead_summary: false,
            skills: SkillsConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from the real environment: hardcoded defaults,
    /// then `~/.config/braze/config.json` (XDG-aware, if present), then
    /// `BRAZE_*` environment variables.
    ///
    /// CLI overrides are not part of this call — `braze-cli` (Fase 5) will
    /// call [`Config::apply_overrides`] on the result once it has parsed
    /// its flags.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with(paths::config_file_path().as_deref(), std::env::vars())
    }

    /// Same layering as [`Config::load`], but with explicit, injectable
    /// sources for the config file path and the environment variables.
    /// This is what makes the merge logic testable without touching real
    /// files or process environment state.
    pub fn load_with<I, K, V>(config_file: Option<&Path>, env_vars: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut config = Config::default();

        if let Some(path) = config_file
            && let Some(file_overrides) = file::load_file(path)?
        {
            config.apply_overrides(file_overrides);
        }

        let env_overrides = ConfigOverrides::from_env(env_vars)?;
        config.apply_overrides(env_overrides);

        config.validate()?;
        Ok(config)
    }

    /// Cross-field / range validation that can't be expressed per-field
    /// via serde `#[serde(default)]` alone — called automatically at the
    /// end of [`Config::load_with`].
    fn validate(&self) -> Result<(), ConfigError> {
        // N-41 (docs/AUDITORIA-2026-07-v2.md): once the raw event log
        // reaches `tactical_window` events, `SimpleContextCompactor`
        // caps the tactical slice at that size — if `tactical_window` is
        // not strictly smaller than `tactical_compaction_threshold`, the
        // tactical slice never drops back under the threshold again,
        // triggering a compaction (and a `CompactionOccurred` event) on
        // essentially every subsequent `load_messages` call.
        if self.tactical_window >= self.tactical_compaction_threshold {
            return Err(ConfigError::Invalid(format!(
                "tactical_window ({}) must be smaller than \
                 tactical_compaction_threshold ({}) — otherwise compaction \
                 triggers on every turn",
                self.tactical_window, self.tactical_compaction_threshold
            )));
        }
        // Bajo (docs/AUDITORIA-2026-07-v2.md, "sin validación de rango
        // numérico"): `0` isn't just a degenerate edge case for these —
        // `ollama_num_ctx: 0` is sent straight to the real Ollama server
        // with undefined behavior, and `max_tokens: 0` means every
        // completion request asks for zero output tokens.
        if self.ollama_num_ctx == 0 {
            return Err(ConfigError::Invalid(
                "ollama_num_ctx must be greater than 0".to_string(),
            ));
        }
        if self.max_tokens == 0 {
            return Err(ConfigError::Invalid(
                "max_tokens must be greater than 0".to_string(),
            ));
        }
        // B5 (docs/AUDITORIA-2026-07-v3.md): `max_tokens` is shared across
        // every backend, so lowering its *default* to suit Ollama's small
        // `num_ctx` would silently truncate legitimate long completions on
        // Anthropic/OpenRouter (whose context windows are large enough
        // that this never matters) — not a safe global default to change.
        // A warning, scoped to the Ollama backend only, is the fix that
        // doesn't regress the other two: at the stock 8192/4096 defaults,
        // `max_tokens` alone reserves half of `ollama_num_ctx` for output,
        // leaving only ~3072 tokens of prompt budget for a tool-calling
        // executor that rarely needs more than a few hundred output tokens
        // per round.
        if self.default_backend == "ollama"
            && ollama_max_tokens_starves_the_prompt_budget(self.ollama_num_ctx, self.max_tokens)
        {
            tracing::warn!(
                ollama_num_ctx = self.ollama_num_ctx,
                max_tokens = self.max_tokens,
                "max_tokens reserves at least half of ollama_num_ctx for output, leaving \
                 little room for the prompt (system prompt + tools + conversation) — consider \
                 lowering max_tokens (a tool-calling executor rarely needs more than a few \
                 hundred output tokens per round) or raising ollama_num_ctx"
            );
        }
        Ok(())
    }

    /// Apply explicit overrides on top of an already-loaded config,
    /// in-place. Only fields set to `Some` in `overrides` change anything.
    ///
    /// This is the seam `braze-cli` (Fase 5) will use to layer parsed
    /// `clap` flags on top of [`Config::load`]'s result — `braze-config`
    /// never needs to know `clap` exists.
    pub fn apply_overrides(&mut self, overrides: ConfigOverrides) {
        if let Some(v) = overrides.default_backend {
            self.default_backend = v;
        }
        if let Some(v) = overrides.anthropic_api_key {
            self.anthropic_api_key = Some(v);
        }
        if let Some(v) = overrides.anthropic_model {
            self.anthropic_model = Some(v);
        }
        if let Some(v) = overrides.ollama_base_url {
            self.ollama_base_url = v;
        }
        if let Some(v) = overrides.ollama_model {
            self.ollama_model = v;
        }
        if let Some(v) = overrides.ollama_num_ctx {
            self.ollama_num_ctx = v;
        }
        if let Some(v) = overrides.ollama_prompt_tools {
            self.ollama_prompt_tools = v;
        }
        if let Some(v) = overrides.ollama_temperature {
            self.ollama_temperature = Some(v);
        }
        if let Some(v) = overrides.ollama_seed {
            self.ollama_seed = Some(v);
        }
        if let Some(v) = overrides.ollama_top_p {
            self.ollama_top_p = Some(v);
        }
        if let Some(v) = overrides.ollama_top_k {
            self.ollama_top_k = Some(v);
        }
        if let Some(v) = overrides.ollama_repeat_penalty {
            self.ollama_repeat_penalty = Some(v);
        }
        if let Some(v) = overrides.openrouter_api_key {
            self.openrouter_api_key = Some(v);
        }
        if let Some(v) = overrides.openrouter_model {
            self.openrouter_model = Some(v);
        }
        if let Some(v) = overrides.openrouter_base_url {
            self.openrouter_base_url = v;
        }
        if let Some(v) = overrides.max_tokens {
            self.max_tokens = v;
        }
        if let Some(v) = overrides.system_prompt {
            self.system_prompt = Some(v);
        }
        if let Some(v) = overrides.session_dir {
            self.session_dir = v;
        }
        if let Some(v) = overrides.tactical_window {
            self.tactical_window = v;
        }
        if let Some(v) = overrides.tactical_compaction_threshold {
            self.tactical_compaction_threshold = v;
        }
        if let Some(v) = overrides.mcp_servers {
            self.mcp_servers = v;
        }
        if let Some(v) = overrides.best_of_n {
            self.best_of_n = v;
        }
        if let Some(v) = overrides.tui_theme {
            self.tui_theme = v;
        }
        if let Some(v) = overrides.disable_textual_tool_call_rescue {
            self.disable_textual_tool_call_rescue = v;
        }
        if let Some(v) = overrides.enable_prompt_caching {
            self.enable_prompt_caching = v;
        }
        if let Some(v) = overrides.disable_post_edit_check {
            self.disable_post_edit_check = v;
        }
        if let Some(v) = overrides.planner_backend {
            self.planner_backend = Some(v);
        }
        if let Some(v) = overrides.planner_model {
            self.planner_model = Some(v);
        }
        if let Some(v) = overrides.lead_backend {
            self.lead_backend = Some(v);
        }
        if let Some(v) = overrides.lead_model {
            self.lead_model = Some(v);
        }
        if let Some(v) = overrides.lead_turns {
            self.lead_turns = Some(v);
        }
        if let Some(v) = overrides.lead_failure_threshold {
            self.lead_failure_threshold = Some(v);
        }
        if let Some(v) = overrides.lead_escalation_turns {
            self.lead_escalation_turns = Some(v);
        }
        if let Some(v) = overrides.max_turn_iterations {
            self.max_turn_iterations = v;
        }
        if let Some(v) = overrides.planner_max_tokens {
            self.planner_max_tokens = v;
        }
        if let Some(v) = overrides.max_turn_total_tokens {
            self.max_turn_total_tokens = Some(v);
        }
        if let Some(v) = overrides.tool_output_max_bytes {
            self.tool_output_max_bytes = v;
        }
        if let Some(v) = overrides.tool_output_max_lines {
            self.tool_output_max_lines = Some(v);
        }
        // `formatters` is a Vec, not Option — overrides only specify the
        // full replacement list. `None` means "use whatever's already
        // there" (Config default or file-loaded entry). We do NOT merge
        // per-extension.
        if let Some(v) = overrides.formatters {
            self.formatters = v;
        }
        // Same full-replacement contract as `formatters`.
        if let Some(v) = overrides.model_pricing {
            self.model_pricing = v;
        }
        if let Some(v) = overrides.references {
            self.references = v;
        }
        if let Some(v) = overrides.tool_search_threshold {
            self.tool_search_threshold = v;
        }
        if let Some(v) = overrides.enable_task_list {
            self.enable_task_list = v;
        }
        if let Some(v) = overrides.enable_exploration {
            self.enable_exploration = v;
        }
        if let Some(v) = overrides.environment_block {
            self.environment_block = v;
        }
        if let Some(v) = overrides.enable_project_memory {
            self.enable_project_memory = v;
        }
        if let Some(v) = overrides.skills {
            self.skills = v;
        }
    }

    /// Resuelve la entrada de pricing para `(backend, model)`: el backend
    /// debe coincidir exacto; entre las entradas cuyo `model_prefix`
    /// prefija a `model`, gana la de prefijo más largo (`""` es el
    /// catch-all del backend). `None` cuando ninguna entrada matchea —
    /// "precio desconocido", que los consumers deben propagar como
    /// "sin estimación", nunca tratar como $0.
    pub fn pricing_for(&self, backend: &str, model: &str) -> Option<&ModelPricing> {
        self.model_pricing
            .iter()
            .filter(|entry| entry.backend == backend && model.starts_with(&entry.model_prefix))
            .max_by_key(|entry| entry.model_prefix.len())
    }
}

/// `true` when `max_tokens` alone reserves at least half of
/// `ollama_num_ctx` for output — see the warning in [`Config::validate`].
/// Pure and free-standing so the threshold is unit-testable without
/// constructing a whole `Config`.
fn ollama_max_tokens_starves_the_prompt_budget(ollama_num_ctx: u32, max_tokens: u32) -> bool {
    max_tokens.saturating_mul(2) >= ollama_num_ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-config-test-{}-{}",
            std::process::id(),
            label
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn no_env() -> Vec<(String, String)> {
        Vec::new()
    }

    #[test]
    fn defaults_without_file_or_env() {
        let config = Config::load_with(None, no_env()).unwrap();
        assert_eq!(config.default_backend, "ollama");
        assert_eq!(config.anthropic_api_key, None);
        assert_eq!(config.anthropic_model, None);
        assert_eq!(config.ollama_base_url, "http://localhost:11434");
        assert_eq!(config.ollama_model, "llama3.1");
        assert_eq!(config.ollama_num_ctx, 8192);
        assert_eq!(config.openrouter_api_key, None);
        assert_eq!(config.openrouter_model, None);
        assert_eq!(config.openrouter_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.max_tokens, 4096);
        assert_eq!(config.system_prompt, None);
        assert_eq!(config.tactical_window, 20);
        assert_eq!(config.tactical_compaction_threshold, 40);
        assert!(config.mcp_servers.is_empty());
        assert_eq!(config.best_of_n, 1);
        assert_eq!(config.tui_theme, "dark");
    }

    #[test]
    fn best_of_n_is_overridable_via_env() {
        let env = vec![("BRAZE_BEST_OF_N".to_string(), "5".to_string())];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(config.best_of_n, 5);
    }

    #[test]
    fn from_env_rejects_invalid_best_of_n() {
        let env = vec![("BRAZE_BEST_OF_N".to_string(), "not-a-number".to_string())];
        let result = Config::load_with(None, env);
        assert!(matches!(result, Err(ConfigError::InvalidEnvValue { .. })));
    }

    #[test]
    fn tui_theme_is_overridable_via_env() {
        let env = vec![("BRAZE_TUI_THEME".to_string(), "light".to_string())];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(config.tui_theme, "light");
    }

    #[test]
    fn tactical_fields_are_overridable_via_env() {
        let env = vec![
            ("BRAZE_TACTICAL_WINDOW".to_string(), "10".to_string()),
            (
                "BRAZE_TACTICAL_COMPACTION_THRESHOLD".to_string(),
                "25".to_string(),
            ),
        ];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(config.tactical_window, 10);
        assert_eq!(config.tactical_compaction_threshold, 25);
    }

    /// Regression test for N-41 (docs/AUDITORIA-2026-07-v2.md):
    /// `tactical_window >= tactical_compaction_threshold` must be
    /// rejected at load time instead of silently entering permanent-
    /// compaction mode at runtime.
    #[test]
    fn rejects_a_tactical_window_not_smaller_than_the_compaction_threshold() {
        let env = vec![
            ("BRAZE_TACTICAL_WINDOW".to_string(), "40".to_string()),
            (
                "BRAZE_TACTICAL_COMPACTION_THRESHOLD".to_string(),
                "40".to_string(),
            ),
        ];
        let err = Config::load_with(None, env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_ollama_num_ctx_of_zero() {
        let env = vec![("BRAZE_OLLAMA_NUM_CTX".to_string(), "0".to_string())];
        let err = Config::load_with(None, env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn rejects_max_tokens_of_zero() {
        let env = vec![("BRAZE_MAX_TOKENS".to_string(), "0".to_string())];
        let err = Config::load_with(None, env).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    // --- B5 (docs/AUDITORIA-2026-07-v3.md): max_tokens starving the
    // Ollama prompt budget is a warning, not an error ---

    #[test]
    fn ollama_max_tokens_starves_the_prompt_budget_at_stock_defaults() {
        assert!(ollama_max_tokens_starves_the_prompt_budget(8192, 4096));
    }

    #[test]
    fn ollama_max_tokens_does_not_starve_the_prompt_budget_with_a_smaller_max_tokens() {
        assert!(!ollama_max_tokens_starves_the_prompt_budget(8192, 1024));
    }

    #[test]
    fn the_stock_default_config_is_still_valid_despite_the_max_tokens_warning() {
        // The warning above must never become a hard error — the stock
        // defaults (ollama_num_ctx=8192, max_tokens=4096) are a valid,
        // if suboptimal, configuration.
        Config::default()
            .validate()
            .expect("the default config must load even though it triggers the max_tokens warning");
    }

    #[test]
    fn a_non_ollama_default_backend_never_triggers_the_max_tokens_warning_path() {
        // Same starving ratio, but `validate()` must not even look at it
        // when Ollama isn't the active backend — a hard error here would
        // mean the check regressed into unconditional.
        let config = Config {
            default_backend: "anthropic".to_string(),
            max_tokens: 4096,
            ollama_num_ctx: 8192,
            ..Config::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn system_prompt_is_overridable_via_env() {
        let env = vec![(
            "BRAZE_SYSTEM_PROMPT".to_string(),
            "Eres un asistente de prueba.".to_string(),
        )];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(
            config.system_prompt.as_deref(),
            Some("Eres un asistente de prueba.")
        );
    }

    /// opencode-10 (docs/opencode-a-braze.md § 10): `references` load
    /// from the config file, `description` optional; default is empty.
    #[test]
    fn references_load_from_the_config_file() {
        let dir = temp_dir("references_load_from_the_config_file");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"references": [
                {"path": "/home/user/api-docs", "description": "API docs"},
                {"path": "/home/user/scratch"}
            ]}"#,
        )
        .unwrap();

        let config = Config::load_with(Some(&path), no_env()).unwrap();
        assert_eq!(config.references.len(), 2);
        assert_eq!(
            config.references[0].path,
            PathBuf::from("/home/user/api-docs")
        );
        assert_eq!(config.references[0].description.as_deref(), Some("API docs"));
        assert_eq!(config.references[1].description, None);
        assert!(Config::default().references.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_overrides_defaults() {
        let dir = temp_dir("file_overrides_defaults");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"default_backend": "anthropic", "max_tokens": 8192}"#,
        )
        .unwrap();

        let config = Config::load_with(Some(&path), no_env()).unwrap();
        assert_eq!(config.default_backend, "anthropic");
        assert_eq!(config.max_tokens, 8192);
        // Untouched fields keep their defaults.
        assert_eq!(config.ollama_base_url, "http://localhost:11434");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = PathBuf::from("/nonexistent/braze-config/tests/no-such-file.json");
        let config = Config::load_with(Some(&path), no_env()).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn env_overrides_file() {
        let dir = temp_dir("env_overrides_file");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"default_backend": "anthropic"}"#).unwrap();

        let env = vec![("BRAZE_DEFAULT_BACKEND".to_string(), "ollama".to_string())];
        let config = Config::load_with(Some(&path), env).unwrap();
        assert_eq!(config.default_backend, "ollama");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_json_file_produces_config_error_not_panic() {
        let dir = temp_dir("invalid_json_file_produces_config_error_not_panic");
        let path = dir.join("config.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let result = Config::load_with(Some(&path), no_env());
        assert!(matches!(result, Err(ConfigError::InvalidJson { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_env_value_produces_config_error_not_panic() {
        let env = vec![("BRAZE_MAX_TOKENS".to_string(), "not-a-number".to_string())];
        let result = Config::load_with(None, env);
        assert!(matches!(result, Err(ConfigError::InvalidEnvValue { .. })));
    }

    #[test]
    fn apply_overrides_after_load() {
        let mut config = Config::load_with(None, no_env()).unwrap();
        assert_eq!(config.default_backend, "ollama");

        let overrides = ConfigOverrides {
            default_backend: Some("anthropic".to_string()),
            max_tokens: Some(1000),
            ..ConfigOverrides::default()
        };
        config.apply_overrides(overrides);

        assert_eq!(config.default_backend, "anthropic");
        assert_eq!(config.max_tokens, 1000);
        // Fields not present in the overrides are untouched.
        assert_eq!(config.ollama_base_url, "http://localhost:11434");
    }

    #[test]
    fn ollama_num_ctx_is_overridable_via_env() {
        let env = vec![("BRAZE_OLLAMA_NUM_CTX".to_string(), "4096".to_string())];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(config.ollama_num_ctx, 4096);
    }

    // --- pricing_for (Paquete 3, docs/AUDITORIA-2026-07-v6.md) ---

    /// The default table ships with the two entries the project's sweeps
    /// actually exercise: the Ollama catch-all at $0 and
    /// deepseek-v4-flash's live-verified rates.
    #[test]
    fn default_pricing_covers_ollama_and_the_recommended_openrouter_model() {
        let config = Config::load_with(None, no_env()).unwrap();

        let ollama = config
            .pricing_for("ollama", "qwen2.5:3b")
            .expect("ollama catch-all must match any local model");
        assert_eq!(ollama.input_usd_per_mtok, 0.0);
        assert_eq!(ollama.output_usd_per_mtok, 0.0);

        let deepseek = config
            .pricing_for("openrouter", "deepseek/deepseek-v4-flash")
            .expect("the recommended OpenRouter model must be priced");
        assert_eq!(deepseek.input_usd_per_mtok, 0.09);
        assert_eq!(deepseek.output_usd_per_mtok, 0.18);
    }

    /// Unknown model on a backend WITHOUT a catch-all entry → `None`
    /// ("price unknown"), never $0 — the consumer must be able to tell
    /// "free" apart from "no idea".
    #[test]
    fn pricing_for_an_unlisted_model_is_none_not_zero() {
        let config = Config::load_with(None, no_env()).unwrap();
        assert!(config.pricing_for("openrouter", "z-ai/glm-5.2").is_none());
        // Backend must match exactly — an ollama model name asked under
        // the wrong backend doesn't leak the catch-all.
        assert!(config.pricing_for("anthropic", "qwen2.5:3b").is_none());
    }

    /// The longest matching prefix wins over the catch-all.
    #[test]
    fn pricing_for_prefers_the_most_specific_prefix() {
        let mut config = Config::load_with(None, no_env()).unwrap();
        config.model_pricing.push(ModelPricing {
            backend: "ollama".to_string(),
            model_prefix: "qwen3.5".to_string(),
            input_usd_per_mtok: 1.0, // synthetic, to tell the entries apart
            output_usd_per_mtok: 2.0,
            cache_read_usd_per_mtok: None,
            cache_write_usd_per_mtok: None,
        });

        let specific = config.pricing_for("ollama", "qwen3.5-coder").unwrap();
        assert_eq!(specific.input_usd_per_mtok, 1.0, "specific prefix beats catch-all");
        let fallback = config.pricing_for("ollama", "gemma4:e4b").unwrap();
        assert_eq!(fallback.input_usd_per_mtok, 0.0, "catch-all still covers the rest");
    }

    /// I-1 (docs/AUDITORIA-2026-07-v6.md): the escalation knobs default
    /// to `None` (decorator's own defaults apply) and flow end-to-end
    /// from env through `apply_overrides` — `LEAD_TURNS=0` (purely
    /// reactive) included, since `Some(0)` vs `None` is exactly the
    /// distinction the Option encoding exists to preserve.
    #[test]
    fn lead_escalation_knobs_default_to_none_and_are_overridable_via_env() {
        let defaults = Config::load_with(None, no_env()).unwrap();
        assert_eq!(defaults.lead_turns, None);
        assert_eq!(defaults.lead_failure_threshold, None);
        assert_eq!(defaults.lead_escalation_turns, None);

        let env = vec![
            ("BRAZE_LEAD_TURNS".to_string(), "0".to_string()),
            ("BRAZE_LEAD_FAILURE_THRESHOLD".to_string(), "3".to_string()),
            ("BRAZE_LEAD_ESCALATION_TURNS".to_string(), "4".to_string()),
        ];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(config.lead_turns, Some(0));
        assert_eq!(config.lead_failure_threshold, Some(3));
        assert_eq!(config.lead_escalation_turns, Some(4));
    }

    #[test]
    fn openrouter_fields_are_overridable_via_env() {
        let env = vec![
            (
                "BRAZE_OPENROUTER_API_KEY".to_string(),
                "sk-or-test-123".to_string(),
            ),
            (
                "BRAZE_OPENROUTER_MODEL".to_string(),
                "openai/gpt-4o-mini".to_string(),
            ),
            (
                "BRAZE_OPENROUTER_BASE_URL".to_string(),
                "http://example:5555/api/v1".to_string(),
            ),
        ];
        let config = Config::load_with(None, env).unwrap();
        assert_eq!(
            config
                .openrouter_api_key
                .as_ref()
                .map(ApiKey::expose_secret),
            Some("sk-or-test-123")
        );
        assert_eq!(
            config.openrouter_model.as_deref(),
            Some("openai/gpt-4o-mini")
        );
        assert_eq!(config.openrouter_base_url, "http://example:5555/api/v1");
    }

    #[test]
    fn full_pipeline_defaults_file_env_overrides() {
        let dir = temp_dir("full_pipeline_defaults_file_env_overrides");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"default_backend": "anthropic", "ollama_base_url": "http://file:1111"}"#,
        )
        .unwrap();

        let env = vec![(
            "BRAZE_OLLAMA_BASE_URL".to_string(),
            "http://env:2222".to_string(),
        )];

        let mut config = Config::load_with(Some(&path), env).unwrap();
        // File wins over default, env wins over file.
        assert_eq!(config.default_backend, "anthropic");
        assert_eq!(config.ollama_base_url, "http://env:2222");

        // Explicit (CLI) overrides win over everything.
        config.apply_overrides(ConfigOverrides {
            ollama_base_url: Some("http://cli:3333".to_string()),
            ..ConfigOverrides::default()
        });
        assert_eq!(config.ollama_base_url, "http://cli:3333");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
