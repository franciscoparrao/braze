use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SessionError {
    #[error("failed to read session store: {0}")]
    Read(String),

    #[error("failed to write session store: {0}")]
    Write(String),

    #[error("session not found: {0}")]
    NotFound(String),

    #[error("compaction failed: {0}")]
    Compaction(String),
}
