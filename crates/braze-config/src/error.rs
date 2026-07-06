use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("failed to read config file '{path}': {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("config file '{path}' contains invalid JSON: {source}")]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("environment variable '{var}' has an invalid value '{value}': {reason}")]
    InvalidEnvValue {
        var: String,
        value: String,
        reason: String,
    },

    /// Cross-field or range validation that can't be expressed per-field
    /// via serde alone — see `Config::validate` (N-41,
    /// docs/AUDITORIA-2026-07-v2.md).
    #[error("invalid configuration: {0}")]
    Invalid(String),
}
