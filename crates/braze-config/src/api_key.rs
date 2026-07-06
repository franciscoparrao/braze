//! [`ApiKey`]: a secret credential whose `Debug`/`Serialize` forms are
//! always redacted — N-39 (docs/AUDITORIA-2026-07-v2.md): `Config`'s
//! `anthropic_api_key`/`openrouter_api_key` used to be plain `String`s on
//! a struct with `derive(Debug, Serialize)`, so any future
//! `tracing::debug!(?config)` or accidental JSON serialization would leak
//! the raw key.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

/// A secret API key. `Deserialize`s transparently from a plain string
/// (config file / `BRAZE_*` env vars are unaffected), but `Debug` and
/// `Serialize` never expose the real value.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The real secret — only for actually authenticating against the
    /// provider. Named `expose_secret` (not `as_str`/`inner`) so every
    /// call site reads as a deliberate, visible decision to handle the
    /// raw value, not an accidental one.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(REDACTED)")
    }
}

impl Serialize for ApiKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("REDACTED")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_real_key() {
        let key = ApiKey::new("sk-super-secret");
        assert_eq!(format!("{key:?}"), "ApiKey(REDACTED)");
    }

    #[test]
    fn serialize_never_shows_the_real_key() {
        let key = ApiKey::new("sk-super-secret");
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"REDACTED\"");
    }

    #[test]
    fn deserialize_reads_the_real_key_from_a_plain_string() {
        let key: ApiKey = serde_json::from_str("\"sk-super-secret\"").unwrap();
        assert_eq!(key.expose_secret(), "sk-super-secret");
    }

    #[test]
    fn expose_secret_returns_the_real_key() {
        let key = ApiKey::new("sk-super-secret");
        assert_eq!(key.expose_secret(), "sk-super-secret");
    }
}
