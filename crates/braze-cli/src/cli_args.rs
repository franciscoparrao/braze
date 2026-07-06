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
        /// diseño") instead of the plain-text stdin/stdout loop. Opt-in
        /// for now. Irreversible-action tool calls show a real,
        /// keyboard-driven approval overlay (y/n) under `--tui`, same as
        /// the plain path's interactive stdin confirmation.
        #[arg(long)]
        tui: bool,
        /// Color preset for `--tui`: `dark` (default), `light`, or
        /// `high-contrast` — see `braze_tui::Theme`. Ignored without
        /// `--tui`.
        #[arg(long)]
        theme: Option<String>,
        /// Enable the planner/executor split for this run (PLAN.md §
        /// "Split planificador/ejecutor"): `<backend>` or
        /// `<backend>:<modelo>` — e.g. `--planner
        /// openrouter:deepseek/deepseek-v4-flash`. A stronger model plans
        /// each turn once; the primary backend executes. Overrides
        /// `planner_backend`/`planner_model` from config/env.
        #[arg(long)]
        planner: Option<String>,
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
        /// Enable the planner/executor split for this run — same syntax
        /// as `chat --planner`.
        #[arg(long)]
        planner: Option<String>,
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

    /// The `--planner` override, if any, common to both subcommands —
    /// `<backend>` or `<backend>:<modelo>`, split on the *first* `:` only
    /// (Ollama tags and OpenRouter model ids contain `:`/`/` of their
    /// own), same convention as `braze-bench`'s `BackendSpec::parse`.
    /// Returns `(backend, model_override)`.
    pub fn planner_override(&self) -> Option<(&str, Option<&str>)> {
        let raw = match self {
            Command::Chat { planner, .. } | Command::Run { planner, .. } => planner.as_deref()?,
        };
        Some(match raw.split_once(':') {
            Some((backend, model)) => (backend, Some(model)),
            None => (raw, None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_with_planner(planner: Option<&str>) -> Command {
        Command::Chat {
            resume: None,
            backend: None,
            model: None,
            tui: false,
            theme: None,
            planner: planner.map(str::to_string),
        }
    }

    #[test]
    fn planner_override_splits_backend_and_model_on_the_first_colon_only() {
        // An Ollama tag carries its own `:` — only the first one splits.
        let command = chat_with_planner(Some("ollama:qwen2.5:7b"));
        assert_eq!(
            command.planner_override(),
            Some(("ollama", Some("qwen2.5:7b")))
        );
    }

    #[test]
    fn planner_override_accepts_a_bare_backend_name() {
        let command = chat_with_planner(Some("openrouter"));
        assert_eq!(command.planner_override(), Some(("openrouter", None)));
    }

    #[test]
    fn planner_override_is_none_without_the_flag() {
        assert_eq!(chat_with_planner(None).planner_override(), None);
    }
}
