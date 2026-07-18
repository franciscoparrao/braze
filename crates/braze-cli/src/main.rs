//! braze terminal binary: `braze chat` (interactive) and `braze run <prompt>`
//! (one-shot).
//!
//! This is the only place in the workspace that installs the `tracing`
//! subscriber (per PLAN.md: libraries emit traces, only the binary decides
//! how they're rendered) and the only place that composes every crate in
//! the workspace into a running [`braze_engine::Engine`].

mod cli_args;
mod error;
mod permissions_report;
mod terminal_prompt;
mod terminal_question;

use std::process::ExitCode;

use clap::Parser;

use braze_events::{AgentEvent, ChannelTaskNotifier, TextDeltaObserver, TurnObserver};
use braze_types::SessionId;

/// `braze run --output-format json`'s observer: accumulates exactly the
/// text content `TextDeltaObserver` would have streamed to stdout (same
/// deltas, same order), plus every `Usage` event's tokens/stop_reason,
/// instead of printing anything until the turn finishes — a CI/scripting
/// caller gets one parseable object instead of a raw stream mixed with a
/// human-readable `session: <id>` line.
#[derive(Default)]
struct JsonSummaryObserver {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    rounds: u32,
    stop_reason: Option<String>,
}

impl TurnObserver for JsonSummaryObserver {
    fn on_text_delta(&mut self, delta: &str) {
        self.text.push_str(delta);
    }

    fn on_event(&mut self, event: &AgentEvent) {
        if let AgentEvent::Usage {
            input_tokens,
            output_tokens,
            stop_reason,
            ..
        } = event
        {
            self.input_tokens += u64::from(*input_tokens);
            self.output_tokens += u64::from(*output_tokens);
            self.rounds += 1;
            // Last-reported wins, same convention `braze-bench` uses when
            // summarizing a multi-round turn: the final round's stop
            // reason is the one that describes how the turn actually
            // ended, not how an earlier round happened to end.
            if stop_reason.is_some() {
                self.stop_reason = stop_reason.clone();
            }
        }
    }
}
use cli_args::{Cli, Command, PermissionsAction};
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
#[allow(clippy::too_many_arguments)] // composition-root helper: one param per collaborator
fn build_permission_guard(
    cwd: &std::path::Path,
    references: &[braze_config::ReferenceConfig],
    live_session: std::sync::Arc<std::sync::Mutex<braze_types::SessionId>>,
    store: std::sync::Arc<dyn braze_session::SessionStore>,
    replayed_keys: &[braze_types::PermissionKey],
    tui_mode: bool,
    supervised: bool,
    approval_tx: tokio::sync::mpsc::UnboundedSender<braze_tui::ApprovalRequest>,
) -> braze_permissions::PermissionGuard {
    // opencode-10 (docs/opencode-a-braze.md § 10): every configured
    // reference directory is an extra allowlist root — OpenCode's
    // implicit `external_directory: "allow"` — so reading the docs the
    // system prompt just advertised doesn't cost a confirmation each.
    let with_references = |mut allowlist: braze_permissions::WorkdirAllowlist| {
        for reference in references {
            allowlist = allowlist.with_extra_root(reference.path.clone());
        }
        allowlist
    };
    let allowlist_for_classifier =
        with_references(braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf()));
    let allowlist_for_guard =
        with_references(braze_permissions::WorkdirAllowlist::new(cwd.to_path_buf()));
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

/// E′ I.6: snapshot recortado del entorno para el system prompt —
/// fecha, OS, y (si `cwd` es un repo git) branch + `git status --short`
/// capado a 10 líneas. Todo best-effort: un `git` ausente o un
/// directorio sin repo simplemente omite esas líneas, nunca falla el
/// arranque. El cap existe porque el contexto es presupuesto: un
/// worktree con 300 archivos sucios no debe comerse el `num_ctx`.
fn build_environment_snapshot(cwd: &std::path::Path) -> String {
    const MAX_STATUS_LINES: usize = 10;

    let mut lines = Vec::new();
    lines.push(format!(
        "- date: {}",
        chrono_free_date_string()
    ));
    lines.push(format!("- os: {}", std::env::consts::OS));

    let git = |args: &[&str]| -> Option<String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim_end().to_string())
    };

    if let Some(branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        lines.push(format!("- git branch: {branch}"));
        if let Some(status) = git(&["status", "--short"]) {
            if status.is_empty() {
                lines.push("- git status: clean".to_string());
            } else {
                let total = status.lines().count();
                let shown: Vec<&str> = status.lines().take(MAX_STATUS_LINES).collect();
                let mut rendered = format!("- git status ({total} changed):");
                for line in shown {
                    rendered.push_str(&format!("\n  {line}"));
                }
                if total > MAX_STATUS_LINES {
                    rendered.push_str(&format!(
                        "\n  ... and {} more",
                        total - MAX_STATUS_LINES
                    ));
                }
                lines.push(rendered);
            }
        }
    }

    lines.join("\n")
}

/// Local date without a chrono dependency: `date +%F` via the shell is
/// overkill and non-portable; `SystemTime` gives an epoch — days since
/// epoch to a civil date is a small pure computation (Howard Hinnant's
/// algorithm), enough for a "what day is it" grounding line.
fn chrono_free_date_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
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
            Ok(Box::new(
                braze_model::AnthropicBackend::new(
                    api_key.expose_secret().to_string(),
                    model_name,
                )
                // v8 § 5: mismo knob que el brazo OpenRouter de abajo —
                // el caching directo de Anthropic existe desde hoy.
                .with_prompt_caching_enabled(config.enable_prompt_caching),
            ))
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
    // E′ I.5: when `Some`, the `ask_user` tool is exposed (interactive
    // plain chat only) — `run`/the bench pass `None` (no human to ask).
    ask_user_prompt: Option<std::sync::Arc<dyn braze_permissions::QuestionPrompt>>,
) -> Result<
    (
        braze_engine::Engine,
        String,
        // v8 K-8: el handle del ProjectMemoryHook vuelve al caller para
        // poder `flush()` los saves en background antes de que el
        // proceso salga (`braze run` one-shot). `None` cuando
        // `enable_project_memory` está off.
        Option<std::sync::Arc<braze_engine::ProjectMemoryHook>>,
    ),
    CliError,
> {
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
        &config.references,
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

    // E′ I.5: the `ask_user` tool is its own provider, added only when an
    // interactive prompt was supplied — so it never shows up in the tool
    // inventory of `run` or the bench, where there's no one to answer.
    let ask_user_enabled = ask_user_prompt.is_some();
    if let Some(prompt) = ask_user_prompt {
        providers.push(Box::new(braze_tools_local::AskUserProvider::new(prompt)));
    }

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
            &config.references,
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

    // D1 + I-4 (docs/AUDITORIA-2026-07-v6.md): the family hint is
    // name-based, not backend-based — the GLM template leak (U-15/U-16)
    // was observed via OpenRouter, so gating the hint on `backend ==
    // "ollama"` withheld it exactly where it was needed. Every backend
    // passes its model name; `ModelFamily::from_model_name` yields no
    // hint for unrecognized names (Anthropic models included).
    let model_hint_for_prompt = match config.default_backend.as_str() {
        "ollama" => Some(config.ollama_model.clone()),
        "openrouter" => config.openrouter_model.clone(),
        "anthropic" => config.anthropic_model.clone(),
        _ => None,
    };
    // E′ I.6 (docs/harness-engineering-hooks-skills-2026-07-10.md): el
    // snapshot lo arma este composition root — la lib de config solo
    // formatea. Se computa una vez al construir el Engine (no por
    // turno): un snapshot al inicio de sesión, igual que el harness que
    // inspiró el diseño.
    let environment_snapshot = if config.environment_block {
        Some(build_environment_snapshot(cwd))
    } else {
        None
    };
    // docs/project-memory-design.md: construido ANTES del system prompt
    // (necesita el resumen ya renderizado) pero registrado como hook
    // DESPUÉS de construir el Engine, más abajo — mismo orden que
    // `environment_snapshot`. `ProjectMemoryHook::new` carga lo que ya
    // exista en `.braze/memory.json` (best-effort: un archivo roto no
    // bloquea el arranque de la sesión, ver su doc comment).
    let project_memory_hook = if config.enable_project_memory {
        let project_root = braze_memory::resolve_project_root(cwd);
        let memory_path = braze_memory::default_memory_path(&project_root);
        let store: std::sync::Arc<dyn braze_memory::ProjectMemoryStore> =
            std::sync::Arc::new(braze_memory::FileProjectMemoryStore::new(memory_path));
        let project_key = project_root.display().to_string();
        Some(std::sync::Arc::new(
            braze_engine::ProjectMemoryHook::new(store, project_key).await,
        ))
    } else {
        None
    };
    let project_memory_snapshot: Option<String> = project_memory_hook.as_ref().and_then(|hook| {
        braze_memory::render_project_memory_section(
            &hook.snapshot(),
            braze_memory::DEFAULT_PROJECT_MEMORY_BUDGET_TOKENS,
        )
    });
    let system_prompt = config.system_prompt.clone().unwrap_or_else(|| {
        braze_config::default_system_prompt(
            cwd,
            model_hint_for_prompt.as_deref(),
            &config.references,
            environment_snapshot.as_deref(),
            project_memory_snapshot.as_deref(),
        )
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
    // C′.1: providers con más stubs que este umbral quedan detrás del
    // meta-tool search_tools (el caso objetivo: gateways MCP grandes).
    .with_tool_search_threshold(config.tool_search_threshold)
    // C′.2: task list tipada — off salvo config explícita.
    .with_task_list_enabled(config.enable_task_list)
    // v4 P0.2: circuit breaker por tokens acumulados por turno — None
    // (default) lo deshabilita.
    .with_max_turn_total_tokens(config.max_turn_total_tokens);
    // J-13 (docs/AUDITORIA-2026-07-v7.md): ask_user espera a un HUMANO —
    // dispatch inline, exento del timeout de 120s de los background
    // tools (bajo ese reloj, una respuesta lenta se cancelaba y la línea
    // que el humano tecleaba después la consumía el chat loop como un
    // prompt nuevo).
    if ask_user_enabled {
        engine = engine.with_untimed_tool(braze_tools_local::ASK_USER_TOOL);
    }
    // D′: skills locales — discovery al arranque, solo si config lista
    // paths (allowlist vacía = apagado; el bench nunca los pasa).
    if !config.skills.paths.is_empty() {
        let registry =
            std::sync::Arc::new(braze_skills::SkillRegistry::discover(&config.skills.paths));
        if !registry.is_empty() {
            engine = engine.with_skills(
                registry,
                config.skills.max_body_tokens,
                config.skills.max_loaded_per_turn,
            );
        }
    }
    // docs/project-memory-design.md: registrado como hook audit-only
    // (Paquete B′) — observa `AgentEvent`s y persiste a
    // `.braze/memory.json` sin influir el turno. `None` si
    // `config.enable_project_memory` es `false` (el caso común).
    if let Some(hook) = &project_memory_hook {
        engine = engine.with_hook(hook.clone());
    }

    if let Some(budget) = ollama_budget {
        engine = engine.with_context_budget(budget);
    }

    if let Some(planner) = planner {
        engine = engine.with_planner(planner);
    }

    Ok((engine, status_line, project_memory_hook))
}

/// E′ I.8: `braze permissions suggest` — reads every session log under
/// `config.session_dir`, aggregates the permission decisions, and prints
/// the ranking. Read-only; no engine, no model.
async fn run_permissions(
    action: &PermissionsAction,
    config: &braze_config::Config,
) -> Result<(), CliError> {
    use braze_session::SessionStore;

    let PermissionsAction::Suggest(args) = action;

    let store = braze_session::FileSessionStore::new(config.session_dir.clone());
    let session_ids = store
        .list_sessions()
        .await
        .map_err(|err| CliError::Startup(format!("no se pudieron listar las sesiones: {err}")))?;

    let mut sessions = Vec::with_capacity(session_ids.len());
    for id in &session_ids {
        // A single unreadable/corrupt session log shouldn't sink the whole
        // report — skip it with a warning, same posture as the rest of the
        // binary's best-effort diagnostics.
        match store.load(id).await {
            Ok(events) => sessions.push(events),
            Err(err) => {
                tracing::warn!(session = %id, error = %err, "skipping unreadable session log")
            }
        }
    }

    let stats = permissions_report::aggregate(&sessions);
    print!(
        "{}",
        permissions_report::render_report(&stats, args.top, args.min_count)
    );
    println!(
        "\n({} sesiones leídas de {})",
        sessions.len(),
        config.session_dir.display()
    );
    Ok(())
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let mut config = braze_config::Config::load()?;

    // E′ I.8: `permissions suggest` needs neither a model nor an engine —
    // it only reads the on-disk session logs. Dispatched here, before any
    // backend/guard/tool construction, and returns.
    if let Command::Permissions { action } = &cli.command {
        return run_permissions(action, &config).await;
    }

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
        // `Permissions` is dispatched and returned above, before this.
        Command::Permissions { .. } => unreachable!("handled by run_permissions"),
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

    // E′ I.5: `ask_user` is exposed only in the interactive PLAIN chat
    // loop — not in `run` (one-shot, no human at the keyboard), and not
    // in `--tui` yet (v1: the overlay wiring is deferred; the trait lives
    // in braze-permissions so the TUI can add a channel impl later). The
    // prompt shares the chat loop's single stdin reader (see
    // `terminal_question`) so a piped answer isn't swallowed by the
    // loop's read-ahead buffer.
    let plain_chat = matches!(cli.command, Command::Chat { .. }) && !tui_mode;
    let stdin_lines = terminal_question::shared_stdin();
    let ask_user_prompt: Option<std::sync::Arc<dyn braze_permissions::QuestionPrompt>> =
        if plain_chat {
            Some(std::sync::Arc::new(
                terminal_question::TerminalQuestionPrompt::new(std::sync::Arc::clone(&stdin_lines)),
            ))
        } else {
            None
        };

    let (engine, status_line, project_memory_hook) = build_engine(
        &config,
        planner_spec.clone(),
        lead_spec.clone(),
        std::sync::Arc::clone(&live_session),
        std::sync::Arc::clone(&store),
        approval_tx.clone(),
        tui_mode,
        supervised,
        &cwd,
        ask_user_prompt,
    )
    .await?;

    match cli.command {
        Command::Run {
            prompt,
            output_format,
            ..
        } => match output_format {
            cli_args::OutputFormat::Plain => {
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
            cli_args::OutputFormat::Json => {
                let mut summary = JsonSummaryObserver::default();
                engine.run_turn(&session, &prompt, &mut summary).await?;
                println!(
                    "{}",
                    serde_json::json!({
                        "text": summary.text,
                        "session_id": session.to_string(),
                        "input_tokens": summary.input_tokens,
                        "output_tokens": summary.output_tokens,
                        "rounds": summary.rounds,
                        "stop_reason": summary.stop_reason,
                    })
                );
            }
        },
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
                        // TUI: `ask_user` deferred to a channel impl (v1).
                        None,
                    )
                    .await
                    // El engine reconstruido registra su propio hook de
                    // memoria; soltar este handle es seguro mid-sesión —
                    // la task de saves drena sola mientras el runtime
                    // viva (el flush de salida cubre solo el hook
                    // original, suficiente: ambos escriben serializados
                    // al mismo store).
                    .map(|(engine, status_line, _memory_hook)| (engine, status_line))
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

            // The SAME reader `TerminalQuestionPrompt` reads through (E′
            // I.5) — one buffer over stdin, so an `ask_user` answer and
            // the next chat message can't be swallowed by each other's
            // read-ahead.
            loop {
                print!("> ");
                std::io::Write::flush(&mut std::io::stdout()).ok();

                let next = { stdin_lines.lock().await.next_line().await? };
                let Some(line) = next else {
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
        // Dispatched and returned at the top of `run()`.
        Command::Permissions { .. } => unreachable!("handled by run_permissions"),
    }

    // v8 K-8: los saves de la memoria de proyecto corren en una task en
    // background — drenarlos antes de retornar, o un `braze run`
    // one-shot podría salir con el último save aún en cola.
    if let Some(hook) = &project_memory_hook {
        hook.flush().await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// E′ I.6: outside a git repo the snapshot degrades to date + OS —
    /// best-effort by design, never a startup failure.
    #[test]
    fn the_environment_snapshot_degrades_gracefully_without_git() {
        let dir = std::env::temp_dir().join(format!(
            "braze-cli-env-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let snapshot = build_environment_snapshot(&dir);
        assert!(snapshot.contains("- date: "), "got: {snapshot}");
        assert!(snapshot.contains(&format!("- os: {}", std::env::consts::OS)));
        assert!(
            !snapshot.contains("git branch"),
            "no repo → no git lines: {snapshot}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The civil-date computation produces a plausible ISO date (the
    /// alternative was a chrono dependency for one grounding line).
    #[test]
    fn the_date_string_is_iso_shaped() {
        let date = chrono_free_date_string();
        assert_eq!(date.len(), 10, "got: {date}");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
        let year: u32 = date[..4].parse().unwrap();
        assert!((2026..2100).contains(&year), "got: {date}");
    }

    /// `--output-format json`'s observer must collect exactly the text a
    /// plain-mode `TextDeltaObserver` would have streamed, plus summed
    /// usage across every round — the multi-round case is what a
    /// tool-calling turn (not just a one-shot text reply) actually looks
    /// like.
    #[test]
    fn json_summary_observer_accumulates_text_and_usage_across_rounds() {
        let mut observer = JsonSummaryObserver::default();

        observer.on_text_delta("hola ");
        observer.on_text_delta("mundo");
        observer.on_event(&AgentEvent::Usage {
            input_tokens: 100,
            output_tokens: 20,
            stop_reason: Some("tool_use".to_string()),
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        observer.on_event(&AgentEvent::ToolCallCompleted {
            id: "1".to_string(),
            result: braze_types::ToolResult {
                tool_call_id: "1".to_string(),
                content: "ok".to_string(),
                is_error: false,
            },
        });
        observer.on_event(&AgentEvent::Usage {
            input_tokens: 150,
            output_tokens: 5,
            stop_reason: Some("end_turn".to_string()),
            cache_read_tokens: None,
            cache_write_tokens: None,
        });

        assert_eq!(observer.text, "hola mundo");
        assert_eq!(observer.input_tokens, 250);
        assert_eq!(observer.output_tokens, 25);
        assert_eq!(observer.rounds, 2);
        // Last round's stop_reason wins, not the first's.
        assert_eq!(observer.stop_reason.as_deref(), Some("end_turn"));
        // A non-Usage event mirrored in between must not disturb the
        // accumulated usage — this is what proves `on_event` filters by
        // variant instead of just counting every call.
    }

    /// A turn with no tool calls at all (no `Usage` event ever reported —
    /// theoretically possible if a backend never emits one) must not
    /// panic or fabricate a stop reason; the JSON output should show
    /// `null`, not a guessed value.
    #[test]
    fn json_summary_observer_defaults_are_empty_not_fabricated() {
        let observer = JsonSummaryObserver::default();
        assert_eq!(observer.text, "");
        assert_eq!(observer.input_tokens, 0);
        assert_eq!(observer.rounds, 0);
        assert_eq!(observer.stop_reason, None);
    }
}
