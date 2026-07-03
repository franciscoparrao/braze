//! Shared HTTP -> [`ModelError`] mapping, used by both [`crate::AnthropicBackend`]
//! and [`crate::OllamaBackend`].
//!
//! A 429 always maps to [`ModelError::RateLimited`]; every other non-2xx
//! status maps to [`ModelError::Request`]. Response bodies are best-effort
//! decoded to extract a human-readable message from either provider's error
//! shape (Anthropic: `{"error": {"message": "..."}}`, Ollama:
//! `{"error": "..."}`); if decoding fails, the raw body text is used as-is.

use reqwest::Response;

use crate::error::ModelError;

/// Converts a non-2xx [`Response`] into a [`ModelError`]. Consumes the
/// response to read its body (best-effort — network failures while reading
/// the body still produce a `ModelError::Request`, never a panic).
pub(crate) async fn http_error_to_model_error(response: Response, provider: &str) -> ModelError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = extract_error_message(&body).unwrap_or_else(|| body.clone());

    if status.as_u16() == 429 {
        ModelError::RateLimited(format!("{provider} rate-limited (HTTP 429): {message}"))
    } else {
        ModelError::Request(format!("{provider} HTTP {status}: {message}"))
    }
}

/// Best-effort extraction of a human-readable message from a JSON error
/// body. Returns `None` (never panics/errors) when the body isn't JSON or
/// doesn't match a known shape — callers fall back to the raw body text.
fn extract_error_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;

    // Anthropic shape: {"type":"error","error":{"type":"...","message":"..."}}
    if let Some(message) = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Some(message.to_string());
    }

    // Ollama shape: {"error": "model 'x' not found"}
    if let Some(message) = value.get("error").and_then(|e| e.as_str()) {
        return Some(message.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_anthropic_error_shape() {
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"too many requests"}}"#;
        assert_eq!(
            extract_error_message(body),
            Some("too many requests".to_string())
        );
    }

    #[test]
    fn extracts_ollama_error_shape() {
        let body = r#"{"error":"model 'llama3' not found"}"#;
        assert_eq!(
            extract_error_message(body),
            Some("model 'llama3' not found".to_string())
        );
    }

    #[test]
    fn returns_none_for_non_json_body() {
        assert_eq!(extract_error_message("not json at all"), None);
    }

    #[test]
    fn returns_none_for_json_without_known_shape() {
        assert_eq!(extract_error_message(r#"{"foo":"bar"}"#), None);
    }
}
