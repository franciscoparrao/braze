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

/// Headroom reserved out of `ollama_num_ctx` for the system prompt + tool
/// schemas when computing `Engine::with_context_budget` — neither is part
/// of what `Engine::load_messages`'s token estimate measures (durable
/// summary + durable/tactical events only).
const CONTEXT_BUDGET_MARGIN_TOKENS: u32 = 1024;

/// Default system prompt, used unless `config.system_prompt` overrides it.
///
/// Earlier versions of this shipped a single generic sentence with no
/// tool-use guidance, no anti-loop rules, and no working directory — the
/// cheapest lever for small/local models left completely unused (see
/// docs/AUDITORIA-2026-07.md, hallazgo A10). The rules below target the
/// two dominant small-model failure modes this project has observed
/// empirically: repeating an identical tool call instead of using its
/// result, and over-elaborating instead of answering once enough
/// information has been gathered (arXiv 2604.02155's finding that longer
/// reasoning *degrades* tool-calling accuracy in small models).
fn default_system_prompt(cwd: &std::path::Path) -> String {
    format!(
        "You are braze, an agentic CLI assistant. Working directory: {}.\n\
         \n\
         Rules:\n\
         - Never call the same tool with the same arguments twice in one turn — \
         if you already have that result, use it instead of calling again.\n\
         - Once you have enough information to answer, stop calling tools and \
         answer in plain text. Do not keep exploring after you already have \
         what was asked.\n\
         - Keep reasoning brief before acting — a sentence or two, not an \
         extended chain of thought.\n\
         - Relative paths are resolved against the working directory above.",
        cwd.display()
    )
}

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
/// since it isn't `Clone`) plus a `TerminalConfirmationPrompt` — the latter
/// now carrying `session`/`store` so it can persist
/// `PermissionRequested`/`PermissionDecided` events for later `--resume`
/// replay. Every `ToolProvider` this binary constructs (the local tools
/// provider, and one per connected MCP server) gets its own guard from this
/// same helper, each with an independent in-memory "remembered" session
/// cache — seeded from `replayed_keys` so approvals confirmed earlier in
/// this same session (before a restart) aren't re-asked.
fn build_permission_guard(
    cwd: &std::path::Path,
    session: braze_types::SessionId,
    store: std::sync::Arc<dyn braze_session::SessionStore>,
    replayed_keys: &[braze_types::PermissionKey],
) -> braze_permissions::PermissionGuard {
    let allowlist_for_classifier = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let allowlist_for_guard = braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf());
    let classifier = braze_permissions::DefaultClassifier::new(allowlist_for_classifier);
    let guard = braze_permissions::PermissionGuard::new(
        allowlist_for_guard,
        Box::new(classifier),
        Box::new(TerminalConfirmationPrompt::new(session, store)),
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
        "ollama" => Box::new(
            braze_model::OllamaBackend::with_base_url(
                config.ollama_model.clone(),
                config.ollama_base_url.clone(),
            )
            .with_num_ctx(config.ollama_num_ctx),
        ),
        other => {
            return Err(CliError::Startup(format!(
                "unknown backend '{other}' (expected 'anthropic' or 'ollama')"
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
        Err(braze_session::SessionError::NotFound(_)) => Vec::new(),
        Err(err) => return Err(err.into()),
    };

    // Two-layer permission setup: a soft working-dir allowlist plus a
    // default-deny shell classifier + terminal y/n confirmation for
    // irreversible actions. Every `ToolProvider` (local tools, and now each
    // MCP server) gets its own freshly built `PermissionGuard` — see
    // `build_permission_guard` below — since `PermissionGuard` isn't
    // shared/`Clone` across providers. All of them share the same
    // `replayed_keys`, seeded from the same session's own prior decisions.
    let cwd = std::env::current_dir()?;
    let local_guard =
        build_permission_guard(&cwd, session, std::sync::Arc::clone(&store), &replayed_keys);

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
        let mcp_guard =
            build_permission_guard(&cwd, session, std::sync::Arc::clone(&store), &replayed_keys);
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
    let compactor = braze_session::SimpleContextCompactor::default();

    let system_prompt = config
        .system_prompt
        .clone()
        .unwrap_or_else(|| default_system_prompt(&cwd));

    let mut engine = braze_engine::Engine::new(
        model,
        tools,
        std::sync::Arc::clone(&store),
        Box::new(compactor),
        Box::new(notifier),
        system_prompt,
        config.max_tokens,
    );

    // Only Ollama has a small, fixed context window worth budgeting for
    // (Anthropic's is large enough that raw event count remains a fine
    // proxy) — reserve `CONTEXT_BUDGET_MARGIN_TOKENS` out of `num_ctx` for
    // the system prompt + tool schemas, which aren't part of what
    // `Engine::load_messages` measures (see `estimate_prompt_tokens`).
    if config.default_backend == "ollama" {
        let budget = config
            .ollama_num_ctx
            .saturating_sub(config.max_tokens)
            .saturating_sub(CONTEXT_BUDGET_MARGIN_TOKENS);
        engine = engine.with_context_budget(budget);
    }

    match cli.command {
        Command::Run { prompt, .. } => {
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
                    .run_turn(&session, trimmed, &mut |text| {
                        use std::io::Write;
                        print!("{text}");
                        let _ = stdout.flush();
                    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for A10: the default prompt must actually carry
    /// the working directory and the anti-loop/stop-condition guidance —
    /// the whole point of moving off the old one-sentence default.
    #[test]
    fn default_system_prompt_includes_cwd_and_anti_loop_guidance() {
        let prompt = default_system_prompt(std::path::Path::new("/home/user/project"));
        assert!(prompt.contains("/home/user/project"));
        assert!(prompt.contains("Never call the same tool"));
        assert!(prompt.contains("stop calling tools"));
    }
}
