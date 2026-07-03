use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),

    #[error("tool '{name}' invocation failed: {message}")]
    InvocationFailed { name: String, message: String },

    #[error("provider '{0}' is unavailable")]
    ProviderUnavailable(String),
}
