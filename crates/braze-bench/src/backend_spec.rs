//! Parses `--backends` specs (`anthropic`, `anthropic:<model>`,
//! `ollama`, `ollama:<model>`, `openrouter`, `openrouter:<model>`) and
//! builds the `ModelBackend` each one names, reusing whatever
//! `braze_config::Config` already resolved for the API key / base URL —
//! same construction logic `braze-cli/src/main.rs` uses, just
//! parameterized per spec instead of per process.

use braze_config::Config;
use braze_model::{AnthropicBackend, ModelBackend, OllamaBackend, OpenRouterBackend};

use crate::error::BenchError;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Provider {
    Anthropic,
    Ollama,
    OpenRouter,
}

/// One `--backends` entry, already split into provider + optional model
/// override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    provider: Provider,
    model_override: Option<String>,
}

impl BackendSpec {
    /// Parses one comma-separated entry, e.g. `"ollama:qwen2.5:3b"` — the
    /// model name may itself contain colons (Ollama tags do), so only the
    /// *first* colon splits provider from model.
    pub fn parse(spec: &str) -> Result<Self, BenchError> {
        let (provider_str, model_override) = match spec.split_once(':') {
            Some((provider, model)) => (provider, Some(model.to_string())),
            None => (spec, None),
        };
        let provider = match provider_str {
            "anthropic" => Provider::Anthropic,
            "ollama" => Provider::Ollama,
            "openrouter" => Provider::OpenRouter,
            other => {
                return Err(BenchError::Startup(format!(
                    "unknown backend provider '{other}' (expected 'anthropic', 'ollama', or 'openrouter')"
                )));
            }
        };
        Ok(Self {
            provider,
            model_override,
        })
    }

    /// Name shown in the comparison report, e.g. `"ollama:qwen2.5:3b"` or
    /// `"anthropic"` (falls back to the configured model when there's no
    /// override, so the report never shows a bare, ambiguous provider
    /// name).
    pub fn display_name(&self, config: &Config) -> String {
        let provider = match self.provider {
            Provider::Anthropic => "anthropic",
            Provider::Ollama => "ollama",
            Provider::OpenRouter => "openrouter",
        };
        let model = self
            .model_override
            .clone()
            .unwrap_or_else(|| match self.provider {
                Provider::Anthropic => config.anthropic_model.clone().unwrap_or_default(),
                Provider::Ollama => config.ollama_model.clone(),
                Provider::OpenRouter => config.openrouter_model.clone().unwrap_or_default(),
            });
        if model.is_empty() {
            provider.to_string()
        } else {
            format!("{provider}:{model}")
        }
    }

    /// Resolves the local Ollama model this spec would load, if it names an
    /// Ollama backend — `None` for `anthropic` specs, which hold nothing in
    /// local memory to release between backends.
    pub fn ollama_model(&self, config: &Config) -> Option<String> {
        match self.provider {
            Provider::Ollama => Some(
                self.model_override
                    .clone()
                    .unwrap_or_else(|| config.ollama_model.clone()),
            ),
            Provider::Anthropic | Provider::OpenRouter => None,
        }
    }

    /// Builds the `ModelBackend` this spec names.
    pub fn build(&self, config: &Config) -> Result<Box<dyn ModelBackend>, BenchError> {
        match self.provider {
            Provider::Anthropic => {
                let api_key = config.anthropic_api_key.clone().ok_or_else(|| {
                    BenchError::Startup(
                        "falta ANTHROPIC_API_KEY (config file o BRAZE_ANTHROPIC_API_KEY) para \
                         un backend 'anthropic'"
                            .to_string(),
                    )
                })?;
                let model = self
                    .model_override
                    .clone()
                    .or_else(|| config.anthropic_model.clone())
                    .ok_or_else(|| {
                        BenchError::Startup(
                            "falta el modelo anthropic: usa 'anthropic:<modelo>' o configura \
                             BRAZE_ANTHROPIC_MODEL"
                                .to_string(),
                        )
                    })?;
                Ok(Box::new(AnthropicBackend::new(api_key, model)))
            }
            Provider::Ollama => {
                let model = self
                    .model_override
                    .clone()
                    .unwrap_or_else(|| config.ollama_model.clone());
                Ok(Box::new(
                    OllamaBackend::with_base_url(model, config.ollama_base_url.clone())
                        .with_num_ctx(config.ollama_num_ctx),
                ))
            }
            Provider::OpenRouter => {
                let api_key = config.openrouter_api_key.clone().ok_or_else(|| {
                    BenchError::Startup(
                        "falta OPENROUTER_API_KEY (config file o BRAZE_OPENROUTER_API_KEY) para \
                         un backend 'openrouter'"
                            .to_string(),
                    )
                })?;
                let model = self
                    .model_override
                    .clone()
                    .or_else(|| config.openrouter_model.clone())
                    .ok_or_else(|| {
                        BenchError::Startup(
                            "falta el modelo openrouter: usa 'openrouter:<modelo>' o configura \
                             BRAZE_OPENROUTER_MODEL"
                                .to_string(),
                        )
                    })?;
                Ok(Box::new(OpenRouterBackend::with_base_url(
                    api_key,
                    model,
                    config.openrouter_base_url.clone(),
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::load_with(None, Vec::<(String, String)>::new()).unwrap()
    }

    #[test]
    fn parses_bare_provider_names() {
        let ollama = BackendSpec::parse("ollama").unwrap();
        assert_eq!(ollama.provider, Provider::Ollama);
        assert_eq!(ollama.model_override, None);

        let anthropic = BackendSpec::parse("anthropic").unwrap();
        assert_eq!(anthropic.provider, Provider::Anthropic);
        assert_eq!(anthropic.model_override, None);
    }

    #[test]
    fn parses_provider_with_model_override() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        assert_eq!(spec.provider, Provider::Ollama);
        assert_eq!(spec.model_override.as_deref(), Some("qwen2.5:3b"));
    }

    #[test]
    fn unknown_provider_is_a_startup_error() {
        let result = BackendSpec::parse("openai:gpt-4");
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn display_name_uses_override_when_present() {
        let spec = BackendSpec::parse("ollama:gemma3:1b").unwrap();
        assert_eq!(spec.display_name(&config()), "ollama:gemma3:1b");
    }

    #[test]
    fn display_name_falls_back_to_configured_model() {
        let spec = BackendSpec::parse("ollama").unwrap();
        // Default config's `ollama_model` is "llama3.1" (braze-config's
        // documented default).
        assert_eq!(spec.display_name(&config()), "ollama:llama3.1");
    }

    #[test]
    fn build_ollama_backend_never_fails_without_credentials() {
        // Ollama needs no API key, unlike Anthropic — this must succeed
        // even against a default (empty) config.
        let spec = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        assert!(spec.build(&config()).is_ok());
    }

    #[test]
    fn build_anthropic_backend_without_api_key_is_a_startup_error() {
        let spec = BackendSpec::parse("anthropic:claude-x").unwrap();
        let result = spec.build(&config());
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn parses_openrouter_with_model_override() {
        let spec = BackendSpec::parse("openrouter:openai/gpt-4o-mini").unwrap();
        assert_eq!(spec.provider, Provider::OpenRouter);
        assert_eq!(spec.model_override.as_deref(), Some("openai/gpt-4o-mini"));
        assert_eq!(spec.ollama_model(&config()), None);
    }

    #[test]
    fn build_openrouter_backend_without_api_key_is_a_startup_error() {
        let spec = BackendSpec::parse("openrouter:openai/gpt-4o-mini").unwrap();
        let result = spec.build(&config());
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }
}
