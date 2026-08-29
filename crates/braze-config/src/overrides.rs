//! [`ConfigOverrides`]: a sparse (all-`Option`) view of [`crate::Config`]
//! used as the common currency for every layer above the hardcoded
//! defaults — the on-disk file, `BRAZE_*` env vars, and (from `braze-cli`
//! in Fase 5) parsed CLI flags all produce a `ConfigOverrides` and apply it
//! the same way via [`crate::Config::apply_overrides`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api_key::ApiKey;
use crate::config::McpServerConfigStub;
use crate::error::ConfigError;

/// Prefix recognized when scanning environment variables for overrides.
const ENV_PREFIX: &str = "BRAZE_";

/// Sparse overrides for [`crate::Config`]: every field is optional, and
/// only fields present (`Some`) are applied on top of an already-loaded
/// `Config`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigOverrides {
    #[serde(default)]
    pub default_backend: Option<String>,
    #[serde(default)]
    pub anthropic_api_key: Option<ApiKey>,
    #[serde(default)]
    pub anthropic_model: Option<String>,
    #[serde(default)]
    pub ollama_base_url: Option<String>,
    #[serde(default)]
    pub ollama_model: Option<String>,
    #[serde(default)]
    pub ollama_num_ctx: Option<u32>,
    #[serde(default)]
    pub ollama_prompt_tools: Option<bool>,
    #[serde(default)]
    pub ollama_temperature: Option<f32>,
    #[serde(default)]
    pub ollama_seed: Option<u64>,
    #[serde(default)]
    pub ollama_top_p: Option<f32>,
    #[serde(default)]
    pub ollama_top_k: Option<u32>,
    #[serde(default)]
    pub ollama_repeat_penalty: Option<f32>,
    #[serde(default)]
    pub ollama_keep_alive: Option<String>,
    #[serde(default)]
    pub openrouter_api_key: Option<ApiKey>,
    /// OpenCode Zen (`BRAZE_ZEN_API_KEY`).
    #[serde(default)]
    pub zen_api_key: Option<ApiKey>,
    /// `BRAZE_ZEN_MODEL`.
    #[serde(default)]
    pub zen_model: Option<String>,
    /// `BRAZE_ZEN_BASE_URL`.
    #[serde(default)]
    pub zen_base_url: Option<String>,
    #[serde(default)]
    pub openrouter_model: Option<String>,
    #[serde(default)]
    pub openrouter_base_url: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub session_dir: Option<PathBuf>,
    #[serde(default)]
    pub tactical_window: Option<usize>,
    #[serde(default)]
    pub tactical_compaction_threshold: Option<usize>,
    #[serde(default)]
    pub mcp_servers: Option<Vec<McpServerConfigStub>>,
    #[serde(default)]
    pub best_of_n: Option<usize>,
    #[serde(default)]
    pub tui_theme: Option<String>,
    #[serde(default)]
    pub disable_textual_tool_call_rescue: Option<bool>,
    #[serde(default)]
    pub enable_prompt_caching: Option<bool>,
    #[serde(default)]
    pub disable_post_edit_check: Option<bool>,
    #[serde(default)]
    pub disable_syntactic_edit_gate: Option<bool>,
    #[serde(default)]
    pub planner_backend: Option<String>,
    #[serde(default)]
    pub planner_model: Option<String>,
    #[serde(default)]
    pub lead_backend: Option<String>,
    #[serde(default)]
    pub lead_model: Option<String>,
    #[serde(default)]
    pub lead_turns: Option<usize>,
    #[serde(default)]
    pub lead_failure_threshold: Option<usize>,
    #[serde(default)]
    pub lead_escalation_turns: Option<usize>,
    /// Specs `backend[:modelo]` de la cadena de failover, en orden de
    /// preferencia. Por env se pasa como lista separada por comas.
    #[serde(default)]
    pub failover_backends: Option<Vec<String>>,
    #[serde(default)]
    pub failover_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub max_turn_iterations: Option<u32>,
    #[serde(default)]
    pub planner_max_tokens: Option<u32>,
    #[serde(default)]
    pub max_turn_total_tokens: Option<u64>,
    #[serde(default)]
    pub max_turn_wall_clock_secs: Option<u64>,
    #[serde(default)]
    pub max_round_wall_clock_secs: Option<u64>,
    #[serde(default)]
    pub enable_landlock_write_sandbox: Option<bool>,
    #[serde(default)]
    pub enable_bwrap_tool_sandbox: Option<bool>,
    #[serde(default)]
    pub bwrap_allow_network: Option<bool>,
    /// Spill-to-file del tool output truncado
    /// (`BRAZE_ENABLE_TOOL_OUTPUT_SPILL`).
    #[serde(default)]
    pub enable_tool_output_spill: Option<bool>,
    #[serde(default)]
    pub disable_agents_md: Option<bool>,
    #[serde(default)]
    pub tool_output_max_bytes: Option<u32>,
    #[serde(default)]
    pub tool_output_max_lines: Option<u32>,
    /// Replacement formatter list (overrides the full `Config::formatters`
    /// without merging — see that field). No `BRAZE_FORMATTERS` env var
    /// (parsing a list of vectors from a single string is awkward; arrays
    /// arrive via the config FILE or direct `ConfigOverrides` construction
    /// from `braze-cli`).
    #[serde(default)]
    pub formatters: Option<Vec<crate::config::FormatterConfig>>,
    /// Replacement pricing table (full replacement, no merge) — same
    /// file-only posture as `formatters`, and for the same reason.
    #[serde(default)]
    pub model_pricing: Option<Vec<crate::config::ModelPricing>>,
    /// External reference directories (opencode-10,
    /// docs/opencode-a-braze.md § 10) — full replacement, same file-only
    /// posture as `formatters`/`model_pricing`, and for the same reason.
    #[serde(default)]
    pub references: Option<Vec<crate::config::ReferenceConfig>>,
    /// C′.1 — umbral de deferral de tools por provider
    /// (`BRAZE_TOOL_SEARCH_THRESHOLD`).
    #[serde(default)]
    pub tool_search_threshold: Option<usize>,
    /// C′.2 — lista de tareas tipada (`BRAZE_ENABLE_TASK_LIST`).
    #[serde(default)]
    pub enable_task_list: Option<bool>,
    /// Gate de evidencia para cerrar tareas, checkers de Recuris
    /// (`BRAZE_ENABLE_TASK_EVIDENCE`).
    #[serde(default)]
    pub enable_task_evidence: Option<bool>,
    /// Invocación call-time de skills, Recuris § 2.2.2
    /// (`BRAZE_ENABLE_CALL_TIME_SKILLS`).
    #[serde(default)]
    pub enable_call_time_skills: Option<bool>,
    /// I.7 — explorador aislado (`BRAZE_ENABLE_EXPLORATION`).
    #[serde(default)]
    pub enable_exploration: Option<bool>,
    /// SWE-Edit #17 — subagente editor (`BRAZE_ENABLE_EDITOR`).
    #[serde(default)]
    pub enable_editor: Option<bool>,
    /// A/B del impuesto JSON — edición como SEARCH/REPLACE textual
    /// (`BRAZE_ENABLE_EDIT_FENCE`).
    #[serde(default)]
    pub enable_edit_fence: Option<bool>,
    /// E′ I.6 — snapshot de entorno en el system prompt
    /// (`BRAZE_ENVIRONMENT_BLOCK`).
    #[serde(default)]
    pub environment_block: Option<bool>,
    /// docs/project-memory-design.md — memoria de proyecto entre
    /// sesiones (`BRAZE_ENABLE_PROJECT_MEMORY`).
    #[serde(default)]
    pub enable_project_memory: Option<bool>,
    /// D′ — skills locales; reemplazo completo, file-only (estructurado,
    /// misma postura que `references`).
    #[serde(default)]
    pub skills: Option<crate::config::SkillsConfig>,
}

impl ConfigOverrides {
    /// Build overrides from an iterator of environment-like key/value
    /// pairs. Only keys with the `BRAZE_` prefix are considered; anything
    /// else (e.g. `PATH`, `HOME`) is ignored. Unrecognized `BRAZE_*`
    /// suffixes are also ignored rather than rejected, so future fields
    /// don't require every embedder to update in lockstep.
    ///
    /// Takes an injectable iterator (rather than reading `std::env`
    /// directly) so it can be exercised in tests without touching real
    /// process environment state; [`crate::Config::load`] calls this with
    /// `std::env::vars()`.
    pub fn from_env<I, K, V>(vars: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut overrides = ConfigOverrides::default();

        for (key, value) in vars {
            let key = key.as_ref();
            let value = value.as_ref();

            let Some(field) = key.strip_prefix(ENV_PREFIX) else {
                continue;
            };

            match field {
                "DEFAULT_BACKEND" => overrides.default_backend = Some(value.to_string()),
                "ANTHROPIC_API_KEY" => overrides.anthropic_api_key = Some(ApiKey::new(value)),
                "ANTHROPIC_MODEL" => overrides.anthropic_model = Some(value.to_string()),
                "OLLAMA_BASE_URL" => overrides.ollama_base_url = Some(value.to_string()),
                "OLLAMA_MODEL" => overrides.ollama_model = Some(value.to_string()),
                "OLLAMA_NUM_CTX" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_num_ctx = Some(parsed);
                }
                "OLLAMA_PROMPT_TOOLS" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_prompt_tools = Some(parsed);
                }
                "OLLAMA_TEMPERATURE" => {
                    let parsed =
                        value
                            .parse::<f32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_temperature = Some(parsed);
                }
                "OLLAMA_SEED" => {
                    let parsed =
                        value
                            .parse::<u64>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_seed = Some(parsed);
                }
                "OLLAMA_TOP_P" => {
                    let parsed =
                        value
                            .parse::<f32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_top_p = Some(parsed);
                }
                "OLLAMA_TOP_K" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_top_k = Some(parsed);
                }
                "OLLAMA_REPEAT_PENALTY" => {
                    let parsed =
                        value
                            .parse::<f32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.ollama_repeat_penalty = Some(parsed);
                }
                "OLLAMA_KEEP_ALIVE" => overrides.ollama_keep_alive = Some(value.to_string()),
                "OPENROUTER_API_KEY" => overrides.openrouter_api_key = Some(ApiKey::new(value)),
                "OPENROUTER_MODEL" => overrides.openrouter_model = Some(value.to_string()),
                "OPENROUTER_BASE_URL" => overrides.openrouter_base_url = Some(value.to_string()),
                "ZEN_API_KEY" => overrides.zen_api_key = Some(ApiKey::new(value)),
                "ZEN_MODEL" => overrides.zen_model = Some(value.to_string()),
                "ZEN_BASE_URL" => overrides.zen_base_url = Some(value.to_string()),
                "MAX_TOKENS" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.max_tokens = Some(parsed);
                }
                "SYSTEM_PROMPT" => {
                    overrides.system_prompt = Some(value.to_string());
                }
                "SESSION_DIR" => overrides.session_dir = Some(PathBuf::from(value)),
                "TACTICAL_WINDOW" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.tactical_window = Some(parsed);
                }
                "TACTICAL_COMPACTION_THRESHOLD" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.tactical_compaction_threshold = Some(parsed);
                }
                "BEST_OF_N" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.best_of_n = Some(parsed);
                }
                "TUI_THEME" => {
                    overrides.tui_theme = Some(value.to_string());
                }
                "PLANNER_BACKEND" => overrides.planner_backend = Some(value.to_string()),
                "PLANNER_MODEL" => overrides.planner_model = Some(value.to_string()),
                "LEAD_BACKEND" => overrides.lead_backend = Some(value.to_string()),
                "LEAD_MODEL" => overrides.lead_model = Some(value.to_string()),
                // Lista separada por comas: `BRAZE_FAILOVER_BACKENDS=
                // "zen:hy3-free,ollama:qwen2.5:3b"`. Se descartan las
                // entradas vacías para que una coma sobrante no componga
                // un backend sin nombre; el valor vacío entero deja la
                // cadena en `Some(vec![])`, que apaga el decorator igual
                // que el default.
                "FAILOVER_BACKENDS" => {
                    overrides.failover_backends = Some(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|spec| !spec.is_empty())
                            .map(str::to_string)
                            .collect(),
                    );
                }
                "FAILOVER_COOLDOWN_SECS" => {
                    let parsed = value.parse::<u64>().map_err(|e| ConfigError::InvalidEnvValue {
                        var: key.to_string(),
                        value: value.to_string(),
                        reason: e.to_string(),
                    })?;
                    overrides.failover_cooldown_secs = Some(parsed);
                }
                "LEAD_TURNS" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.lead_turns = Some(parsed);
                }
                "LEAD_FAILURE_THRESHOLD" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.lead_failure_threshold = Some(parsed);
                }
                "LEAD_ESCALATION_TURNS" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.lead_escalation_turns = Some(parsed);
                }
                "ENVIRONMENT_BLOCK" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.environment_block = Some(parsed);
                }
                "ENABLE_TASK_LIST" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_task_list = Some(parsed);
                }
                "ENABLE_TASK_EVIDENCE" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_task_evidence = Some(parsed);
                }
                "ENABLE_CALL_TIME_SKILLS" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_call_time_skills = Some(parsed);
                }
                "ENABLE_EXPLORATION" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_exploration = Some(parsed);
                }
                "ENABLE_EDITOR" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_editor = Some(parsed);
                }
                "ENABLE_EDIT_FENCE" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_edit_fence = Some(parsed);
                }
                "ENABLE_PROJECT_MEMORY" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_project_memory = Some(parsed);
                }
                "TOOL_SEARCH_THRESHOLD" => {
                    let parsed =
                        value
                            .parse::<usize>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.tool_search_threshold = Some(parsed);
                }
                "MAX_TURN_ITERATIONS" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.max_turn_iterations = Some(parsed);
                }
                "PLANNER_MAX_TOKENS" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.planner_max_tokens = Some(parsed);
                }
                "MAX_TURN_TOTAL_TOKENS" => {
                    let parsed =
                        value
                            .parse::<u64>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.max_turn_total_tokens = Some(parsed);
                }
                "MAX_TURN_WALL_CLOCK_SECS" => {
                    let parsed =
                        value
                            .parse::<u64>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.max_turn_wall_clock_secs = Some(parsed);
                }
                "MAX_ROUND_WALL_CLOCK_SECS" => {
                    let parsed =
                        value
                            .parse::<u64>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.max_round_wall_clock_secs = Some(parsed);
                }
                "TOOL_OUTPUT_MAX_BYTES" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.tool_output_max_bytes = Some(parsed);
                }
                "TOOL_OUTPUT_MAX_LINES" => {
                    let parsed =
                        value
                            .parse::<u32>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.tool_output_max_lines = Some(parsed);
                }
                "DISABLE_TEXTUAL_TOOL_CALL_RESCUE" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.disable_textual_tool_call_rescue = Some(parsed);
                }
                "ENABLE_PROMPT_CACHING" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_prompt_caching = Some(parsed);
                }
                "DISABLE_POST_EDIT_CHECK" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.disable_post_edit_check = Some(parsed);
                }
                "DISABLE_SYNTACTIC_EDIT_GATE" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.disable_syntactic_edit_gate = Some(parsed);
                }
                "ENABLE_LANDLOCK_WRITE_SANDBOX" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_landlock_write_sandbox = Some(parsed);
                }
                "ENABLE_BWRAP_TOOL_SANDBOX" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_bwrap_tool_sandbox = Some(parsed);
                }
                "BWRAP_ALLOW_NETWORK" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.bwrap_allow_network = Some(parsed);
                }
                "ENABLE_TOOL_OUTPUT_SPILL" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.enable_tool_output_spill = Some(parsed);
                }
                "DISABLE_AGENTS_MD" => {
                    let parsed =
                        value
                            .parse::<bool>()
                            .map_err(|e| ConfigError::InvalidEnvValue {
                                var: key.to_string(),
                                value: value.to_string(),
                                reason: e.to_string(),
                            })?;
                    overrides.disable_agents_md = Some(parsed);
                }
                // Unrecognized `BRAZE_*` var: ignore (forward-compatible
                // with a different braze version), but log it — bajo
                // (docs/AUDITORIA-2026-07-v2.md, "claves de config
                // desconocidas se ignoran sin warning"), same rationale
                // as `file::load_file`'s unknown-key warning.
                _ => {
                    tracing::warn!(var = %key, "unrecognized BRAZE_* environment variable; ignored")
                }
            }
        }

        Ok(overrides)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_ignores_non_braze_vars() {
        let vars = [("PATH", "/usr/bin"), ("HOME", "/home/someone")];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides, ConfigOverrides::default());
    }

    #[test]
    fn from_env_parses_known_fields() {
        let vars = [
            ("BRAZE_DEFAULT_BACKEND", "anthropic"),
            ("BRAZE_ANTHROPIC_API_KEY", "sk-test-123"),
            ("BRAZE_ANTHROPIC_MODEL", "claude-test-model"),
            ("BRAZE_OLLAMA_BASE_URL", "http://example:1234"),
            ("BRAZE_OLLAMA_MODEL", "llama3.1-test"),
            ("BRAZE_OLLAMA_NUM_CTX", "4096"),
            ("BRAZE_MAX_TOKENS", "8192"),
            ("BRAZE_SYSTEM_PROMPT", "be terse"),
            ("BRAZE_SESSION_DIR", "/tmp/sessions"),
            ("BRAZE_TACTICAL_WINDOW", "10"),
            ("BRAZE_TACTICAL_COMPACTION_THRESHOLD", "25"),
            ("BRAZE_BEST_OF_N", "3"),
        ];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.default_backend.as_deref(), Some("anthropic"));
        assert_eq!(
            overrides
                .anthropic_api_key
                .as_ref()
                .map(ApiKey::expose_secret),
            Some("sk-test-123")
        );
        assert_eq!(
            overrides.anthropic_model.as_deref(),
            Some("claude-test-model")
        );
        assert_eq!(
            overrides.ollama_base_url.as_deref(),
            Some("http://example:1234")
        );
        assert_eq!(overrides.ollama_model.as_deref(), Some("llama3.1-test"));
        assert_eq!(overrides.ollama_num_ctx, Some(4096));
        assert_eq!(overrides.max_tokens, Some(8192));
        assert_eq!(overrides.system_prompt.as_deref(), Some("be terse"));
        assert_eq!(overrides.session_dir, Some(PathBuf::from("/tmp/sessions")));
        assert_eq!(overrides.tactical_window, Some(10));
        assert_eq!(overrides.tactical_compaction_threshold, Some(25));
        assert_eq!(overrides.best_of_n, Some(3));
    }

    #[test]
    fn from_env_rejects_invalid_max_tokens() {
        let vars = [("BRAZE_MAX_TOKENS", "not-a-number")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }

    #[test]
    fn from_env_rejects_invalid_tactical_window() {
        let vars = [("BRAZE_TACTICAL_WINDOW", "not-a-number")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }

    #[test]
    fn from_env_parses_ollama_sampling_fields() {
        let vars = [
            ("BRAZE_OLLAMA_TEMPERATURE", "0.7"),
            ("BRAZE_OLLAMA_SEED", "42"),
            ("BRAZE_OLLAMA_TOP_P", "0.8"),
            ("BRAZE_OLLAMA_TOP_K", "20"),
            ("BRAZE_OLLAMA_REPEAT_PENALTY", "1.05"),
        ];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.ollama_temperature, Some(0.7));
        assert_eq!(overrides.ollama_seed, Some(42));
        assert_eq!(overrides.ollama_top_p, Some(0.8));
        assert_eq!(overrides.ollama_top_k, Some(20));
        assert_eq!(overrides.ollama_repeat_penalty, Some(1.05));
    }

    /// Fix keep-alive por-request (2026-08-12): la env llega como string
    /// tal cual — `"2m"` es una duración Go, no un número parseable aquí;
    /// validar el formato es del server (y `Config::validate` solo veta
    /// el string vacío).
    #[test]
    fn from_env_parses_ollama_keep_alive() {
        let overrides = ConfigOverrides::from_env([("BRAZE_OLLAMA_KEEP_ALIVE", "2m")]).unwrap();
        assert_eq!(overrides.ollama_keep_alive.as_deref(), Some("2m"));
    }

    #[test]
    fn from_env_rejects_invalid_ollama_temperature() {
        let vars = [("BRAZE_OLLAMA_TEMPERATURE", "not-a-number")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }

    #[test]
    fn from_env_parses_openrouter_fields() {
        let vars = [
            ("BRAZE_OPENROUTER_API_KEY", "sk-or-test-123"),
            ("BRAZE_OPENROUTER_MODEL", "openai/gpt-4o-mini"),
            ("BRAZE_OPENROUTER_BASE_URL", "http://example:5555/api/v1"),
        ];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(
            overrides
                .openrouter_api_key
                .as_ref()
                .map(ApiKey::expose_secret),
            Some("sk-or-test-123")
        );
        assert_eq!(
            overrides.openrouter_model.as_deref(),
            Some("openai/gpt-4o-mini")
        );
        assert_eq!(
            overrides.openrouter_base_url.as_deref(),
            Some("http://example:5555/api/v1")
        );
    }

    #[test]
    fn from_env_parses_planner_fields() {
        let vars = [
            ("BRAZE_PLANNER_BACKEND", "openrouter"),
            ("BRAZE_PLANNER_MODEL", "deepseek/deepseek-v4-flash"),
        ];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.planner_backend.as_deref(), Some("openrouter"));
        assert_eq!(
            overrides.planner_model.as_deref(),
            Some("deepseek/deepseek-v4-flash")
        );
    }

    #[test]
    fn from_env_parses_disable_post_edit_check() {
        let overrides =
            ConfigOverrides::from_env([("BRAZE_DISABLE_POST_EDIT_CHECK", "true")]).unwrap();
        assert_eq!(overrides.disable_post_edit_check, Some(true));
    }

    #[test]
    fn from_env_parses_disable_syntactic_edit_gate() {
        let overrides =
            ConfigOverrides::from_env([("BRAZE_DISABLE_SYNTACTIC_EDIT_GATE", "true")]).unwrap();
        assert_eq!(overrides.disable_syntactic_edit_gate, Some(true));
    }

    #[test]
    fn from_env_parses_disable_textual_tool_call_rescue() {
        let vars = [("BRAZE_DISABLE_TEXTUAL_TOOL_CALL_RESCUE", "true")];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.disable_textual_tool_call_rescue, Some(true));
    }

    /// Regresión del bug encontrado en la verificación en vivo del
    /// sandbox (v9 Paquete 4): un campo nuevo de `Config` que NO se
    /// espeja en `ConfigOverrides` + `apply_overrides` queda mudo — el
    /// flag del config file/env se ignora en silencio. Estos dos cierran
    /// esa brecha para las dos palancas nuevas.
    #[test]
    fn from_env_parses_enable_landlock_write_sandbox() {
        let overrides =
            ConfigOverrides::from_env([("BRAZE_ENABLE_LANDLOCK_WRITE_SANDBOX", "true")]).unwrap();
        assert_eq!(overrides.enable_landlock_write_sandbox, Some(true));
    }

    #[test]
    fn from_env_parses_disable_agents_md() {
        let overrides = ConfigOverrides::from_env([("BRAZE_DISABLE_AGENTS_MD", "true")]).unwrap();
        assert_eq!(overrides.disable_agents_md, Some(true));
    }

    #[test]
    fn from_env_rejects_invalid_disable_textual_tool_call_rescue() {
        let vars = [("BRAZE_DISABLE_TEXTUAL_TOOL_CALL_RESCUE", "not-a-bool")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }

    #[test]
    fn from_env_parses_enable_prompt_caching() {
        let vars = [("BRAZE_ENABLE_PROMPT_CACHING", "false")];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.enable_prompt_caching, Some(false));
    }

    #[test]
    fn from_env_rejects_invalid_enable_prompt_caching() {
        let vars = [("BRAZE_ENABLE_PROMPT_CACHING", "not-a-bool")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }

    #[test]
    fn from_env_ignores_unknown_braze_suffix() {
        let vars = [("BRAZE_SOME_FUTURE_FIELD", "value")];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides, ConfigOverrides::default());
    }

    /// I-1 (docs/AUDITORIA-2026-07-v6.md): the three escalation knobs
    /// arrive via env — including `LEAD_TURNS=0`, the purely-reactive
    /// mode that motivated exposing them.
    #[test]
    fn from_env_parses_lead_escalation_knobs() {
        let vars = [
            ("BRAZE_LEAD_TURNS", "0"),
            ("BRAZE_LEAD_FAILURE_THRESHOLD", "3"),
            ("BRAZE_LEAD_ESCALATION_TURNS", "4"),
        ];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.lead_turns, Some(0));
        assert_eq!(overrides.lead_failure_threshold, Some(3));
        assert_eq!(overrides.lead_escalation_turns, Some(4));
    }

    #[test]
    fn from_env_rejects_invalid_lead_turns() {
        let vars = [("BRAZE_LEAD_TURNS", "not-a-number")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }
}
