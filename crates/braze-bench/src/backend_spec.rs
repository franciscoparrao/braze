//! Parses `--backends` specs (`anthropic`, `anthropic:<model>`,
//! `ollama`, `ollama:<model>`, `openrouter`, `openrouter:<model>`) and
//! builds the `ModelBackend` each one names, reusing whatever
//! `braze_config::Config` already resolved for the API key / base URL —
//! same construction logic `braze-cli/src/main.rs` uses, just
//! parameterized per spec instead of per process.
//!
//! Planner/executor split (PLAN.md § "Split planificador/ejecutor") and
//! reactive lead/worker escalation: a spec may carry a planner sub-spec
//! after the literal `"+plan:"` — e.g.
//! `ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash` — and/or
//! a lead sub-spec after `"+lead:"` — e.g.
//! `ollama:qwen2.5:3b+lead:openrouter:anthropic/claude-sonnet-5`. That
//! lets one sweep put the small-worker baseline, planned variant, led
//! variant, and combined planned+led variant side by side,
//! apples-to-apples. The `"+plan:"`/`"+lead:"` tokens were chosen over a
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
//! suffix on the string — stripped before the `"+plan:"`/`"+lead:"`
//! split, so it never has to compose with child sub-spec grammars. See
//! [`AblationOverrides`] for the recognized keys.

use braze_config::Config;
use braze_model::{
    AnthropicBackend, EscalatingBackend, ModelBackend, OllamaBackend, OpenRouterBackend,
};

use crate::error::BenchError;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Provider {
    Anthropic,
    Ollama,
    OpenRouter,
    /// LocalBackend (llama.cpp in-process). Construir requiere el feature
    /// `local`; sin él, `build` da un error claro.
    Local,
}

/// One `--backends` entry, already split into provider + optional model
/// override — plus, when the entry carried `"+plan:"` and/or `"+lead:"`
/// suffixes, the planner/lead sub-specs (never themselves nested: one
/// planner and one lead per entry), and whatever `"+ablate:"` overrides
/// it carried (E1, docs/AUDITORIA-2026-07-v3.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    provider: Provider,
    model_override: Option<String>,
    planner: Option<Box<BackendSpec>>,
    lead: Option<Box<BackendSpec>>,
    ablation: AblationOverrides,
}

impl BackendSpec {
    /// Parses one comma-separated entry: `"ollama:qwen2.5:3b"`, a planned
    /// variant like
    /// `"ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash"`,
    /// a led variant like
    /// `"ollama:qwen2.5:3b+lead:openrouter:anthropic/claude-sonnet-5"`,
    /// a combined planned+led variant, and/or an ablated variant like
    /// `"ollama:qwen2.5:3b+ablate:no-rescue;strict-edit"` — `"+ablate:"`
    /// (if present) is always the trailing suffix, stripped first; the
    /// remainder is then split on the literal `"+plan:"`/`"+lead:"`
    /// tokens (see the module doc comment for why those tokens).
    pub fn parse(spec: &str) -> Result<Self, BenchError> {
        let (base, ablation) = match spec.rfind("+ablate:") {
            Some(idx) => (
                &spec[..idx],
                AblationOverrides::parse(&spec[idx + "+ablate:".len()..])?,
            ),
            None => (spec, AblationOverrides::default()),
        };

        let mut parsed = Self::parse_composite(base, spec)?;
        parsed.ablation = ablation;
        Ok(parsed)
    }

    fn parse_composite(base: &str, original: &str) -> Result<Self, BenchError> {
        let plan_count = base.matches("+plan:").count();
        if plan_count > 1 {
            return Err(BenchError::Startup(format!(
                "invalid spec '{original}': only one '+plan:' planner per entry"
            )));
        }
        let lead_count = base.matches("+lead:").count();
        if lead_count > 1 {
            return Err(BenchError::Startup(format!(
                "invalid spec '{original}': only one '+lead:' lead per entry"
            )));
        }

        let mut suffixes = Vec::new();
        if let Some(idx) = base.find("+plan:") {
            suffixes.push(("plan", idx, "+plan:".len()));
        }
        if let Some(idx) = base.find("+lead:") {
            suffixes.push(("lead", idx, "+lead:".len()));
        }
        suffixes.sort_by_key(|(_, idx, _)| *idx);

        let Some((_, first_suffix_idx, _)) = suffixes.first().copied() else {
            return Self::parse_single(base);
        };

        let executor = &base[..first_suffix_idx];
        if executor.is_empty() {
            return Err(BenchError::Startup(format!(
                "invalid backend spec '{original}': expected a non-empty executor before suffixes"
            )));
        }

        let mut parsed = Self::parse_single(executor)?;
        for (i, (kind, idx, token_len)) in suffixes.iter().copied().enumerate() {
            let value_start = idx + token_len;
            let value_end = suffixes
                .get(i + 1)
                .map(|(_, next_idx, _)| *next_idx)
                .unwrap_or(base.len());
            let value = &base[value_start..value_end];
            if value.is_empty() {
                return Err(BenchError::Startup(format!(
                    "invalid '+{kind}:' spec '{original}': expected a non-empty {kind} backend"
                )));
            }
            let child = Self::parse_single(value)?;
            match kind {
                "plan" => parsed.planner = Some(Box::new(child)),
                "lead" => parsed.lead = Some(Box::new(child)),
                _ => unreachable!("known backend suffix kind"),
            }
        }
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
            "local" => Provider::Local,
            other => {
                return Err(BenchError::Startup(format!(
                    "unknown backend provider '{other}' (expected 'anthropic', 'ollama', 'openrouter', or 'local')"
                )));
            }
        };
        Ok(Self {
            provider,
            model_override,
            planner: None,
            lead: None,
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
    /// a led variant like
    /// `"ollama:qwen2.5:3b+lead:openrouter:anthropic/claude-sonnet-5"`,
    /// and/or (E1, docs/AUDITORIA-2026-07-v3.md) an ablated variant like
    /// `"ollama:qwen2.5:3b+ablate:no-rescue"` — without echoing the
    /// active ablation here, a sweep's baseline and ablated rows would
    /// render as identical backend names in the report, making the two
    /// indistinguishable. Falls back to the configured model when
    /// there's no override, so the report never shows a bare, ambiguous
    /// provider name.
    pub fn display_name(&self, config: &Config) -> String {
        let mut name = self.display_name_single(config);
        if let Some(planner) = &self.planner {
            name.push_str("+plan:");
            name.push_str(&planner.display_name_single(config));
        }
        if let Some(lead) = &self.lead {
            name.push_str("+lead:");
            name.push_str(&lead.display_name_single(config));
        }
        format!("{name}{}", self.ablation.display_suffix())
    }

    fn display_name_single(&self, config: &Config) -> String {
        let provider = match self.provider {
            Provider::Anthropic => "anthropic",
            Provider::Ollama => "ollama",
            Provider::OpenRouter => "openrouter",
            Provider::Local => "local",
        };
        let model = self
            .model_override
            .clone()
            .unwrap_or_else(|| match self.provider {
                Provider::Anthropic => config.anthropic_model.clone().unwrap_or_default(),
                // El local reusa la ref de modelo de Ollama (mismo blob).
                Provider::Ollama | Provider::Local => config.ollama_model.clone(),
                Provider::OpenRouter => config.openrouter_model.clone().unwrap_or_default(),
            });
        if model.is_empty() {
            provider.to_string()
        } else {
            format!("{provider}:{model}")
        }
    }

    /// Every local Ollama model this spec would load — executor, planner,
    /// and/or lead — so the sweep can `ollama stop` all of them before the
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
        if let Some(lead) = &self.lead {
            push_if_ollama(lead);
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

    /// Las mitades de este spec cuyo provider **ignora** los knobs de
    /// sampling finos, etiquetadas por rol (`"executor (anthropic)"`,
    /// `"lead (openrouter)"`, ...) — vacío si todas los honran. H-13
    /// (docs/AUDITORIA-2026-07-v5.md): `--top-p`/`--top-k`/
    /// `--repeat-penalty` no viajan a los builders de Anthropic/OpenRouter,
    /// así que un sweep mixto que los fija queda desbalanceado en hasta 3
    /// dimensiones sin marcar; `main` avisa una vez por spec afectado en
    /// vez de callarse.
    ///
    /// **`local` salió de esta lista el 2026-07-26**: el LocalBackend ahora
    /// aplica los cinco knobs (`with_sweep`), así que seguir avisando por
    /// él sería un falso positivo. Antes de eso ignoraba `sampling`
    /// ENTERO — incluida la temperatura, que este aviso nunca cubrió
    /// porque se asumía universal.
    pub fn non_ollama_halves(&self) -> Vec<String> {
        let mut halves = Vec::new();
        let mut push_if_not_ollama = |role: &str, spec: &BackendSpec| {
            if !matches!(spec.provider, Provider::Ollama | Provider::Local) {
                halves.push(format!("{role} ({})", spec.provider_name()));
            }
        };
        push_if_not_ollama("executor", self);
        if let Some(planner) = &self.planner {
            push_if_not_ollama("planner", planner);
        }
        if let Some(lead) = &self.lead {
            push_if_not_ollama("lead", lead);
        }
        halves
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
                Provider::Ollama | Provider::Local => config.ollama_model.clone(),
                Provider::OpenRouter => config.openrouter_model.clone().unwrap_or_default(),
            })
    }

    /// The provider's config-facing name — the same string
    /// `Config::pricing_for` keys on.
    fn provider_name(&self) -> &'static str {
        match self.provider {
            Provider::Anthropic => "anthropic",
            Provider::Ollama => "ollama",
            Provider::OpenRouter => "openrouter",
            Provider::Local => "local",
        }
    }

    /// The flat per-Mtok rates used to estimate this spec's task cost —
    /// `None` means "cost unknown, report no estimate" (Paquete 3,
    /// docs/AUDITORIA-2026-07-v6.md § roadmap).
    ///
    /// Composite-spec rule: `AgentEvent::Usage` does NOT record which
    /// model produced each round, so a `+plan:`/`+lead:` spec whose
    /// halves bill at different rates can't be costed from the event log
    /// — this resolves `Some` only when EVERY model in the spec
    /// (executor + planner + lead) resolves to *identical* rates. That
    /// covers the common cases (simple specs; all-Ollama composites,
    /// where everything is $0) honestly, and refuses to guess for the
    /// rest. Per-round model attribution in `Usage` would lift the
    /// limitation; out of scope here.
    pub fn resolve_pricing(&self, config: &Config) -> Option<PricingRates> {
        let executor = PricingRates::from_entry(
            config.pricing_for(self.provider_name(), &self.executor_model_name(config))?,
        );
        for half in [&self.planner, &self.lead].into_iter().flatten() {
            let half_rates = PricingRates::from_entry(
                config.pricing_for(half.provider_name(), &half.executor_model_name(config))?,
            );
            if half_rates != executor {
                return None;
            }
        }
        Some(executor)
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

    /// Builds the lead backend, if this spec carries one — same
    /// `sampling` as the worker/executor (N-34: one sampling regime per
    /// sweep, lead included).
    pub fn build_lead(
        &self,
        config: &Config,
        sampling: SamplingSpec,
    ) -> Result<Option<Box<dyn ModelBackend>>, BenchError> {
        self.lead
            .as_ref()
            .map(|lead| lead.build(config, sampling))
            .transpose()
    }

    /// Builds the executor, wrapping it in [`EscalatingBackend`] when the
    /// spec carries a `"+lead:"` suffix. This mirrors `braze-cli`'s
    /// production composition root: the primary backend is the worker,
    /// and the lead opens/escalates reactively.
    pub fn build_agent_model(
        &self,
        config: &Config,
        sampling: SamplingSpec,
    ) -> Result<Box<dyn ModelBackend>, BenchError> {
        match self.build_escalating(config, sampling)? {
            Some(escalating) => Ok(Box::new(escalating)),
            None => self.build(config, sampling),
        }
    }

    /// The concrete [`EscalatingBackend`] a `"+lead:"` spec composes, or
    /// `None` for a plain spec — split from [`Self::build_agent_model`]
    /// (which boxes it as `dyn ModelBackend`) so tests can assert the
    /// knob wiring below actually reached the decorator (I-1,
    /// docs/AUDITORIA-2026-07-v6.md: the previous composition applied NO
    /// knobs, and being buried behind the trait object made that
    /// unobservable — every `+lead:` A/B silently ran the proactive
    /// 3-turn opening).
    ///
    /// Knob precedence, mirroring every other lever in `runner::run_task`:
    /// an explicit `+ablate:lead-*` key wins; otherwise `Config`'s value;
    /// otherwise (`None` both) the decorator's own default.
    pub fn build_escalating(
        &self,
        config: &Config,
        sampling: SamplingSpec,
    ) -> Result<Option<EscalatingBackend>, BenchError> {
        // E1 `+ablate:no-lead`: the row keeps its `+lead:` display
        // identity (so the A/B pairs up in the report) but runs the bare
        // worker — measuring what the lead is worth by removing exactly
        // it and nothing else.
        if self.ablation().disable_lead {
            return Ok(None);
        }
        let Some(lead) = self.build_lead(config, sampling)? else {
            return Ok(None);
        };
        let worker = self.build(config, sampling)?;
        let ablation = self.ablation();
        Ok(Some(
            EscalatingBackend::new(lead, worker).with_configured_knobs(
                ablation.lead_turns.or(config.lead_turns),
                ablation
                    .lead_failure_threshold
                    .or(config.lead_failure_threshold),
                ablation
                    .lead_escalation_turns
                    .or(config.lead_escalation_turns),
            ),
        ))
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
                        .with_temperature(sampling.temperature)
                        // Misma precedencia H-2 que el brazo OpenRouter:
                        // la ablación explícita gana, si no manda config.
                        .with_prompt_caching_enabled(
                            config.enable_prompt_caching && !self.ablation().disable_prompt_caching,
                        ),
                ))
            }
            Provider::Ollama => {
                let model = self
                    .model_override
                    .clone()
                    .unwrap_or_else(|| config.ollama_model.clone());
                // Brazos B/C del A/B de constrained decoding
                // (docs/constrained-decoding-ab-design.md). Solo el
                // executor: `build` corre también para las mitades
                // planner/lead, pero esas son specs propios cuyo
                // `ablation` es el default vacío — un lead nativo detrás
                // de un worker prompt-tools queda nativo, que es lo
                // correcto.
                let mut backend =
                    OllamaBackend::with_base_url(model, config.ollama_base_url.clone())
                        .with_num_ctx(config.ollama_num_ctx)
                        .with_temperature(sampling.temperature)
                        .with_prompt_tools(self.ablation().enable_prompt_tools)
                        .with_constrained_tools(self.ablation().enable_constrained_tools);
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
                .with_temperature(sampling.temperature)
                // H-2 (docs/AUDITORIA-2026-07-v5.md): this call was
                // missing entirely — the bench always ran the backend's
                // default (caching ON), so `Config::enable_prompt_caching
                // = false` was honored by braze-cli but silently ignored
                // here, and no `+ablate:no-caching` row was possible.
                // Same precedence as every other lever: explicit ablation
                // wins, else config.
                .with_prompt_caching_enabled(
                    config.enable_prompt_caching && !self.ablation().disable_prompt_caching,
                );
                if let Some(seed) = sampling.seed {
                    backend = backend.with_seed(seed);
                }
                Ok(Box::new(backend))
            }
            Provider::Local => self.build_local(config, sampling),
        }
    }

    /// Construye el `LocalBackend` (feature `local`). El modelo se resuelve
    /// como en el CLI: ref de Ollama (`qwen2.5:3b`, blob vía manifest) o
    /// ruta a un `.gguf`. Reusa `ollama_num_ctx` como `num_ctx`.
    #[cfg(feature = "local")]
    fn build_local(
        &self,
        config: &Config,
        sampling: SamplingSpec,
    ) -> Result<Box<dyn ModelBackend>, BenchError> {
        let model_ref = self
            .model_override
            .clone()
            .unwrap_or_else(|| config.ollama_model.clone());
        let n_ctx = config.ollama_num_ctx;
        // round-economics: `+ablate:gpu-layers=N` es el precio de la ronda
        // como brazo de ESTA fila — mismos pesos y (bajo greedy) los mismos
        // tokens, a otro precio.
        let gpu_layers = self.ablation.gpu_layers;
        let backend = if model_ref.contains('/') || model_ref.ends_with(".gguf") {
            braze_model::LocalBackend::from_gguf_path(&model_ref, &model_ref, n_ctx, gpu_layers)
        } else {
            let root = std::env::var("BRAZE_OLLAMA_MODELS_ROOT")
                .unwrap_or_else(|_| "/usr/share/ollama/.ollama".to_string());
            braze_model::LocalBackend::from_ollama_model(
                &root, &model_ref, &model_ref, n_ctx, gpu_layers,
            )
        }
        .map_err(|e| BenchError::Startup(format!("backend local: {e}")))?;
        // N-34: hasta el 2026-07-26 el LocalBackend ignoraba `sampling`
        // entero — `--temperature` era un no-op y todo brazo local corría
        // greedy, así que la garantía de "un régimen por sweep" no se
        // cumplía para `local`. `with_sweep` fusiona lo que el sweep fija
        // sobre la base del entorno, para que min-p/DRY sigan siendo
        // ablacionables por env dentro de un sweep.
        let sampling = braze_model::LocalSampling::from_env().with_sweep(
            sampling.temperature,
            sampling.seed,
            sampling.top_p,
            sampling.top_k,
            sampling.repeat_penalty,
        );
        Ok(Box::new(backend.with_sampling(sampling)))
    }

    #[cfg(not(feature = "local"))]
    fn build_local(
        &self,
        _config: &Config,
        _sampling: SamplingSpec,
    ) -> Result<Box<dyn ModelBackend>, BenchError> {
        Err(BenchError::Startup(
            "el backend 'local' requiere compilar braze-bench con `--features local`".to_string(),
        ))
    }
}

/// Flat per-Mtok USD rates resolved from [`Config::pricing_for`] for one
/// backend row — the value-type `compute_metrics` consumes (Copy, no
/// borrow of `Config`). See [`BackendSpec::resolve_pricing`] for the
/// composite-spec resolution rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PricingRates {
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    /// `None` = the provider doesn't price cache reads separately (those
    /// tokens bill as normal input in the estimate).
    pub cache_read_usd_per_mtok: Option<f64>,
    /// Same contract as `cache_read_usd_per_mtok`.
    pub cache_write_usd_per_mtok: Option<f64>,
}

impl PricingRates {
    fn from_entry(entry: &braze_config::ModelPricing) -> Self {
        Self {
            input_usd_per_mtok: entry.input_usd_per_mtok,
            output_usd_per_mtok: entry.output_usd_per_mtok,
            cache_read_usd_per_mtok: entry.cache_read_usd_per_mtok,
            cache_write_usd_per_mtok: entry.cache_write_usd_per_mtok,
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
    /// `+ablate:no-syntactic-gate` — disables the pre-application
    /// syntactic gate (`LocalToolsProvider::with_syntactic_edit_gate(false)`).
    pub disable_syntactic_edit_gate: bool,
    /// `+ablate:no-spill` — disables spill-to-file of truncated tool
    /// output (`LocalToolsProvider::with_tool_output_spill(false)`), so
    /// the model can't recover a truncated grep/build from
    /// `.braze/spill/` — the ablation for measuring whether lossless
    /// truncation actually helps (docs/tool-output-spill-design-2026-08-11.md).
    /// The head+tail truncation stays on either way.
    pub disable_tool_output_spill: bool,
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
    /// `+ablate:lead-turns=N` — overrides `Config::lead_turns` for this
    /// spec's `EscalatingBackend` (I-1, docs/AUDITORIA-2026-07-v6.md).
    /// `lead-turns=0` is the purely-reactive arm the SI-2 A/B needed and
    /// couldn't express: without it every `+lead:` row ran the proactive
    /// 3-turn opening while the analysis attributed the effect to
    /// reactive escalation. Only meaningful on a spec that carries
    /// `+lead:`; silently unused otherwise (same posture as `best-of-n`
    /// on a config that doesn't vote).
    pub lead_turns: Option<usize>,
    /// `+ablate:lead-threshold=N` — overrides
    /// `Config::lead_failure_threshold` (the decorator clamps 0 up to 1).
    pub lead_failure_threshold: Option<usize>,
    /// `+ablate:lead-window=N` — overrides `Config::lead_escalation_turns`
    /// (clamped to at least 1).
    pub lead_escalation_turns: Option<usize>,
    /// `+ablate:no-caching` — disables OpenRouter prompt-caching
    /// breakpoints for this row (H-2, docs/AUDITORIA-2026-07-v5.md:
    /// `build` never called `with_prompt_caching_enabled`, so
    /// `Config::enable_prompt_caching = false` was honored in production
    /// but silently ignored in the bench — and the "with vs without
    /// caching" A/B the cache-token metrics exist for was inexpressible).
    /// No-op for backends without explicit caching (Ollama, Anthropic).
    pub disable_prompt_caching: bool,
    /// `+ablate:no-prune` — disables the ACI collapse of old observations
    /// to one line (opencode ítem 2 resto, docs/AUDITORIA-2026-07-v6.md §
    /// backlog opencode): the collapse is a central lever of the
    /// SLM-first thesis and couldn't be turned OFF to measure its
    /// contribution. `Engine::with_observation_collapse_enabled(false)`.
    pub disable_observation_collapse: bool,
    /// `+ablate:no-planner` — runs a `+plan:` spec WITHOUT attaching its
    /// planner (E1): the row keeps the same display identity (so the A/B
    /// pairs up in the report) but measures the executor alone.
    pub disable_planner: bool,
    /// `+ablate:no-lead` — runs a `+lead:` spec WITHOUT the
    /// `EscalatingBackend` wrapper (E1): worker alone, same identity.
    pub disable_lead: bool,
    /// `+ablate:no-compaction` — disables tactical compaction entirely
    /// (threshold `usize::MAX`; E1). The token-budget trigger is also
    /// bypassed for the row, so a long turn CAN blow the model's real
    /// context — that's the point of the ablation: measuring what
    /// compaction is worth means letting its absence hurt.
    pub disable_compaction: bool,
    /// `+ablate:no-harness-notes` — disables the A′.2 mid-turn harness
    /// notes (`Engine::with_harness_notes_enabled(false)`): the ablation
    /// that measures whether announcing the budget/iteration deadline
    /// actually converts aborted turns into converged ones.
    pub disable_harness_notes: bool,
    /// `+ablate:tool-search-threshold=N` — overrides
    /// `Config::tool_search_threshold` (C′.1): la fila puede forzar la
    /// deferral con un umbral bajo (o desactivarla con uno enorme) para
    /// el A/B con un provider sintético de ruido.
    pub tool_search_threshold: Option<usize>,
    /// `+ablate:task-list` — ENABLES the C′.2 typed task list for this
    /// row. The one enabling key in a matrix of disablers, documented
    /// exception: every other lever defaults ON (so its ablation
    /// disables), this one defaults OFF (two extra tools are potential
    /// SLM distractors) — the suffix still means what every suffix
    /// means: "this row diverges from the config default".
    pub enable_task_list: bool,
    /// `+ablate:explore` — ENABLES the I.7 isolated exploration child
    /// loop (`docs/explorador-aislado-ab-design.md`). Same documented
    /// enabling-key exception as `enable_task_list`: the lever defaults
    /// OFF (one extra tool is a potential SLM distractor), so its
    /// suffix enables rather than disables.
    pub enable_exploration: bool,
    /// `+ablate:editor` — ENABLES el subagente editor SWE-Edit
    /// (`docs/editor-subagent-design-2026-08-10.md`). Misma excepción de
    /// clave-que-habilita que `explore`: el lever es OFF por default.
    pub enable_editor: bool,
    /// `+ablate:edit-fence` — ENABLES el canal SEARCH/REPLACE textual
    /// del A/B del impuesto JSON
    /// (docs/hypothesis-2026-08-10-json-tax-edit-fence.md): `edit_file`
    /// sale del inventario y la edición viaja como bloques de texto que
    /// el engine parsea como canal primario (NUNCA contado como rescue —
    /// el mecanismo del A/B exige `rescued_tool_calls` limpio). Misma
    /// excepción de clave-que-habilita que `task-list`. Backend-agnóstico.
    pub enable_edit_fence: bool,
    /// `+ablate:prompt-tools` — brazo B del A/B pre-registrado de
    /// constrained decoding (docs/constrained-decoding-ab-design.md): el
    /// request Ollama va SIN el campo `tools` (inventario como addendum
    /// del system prompt + envelope JSON), y el engine parsea el envelope
    /// de vuelta. Enabling key, same documented exception as
    /// `enable_task_list`. Ollama-only — `main` warns for other executors.
    pub enable_prompt_tools: bool,
    /// `+ablate:constrained-tools` — brazo C: prompt-tools (implied) plus
    /// Ollama structured outputs (`format` = envelope JSON schema), so
    /// the decoder cannot emit anything but the envelope.
    pub enable_constrained_tools: bool,
    /// `+ablate:project-memory` — ENABLES `braze_engine::ProjectMemoryHook`
    /// for this row (docs/project-memory-design.md). Same documented
    /// enabling-key exception as `enable_task_list`. Honest caveat: a
    /// bench task's sandbox is fresh per repetition
    /// (`TaskSandbox::new`), so within ONE task run there is never a
    /// prior session's memory to load from — this key measures that the
    /// hook mechanism fires correctly within a turn (touched files,
    /// `TaskCompleted` signals persisted to the sandbox's own
    /// `.braze/memory.json`), not the cross-session value the design
    /// doc's own § "mejor opción" flags as needing a multi-turn suite
    /// this bench doesn't have yet.
    pub enable_project_memory: bool,
    /// `+ablate:project-memory-seeded` — ENABLES the hook (like
    /// `project-memory`) AND asks the runner to synthesize a
    /// `.braze/memory.json` seed in the sandbox before the session
    /// starts, derived deterministically from the task's own
    /// `setup_files` (as if a previous session had created them — which
    /// is literally what `TaskSandbox::new` just did). This is the arm
    /// that measures the PROMPT-side effect of the lever: with a fresh
    /// sandbox the plain `project-memory` arm always renders an empty
    /// section (see that key's doc), so injection needs a seed, and a
    /// static fixture can't provide one — K-7 discards any memory whose
    /// `project_key` isn't the real (unique, temp) sandbox root. Only
    /// the bench synthesizes the seed, at the real root, with zero
    /// experimenter-authored content; K-7 stays intact in production.
    /// Pre-registered A/B: docs/hypothesis-2026-08-04-project-memory-ab.md.
    pub seed_project_memory: bool,
    /// `+ablate:lead-summary` — ENABLES summary-por-lead (v8 § 6): la
    /// compactación le pide el summary de los eventos dropeados al
    /// backend del `+lead:` de esta misma fila (segunda instancia), con
    /// fallback al digest extractivo ante cualquier fallo. Same
    /// documented enabling-key exception as `enable_task_list`. Solo
    /// tiene efecto en filas que además llevan `+lead:` — sin lead no
    /// hay summarizer que construir y la fila corre como siempre.
    pub enable_lead_summary: bool,
    /// `+ablate:ttc=N` — test-time compute local (v8 § 6.15): cada
    /// "repetición" de la fila corre N rollouts completos e
    /// independientes de la tarea y reporta UNA fila — el ganador por
    /// auto-consistencia sobre el artefacto (`runner::select_ttc_winner`),
    /// con tokens/walltime/costo SUMADOS sobre los N. La pregunta que
    /// responde su A/B: ¿n rollouts de un modelo chico compran pass rate
    /// a n× el costo? — la palanca más natural cuando la inferencia es
    /// local y los tokens casi gratis. `None`/`Some(1)` = fila normal.
    pub ttc_rollouts: Option<u32>,
    /// `+ablate:verify-gate[=N]` — el gate de verificación de fin de
    /// turno (H2, docs/verification-lever-design-2026-07-22.md). `Some(N)`
    /// lo prende con `max_rounds=N` (bare `verify-gate` = 2) usando
    /// `cargo check` como comando en las tareas con `expect_cargo_check`.
    /// `None` (el default) = brazo control, sin gate.
    pub verify_gate: Option<usize>,
    /// `+ablate:max-iterations=N` — sobreescribe
    /// `Config::max_turn_iterations` para ESTA fila
    /// (`Engine::with_max_turn_iterations`).
    ///
    /// Existe por round-economics: el contraste "avara vs derrochadora"
    /// es, antes que nada, un tope de rondas distinto por brazo, y el
    /// tope vivía solo en `Config` — es decir, era global al sweep, así
    /// que las dos configuraciones no podían correr en la misma corrida
    /// y quedar pareadas por (tarea, repetición) para McNemar. Con la
    /// llave acá, el factorial entero cabe en un sweep.
    pub max_turn_iterations: Option<usize>,
    /// `+ablate:gpu-layers=N` — capas ofloadeadas a GPU del `LocalBackend`
    /// para ESTA fila, equivalente por brazo a `BRAZE_LOCAL_GPU_LAYERS`
    /// (que es del proceso y por lo tanto del sweep entero).
    ///
    /// Es el instrumento B de round-economics: mismos pesos, mismos
    /// tokens bajo decodificación greedy, distinto precio por ronda. Sin
    /// esta llave, "GPU" y "CPU" son dos sweeps separados y el pareo
    /// tarea-a-tarea que la estadística del Paper 1 usa hay que
    /// reconstruirlo a mano desde dos JSON. Ignorada por los backends que
    /// no son `local:`.
    pub gpu_layers: Option<u32>,
}

impl AblationOverrides {
    /// Every key `parse` accepts, verbatim, for the unknown-key error
    /// message. J-22 (docs/AUDITORIA-2026-07-v7.md): this list drifted
    /// from the parser (it was missing `no-harness-notes`, `task-list`
    /// and `tool-search-threshold`), so a typo on one of the missing
    /// keys got "help" that implied the lever didn't exist —
    /// `recognized_keys_lists_every_parseable_key` now pins the two in
    /// sync.
    const RECOGNIZED_KEYS: &'static str = "no-rescue, no-post-edit-check, no-syntactic-gate, strict-edit, \
         no-caching, no-prune, no-planner, no-lead, no-compaction, no-harness-notes, no-spill, \
         task-list, explore, editor, edit-fence, prompt-tools, constrained-tools, project-memory, lead-summary, verify-gate=N, ttc=N, best-of-n=N, \
         tactical-window=N, tactical-threshold=N, full-observations=N, \
         tool-search-threshold=N, lead-turns=N, lead-threshold=N, lead-window=N, \
         max-iterations=N, gpu-layers=N";

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
                "no-syntactic-gate" => out.disable_syntactic_edit_gate = true,
                "no-spill" => out.disable_tool_output_spill = true,
                "strict-edit" => out.edit_strict_mode = true,
                "no-caching" => out.disable_prompt_caching = true,
                "no-prune" => out.disable_observation_collapse = true,
                "no-planner" => out.disable_planner = true,
                "no-lead" => out.disable_lead = true,
                "no-compaction" => out.disable_compaction = true,
                "no-harness-notes" => out.disable_harness_notes = true,
                "best-of-n" => out.best_of_n = Some(Self::parse_usize(key, value)?),
                "ttc" => {
                    let n = Self::parse_usize(key, value)? as u32;
                    if n == 0 {
                        return Err(BenchError::Startup(
                            "ttc=N requiere N >= 1 (N=1 es una fila normal)".to_string(),
                        ));
                    }
                    out.ttc_rollouts = Some(n);
                }
                "tactical-window" => out.tactical_window = Some(Self::parse_usize(key, value)?),
                "tactical-threshold" => {
                    out.tactical_compaction_threshold = Some(Self::parse_usize(key, value)?)
                }
                "full-observations" => {
                    out.tactical_full_observations = Some(Self::parse_usize(key, value)?)
                }
                "lead-turns" => out.lead_turns = Some(Self::parse_usize(key, value)?),
                "lead-threshold" => {
                    out.lead_failure_threshold = Some(Self::parse_usize(key, value)?)
                }
                "verify-gate" => {
                    // Bare `verify-gate` = max_rounds 2 (the pre-registered
                    // treatment); `verify-gate=N` overrides.
                    let n = match value {
                        None => 2,
                        Some(_) => Self::parse_usize(key, value)?,
                    };
                    out.verify_gate = Some(n);
                }
                "task-list" => out.enable_task_list = true,
                "explore" => out.enable_exploration = true,
                "editor" => out.enable_editor = true,
                "edit-fence" => out.enable_edit_fence = true,
                "prompt-tools" => out.enable_prompt_tools = true,
                "constrained-tools" => out.enable_constrained_tools = true,
                "project-memory" => out.enable_project_memory = true,
                "project-memory-seeded" => {
                    out.enable_project_memory = true;
                    out.seed_project_memory = true;
                }
                "lead-summary" => out.enable_lead_summary = true,
                "tool-search-threshold" => {
                    out.tool_search_threshold = Some(Self::parse_usize(key, value)?)
                }
                "lead-window" => out.lead_escalation_turns = Some(Self::parse_usize(key, value)?),
                "max-iterations" => {
                    let n = Self::parse_usize(key, value)?;
                    if n == 0 {
                        return Err(BenchError::Startup(
                            "max-iterations=N requiere N >= 1 (N=0 no correría ninguna ronda)"
                                .to_string(),
                        ));
                    }
                    out.max_turn_iterations = Some(n);
                }
                "gpu-layers" => out.gpu_layers = Some(Self::parse_usize(key, value)? as u32),
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

    /// Whether this row runs the Ollama executor in prompt-tools mode at
    /// all — `constrained-tools` implies `prompt-tools`
    /// (docs/constrained-decoding-ab-design.md § "Mecanismo mínimo",
    /// punto 5), so brazo C only needs its own key.
    pub fn prompt_tools_active(&self) -> bool {
        self.enable_prompt_tools || self.enable_constrained_tools
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
        if self.disable_syntactic_edit_gate {
            parts.push("no-syntactic-gate".to_string());
        }
        if self.disable_tool_output_spill {
            parts.push("no-spill".to_string());
        }
        if self.edit_strict_mode {
            parts.push("strict-edit".to_string());
        }
        if self.disable_prompt_caching {
            parts.push("no-caching".to_string());
        }
        if self.disable_observation_collapse {
            parts.push("no-prune".to_string());
        }
        if self.disable_planner {
            parts.push("no-planner".to_string());
        }
        if self.disable_lead {
            parts.push("no-lead".to_string());
        }
        if self.disable_compaction {
            parts.push("no-compaction".to_string());
        }
        if self.disable_harness_notes {
            parts.push("no-harness-notes".to_string());
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
        if self.enable_task_list {
            parts.push("task-list".to_string());
        }
        if self.enable_exploration {
            parts.push("explore".to_string());
        }
        if self.enable_editor {
            parts.push("editor".to_string());
        }
        if self.enable_edit_fence {
            parts.push("edit-fence".to_string());
        }
        if self.enable_prompt_tools {
            parts.push("prompt-tools".to_string());
        }
        if self.enable_constrained_tools {
            parts.push("constrained-tools".to_string());
        }
        if self.seed_project_memory {
            parts.push("project-memory-seeded".to_string());
        } else if self.enable_project_memory {
            parts.push("project-memory".to_string());
        }
        if self.enable_lead_summary {
            parts.push("lead-summary".to_string());
        }
        if let Some(n) = self.ttc_rollouts {
            parts.push(format!("ttc={n}"));
        }
        if let Some(n) = self.tool_search_threshold {
            parts.push(format!("tool-search-threshold={n}"));
        }
        if let Some(n) = self.lead_turns {
            parts.push(format!("lead-turns={n}"));
        }
        if let Some(n) = self.lead_failure_threshold {
            parts.push(format!("lead-threshold={n}"));
        }
        if let Some(n) = self.lead_escalation_turns {
            parts.push(format!("lead-window={n}"));
        }
        if let Some(n) = self.max_turn_iterations {
            parts.push(format!("max-iterations={n}"));
        }
        if let Some(n) = self.gpu_layers {
            parts.push(format!("gpu-layers={n}"));
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
    /// `None` defers to the model's Modelfile. Ignored by the
    /// anthropic/openrouter builders, whose wire formats don't take
    /// them through this crate yet — uniformity across a mixed sweep
    /// isn't achievable here, so `main` warns once per affected spec
    /// (H-13, via [`BackendSpec::non_ollama_halves`]) instead of
    /// letting the imbalance pass silently.
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

    /// H-13 (docs/AUDITORIA-2026-07-v5.md): `main` warns per spec with
    /// non-Ollama halves when the Ollama-only sampling knobs are set —
    /// these pin which halves count as "ignoring" for that warning.
    #[test]
    fn non_ollama_halves_is_empty_for_an_all_ollama_composite() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:gemma4:e4b").unwrap();
        assert!(spec.non_ollama_halves().is_empty());
    }

    #[test]
    fn non_ollama_halves_labels_each_half_by_role() {
        let spec = BackendSpec::parse(
            "openrouter:deepseek/deepseek-v4-flash+plan:ollama:qwen2.5:3b+lead:anthropic:claude-sonnet-5",
        )
        .unwrap();
        assert_eq!(
            spec.non_ollama_halves(),
            vec![
                "executor (openrouter)".to_string(),
                "lead (anthropic)".to_string()
            ]
        );
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

    // --- specs con lead reactivo (EscalatingBackend) ---

    #[test]
    fn parses_a_lead_spec_into_executor_and_lead_halves() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+lead:openrouter:anthropic/claude-sonnet-5")
                .unwrap();
        assert_eq!(spec.provider, Provider::Ollama);
        assert_eq!(spec.model_override.as_deref(), Some("qwen2.5:3b"));
        let lead = spec.lead.as_deref().expect("lead half expected");
        assert_eq!(lead.provider, Provider::OpenRouter);
        assert_eq!(
            lead.model_override.as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
    }

    #[test]
    fn display_name_of_a_lead_spec_shows_both_halves() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+lead:openrouter:anthropic/claude-sonnet-5")
                .unwrap();
        assert_eq!(
            spec.display_name(&config()),
            "ollama:qwen2.5:3b+lead:openrouter:anthropic/claude-sonnet-5"
        );
    }

    #[test]
    fn a_lead_spec_with_an_empty_half_is_a_startup_error() {
        assert!(matches!(
            BackendSpec::parse("+lead:openrouter:x"),
            Err(BenchError::Startup(_))
        ));
        assert!(matches!(
            BackendSpec::parse("ollama:x+lead:"),
            Err(BenchError::Startup(_))
        ));
    }

    #[test]
    fn a_spec_with_two_leads_is_a_startup_error() {
        assert!(matches!(
            BackendSpec::parse("ollama:x+lead:ollama:y+lead:ollama:z"),
            Err(BenchError::Startup(_))
        ));
    }

    #[test]
    fn plan_and_lead_suffixes_compose_in_either_order() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:qwen2.5:14b+plan:ollama:qwen2.5:7b")
                .unwrap();
        assert_eq!(
            spec.planner
                .as_deref()
                .and_then(|planner| planner.model_override.as_deref()),
            Some("qwen2.5:7b")
        );
        assert_eq!(
            spec.lead
                .as_deref()
                .and_then(|lead| lead.model_override.as_deref()),
            Some("qwen2.5:14b")
        );
        assert_eq!(
            spec.display_name(&config()),
            "ollama:qwen2.5:3b+plan:ollama:qwen2.5:7b+lead:ollama:qwen2.5:14b"
        );
    }

    #[test]
    fn ollama_models_reports_executor_local_planner_and_local_lead() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+plan:ollama:qwen2.5:7b+lead:ollama:qwen2.5:14b")
                .unwrap();
        assert_eq!(
            spec.ollama_models(&config()),
            vec![
                "qwen2.5:3b".to_string(),
                "qwen2.5:7b".to_string(),
                "qwen2.5:14b".to_string(),
            ]
        );

        let remote_lead =
            BackendSpec::parse("ollama:qwen2.5:3b+lead:openrouter:anthropic/x").unwrap();
        assert_eq!(
            remote_lead.ollama_models(&config()),
            vec!["qwen2.5:3b".to_string()]
        );
    }

    #[test]
    fn build_lead_is_none_for_a_plain_spec_and_some_for_a_lead_spec() {
        let plain = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        assert!(
            plain
                .build_lead(&config(), sampling())
                .expect("plain spec must not error")
                .is_none()
        );

        let led = BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b").unwrap();
        assert!(
            led.build_lead(&config(), sampling())
                .expect("ollama lead must build without credentials")
                .is_some()
        );
    }

    #[test]
    fn build_lead_without_credentials_is_a_startup_error() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+lead:openrouter:anthropic/x").unwrap();
        let result = spec.build_lead(&config(), sampling());
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn build_agent_model_wraps_executor_when_lead_is_configured() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b").unwrap();
        let model = spec
            .build_agent_model(&config(), sampling())
            .expect("ollama lead and worker must build");
        assert_eq!(model.name(), "escalating(ollama->ollama)");
    }

    // --- escalation knobs (I-1, docs/AUDITORIA-2026-07-v6.md) ---

    // --- resolve_pricing (Paquete 3, docs/AUDITORIA-2026-07-v6.md) ---

    /// Simple Ollama spec → the catch-all $0 entry resolves.
    #[test]
    fn resolve_pricing_resolves_a_simple_ollama_spec_to_zero() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        let pricing = spec.resolve_pricing(&config()).expect("ollama is priced");
        assert_eq!(pricing.input_usd_per_mtok, 0.0);
        assert_eq!(pricing.output_usd_per_mtok, 0.0);
    }

    /// All-Ollama composite: every half resolves to the same $0 rates →
    /// costable.
    #[test]
    fn resolve_pricing_resolves_an_all_ollama_composite() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:gemma4:e4b").unwrap();
        assert!(spec.resolve_pricing(&config()).is_some());
    }

    /// A composite whose halves bill at DIFFERENT rates can't be costed
    /// from the event log (Usage doesn't attribute rounds to models) —
    /// `None`, never a guess.
    #[test]
    fn resolve_pricing_refuses_a_mixed_rate_composite() {
        let mut cfg = config();
        cfg.openrouter_api_key = Some(braze_config::ApiKey::new("k"));
        cfg.model_pricing.push(braze_config::ModelPricing {
            backend: "openrouter".to_string(),
            model_prefix: "caro/modelo".to_string(),
            input_usd_per_mtok: 5.0,
            output_usd_per_mtok: 15.0,
            cache_read_usd_per_mtok: None,
            cache_write_usd_per_mtok: None,
        });
        let spec = BackendSpec::parse("ollama:qwen2.5:3b+lead:openrouter:caro/modelo").unwrap();
        assert!(spec.resolve_pricing(&cfg).is_none());
    }

    /// An unlisted model anywhere in the spec → `None`.
    #[test]
    fn resolve_pricing_is_none_for_an_unlisted_model() {
        let spec = BackendSpec::parse("openrouter:z-ai/glm-5.2").unwrap();
        assert!(spec.resolve_pricing(&config()).is_none());
    }

    /// The wiring gap I-1 exists to close: before `build_escalating`,
    /// NO knob ever reached the decorator — every `+lead:` row ran the
    /// proactive 3-turn opening regardless of config. These assert the
    /// three-way precedence: ablation > config > decorator default.
    #[test]
    fn build_escalating_applies_config_knobs_when_no_ablation_overrides_them() {
        let mut cfg = config();
        cfg.lead_turns = Some(0); // purely reactive
        cfg.lead_failure_threshold = Some(4);
        cfg.lead_escalation_turns = Some(2);

        let spec = BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b").unwrap();
        let escalating = spec
            .build_escalating(&cfg, sampling())
            .expect("must build")
            .expect("a +lead: spec composes an EscalatingBackend");
        assert_eq!(escalating.lead_turns(), 0);
        assert_eq!(escalating.failure_threshold(), 4);
        assert_eq!(escalating.escalation_turns(), 2);
    }

    #[test]
    fn an_ablate_lead_key_overrides_the_config_value() {
        let mut cfg = config();
        cfg.lead_turns = Some(5); // config says 5...

        let spec = BackendSpec::parse(
            "ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b+ablate:lead-turns=0;lead-threshold=3",
        )
        .unwrap();
        let escalating = spec
            .build_escalating(&cfg, sampling())
            .expect("must build")
            .expect("a +lead: spec composes an EscalatingBackend");
        // ...but the per-spec ablation wins: this row is the
        // purely-reactive arm of the A/B.
        assert_eq!(escalating.lead_turns(), 0);
        assert_eq!(escalating.failure_threshold(), 3);
    }

    #[test]
    fn build_escalating_is_none_for_a_spec_without_lead() {
        let spec = BackendSpec::parse("ollama:qwen2.5:3b").unwrap();
        assert!(
            spec.build_escalating(&config(), sampling())
                .expect("plain spec must not error")
                .is_none()
        );
    }

    /// The six boolean levers of the ablation matrix (H-2 no-caching,
    /// opencode-2 no-prune, E1 no-planner/no-lead/no-compaction, A′.2
    /// no-harness-notes) parse and round-trip through the display name —
    /// the row identity a results.json reader dedupes on.
    #[test]
    fn parses_the_ablation_matrix_boolean_keys_and_displays_them_back() {
        let spec = BackendSpec::parse(
            "ollama:qwen2.5:3b+ablate:no-caching;no-prune;no-planner;no-lead;no-compaction;no-harness-notes",
        )
        .unwrap();
        let ablation = spec.ablation();
        assert!(ablation.disable_prompt_caching);
        assert!(ablation.disable_observation_collapse);
        assert!(ablation.disable_planner);
        assert!(ablation.disable_lead);
        assert!(ablation.disable_compaction);
        assert!(ablation.disable_harness_notes);
        let display = spec.display_name(&config());
        for key in [
            "no-caching",
            "no-prune",
            "no-planner",
            "no-lead",
            "no-compaction",
            "no-harness-notes",
        ] {
            assert!(display.contains(key), "missing {key} in: {display}");
        }
    }

    /// E1 `+ablate:no-lead`: the spec keeps its `+lead:` half (display
    /// identity, ollama_models listing) but composes NO EscalatingBackend
    /// — the worker runs bare, so the pair (with/without the suffix)
    /// differs in exactly one lever.
    #[test]
    fn an_ablate_no_lead_spec_builds_the_bare_worker() {
        let spec =
            BackendSpec::parse("ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b+ablate:no-lead").unwrap();
        assert!(
            spec.build_escalating(&config(), sampling())
                .expect("must build")
                .is_none(),
            "no-lead must suppress the EscalatingBackend wrapper"
        );
        let model = spec
            .build_agent_model(&config(), sampling())
            .expect("must build");
        assert_eq!(
            model.name(),
            "ollama",
            "the bare worker, not escalating(...)"
        );
    }

    #[test]
    fn parses_lead_knob_ablation_keys_and_displays_them_back() {
        let spec = BackendSpec::parse(
            "ollama:qwen2.5:3b+lead:ollama:qwen2.5:7b+ablate:lead-turns=0;lead-threshold=2;lead-window=4",
        )
        .unwrap();
        let ablation = spec.ablation();
        assert_eq!(ablation.lead_turns, Some(0));
        assert_eq!(ablation.lead_failure_threshold, Some(2));
        assert_eq!(ablation.lead_escalation_turns, Some(4));
        // The display name round-trips the keys, so a results.json row is
        // traceable back to the exact arm that produced it.
        let display = spec.display_name(&config());
        assert!(display.contains("lead-turns=0"), "got: {display}");
        assert!(display.contains("lead-threshold=2"), "got: {display}");
        assert!(display.contains("lead-window=4"), "got: {display}");
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

    /// A/B del impuesto JSON: la clave que habilita el canal
    /// SEARCH/REPLACE textual, con su identidad en el display name (el
    /// JSON del sweep queda autodocumentado vía `backend_specs`).
    #[test]
    fn parses_edit_fence_enabling_key_and_displays_it() {
        let spec = BackendSpec::parse("ollama:gemma4:e4b+ablate:edit-fence").unwrap();
        assert!(spec.ablation().enable_edit_fence);
        assert!(
            spec.display_name(&braze_config::Config::default())
                .ends_with("+ablate:edit-fence")
        );
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

    /// J-22 (docs/AUDITORIA-2026-07-v7.md): `RECOGNIZED_KEYS` drifted
    /// from the parser — the unknown-key error listed keys as "the valid
    /// ones" while omitting three that parse fine, so a typo on an
    /// omitted key got help implying the lever didn't exist. This pins
    /// the advertised list to the parser: every advertised key must
    /// actually parse. (The reverse direction — a new match arm without
    /// a `RECOGNIZED_KEYS` entry — is what the const's doc comment
    /// instructs; there's no way to enumerate match arms at runtime.)
    #[test]
    fn recognized_keys_lists_every_parseable_key() {
        for entry in AblationOverrides::RECOGNIZED_KEYS.split(',') {
            let entry = entry.trim();
            // "best-of-n=N" advertises a numeric key: exercise it with a
            // real value; bare keys parse as-is.
            let pair = match entry.strip_suffix("=N") {
                Some(key) => format!("{key}=3"),
                None => entry.to_string(),
            };
            let spec = format!("ollama:qwen2.5:3b+ablate:{pair}");
            assert!(
                BackendSpec::parse(&spec).is_ok(),
                "advertised '+ablate:' key '{entry}' does not actually parse"
            );
        }
    }

    /// round-economics: el factorial entero (dos precios de ronda × dos
    /// configuraciones) tiene que caber en UN sweep, porque el pareo
    /// (tarea, repetición) de McNemar es dentro de la corrida. Estas dos
    /// llaves son lo que lo permite, y ambas tienen que sobrevivir al
    /// `display_name` — si no, las cuatro filas se ven iguales en la
    /// tabla y en el JSON.
    #[test]
    fn the_round_economics_factorial_fits_in_one_sweep_and_stays_distinguishable() {
        let config = Config::default();
        let avara_cpu =
            BackendSpec::parse("local:qwen2.5:3b+ablate:max-iterations=4;gpu-layers=0").unwrap();
        let derrochadora_gpu =
            BackendSpec::parse("local:qwen2.5:3b+ablate:max-iterations=40;ttc=3;gpu-layers=99")
                .unwrap();

        assert_eq!(avara_cpu.ablation().max_turn_iterations, Some(4));
        assert_eq!(avara_cpu.ablation().gpu_layers, Some(0));
        assert_eq!(derrochadora_gpu.ablation().max_turn_iterations, Some(40));
        assert_eq!(derrochadora_gpu.ablation().gpu_layers, Some(99));
        assert_eq!(derrochadora_gpu.ablation().ttc_rollouts, Some(3));

        let avara_name = avara_cpu.display_name(&config);
        let derrochadora_name = derrochadora_gpu.display_name(&config);
        assert!(
            avara_name.contains("max-iterations=4") && avara_name.contains("gpu-layers=0"),
            "el brazo avaro/CPU tiene que ser identificable en la tabla: {avara_name}"
        );
        assert!(
            derrochadora_name.contains("max-iterations=40")
                && derrochadora_name.contains("gpu-layers=99"),
            "el brazo derrochador/GPU tiene que ser identificable en la tabla: \
             {derrochadora_name}"
        );
        assert_ne!(avara_name, derrochadora_name);
    }

    /// `max-iterations=0` no correría ninguna ronda — un brazo que falla
    /// el 100% de las tareas sin llamar al modelo, y que en la tabla se ve
    /// igual que un modelo incapaz.
    #[test]
    fn max_iterations_zero_is_a_startup_error() {
        let result = BackendSpec::parse("local:qwen2.5:3b+ablate:max-iterations=0");
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    #[test]
    fn a_numeric_ablate_key_missing_its_value_is_a_startup_error() {
        let result = BackendSpec::parse("ollama:qwen2.5:3b+ablate:best-of-n");
        assert!(matches!(result, Err(BenchError::Startup(_))));
    }

    /// Brazos B/C del A/B de constrained decoding
    /// (docs/constrained-decoding-ab-design.md): each arm's key parses,
    /// echoes in the display name (so baseline and ablated rows stay
    /// distinguishable in the report), and `constrained-tools` implies
    /// prompt-tools mode without needing both keys.
    #[test]
    fn prompt_tools_and_constrained_tools_keys_parse_and_display() {
        let config = Config::default();

        let b_arm = BackendSpec::parse("ollama:llama3.2:1b+ablate:prompt-tools").unwrap();
        assert!(b_arm.ablation().enable_prompt_tools);
        assert!(!b_arm.ablation().enable_constrained_tools);
        assert!(b_arm.ablation().prompt_tools_active());
        assert_eq!(
            b_arm.display_name(&config),
            "ollama:llama3.2:1b+ablate:prompt-tools"
        );

        let c_arm = BackendSpec::parse("ollama:llama3.2:1b+ablate:constrained-tools").unwrap();
        assert!(!c_arm.ablation().enable_prompt_tools);
        assert!(c_arm.ablation().enable_constrained_tools);
        assert!(
            c_arm.ablation().prompt_tools_active(),
            "constrained-tools must imply prompt-tools mode"
        );
        assert_eq!(
            c_arm.display_name(&config),
            "ollama:llama3.2:1b+ablate:constrained-tools"
        );

        let baseline = BackendSpec::parse("ollama:llama3.2:1b").unwrap();
        assert!(!baseline.ablation().prompt_tools_active());
    }

    /// `+ablate:project-memory` — same enabling-key exception as
    /// `task-list` (docs/project-memory-design.md).
    #[test]
    fn project_memory_key_parses_and_displays() {
        let config = Config::default();

        let arm = BackendSpec::parse("ollama:llama3.2:1b+ablate:project-memory").unwrap();
        assert!(arm.ablation().enable_project_memory);
        assert_eq!(
            arm.display_name(&config),
            "ollama:llama3.2:1b+ablate:project-memory"
        );

        let baseline = BackendSpec::parse("ollama:llama3.2:1b").unwrap();
        assert!(!baseline.ablation().enable_project_memory);
    }

    /// `+ablate:project-memory-seeded` — the injection arm: implies the
    /// hook AND asks the runner for a synthesized seed. Its display name
    /// must say `seeded` (not the plain key) so H-17's per-row record
    /// distinguishes the two arms in the sweep JSON.
    #[test]
    fn project_memory_seeded_key_implies_the_hook_and_displays_as_seeded() {
        let config = Config::default();

        let arm = BackendSpec::parse("ollama:llama3.2:1b+ablate:project-memory-seeded").unwrap();
        assert!(arm.ablation().enable_project_memory);
        assert!(arm.ablation().seed_project_memory);
        assert_eq!(
            arm.display_name(&config),
            "ollama:llama3.2:1b+ablate:project-memory-seeded"
        );

        // The plain arm must NOT claim to be seeded.
        let plain = BackendSpec::parse("ollama:llama3.2:1b+ablate:project-memory").unwrap();
        assert!(!plain.ablation().seed_project_memory);
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
