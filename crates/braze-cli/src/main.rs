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
    approval_tx: tokio::sync::mpsc::UnboundedSender<braze_tui::ApprovalRequest>,
) -> braze_permissions::PermissionGuard {
    let allowlist_for_classifier = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let allowlist_for_guard = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let classifier = braze_permissions::DefaultClassifier::new(allowlist_for_classifier);
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
    let guard = braze_permissions::PermissionGuard::new(
        allowlist_for_guard,
        Box::new(classifier),
        confirmation,
    );
    guard.seed_remembered(replayed_keys.iter().cloned());
    guard
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
        let overrides = match config.default_backend.as_str() {
            "anthropic" => braze_config::ConfigOverrides {
                anthropic_model: Some(model.to_string()),
                ..Default::default()
            },
            "ollama" => braze_config::ConfigOverrides {
                ollama_model: Some(model.to_string()),
                ..Default::default()
            },
            "openrouter" => braze_config::ConfigOverrides {
                openrouter_model: Some(model.to_string()),
                ..Default::default()
            },
            other => {
                return Err(CliError::Startup(format!(
                    "unknown backend '{other}' (expected 'anthropic', 'ollama', or 'openrouter')"
                )));
            }
        };
        config.apply_overrides(overrides);
    }
    if let Some(theme) = cli.command.theme_override() {
        config.apply_overrides(braze_config::ConfigOverrides {
            tui_theme: Some(theme.to_string()),
            ..Default::default()
        });
    }

    let model: Box<dyn braze_model::ModelBackend> = match config.default_backend.as_str() {
        "anthropic" => {
            let api_key = config.anthropic_api_key.clone().ok_or_else(|| {
                CliError::Startup(
                    "falta ANTHROPIC_API_KEY (config file, BRAZE_ANTHROPIC_API_KEY, o --backend anthropic sin key configurada)"
                        .to_string(),
                )
            })?;
            let model_name = config.anthropic_model.clone().ok_or_else(|| {
                CliError::Startup(
                    "falta --model o BRAZE_ANTHROPIC_MODEL para el backend anthropic".to_string(),
                )
            })?;
            Box::new(braze_model::AnthropicBackend::new(
                api_key.expose_secret().to_string(),
                model_name,
            ))
        }
        "ollama" => Box::new(
            braze_model::OllamaBackend::with_base_url(
                config.ollama_model.clone(),
                config.ollama_base_url.clone(),
            )
            .with_num_ctx(config.ollama_num_ctx),
        ),
        "openrouter" => {
            let api_key = config.openrouter_api_key.clone().ok_or_else(|| {
                CliError::Startup(
                    "falta OPENROUTER_API_KEY (config file, BRAZE_OPENROUTER_API_KEY, o --backend openrouter sin key configurada)"
                        .to_string(),
                )
            })?;
            let model_name = config.openrouter_model.clone().ok_or_else(|| {
                CliError::Startup(
                    "falta --model o BRAZE_OPENROUTER_MODEL para el backend openrouter".to_string(),
                )
            })?;
            Box::new(braze_model::OpenRouterBackend::with_base_url(
                api_key.expose_secret().to_string(),
                model_name,
                config.openrouter_base_url.clone(),
            ))
        }
        other => {
            return Err(CliError::Startup(format!(
                "unknown backend '{other}' (expected 'anthropic', 'ollama', or 'openrouter')"
            )));
        }
    };

    // `store` is built here, as an `Arc`, so it can be shared between the
    // permission guards (which persist `PermissionRequested`/
    // `PermissionDecided` events via `TerminalConfirmationPrompt`) and the
    // `Engine` itself, without constructing two independent
    // `FileSessionStore` instances pointed at the same directory.
    let store: std::sync::Arc<dyn braze_session::SessionStore> = std::sync::Arc::new(
        braze_session::FileSessionStore::new(config.session_dir.clone()),
    );

    // Replays previously-*approved* permission decisions for this exact
    // session (a no-op for a brand-new session, since `load` finds
    // nothing) so `braze chat --resume` doesn't re-ask for actions already
    // confirmed earlier in the same conversation. See PLAN.md's "Grupo 2
    // del roadmap SOTA" note on why this was deferred until `Engine`'s
    // `SessionStore` could be shared (`Arc`) with startup code that also
    // needs to read it.
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
        Err(braze_session::SessionError::NotFound(_)) => {
            // Bajo (docs/AUDITORIA-2026-07-v2.md, "--resume
            // <uuid-inexistente> arranca sesión vacía en silencio"): a
            // brand-new session (no `--resume` at all) hitting
            // `NotFound` is completely expected and must stay silent —
            // only an *explicit* `--resume <id>` naming a session this
            // store has never heard of is worth a warning, since that's
            // almost certainly a typo the user would want to know about
            // instead of silently starting a fresh, empty session.
            if matches!(
                cli.command,
                Command::Chat {
                    resume: Some(_),
                    ..
                }
            ) {
                eprintln!(
                    "braze: aviso: no se encontró la sesión {session} — iniciando una nueva sesión vacía"
                );
            }
            Vec::new()
        }
        Err(err) => return Err(err.into()),
    };

    // Only `chat --tui` drives the terminal in raw mode — see
    // `build_permission_guard`'s doc comment for why that changes which
    // `ConfirmationPrompt` gets built.
    let tui_mode = matches!(cli.command, Command::Chat { tui: true, .. });

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

    // Two-layer permission setup: a soft working-dir allowlist plus a
    // default-deny shell classifier + terminal y/n confirmation for
    // irreversible actions. Every `ToolProvider` (local tools, and now each
    // MCP server) gets its own freshly built `PermissionGuard` — see
    // `build_permission_guard` below — since `PermissionGuard` isn't
    // shared/`Clone` across providers. All of them share the same
    // `replayed_keys`, seeded from the same session's own prior decisions.
    let cwd = std::env::current_dir()?;
    // N-12 (docs/AUDITORIA-2026-07-v2.md): shared with every
    // `PermissionGuard` built below *and* with `braze_tui::run` — a
    // backtrack in the TUI writes the new session id here, so a
    // permission decision made afterward persists against it instead of
    // the session this process started with.
    let live_session = std::sync::Arc::new(std::sync::Mutex::new(session));
    let local_guard = build_permission_guard(
        &cwd,
        std::sync::Arc::clone(&live_session),
        std::sync::Arc::clone(&store),
        &replayed_keys,
        tui_mode,
        approval_tx.clone(),
    );

    let local_provider = braze_tools_local::LocalToolsProvider::new(local_guard);
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
            &cwd,
            std::sync::Arc::clone(&live_session),
            std::sync::Arc::clone(&store),
            &replayed_keys,
            tui_mode,
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

    let system_prompt = config
        .system_prompt
        .clone()
        .unwrap_or_else(|| braze_config::default_system_prompt(&cwd));

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
    .with_textual_rescue_enabled(!config.disable_textual_tool_call_rescue);

    // Only Ollama has a small, fixed context window worth budgeting for
    // (Anthropic's is large enough that raw event count remains a fine
    // proxy) — reserve `CONTEXT_BUDGET_MARGIN_TOKENS` out of `num_ctx` for
    // the system prompt + tool schemas, which aren't part of what
    // `Engine::load_messages` measures (see `estimate_prompt_tokens`).
    if config.default_backend == "ollama" {
        let budget =
            braze_config::ollama_context_budget_tokens(config.ollama_num_ctx, config.max_tokens);
        engine = engine.with_context_budget(budget);
    }

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
            braze_tui::run(
                engine,
                std::sync::Arc::clone(&live_session),
                std::sync::Arc::clone(&store),
                approval_rx,
                status_line,
                tui_theme,
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
