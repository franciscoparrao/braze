use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PermissionError {
    #[error("action denied: {0}")]
    Denied(String),
}
