use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api_key::ApiKey;
use crate::error::ConfigError;
use crate::file;
use crate::overrides::ConfigOverrides;
use crate::paths;

/// Minimal description of an MCP server to connect to by default.
///
/// `braze-mcp-client` (Fase 4) will consume this to spawn stdio-based MCP
/// servers; nothing reads it yet, but the shape needs to exist now so the
/// config file/env/override plumbing has somewhere to put it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfigStub {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
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
            disable_post_edit_check: false,
            planner_backend: None,
            planner_model: None,
            lead_backend: None,
            lead_model: None,
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
    }
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
