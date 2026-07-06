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
    pub openrouter_api_key: Option<ApiKey>,
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
    pub disable_post_edit_check: Option<bool>,
    #[serde(default)]
    pub planner_backend: Option<String>,
    #[serde(default)]
    pub planner_model: Option<String>,
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
                "OPENROUTER_API_KEY" => overrides.openrouter_api_key = Some(ApiKey::new(value)),
                "OPENROUTER_MODEL" => overrides.openrouter_model = Some(value.to_string()),
                "OPENROUTER_BASE_URL" => overrides.openrouter_base_url = Some(value.to_string()),
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
    fn from_env_parses_disable_textual_tool_call_rescue() {
        let vars = [("BRAZE_DISABLE_TEXTUAL_TOOL_CALL_RESCUE", "true")];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides.disable_textual_tool_call_rescue, Some(true));
    }

    #[test]
    fn from_env_rejects_invalid_disable_textual_tool_call_rescue() {
        let vars = [("BRAZE_DISABLE_TEXTUAL_TOOL_CALL_RESCUE", "not-a-bool")];
        let err = ConfigOverrides::from_env(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidEnvValue { .. }));
    }

    #[test]
    fn from_env_ignores_unknown_braze_suffix() {
        let vars = [("BRAZE_SOME_FUTURE_FIELD", "value")];
        let overrides = ConfigOverrides::from_env(vars).unwrap();
        assert_eq!(overrides, ConfigOverrides::default());
    }
}
