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

    /// Something went wrong *after* the stream had already started —
    /// a transport error, a mid-stream provider error event (e.g.
    /// Anthropic's `overloaded_error`), or the connection closing before
    /// a terminal event was ever seen. Distinct from `Request` (which is
    /// reserved for failures before/while establishing the stream): this
    /// is what lets `Engine::run_turn` tell "the stream completed
    /// normally" apart from "the stream died partway through" instead of
    /// silently treating whatever partial text/tool-calls arrived as a
    /// complete, converged response.
    #[error("model backend's completion stream failed: {0}")]
    StreamError(String),

    /// A per-destination circuit breaker (`circuit_breaker.rs`) rejected
    /// this call without touching the network — enough recent calls to
    /// this same provider+URL failed that the breaker tripped, and its
    /// cooldown hasn't elapsed. Distinct from `Request`/`RateLimited`
    /// (which both mean "a real HTTP round-trip happened and failed")
    /// so a caller/log line can tell "the network said no" apart from
    /// "we didn't even ask, because it's been saying no repeatedly".
    #[error("{0}")]
    CircuitOpen(String),
}

/// Fragmentos (en minúsculas) con que los servidores reportan un prompt
/// más largo que la ventana de contexto del modelo. llama-server: «request
/// (34044 tokens) exceeds the available context size (32768 tokens)»
/// (sesión Juego de la Vida, 2026-10-22); API de OpenAI y compatibles:
/// `context_length_exceeded` / «maximum context length»; Anthropic:
/// «prompt is too long». Ollama no aparece porque trunca el prompt en
/// silencio en vez de rechazarlo.
const CONTEXT_OVERFLOW_MARKERS: &[&str] = &[
    "exceeds the available context size",
    "context_length_exceeded",
    "maximum context length",
    "prompt is too long",
];

impl ModelError {
    /// Si el backend rechazó el request porque el prompt no cabe en la
    /// ventana de contexto del modelo. Es la señal con que el engine
    /// compacta a la fuerza y reintenta la ronda en vez de perder el turno.
    pub fn is_context_overflow(&self) -> bool {
        let text = match self {
            ModelError::Request(t) | ModelError::StreamError(t) => t,
            _ => return false,
        };
        let text = text.to_lowercase();
        CONTEXT_OVERFLOW_MARKERS.iter().any(|m| text.contains(m))
    }
}

#[cfg(test)]
mod context_overflow_tests {
    use super::*;

    #[test]
    fn recognizes_the_overflow_messages_of_each_server_family() {
        let llama = ModelError::Request(
            "zen HTTP 400 Bad Request: request (34044 tokens) exceeds the available context \
             size (32768 tokens), try increasing it"
                .into(),
        );
        let openai = ModelError::Request(
            "HTTP 400: {\"error\":{\"code\":\"context_length_exceeded\"}}".into(),
        );
        let anthropic = ModelError::Request("HTTP 400: prompt is too long: 210000 tokens".into());
        assert!(llama.is_context_overflow());
        assert!(openai.is_context_overflow());
        assert!(anthropic.is_context_overflow());
    }

    #[test]
    fn other_errors_are_not_overflow() {
        assert!(!ModelError::Request("HTTP 400 Bad Request: invalid tool schema".into())
            .is_context_overflow());
        assert!(!ModelError::RateLimited("prompt is too long".into()).is_context_overflow());
        assert!(!ModelError::Decode("exceeds the available context size".into())
            .is_context_overflow());
    }
}
