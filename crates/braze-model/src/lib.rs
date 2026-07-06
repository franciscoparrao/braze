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
mod error;
mod escalation;
mod http_client;
mod http_error;
mod ollama;
mod ollama_wire;
mod openrouter;
mod openrouter_wire;
mod synth_id;
#[cfg(test)]
mod test_support;

pub use anthropic::AnthropicBackend;
pub use backend::{CompletionEvent, CompletionRequest, ModelBackend};
pub use error::ModelError;
pub use escalation::EscalatingBackend;
pub use ollama::{OllamaBackend, list_ollama_models};
pub use openrouter::OpenRouterBackend;
