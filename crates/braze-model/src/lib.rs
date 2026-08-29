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
mod failover;
// Compilados también sin el feature `local` (bajo `test`): estos módulos
// son puros (sin llama.cpp) y así sus tests corren en el `cargo test`
// normal del workspace, donde `local` no se compila.
#[cfg(any(feature = "local", test))]
mod chatml;
#[cfg(test)]
mod chat_template_fixtures;
#[cfg(any(feature = "local", test))]
mod gemma;
#[cfg(any(feature = "local", test))]
mod harmony;
mod http_client;
mod http_error;
#[cfg(feature = "local")]
mod local;
// Compilado solo bajo `test`: en el build real lo consume `build.rs` por
// `include!`, no el crate. Acá está para que sus tests corran.
#[cfg(test)]
mod lock_version;
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
pub use failover::FailoverBackend;
#[cfg(feature = "local")]
pub use local::{LocalBackend, LocalSampling, TuneReport, resolve_local_gguf, tune_model};
pub use ollama::{
    OllamaBackend, list_ollama_models, ollama_model_digest, ollama_server_version,
    warm_up_ollama_model,
};
pub use openrouter::OpenRouterBackend;

/// Identidad del motor de inferencia in-process linkeado en este binario,
/// para la procedencia de un sweep del `LocalBackend`.
///
/// `Some("llama-cpp-2 <version>[+cuda]")` cuando el binario se compiló con
/// el feature `local`; `None` cuando no — y ese `None` es informativo, no
/// una falla: significa que ningún backend de este binario puede ser
/// `local:`, así que no hay motor in-process que identificar.
///
/// Por qué existe: llama.cpp queda linkeado DENTRO del binario, así que
/// —a diferencia de Ollama, que expone `/api/version`— no hay nada que
/// consultar en runtime. La versión se embebe al compilar (`build.rs`) y
/// viaja con el ejecutable, incluso si se copia a otra máquina sin el
/// árbol de fuentes (el caso Nitro). Cambios de kernels, cuantización o
/// decodificación entre versiones de llama.cpp mueven resultados sin mover
/// modelo, seed ni sampling: dos sweeps con bindings distintos no son la
/// misma condición experimental.
///
/// El sufijo `+cuda` distingue el build con offload GPU del build CPU:
/// misma versión de bindings, kernels distintos, y en la práctica
/// resultados distintos.
pub fn local_engine_version() -> Option<String> {
    #[cfg(feature = "local")]
    {
        let version = env!("BRAZE_LLAMA_CPP_2_VERSION");
        let suffix = if cfg!(feature = "cuda") { "+cuda" } else { "" };
        Some(format!("llama-cpp-2 {version}{suffix}"))
    }
    #[cfg(not(feature = "local"))]
    {
        None
    }
}

#[cfg(test)]
mod engine_version_tests {
    /// Sin el feature `local` (el build normal del workspace) no hay motor
    /// in-process, y el campo debe decir eso en vez de inventar una
    /// versión. Con `local`, debe traer la versión resuelta y no el
    /// placeholder `unknown` del build script — un `unknown` filtrándose a
    /// la metadata sería peor que no tener el campo: parece procedencia
    /// sin serlo.
    #[test]
    fn engine_version_matches_the_build_features() {
        let got = super::local_engine_version();
        if cfg!(feature = "local") {
            let got = got.expect("con el feature `local` hay motor que identificar");
            assert!(got.starts_with("llama-cpp-2 "), "got: {got}");
            assert!(
                !got.contains("unknown"),
                "el build script no resolvió la versión del lock: {got}"
            );
            assert_eq!(got.contains("+cuda"), cfg!(feature = "cuda"), "got: {got}");
        } else {
            assert_eq!(got, None);
        }
    }
}
