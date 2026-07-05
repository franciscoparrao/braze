//! `braze-cli`'s own error enum, following the workspace convention (one
//! `<Crate>Error` per crate) even though this crate is a binary — startup
//! failures still deserve a clear, typed shape rather than ad hoc strings
//! scattered through `main`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("failed to load configuration: {0}")]
    Config(#[from] braze_config::ConfigError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Engine(#[from] braze_engine::EngineError),

    #[error("session store error: {0}")]
    Session(#[from] braze_session::SessionError),

    #[error("terminal UI error: {0}")]
    Tui(#[from] braze_tui::TuiError),

    /// Catch-all for startup problems with a human-authored, specific
    /// message (missing API key/model, unknown backend, unparseable
    /// `--resume` session id, ...) — deliberately a plain string rather
    /// than a family of near-duplicate variants, per the task's "no
    /// agregues una variante por cada posible fallo" guidance.
    #[error("{0}")]
    Startup(String),
}
