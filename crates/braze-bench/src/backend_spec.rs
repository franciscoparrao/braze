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

    /// Builds the `ModelBackend` this spec names, with `sampling` applied
    /// identically to every provider (N-34, docs/AUDITORIA-2026-07-v2.md)
    /// — without this, comparing e.g. Ollama pinned to a low temperature
    /// against Anthropic/OpenRouter left at their provider default
    /// (~1.0) compares different sampling regimes, not different models.
    pub fn build(
        &self,
        config: &Config,
        sampling: SamplingSpec,
    ) -> Result<Box<dyn ModelBackend>, BenchError> {
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
                // No `seed`: the Anthropic Messages API has no such
                // parameter, so a run against it can never be fully
                // reproducible — temperature parity is the most this
                // backend can offer toward N-34.
                Ok(Box::new(
                    AnthropicBackend::new(api_key.expose_secret().to_string(), model)
                        .with_temperature(sampling.temperature),
                ))
            }
            Provider::Ollama => {
                let model = self
                    .model_override
                    .clone()
                    .unwrap_or_else(|| config.ollama_model.clone());
                let mut backend =
                    OllamaBackend::with_base_url(model, config.ollama_base_url.clone())
                        .with_num_ctx(config.ollama_num_ctx)
                        .with_temperature(sampling.temperature);
                if let Some(seed) = sampling.seed {
                    backend = backend.with_seed(seed);
                }
                Ok(Box::new(backend))
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
                let mut backend = OpenRouterBackend::with_base_url(
                    api_key.expose_secret().to_string(),
                    model,
                    config.openrouter_base_url.clone(),
                )
                .with_temperature(sampling.temperature);
                if let Some(seed) = sampling.seed {
                    backend = backend.with_seed(seed);
                }
                Ok(Box::new(backend))
            }
        }
    }
}

/// Sampling parameters applied uniformly across every backend in a sweep
/// — see [`BackendSpec::build`]'s doc comment for why uniformity is the
/// point (N-34, docs/AUDITORIA-2026-07-v2.md).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplingSpec {
    pub temperature: f32,
    /// Ignored for an `anthropic` spec — the Messages API has no `seed`
    /// parameter.
    pub seed: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::load_with(None, Vec::<(String, String)>::new()).unwrap()
    }

    fn sampling() -> SamplingSpec {
        SamplingSpec {
            temperature: 0.2,
            seed: Some(42),
        }
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
        assert!(spec.build(&config(), sampling()).is_ok());
    }

    #[test]
    fn build_anthropic_backend_without_api_key_is_a_startup_error() {
        let spec = BackendSpec::parse("anthropic:claude-x").unwrap();
        let result = spec.build(&config(), sampling());
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
        let result = spec.build(&config(), sampling());
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }
}
