use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("request to model backend failed: {0}")]
    Request(String),

    #[error("model backend returned an unparseable response: {0}")]
    Decode(String),

    #[error("model backend rate-limited the request: {0}")]
    RateLimited(String),
}
