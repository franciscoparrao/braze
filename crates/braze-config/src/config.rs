use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// `BRAZE_ANTHROPIC_API_KEY`.
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
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
    /// `BRAZE_OPENROUTER_API_KEY`.
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
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
    /// MCP servers to connect to by default (consumed by `braze-mcp-client`, Fase 4).
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfigStub>,
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
            mcp_servers: Vec::new(),
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

        Ok(config)
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
        if let Some(v) = overrides.mcp_servers {
            self.mcp_servers = v;
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
        assert!(config.mcp_servers.is_empty());
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
        assert_eq!(config.openrouter_api_key.as_deref(), Some("sk-or-test-123"));
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
