//! Parses `--backends` specs (`anthropic`, `anthropic:<model>`,
//! `ollama`, `ollama:<model>`, `openrouter`, `openrouter:<model>`) and
//! builds the `ModelBackend` each one names, reusing whatever
//! `braze_config::Config` already resolved for the API key / base URL —
//! same construction logic `braze-cli/src/main.rs` uses, just
//! parameterized per spec instead of per process.
//!
//! Planner/executor split (PLAN.md § "Split planificador/ejecutor"): a
//! spec may carry a planner sub-spec after the literal `"+plan:"` — e.g.
//! `ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash` — so a
//! sweep can put the baseline and the planned variant side by side in the
//! same run, apples-to-apples. The `"+plan:"` token was chosen over a
//! bare separator character because model ids legitimately contain `:`
//! (Ollama tags) and `/` (OpenRouter), and `,` is already `--backends`'
//! entry delimiter.
//!
//! Ablation matrix (E1, docs/AUDITORIA-2026-07-v3.md): a spec may also
//! carry a trailing `"+ablate:<key>[=<value>];..."` suffix — e.g.
//! `ollama:qwen2.5:3b+ablate:no-rescue;strict-edit` — composable toggles
//! for harness levers that otherwise have no way to vary *within* one
//! sweep invocation (before this, measuring "with vs. without textual
//! rescue" meant two separate `braze-bench` processes with different env
//! vars, which is also a paired-sample statistics problem: different
//! process, different moment, not a controlled A/B). Always the last
//! suffix on the string — stripped before the `"+plan:"` split, so it
//! never has to compose with the planner sub-spec's own grammar. See
//! [`AblationOverrides`] for the recognized keys.

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
/// override — plus, when the entry carried a `"+plan:"` suffix, the
/// planner sub-spec (never itself nested: one planner per entry), and
/// whatever `"+ablate:"` overrides it carried (E1,
/// docs/AUDITORIA-2026-07-v3.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    provider: Provider,
    model_override: Option<String>,
    planner: Option<Box<BackendSpec>>,
    ablation: AblationOverrides,
}

impl BackendSpec {
    /// Parses one comma-separated entry: `"ollama:qwen2.5:3b"`, a planned
    /// variant like
    /// `"ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash"`,
    /// and/or an ablated variant like
    /// `"ollama:qwen2.5:3b+ablate:no-rescue;strict-edit"` — `"+ablate:"`
    /// (if present) is always the trailing suffix, stripped first; the
    /// remainder is then split on the literal `"+plan:"` as before (see
    /// the module doc comment for why that token).
    pub fn parse(spec: &str) -> Result<Self, BenchError> {
        let (base, ablation) = match spec.rfind("+ablate:") {
            Some(idx) => (
                &spec[..idx],
                AblationOverrides::parse(&spec[idx + "+ablate:".len()..])?,
            ),
            None => (spec, AblationOverrides::default()),
        };

        let mut parsed = match base.split_once("+plan:") {
            Some((executor, planner)) => {
                if executor.is_empty() || planner.is_empty() {
                    return Err(BenchError::Startup(format!(
                        "invalid '+plan:' spec '{spec}': expected \
                         '<executor>+plan:<planner>' with both halves non-empty"
                    )));
                }
                if planner.contains("+plan:") {
                    return Err(BenchError::Startup(format!(
                        "invalid spec '{spec}': only one '+plan:' planner per entry"
                    )));
                }
                let mut parsed = Self::parse_single(executor)?;
                parsed.planner = Some(Box::new(Self::parse_single(planner)?));
                parsed
            }
            None => Self::parse_single(base)?,
        };
        parsed.ablation = ablation;
        Ok(parsed)
    }

    /// Parses one plain (planner-free, ablation-free) `provider[:model]`
    /// spec — the model name may itself contain colons (Ollama tags do),
    /// so only the *first* colon splits provider from model.
    fn parse_single(spec: &str) -> Result<Self, BenchError> {
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
            planner: None,
            ablation: AblationOverrides::default(),
        })
    }

    /// The ablation overrides this spec carried (default: none) — read by
    /// `runner::run_task` to override the `Engine`/`LocalToolsProvider`
    /// knobs it would otherwise build purely from `Config`.
    pub fn ablation(&self) -> AblationOverrides {
        self.ablation
    }

    /// Name shown in the comparison report, e.g. `"ollama:qwen2.5:3b"`,
    /// `"anthropic"`, a planned variant like
    /// `"ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash"`,
    /// and/or (E1, docs/AUDITORIA-2026-07-v3.md) an ablated variant like
    /// `"ollama:qwen2.5:3b+ablate:no-rescue"` — without echoing the
    /// active ablation here, a sweep's baseline and ablated rows would
    /// render as identical backend names in the report, making the two
    /// indistinguishable. Falls back to the configured model when
    /// there's no override, so the report never shows a bare, ambiguous
    /// provider name.
    pub fn display_name(&self, config: &Config) -> String {
        let base = self.display_name_single(config);
        let with_plan = match &self.planner {
            Some(planner) => format!("{base}+plan:{}", planner.display_name_single(config)),
            None => base,
        };
        format!("{with_plan}{}", self.ablation.display_suffix())
    }

    fn display_name_single(&self, config: &Config) -> String {
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

    /// Every local Ollama model this spec would load — executor and/or
    /// planner — so the sweep can `ollama stop` all of them before the
    /// next backend row starts (memory contention shows up as [Timeout],
    /// not as a reasoning failure — see `main.rs`'s `no_ollama_stop`).
    /// Empty for specs that touch no local model.
    pub fn ollama_models(&self, config: &Config) -> Vec<String> {
        let mut models = Vec::new();
        let mut push_if_ollama = |spec: &BackendSpec| {
            if spec.provider == Provider::Ollama {
                models.push(
                    spec.model_override
                        .clone()
                        .unwrap_or_else(|| config.ollama_model.clone()),
                );
            }
        };
        push_if_ollama(self);
        if let Some(planner) = &self.planner {
            push_if_ollama(planner);
        }
        models.dedup();
        models
    }

    /// Whether the *executor* half of this spec is a local Ollama model —
    /// what `runner` keys the Ollama context budget on (N-36), mirroring
    /// how production keys it on `default_backend`.
    pub fn executor_is_ollama(&self) -> bool {
        self.provider == Provider::Ollama
    }

    /// The executor's resolved model name (override, or `config`'s
    /// default for its provider) — no provider prefix, unlike
    /// `display_name`. Used to pick the model-family system-prompt hint
    /// (docs/AUDITORIA-2026-07-v3.md, hallazgo D1), which only cares
    /// about the model name, not which provider is serving it.
    pub fn executor_model_name(&self, config: &Config) -> String {
        self.model_override
            .clone()
            .unwrap_or_else(|| match self.provider {
                Provider::Anthropic => config.anthropic_model.clone().unwrap_or_default(),
                Provider::Ollama => config.ollama_model.clone(),
                Provider::OpenRouter => config.openrouter_model.clone().unwrap_or_default(),
            })
    }

    /// Builds the planner backend, if this spec carries one — same
    /// `sampling` as the executor (N-34: one sampling regime per sweep,
    /// planner included).
    pub fn build_planner(
        &self,
        config: &Config,
        sampling: SamplingSpec,
    ) -> Result<Option<Box<dyn ModelBackend>>, BenchError> {
        self.planner
            .as_ref()
            .map(|planner| planner.build(config, sampling))
            .transpose()
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
                if let Some(top_p) = sampling.top_p {
                    backend = backend.with_top_p(top_p);
                }
                if let Some(top_k) = sampling.top_k {
                    backend = backend.with_top_k(top_k);
                }
                if let Some(repeat_penalty) = sampling.repeat_penalty {
                    backend = backend.with_repeat_penalty(repeat_penalty);
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

/// Composable harness-lever overrides carried by a spec's trailing
/// `"+ablate:<key>[=<value>];..."` suffix (E1, docs/AUDITORIA-2026-07-v3.md)
/// — `runner::run_task` applies whichever of these are set on top of the
/// `Config`-derived defaults it would otherwise use, so one sweep can put
/// a baseline row and one or more ablated rows side by side.
///
/// `None`/`false` (the type's `Default`) always means "no override, use
/// `Config`'s value" — an ablation key only ever *disables* a lever or
/// *overrides* a count; there's no key to force a lever back on, since
/// that's already what leaving it out of the suffix does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AblationOverrides {
    /// `+ablate:no-rescue` — disables the textual tool-call rescue ladder
    /// (`Engine::with_textual_rescue_enabled(false)`).
    pub disable_textual_rescue: bool,
    /// `+ablate:no-post-edit-check` — disables the post-edit `cargo
    /// check` guardrail (`LocalToolsProvider::with_post_edit_check(false)`).
    pub disable_post_edit_check: bool,
    /// `+ablate:strict-edit` — disables `edit_file`'s fuzzy matching
    /// ladder, rungs 2-3 (`LocalToolsProvider::with_edit_strict_mode(true)`).
    pub edit_strict_mode: bool,
    /// `+ablate:best-of-n=N` — overrides `Config::best_of_n`.
    pub best_of_n: Option<usize>,
    /// `+ablate:tactical-window=N` — overrides `Config::tactical_window`.
    pub tactical_window: Option<usize>,
    /// `+ablate:tactical-threshold=N` — overrides
    /// `Config::tactical_compaction_threshold`.
    pub tactical_compaction_threshold: Option<usize>,
    /// `+ablate:full-observations=N` — overrides
    /// `Engine::with_tactical_full_observations` (see
    /// `braze_engine::history::TACTICAL_FULL_OBSERVATIONS`'s doc comment).
    pub tactical_full_observations: Option<usize>,
}

impl AblationOverrides {
    const RECOGNIZED_KEYS: &'static str = "no-rescue, no-post-edit-check, strict-edit, \
         best-of-n=N, tactical-window=N, tactical-threshold=N, full-observations=N";

    fn parse(raw: &str) -> Result<Self, BenchError> {
        let mut out = Self::default();
        for pair in raw.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (pair, None),
            };
            match key {
                "no-rescue" => out.disable_textual_rescue = true,
                "no-post-edit-check" => out.disable_post_edit_check = true,
                "strict-edit" => out.edit_strict_mode = true,
                "best-of-n" => out.best_of_n = Some(Self::parse_usize(key, value)?),
                "tactical-window" => out.tactical_window = Some(Self::parse_usize(key, value)?),
                "tactical-threshold" => {
                    out.tactical_compaction_threshold = Some(Self::parse_usize(key, value)?)
                }
                "full-observations" => {
                    out.tactical_full_observations = Some(Self::parse_usize(key, value)?)
                }
                other => {
                    return Err(BenchError::Startup(format!(
                        "unknown '+ablate:' key '{other}' (expected one of: {})",
                        Self::RECOGNIZED_KEYS
                    )));
                }
            }
        }
        Ok(out)
    }

    fn parse_usize(key: &str, value: Option<&str>) -> Result<usize, BenchError> {
        let value = value.ok_or_else(|| {
            BenchError::Startup(format!(
                "'+ablate:' key '{key}' requires a value, e.g. '{key}=3'"
            ))
        })?;
        value.parse::<usize>().map_err(|_| {
            BenchError::Startup(format!(
                "'+ablate:' key '{key}' value '{value}' must be a non-negative integer"
            ))
        })
    }

    /// Renders the active overrides back into `"+ablate:..."` form for
    /// [`BackendSpec::display_name`] — empty when nothing is set.
    fn display_suffix(&self) -> String {
        let mut parts = Vec::new();
        if self.disable_textual_rescue {
            parts.push("no-rescue".to_string());
        }
        if self.disable_post_edit_check {
            parts.push("no-post-edit-check".to_string());
        }
        if self.edit_strict_mode {
            parts.push("strict-edit".to_string());
        }
        if let Some(n) = self.best_of_n {
            parts.push(format!("best-of-n={n}"));
        }
        if let Some(n) = self.tactical_window {
            parts.push(format!("tactical-window={n}"));
        }
        if let Some(n) = self.tactical_compaction_threshold {
            parts.push(format!("tactical-threshold={n}"));
        }
        if let Some(n) = self.tactical_full_observations {
            parts.push(format!("full-observations={n}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("+ablate:{}", parts.join(";"))
        }
    }
}

/// Sampling parameters applied uniformly across every backend in a sweep
/// — see [`BackendSpec::build`]'s doc comment for why uniformity is the
/// point (N-34, docs/AUDITORIA-2026-07-v2.md).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct SamplingSpec {
    pub temperature: f32,
    /// Ignored for an `anthropic` spec — the Messages API has no `seed`
    /// parameter.
    pub seed: Option<u64>,
    /// Ollama-only sampling knobs (ítem 7 del backlog 2026-07-06):
    /// `None` defers to the model's Modelfile. Ignored (with no warning
    /// — uniformity across a mixed sweep isn't achievable here) by the
    /// anthropic/openrouter builders, whose wire formats don't take
    /// them through this crate yet.
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub repeat_penalty: Option<f32>,
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
            top_p: None,
            top_k: None,
            repeat_penalty: None,
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
        assert!(spec.ollama_models(&config()).is_empty());
    }

    #[test]
    fn build_openrouter_backend_without_api_key_is_a_startup_error() {
        let spec = BackendSpec::parse("openrouter:openai/gpt-4o-mini").unwrap();
        let result = spec.build(&config(), sampling());
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    // --- specs con planner (PLAN.md § "Split planificador/ejecutor", oleada 4) ---

    #[test]
    fn parses_a_plan_spec_into_executor_and_planner_halves() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash")
                .unwrap();
        assert_eq!(spec.provider, Provider::Ollama);
        assert_eq!(spec.model_override.as_deref(), Some("qwen2.5:3b"));
        let planner = spec.planner.as_deref().expect("planner half expected");
        assert_eq!(planner.provider, Provider::OpenRouter);
        assert_eq!(
            planner.model_override.as_deref(),
            Some("deepseek/deepseek-v4-flash")
        );
    }

    #[test]
    fn display_name_of_a_plan_spec_shows_both_halves() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash")
                .unwrap();
        assert_eq!(
            spec.display_name(&config()),
            "ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash"
        );
    }

    #[test]
    fn a_plan_spec_with_an_empty_half_is_a_startup_error() {
        assert!(matches!(
            BackendSpec::parse("+plan:openrouter:x"),
            Err(BenchError::Startup(_))
        ));
        assert!(matches!(
            BackendSpec::parse("ollama:x+plan:"),
            Err(BenchError::Startup(_))
        ));
    }

    #[test]
    fn a_spec_with_two_planners_is_a_startup_error() {
        assert!(matches!(
            BackendSpec::parse("ollama:x+plan:ollama:y+plan:ollama:z"),
            Err(BenchError::Startup(_))
        ));
    }

    #[test]
    fn ollama_models_reports_executor_and_local_planner() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+plan:ollama:qwen2.5:7b").unwrap();
        assert_eq!(
            spec.ollama_models(&config()),
            vec!["qwen2.5:3b".to_string(), "qwen2.5:7b".to_string()]
        );

        let remote_planner =
            BackendSpec::parse("ollama:qwen2.5:3b+plan:openrouter:deepseek/x").unwrap();
        assert_eq!(
            remote_planner.ollama_models(&config()),
            vec!["qwen2.5:3b".to_string()]
        );
    }

    #[test]
    fn build_planner_is_none_for_a_plain_spec_and_some_for_a_plan_spec() {
        let plain = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        assert!(
            plain
                .build_planner(&config(), sampling())
                .expect("plain spec must not error")
                .is_none()
        );

        // An Ollama planner needs no credentials — must build.
        let planned = BackendSpec::parse("ollama:qwen2.5:3b+plan:ollama:qwen2.5:7b").unwrap();
        assert!(
            planned
                .build_planner(&config(), sampling())
                .expect("ollama planner must build without credentials")
                .is_some()
        );
    }

    #[test]
    fn build_planner_without_credentials_is_a_startup_error() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+plan:openrouter:deepseek/x").unwrap();
        let result = spec.build_planner(&config(), sampling());
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    // --- ablation matrix (E1, docs/AUDITORIA-2026-07-v3.md) ---

    #[test]
    fn a_spec_with_no_ablate_suffix_has_no_overrides() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        assert_eq!(spec.ablation(), AblationOverrides::default());
    }

    #[test]
    fn parses_boolean_ablation_flags() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+ablate:no-rescue;strict-edit").unwrap();
        let ablation = spec.ablation();
        assert!(ablation.disable_textual_rescue);
        assert!(ablation.edit_strict_mode);
        assert!(!ablation.disable_post_edit_check);
        // The base spec itself must still parse correctly — the suffix
        // must not leak into the provider/model split.
        assert_eq!(spec.provider, Provider::Ollama);
        assert_eq!(spec.model_override.as_deref(), Some("qwen2.5:3b"));
    }

    #[test]
    fn parses_numeric_ablation_overrides() {
        let spec = BackendSpec::parse(
            "ollama:qwen2.5:3b+ablate:best-of-n=3;tactical-window=20;tactical-threshold=8;full-observations=1",
        )
        .unwrap();
        let ablation = spec.ablation();
        assert_eq!(ablation.best_of_n, Some(3));
        assert_eq!(ablation.tactical_window, Some(20));
        assert_eq!(ablation.tactical_compaction_threshold, Some(8));
        assert_eq!(ablation.tactical_full_observations, Some(1));
    }

    #[test]
    fn an_ablate_suffix_composes_with_a_plan_suffix() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+plan:openrouter:deepseek/x+ablate:no-rescue")
                .unwrap();
        assert!(spec.ablation().disable_textual_rescue);
        let planner = spec.planner.as_deref().expect("planner half expected");
        assert_eq!(planner.provider, Provider::OpenRouter);
        // The planner sub-spec doesn't carry the ablation itself — it's
        // an executor-side (Engine/LocalToolsProvider) override, not a
        // model-construction one.
        assert_eq!(planner.ablation(), AblationOverrides::default());
    }

    #[test]
    fn an_unknown_ablate_key_is_a_startup_error() {
        let result = BackendSpec::parse("ollama:qwen2.5:3b+ablate:not-a-real-key");
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn a_numeric_ablate_key_missing_its_value_is_a_startup_error() {
        let result = BackendSpec::parse("ollama:qwen2.5:3b+ablate:best-of-n");
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn a_numeric_ablate_key_with_a_non_integer_value_is_a_startup_error() {
        let result = BackendSpec::parse("ollama:qwen2.5:3b+ablate:best-of-n=abc");
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn display_name_echoes_the_active_ablation() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+ablate:no-rescue").unwrap();
        assert_eq!(
            spec.display_name(&config()),
            "ollama:qwen2.5:3b+ablate:no-rescue"
        );
    }
}
