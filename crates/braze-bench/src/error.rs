//! `braze-bench`'s own error enum, following the same one-`<Crate>Error`-
//! per-crate convention as `braze-cli::CliError`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BenchError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid task suite TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("failed to load configuration: {0}")]
    Config(#[from] braze_config::ConfigError),

    #[error("session store error: {0}")]
    Session(#[from] braze_session::SessionError),

    #[error(transparent)]
    Engine(#[from] braze_engine::EngineError),

    /// Catch-all for backend-spec parsing/construction problems (unknown
    /// provider, missing API key, ...) — a specific, human-authored
    /// message rather than a family of near-duplicate variants, same
    /// rationale as `CliError::Startup`.
    #[error("{0}")]
    Startup(String),
}
