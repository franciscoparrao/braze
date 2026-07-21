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
// Compilados también sin el feature `local` (bajo `test`): estos módulos
// son puros (sin llama.cpp) y así sus tests corren en el `cargo test`
// normal del workspace, donde `local` no se compila.
#[cfg(any(feature = "local", test))]
mod gemma;
#[cfg(any(feature = "local", test))]
mod harmony;
mod http_client;
mod http_error;
#[cfg(feature = "local")]
mod local;
mod ollama;
mod ollama_wire;
mod openrouter;
mod openrouter_wire;
mod retry;
// Mismo patrón que `harmony`: puro, testeable sin compilar llama.cpp.
#[cfg(any(feature = "local", test))]
mod stencil;
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
