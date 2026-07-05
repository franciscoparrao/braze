//! `clap` v4 derive command structure for the `braze` binary.
//!
//! ```text
//! braze chat [--resume <session-id>] [--backend anthropic|ollama|openrouter] [--model <nombre>] [--tui]
//! braze run <prompt> [--backend anthropic|ollama|openrouter] [--model <nombre>]
//! ```
//!
//! `--backend`/`--model` are optional overrides layered on top of whatever
//! [`braze_config::Config::load`] already resolved (defaults -> config
//! file -> `BRAZE_*` env vars) via [`braze_config::ConfigOverrides`] — this
//! crate never reimplements config loading, it only adds the final,
//! highest-priority layer.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "braze",
    about = "braze: an experimental agentic CLI assistant",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interactive chat session (reads lines from stdin until EOF/`exit`/`quit`).
    Chat {
        /// Resume a previously started session by id instead of starting a new one.
        #[arg(long)]
        resume: Option<String>,
        /// Override the configured default model backend for this run.
        #[arg(long)]
        backend: Option<String>,
        /// Override the configured model name for this run.
        #[arg(long)]
        model: Option<String>,
        /// Use the terminal UI (`braze-tui`, PLAN.md § "Fase TUI —
        /// diseño") instead of the plain-text stdin/stdout loop.
        /// Opt-in for now — irreversible-action tool calls are denied
        /// automatically under `--tui` until the real approval overlay
        /// ships (oleada 4); the plain path still confirms interactively.
        #[arg(long)]
        tui: bool,
        /// Color preset for `--tui`: `dark` (default), `light`, or
        /// `high-contrast` — see `braze_tui::Theme`. Ignored without
        /// `--tui`.
        #[arg(long)]
        theme: Option<String>,
    },
    /// One-shot: run a single prompt and exit.
    Run {
        /// The prompt to send.
        prompt: String,
        /// Override the configured default model backend for this run.
        #[arg(long)]
        backend: Option<String>,
        /// Override the configured model name for this run.
        #[arg(long)]
        model: Option<String>,
    },
}

impl Command {
    /// The `--backend` override, if any, common to both subcommands.
    pub fn backend_override(&self) -> Option<&str> {
        match self {
            Command::Chat { backend, .. } | Command::Run { backend, .. } => backend.as_deref(),
        }
    }

    /// The `--model` override, if any, common to both subcommands.
    pub fn model_override(&self) -> Option<&str> {
        match self {
            Command::Chat { model, .. } | Command::Run { model, .. } => model.as_deref(),
        }
    }

    /// The `--theme` override, if any — `Chat`-only, `Run` never touches
    /// `braze-tui` at all.
    pub fn theme_override(&self) -> Option<&str> {
        match self {
            Command::Chat { theme, .. } => theme.as_deref(),
            Command::Run { .. } => None,
        }
    }
}
