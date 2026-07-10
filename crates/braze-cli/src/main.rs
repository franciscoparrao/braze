//! braze terminal binary: `braze chat` (interactive) and `braze run <prompt>`
//! (one-shot).
//!
//! This is the only place in the workspace that installs the `tracing`
//! subscriber (per PLAN.md: libraries emit traces, only the binary decides
//! how they're rendered) and the only place that composes every crate in
//! the workspace into a running [`braze_engine::Engine`].

mod cli_args;
mod error;
mod terminal_prompt;

use std::process::ExitCode;

use clap::Parser;
use tokio::io::AsyncBufReadExt;

use braze_events::{ChannelTaskNotifier, TextDeltaObserver};
use braze_types::SessionId;
use cli_args::{Cli, Command};
use error::CliError;
use terminal_prompt::TerminalConfirmationPrompt;

#[tokio::main]
async fn main() -> ExitCode {
    // Installed exactly once, here, never in any library crate — respects
    // `RUST_LOG`, writes to stderr so it never interleaves with the
    // conversation printed to stdout.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("braze: error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Builds a fresh `PermissionGuard` scoped to `cwd`: a `WorkdirAllowlist` +
/// `DefaultClassifier` pair (each needs its own `WorkdirAllowlist` instance
/// since it isn't `Clone`) plus a `ConfirmationPrompt`. Every `ToolProvider`
/// this binary constructs (the local tools provider, and one per connected
/// MCP server) gets its own guard from this same helper, each with an
/// independent in-memory "remembered" session cache — seeded from
/// `replayed_keys` so approvals confirmed earlier in this same session
/// (before a restart) aren't re-asked.
///
/// `tui_mode` selects which `ConfirmationPrompt` gets built: the plain
/// path's `TerminalConfirmationPrompt` reads y/n answers from stdin, which
/// carries `session`/`store` so it can persist
/// `PermissionRequested`/`PermissionDecided` events for later `--resume`
/// replay — but its stdin reads don't work correctly once the terminal is
/// in raw mode (`braze-tui`'s requirement). Under `--tui` this builds
/// `braze_tui::ChannelConfirmationPrompt` instead, which asks over
/// `approval_tx` (the sending half of the channel `braze_tui::run`'s
/// caller also holds the receiving half of) rather than blocking on
/// stdin — same session-store persistence, different question channel.
fn build_permission_guard(
    cwd: &std::path::Path,
    live_session: std::sync::Arc<std::sync::Mutex<braze_types::SessionId>>,
    store: std::sync::Arc<dyn braze_session::SessionStore>,
    replayed_keys: &[braze_types::PermissionKey],
    tui_mode: bool,
    supervised: bool,
    approval_tx: tokio::sync::mpsc::UnboundedSender<braze_tui::ApprovalRequest>,
) -> braze_permissions::PermissionGuard {
    let allowlist_for_classifier = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let allowlist_for_guard = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    // `--supervised`: every action goes through the confirmation prompt
    // below, regardless of what `DefaultClassifier` would normally rate
    // it — see `AlwaysIrreversibleClassifier`'s doc comment.
    let classifier: Box<dyn braze_permissions::ActionClassifier> = if supervised {
        Box::new(braze_permissions::AlwaysIrreversibleClassifier)
    } else {
        Box::new(braze_permissions::DefaultClassifier::new(
            allowlist_for_classifier,
        ))
    };
    let confirmation: Box<dyn braze_permissions::ConfirmationPrompt> = if tui_mode {
        // N-12 (docs/AUDITORIA-2026-07-v2.md): the TUI's confirmation
        // prompt reads the *current* session out of this shared handle
        // on every `confirm()` call — `App::backtrack_to` writes a fresh
        // id into the identical `Arc` once the user backtracks, so a
        // later permission decision lands in the right session's
        // rollout log instead of the one this guard was built for.
        Box::new(braze_tui::ChannelConfirmationPrompt::new(
            live_session,
            store,
            approval_tx,
        ))
    } else {
        // The plain chat/run loop has no backtrack (a TUI-only feature)
        // and never re-seeds this after startup, so a one-time read is
        // equivalent to holding a live handle.
        let session = *live_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Box::new(TerminalConfirmationPrompt::new(session, store))
    };
    let guard =
        braze_permissions::PermissionGuard::new(allowlist_for_guard, classifier, confirmation);
    guard.seed_remembered(replayed_keys.iter().cloned());
    guard
}

/// Builds one `ModelBackend` by name from the already-resolved config —
/// the primary (executor) backend and, when the planner/executor split is
/// enabled, the planner too (PLAN.md § "Split planificador/ejecutor") go
/// through this same constructor, so both get identical credential
/// resolution and error messages. `model_override` takes precedence over
/// the backend's configured model; the primary passes `None` because its
/// `--model` override was already folded into `config` via
/// `apply_overrides`.
fn build_model_backend(
    config: &braze_config::Config,
    backend: &str,
    model_override: Option<&str>,
) -> Result<Box<dyn braze_model::ModelBackend>, CliError> {
    match backend {
        "anthropic" => {
            let api_key = config.anthropic_api_key.clone().ok_or_else(|| {
                CliError::Startup(
                    "falta ANTHROPIC_API_KEY (config file, BRAZE_ANTHROPIC_API_KEY, o --backend anthropic sin key configurada)"
                        .to_string(),
                )
            })?;
            let model_name = model_override
                .map(str::to_string)
                .or_else(|| config.anthropic_model.clone())
                .ok_or_else(|| {
                    CliError::Startup(
                        "falta --model o BRAZE_ANTHROPIC_MODEL para el backend anthropic"
                            .to_string(),
                    )
                })?;
            Ok(Box::new(braze_model::AnthropicBackend::new(
                api_key.expose_secret().to_string(),
                model_name,
            )))
        }
        "ollama" => {
            let model_name = model_override
                .map(str::to_string)
                .unwrap_or_else(|| config.ollama_model.clone());
            let mut backend = braze_model::OllamaBackend::with_base_url(
                model_name,
                config.ollama_base_url.clone(),
            )
            .with_num_ctx(config.ollama_num_ctx);
            // D2 (docs/AUDITORIA-2026-07-v3.md): these five knobs existed
            // on `OllamaBackend` and as `braze-bench` CLI flags, but were
            // never wired into a real invocation — a sampling regime found
            // better in a bench sweep (e.g. Qwen's own recommended temp
            // 0.7/top_p 0.8/top_k 20/repeat_penalty 1.05) could be
            // measured but never actually applied to `braze chat`/`braze
            // run`. `None` (the default for all five) leaves
            // `OllamaBackend`'s own defaults in place, unchanged.
            if let Some(temperature) = config.ollama_temperature {
                backend = backend.with_temperature(temperature);
            }
            if let Some(seed) = config.ollama_seed {
                backend = backend.with_seed(seed);
            }
            if let Some(top_p) = config.ollama_top_p {
                backend = backend.with_top_p(top_p);
            }
            if let Some(top_k) = config.ollama_top_k {
                backend = backend.with_top_k(top_k);
            }
            if let Some(repeat_penalty) = config.ollama_repeat_penalty {
                backend = backend.with_repeat_penalty(repeat_penalty);
            }
            Ok(Box::new(backend))
        }
        "openrouter" => {
            let api_key = config.openrouter_api_key.clone().ok_or_else(|| {
                CliError::Startup(
                    "falta OPENROUTER_API_KEY (config file, BRAZE_OPENROUTER_API_KEY, o --backend openrouter sin key configurada)"
                        .to_string(),
                )
            })?;
            let model_name = model_override
                .map(str::to_string)
                .or_else(|| config.openrouter_model.clone())
                .ok_or_else(|| {
                    CliError::Startup(
                        "falta --model o BRAZE_OPENROUTER_MODEL para el backend openrouter"
                            .to_string(),
                    )
                })?;
            Ok(Box::new(
                braze_model::OpenRouterBackend::with_base_url(
                    api_key.expose_secret().to_string(),
                    model_name,
                    config.openrouter_base_url.clone(),
                )
                .with_prompt_caching_enabled(config.enable_prompt_caching),
            ))
        }
        other => Err(CliError::Startup(format!(
            "unknown backend '{other}' (expected 'anthropic', 'ollama', or 'openrouter')"
        ))),
    }
}

/// Maps a model-name override onto the config field belonging to
/// `backend` — shared by the startup `--model` flag and the TUI `/model`
/// factory, so both reject an unknown backend with the identical error.
fn model_override_for(
    backend: &str,
    model: &str,
) -> Result<braze_config::ConfigOverrides, CliError> {
    match backend {
        "anthropic" => Ok(braze_config::ConfigOverrides {
            anthropic_model: Some(model.to_string()),
            ..Default::default()
        }),
        "ollama" => Ok(braze_config::ConfigOverrides {
            ollama_model: Some(model.to_string()),
            ..Default::default()
        }),
        "openrouter" => Ok(braze_config::ConfigOverrides {
            openrouter_model: Some(model.to_string()),
            ..Default::default()
        }),
        other => Err(CliError::Startup(format!(
            "unknown backend '{other}' (expected 'anthropic', 'ollama', or 'openrouter')"
        ))),
    }
}

/// Composes a complete [`braze_engine::Engine`] (plus its short
/// "backend:model" status-bar label) from an already-resolved config:
/// permission guards seeded from the live session's prior approvals,
/// local + MCP tool providers, compactor, system prompt, context budget
/// and the optional planner backend. Extracted from `run` so the TUI's
/// `/model` switch (PLAN.md § "fase TUI 2") can rebuild the engine
/// mid-session with a different backend/model — same composition root,
/// same session id, fresh `ModelBackend`.
///
/// `planner_spec` is `(backend, model_override)` — resolved by the caller
/// (CLI `--planner` wins over config) and passed through verbatim so a
/// mid-session rebuild preserves exactly the planner the run started
/// with. Reads `live_session` fresh for the approval replay, so a rebuild
/// after a TUI backtrack seeds from the session the user is actually on.
#[allow(clippy::too_many_arguments)] // the composition root: one param per resolved collaborator
async fn build_engine(
    config: &braze_config::Config,
    planner_spec: Option<(String, Option<String>)>,
    lead_spec: Option<(String, Option<String>)>,
    live_session: std::sync::Arc<std::sync::Mutex<braze_types::SessionId>>,
    store: std::sync::Arc<dyn braze_session::SessionStore>,
    approval_tx: tokio::sync::mpsc::UnboundedSender<braze_tui::ApprovalRequest>,
    tui_mode: bool,
    supervised: bool,
    cwd: &std::path::Path,
) -> Result<(braze_engine::Engine, String), CliError> {
    let mut model = build_model_backend(config, &config.default_backend, None)?;

    // Reactive lead/worker escalation (estilo Goose, ítem 6 del backlog
    // 2026-07-06): the primary backend becomes the *worker*; the lead
    // opens the session and returns whenever the worker strings failed
    // observations together. A decorator around `ModelBackend`, invisible
    // to the engine — and composable with the planner below (proactive
    // model change by phase vs reactive by observed failure).
    if let Some((backend, model_override)) = &lead_spec {
        let lead = build_model_backend(config, backend, model_override.as_deref())?;
        // I-1 (docs/AUDITORIA-2026-07-v6.md): the three escalation knobs
        // from config/env (`lead_turns`/`lead_failure_threshold`/
        // `lead_escalation_turns`, `None` = decorator default) — before
        // this, `braze chat --lead` always ran the proactive 3-turn
        // opening with no way to request the purely-reactive mode
        // (`lead_turns = 0`).
        model = Box::new(
            braze_model::EscalatingBackend::new(lead, model).with_configured_knobs(
                config.lead_turns,
                config.lead_failure_threshold,
                config.lead_escalation_turns,
            ),
        );
    }

    let planner = planner_spec
        .map(|(backend, model_override)| {
            build_model_backend(config, &backend, model_override.as_deref())
        })
        .transpose()?;

    let session = *live_session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Replays previously-*approved* permission decisions for this exact
    // session (a no-op for a brand-new session, since `load` finds
    // nothing) so `braze chat --resume` — and a mid-session engine
    // rebuild — doesn't re-ask for actions already confirmed earlier in
    // the same conversation. The "unknown --resume id" warning lives in
    // `run` (it needs the CLI args), not here.
    let replayed_keys: Vec<braze_types::PermissionKey> = match store.load(&session).await {
        Ok(events) => events
            .into_iter()
            .filter_map(|event| match event {
                braze_events::AgentEvent::PermissionDecided {
                    allowed: true,
                    key: Some(key),
                    ..
                } => Some(key),
                _ => None,
            })
            .collect(),
        Err(braze_session::SessionError::NotFound(_)) => Vec::new(),
        Err(err) => return Err(err.into()),
    };

    // Two-layer permission setup: a soft working-dir allowlist plus a
    // default-deny shell classifier + terminal y/n confirmation for
    // irreversible actions. Every `ToolProvider` (local tools, and now each
    // MCP server) gets its own freshly built `PermissionGuard` — see
    // `build_permission_guard` — since `PermissionGuard` isn't
    // shared/`Clone` across providers. All of them share the same
    // `replayed_keys`, seeded from the same session's own prior decisions.
    let local_guard = build_permission_guard(
        cwd,
        std::sync::Arc::clone(&live_session),
        std::sync::Arc::clone(&store),
        &replayed_keys,
        tui_mode,
        supervised,
        approval_tx.clone(),
    );

    let local_provider = braze_tools_local::LocalToolsProvider::new(local_guard)
        .with_post_edit_check(!config.disable_post_edit_check)
        .with_output_budget(config.tool_output_max_bytes as usize)
        .with_output_max_lines(config.tool_output_max_lines)
        .with_formatters(config.formatters.clone());
    let mut providers: Vec<Box<dyn braze_tools_core::ToolProvider>> =
        vec![Box::new(local_provider)];

    // D5 (auditoría 2026-07): two `mcp_servers` entries sharing a `name`
    // would produce identical `mcp__<name>__<tool>` advertised names,
    // reintroducing the exact collision namespacing exists to prevent.
    // `ToolRegistry::all_stubs` only catches this at runtime (a warning per
    // round) — this catches the common config-typo case once, at startup.
    {
        let mut seen_server_names = std::collections::HashSet::new();
        for server in &config.mcp_servers {
            if !seen_server_names.insert(server.name.as_str()) {
                tracing::warn!(
                    server = %server.name,
                    "two mcp_servers entries share the same name — their tools will \
                     collide under mcp__<name>__<tool> namespacing"
                );
            }
        }
    }

    // Best-effort: a dead/misconfigured MCP server never aborts startup,
    // it's just unavailable for this run (logged as a warning).
    for server in &config.mcp_servers {
        let mcp_guard = build_permission_guard(
            cwd,
            std::sync::Arc::clone(&live_session),
            std::sync::Arc::clone(&store),
            &replayed_keys,
            tui_mode,
            supervised,
            approval_tx.clone(),
        );
        match braze_mcp_client::McpToolProvider::connect(
            server.name.clone(),
            server.command.clone(),
            server.args.clone(),
            mcp_guard,
        )
        .await
        {
            Ok(provider) => providers.push(Box::new(provider)),
            Err(err) => {
                tracing::warn!(
                    server = %server.name,
                    error = %err,
                    "failed to connect to MCP server; continuing without it"
                );
            }
        }
    }

    let tools = braze_tools_core::ToolRegistry::new(providers);
    let notifier = ChannelTaskNotifier::new();
    // C10 (docs/AUDITORIA-2026-07.md): tactical window/threshold come from
    // config instead of `SimpleContextCompactor::default()`'s hardcoded
    // constant, so they can be tuned per backend without recompiling.
    let compactor = braze_session::SimpleContextCompactor::new(config.tactical_window);

    // D1 (docs/AUDITORIA-2026-07-v3.md): only the Ollama executor gets a
    // model-name hint — Anthropic/OpenRouter's native tool-calling needs
    // no textual fallback example.
    let model_hint_for_prompt =
        (config.default_backend == "ollama").then(|| config.ollama_model.clone());
    let system_prompt = config.system_prompt.clone().unwrap_or_else(|| {
        braze_config::default_system_prompt(cwd, model_hint_for_prompt.as_deref())
    });

    // Only Ollama has a small, fixed context window worth budgeting for
    // (Anthropic's is large enough that raw event count remains a fine
    // proxy). Computed here, before `tools`/`system_prompt` move into
    // `Engine::new` below (hallazgo B4, docs/AUDITORIA-2026-07-v3.md): the
    // margin needs the real system prompt length plus the size of every
    // currently-advertised tool stub (including MCP ones), not a fixed
    // constant that can't grow with how many tools are configured.
    let ollama_budget = if config.default_backend == "ollama" {
        let tool_definitions_bytes =
            braze_tools_core::tool_stub_definition_bytes(&tools.all_stubs_lossy().await);
        Some(braze_config::ollama_context_budget_tokens(
            config.ollama_num_ctx,
            config.max_tokens,
            &system_prompt,
            tool_definitions_bytes,
        ))
    } else {
        None
    };

    // Short "backend:model" label for the TUI's status bar — computed
    // here (not inside `braze-tui`) since `Engine` doesn't expose the
    // model name of the `ModelBackend` it was built with.
    let status_line = match config.default_backend.as_str() {
        "anthropic" => format!(
            "anthropic:{}",
            config.anthropic_model.as_deref().unwrap_or("?")
        ),
        "ollama" => format!("ollama:{}", config.ollama_model),
        "openrouter" => format!(
            "openrouter:{}",
            config.openrouter_model.as_deref().unwrap_or("?")
        ),
        other => other.to_string(),
    };

    // D6 (docs/AUDITORIA-2026-07-v3.md): `best_of_n` is a real model call
    // per candidate — fine for a cheap/parallel cloud backend, a footgun
    // against a local Ollama model already at 90-100s/task on CPU (N×
    // the latency, serialized). The default (1) is unaffected; this only
    // warns when a user has explicitly raised it while also running
    // Ollama.
    if config.default_backend == "ollama" && config.best_of_n > 1 {
        tracing::warn!(
            best_of_n = config.best_of_n,
            "best_of_n > 1 against the Ollama backend multiplies latency by best_of_n \
             (each candidate is a full sequential model call) — this technique pays off on \
             cheap/parallel cloud backends, not a local model already CPU-bound per turn"
        );
    }

    let mut engine = braze_engine::Engine::new(
        model,
        tools,
        std::sync::Arc::clone(&store),
        Box::new(compactor),
        Box::new(notifier),
        system_prompt,
        config.max_tokens,
    )
    .with_tactical_compaction_threshold(config.tactical_compaction_threshold)
    .with_best_of_n(config.best_of_n)
    .with_textual_rescue_enabled(!config.disable_textual_tool_call_rescue)
    .with_max_turn_iterations(config.max_turn_iterations as usize)
    .with_planner_max_tokens(config.planner_max_tokens)
    // v4 P0.2: circuit breaker por tokens acumulados por turno — None
    // (default) lo deshabilita.
    .with_max_turn_total_tokens(config.max_turn_total_tokens);

    if let Some(budget) = ollama_budget {
        engine = engine.with_context_budget(budget);
    }

    if let Some(planner) = planner {
        engine = engine.with_planner(planner);
    }

    Ok((engine, status_line))
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let mut config = braze_config::Config::load()?;

    // Resolved early, before the model backend/guards/tools are built, so
    // both `build_permission_guard` (for `--resume` replay) and
    // `Engine::new` can share the exact same session id and `SessionStore`
    // handle from this point on.
    let session: SessionId = match &cli.command {
        Command::Run { .. } => SessionId::new(),
        Command::Chat {
            resume: Some(id_str),
            ..
        } => id_str
            .parse::<SessionId>()
            .map_err(|err| CliError::Startup(format!("invalid session id '{id_str}': {err}")))?,
        Command::Chat { resume: None, .. } => SessionId::new(),
    };

    // `--backend` is applied first so that a bare `--model` (with no
    // `--backend`) is unambiguous: it overrides whichever model field
    // belongs to the backend actually in effect after this step.
    if let Some(backend) = cli.command.backend_override() {
        let overrides = braze_config::ConfigOverrides {
            default_backend: Some(backend.to_string()),
            ..Default::default()
        };
        config.apply_overrides(overrides);
    }
    if let Some(model) = cli.command.model_override() {
        let overrides = model_override_for(&config.default_backend, model)?;
        config.apply_overrides(overrides);
    }
    if let Some(url) = cli.command.ollama_url_override() {
        config.apply_overrides(braze_config::ConfigOverrides {
            ollama_base_url: Some(url.to_string()),
            ..Default::default()
        });
    }
    if let Some(theme) = cli.command.theme_override() {
        config.apply_overrides(braze_config::ConfigOverrides {
            tui_theme: Some(theme.to_string()),
            ..Default::default()
        });
    }

    // Planner/executor split (PLAN.md § "Split planificador/ejecutor"):
    // `--planner` wins over `planner_backend`/`planner_model` from
    // config/env. Resolved here (not inside `build_engine`) so the TUI's
    // `/model` rebuild preserves exactly the planner this run started
    // with, and an invalid spec fails at startup with a clear error
    // (`build_engine` validates it eagerly), before anything else happens.
    let planner_spec: Option<(String, Option<String>)> = match cli.command.planner_override() {
        Some((backend, model_override)) => {
            Some((backend.to_string(), model_override.map(str::to_string)))
        }
        None => config
            .planner_backend
            .clone()
            .map(|backend| (backend, config.planner_model.clone())),
    };
    // Reactive lead/worker escalation — resolved exactly like the
    // planner: `--lead` wins over `lead_backend`/`lead_model` from
    // config/env, and an invalid spec fails at startup.
    let lead_spec: Option<(String, Option<String>)> = match cli.command.lead_override() {
        Some((backend, model_override)) => {
            Some((backend.to_string(), model_override.map(str::to_string)))
        }
        None => config
            .lead_backend
            .clone()
            .map(|backend| (backend, config.lead_model.clone())),
    };

    // `store` is built here, as an `Arc`, so it can be shared between the
    // permission guards (which persist `PermissionRequested`/
    // `PermissionDecided` events via `TerminalConfirmationPrompt`) and the
    // `Engine` itself, without constructing two independent
    // `FileSessionStore` instances pointed at the same directory.
    let store: std::sync::Arc<dyn braze_session::SessionStore> = std::sync::Arc::new(
        braze_session::FileSessionStore::new(config.session_dir.clone()),
    );

    // Bajo (docs/AUDITORIA-2026-07-v2.md, "--resume <uuid-inexistente>
    // arranca sesión vacía en silencio"): a brand-new session (no
    // `--resume` at all) hitting `NotFound` is completely expected and
    // must stay silent — only an *explicit* `--resume <id>` naming a
    // session this store has never heard of is worth a warning, since
    // that's almost certainly a typo the user would want to know about
    // instead of silently starting a fresh, empty session. Checked here
    // (where the CLI args live), not in `build_engine`, which treats
    // `NotFound` as an ordinary empty session.
    if matches!(
        cli.command,
        Command::Chat {
            resume: Some(_),
            ..
        }
    ) && matches!(
        store.load(&session).await,
        Err(braze_session::SessionError::NotFound(_))
    ) {
        eprintln!(
            "braze: aviso: no se encontró la sesión {session} — iniciando una nueva sesión vacía"
        );
    }

    // Only `chat --tui` drives the terminal in raw mode — see
    // `build_permission_guard`'s doc comment for why that changes which
    // `ConfirmationPrompt` gets built.
    let tui_mode = matches!(cli.command, Command::Chat { tui: true, .. });
    let supervised = cli.command.supervised();

    // Resolved eagerly (fails fast on an unrecognized name, before any
    // engine/session work) even though it's only ever consumed by the
    // `tui: true` arm below — mirrors how `default_backend`/model
    // resolution already fails at startup rather than partway through.
    let tui_theme = if tui_mode {
        braze_tui::Theme::from_name(&config.tui_theme).ok_or_else(|| {
            CliError::Startup(format!(
                "tema de TUI desconocido: '{}' (esperado 'dark', 'light', o 'high-contrast')",
                config.tui_theme
            ))
        })?
    } else {
        braze_tui::Theme::default()
    };

    // Constructed unconditionally (cheap) even for the plain path, where
    // it's simply never sent into a `ChannelConfirmationPrompt` and
    // `approval_rx` is never read — only `--tui` wires it up for real.
    let (approval_tx, approval_rx) =
        tokio::sync::mpsc::unbounded_channel::<braze_tui::ApprovalRequest>();

    let cwd = std::env::current_dir()?;
    // N-12 (docs/AUDITORIA-2026-07-v2.md): shared with every
    // `PermissionGuard` `build_engine` creates *and* with
    // `braze_tui::run` — a backtrack in the TUI writes the new session id
    // here, so a permission decision made afterward persists against it
    // instead of the session this process started with.
    let live_session = std::sync::Arc::new(std::sync::Mutex::new(session));

    let (engine, status_line) = build_engine(
        &config,
        planner_spec.clone(),
        lead_spec.clone(),
        std::sync::Arc::clone(&live_session),
        std::sync::Arc::clone(&store),
        approval_tx.clone(),
        tui_mode,
        supervised,
        &cwd,
    )
    .await?;

    match cli.command {
        Command::Run { prompt, .. } => {
            println!("session: {session}");

            let mut stdout = std::io::stdout();
            engine
                .run_turn(
                    &session,
                    &prompt,
                    &mut TextDeltaObserver(|text: &str| {
                        use std::io::Write;
                        print!("{text}");
                        let _ = stdout.flush();
                    }),
                )
                .await?;
            println!();
        }
        Command::Chat { tui: true, .. } => {
            // Candidates for the `/model` picker: every backend the
            // current config could actually build (credentials + model
            // name present), with the Ollama entry expanded to the
            // server's installed models — best-effort: a down Ollama
            // degrades to the single configured name (the server may
            // come back up mid-session), never blocks startup.
            let mut model_candidates: Vec<String> = Vec::new();
            if config.anthropic_api_key.is_some()
                && let Some(model) = config.anthropic_model.as_deref()
            {
                model_candidates.push(format!("anthropic:{model}"));
            }
            if config.openrouter_api_key.is_some()
                && let Some(model) = config.openrouter_model.as_deref()
            {
                model_candidates.push(format!("openrouter:{model}"));
            }
            match braze_model::list_ollama_models(&config.ollama_base_url).await {
                Ok(models) => model_candidates
                    .extend(models.into_iter().map(|model| format!("ollama:{model}"))),
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "no se pudo listar los modelos de Ollama para el picker /model"
                    );
                    model_candidates.push(format!("ollama:{}", config.ollama_model));
                }
            }

            // The `/model` factory: same `build_engine` composition as
            // startup, over a per-call clone of the resolved config with
            // the requested `backend[:modelo]` layered on top — split on
            // the *first* `:` only, same convention as `--planner`
            // (Ollama tags and OpenRouter ids carry their own `:`/`/`).
            let factory_config = config.clone();
            let factory_planner_spec = planner_spec.clone();
            let factory_lead_spec = lead_spec.clone();
            let factory_live_session = std::sync::Arc::clone(&live_session);
            let factory_store = std::sync::Arc::clone(&store);
            let factory_approval_tx = approval_tx.clone();
            let factory_cwd = cwd.clone();
            let engine_factory: braze_tui::EngineFactory = Box::new(move |spec: String| {
                let mut config = factory_config.clone();
                let planner_spec = factory_planner_spec.clone();
                let lead_spec = factory_lead_spec.clone();
                let live_session = std::sync::Arc::clone(&factory_live_session);
                let store = std::sync::Arc::clone(&factory_store);
                let approval_tx = factory_approval_tx.clone();
                let cwd = factory_cwd.clone();
                Box::pin(async move {
                    let (backend, model) = match spec.split_once(':') {
                        Some((backend, model)) => (backend, Some(model)),
                        None => (spec.as_str(), None),
                    };
                    config.apply_overrides(braze_config::ConfigOverrides {
                        default_backend: Some(backend.to_string()),
                        ..Default::default()
                    });
                    if let Some(model) = model {
                        let overrides =
                            model_override_for(backend, model).map_err(|err| err.to_string())?;
                        config.apply_overrides(overrides);
                    }
                    build_engine(
                        &config,
                        planner_spec,
                        lead_spec,
                        live_session,
                        store,
                        approval_tx,
                        true,
                        supervised,
                        &cwd,
                    )
                    .await
                    .map_err(|err| err.to_string())
                })
            });

            braze_tui::run(
                engine,
                std::sync::Arc::clone(&live_session),
                std::sync::Arc::clone(&store),
                approval_rx,
                status_line,
                tui_theme,
                engine_factory,
                model_candidates,
            )
            .await?;
        }
        Command::Chat { .. } => {
            println!("session: {session} (usa --resume {session} para continuarla luego)");

            let stdin = tokio::io::stdin();
            let mut lines = tokio::io::BufReader::new(stdin).lines();

            loop {
                print!("> ");
                std::io::Write::flush(&mut std::io::stdout()).ok();

                let Some(line) = lines.next_line().await? else {
                    break; // EOF (Ctrl-D)
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "exit" || trimmed == "quit" {
                    break;
                }

                let mut stdout = std::io::stdout();
                let result = engine
                    .run_turn(
                        &session,
                        trimmed,
                        &mut TextDeltaObserver(|text: &str| {
                            use std::io::Write;
                            print!("{text}");
                            let _ = stdout.flush();
                        }),
                    )
                    .await;
                // A single failed turn (a transient backend error, the
                // model exhausting its iteration cap, ...) must not kill
                // the whole interactive session — print the error and let
                // the user try again, instead of propagating with `?` and
                // ending the process.
                if let Err(err) = result {
                    eprintln!("braze: turn failed: {err}");
                }
                println!();
            }
        }
    }

    Ok(())
}
