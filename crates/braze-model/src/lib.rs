//! `ModelBackend` trait — abstracts over LLM providers so the engine never
//! assumes a single vendor's shapes.
//!
//! Frozen contract (PLAN.md): concrete implementations (`AnthropicBackend`,
//! `OllamaBackend`) land in Fase 3. Both stream via `tokio` internally;
//! the workspace is async end-to-end (see PLAN.md, desviación de
//! convención).

mod anthropic;
mod anthropic_wire;
mod args_repair;
mod backend;
mod circuit_breaker;
mod error;
mod escalation;
mod http_client;
mod http_error;
#[cfg(feature = "local")]
mod local;
mod ollama;
mod ollama_wire;
mod openrouter;
mod openrouter_wire;
mod retry;
mod synth_id;
#[cfg(test)]
mod test_support;

pub use anthropic::AnthropicBackend;
pub use backend::{CompletionEvent, CompletionRequest, ModelBackend};
pub use error::ModelError;
pub use escalation::EscalatingBackend;
#[cfg(feature = "local")]
pub use local::LocalBackend;
pub use ollama::{
    OllamaBackend, list_ollama_models, ollama_model_digest, ollama_server_version,
    warm_up_ollama_model,
};
pub use openrouter::OpenRouterBackend;
