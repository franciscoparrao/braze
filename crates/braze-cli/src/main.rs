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

use braze_events::ChannelTaskNotifier;
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
/// since it isn't `Clone`) plus a `TerminalConfirmationPrompt`. Every
/// `ToolProvider` this binary constructs (the local tools provider, and one
/// per connected MCP server) gets its own guard from this same helper, each
/// with an independent in-memory "remembered" session cache.
fn build_permission_guard(cwd: &std::path::Path) -> braze_permissions::PermissionGuard {
    let allowlist_for_classifier = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let allowlist_for_guard = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let classifier = braze_permissions::DefaultClassifier::new(allowlist_for_classifier);
    braze_permissions::PermissionGuard::new(
        allowlist_for_guard,
        Box::new(classifier),
        Box::new(TerminalConfirmationPrompt),
    )
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let mut config = braze_config::Config::load()?;

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
            other => {
                return Err(CliError::Startup(format!(
                    "unknown backend '{other}' (expected 'anthropic' or 'ollama')"
                )));
            }
        };
        config.apply_overrides(overrides);
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
            Box::new(braze_model::AnthropicBackend::new(api_key, model_name))
        }
        "ollama" => Box::new(braze_model::OllamaBackend::with_base_url(
            config.ollama_model.clone(),
            config.ollama_base_url.clone(),
        )),
        other => {
            return Err(CliError::Startup(format!(
                "unknown backend '{other}' (expected 'anthropic' or 'ollama')"
            )));
        }
    };

    // Two-layer permission setup: a soft working-dir allowlist plus a
    // default-deny shell classifier + terminal y/n confirmation for
    // irreversible actions. Every `ToolProvider` (local tools, and now each
    // MCP server) gets its own freshly built `PermissionGuard` — see
    // `build_permission_guard` below — since `PermissionGuard` isn't
    // shared/`Clone` across providers.
    let cwd = std::env::current_dir()?;
    let local_guard = build_permission_guard(&cwd);

    let local_provider = braze_tools_local::LocalToolsProvider::new(local_guard);
    let mut providers: Vec<Box<dyn braze_tools_core::ToolProvider>> =
        vec![Box::new(local_provider)];

    // Best-effort: a dead/misconfigured MCP server never aborts startup,
    // it's just unavailable for this run (logged as a warning).
    for server in &config.mcp_servers {
        let mcp_guard = build_permission_guard(&cwd);
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
    let store = braze_session::FileSessionStore::new(config.session_dir.clone());
    let compactor = braze_session::SimpleContextCompactor::default();

    let system_prompt = "You are braze, an experimental agentic CLI assistant.".to_string();

    let engine = braze_engine::Engine::new(
        model,
        tools,
        Box::new(store),
        Box::new(compactor),
        Box::new(notifier),
        system_prompt,
        config.max_tokens,
    );

    match cli.command {
        Command::Run { prompt, .. } => {
            let session = SessionId::new();
            println!("session: {session}");

            let mut stdout = std::io::stdout();
            engine
                .run_turn(&session, &prompt, &mut |text| {
                    use std::io::Write;
                    print!("{text}");
                    let _ = stdout.flush();
                })
                .await?;
            println!();
        }
        Command::Chat { resume, .. } => {
            let session = match resume {
                Some(id_str) => id_str.parse::<SessionId>().map_err(|err| {
                    CliError::Startup(format!("invalid session id '{id_str}': {err}"))
                })?,
                None => SessionId::new(),
            };
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
                engine
                    .run_turn(&session, trimmed, &mut |text| {
                        use std::io::Write;
                        print!("{text}");
                        let _ = stdout.flush();
                    })
                    .await?;
                println!();
            }
        }
    }

    Ok(())
}
