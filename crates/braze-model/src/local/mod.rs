//! `LocalBackend` — inferencia in-process sobre `llama.cpp` (vía
//! `llama-cpp-2`), reusando los GGUF que Ollama ya bajó. Quinto
//! `impl ModelBackend`. Ver `docs/local-backend-design-2026-07-20.md`.
//!
//! Diferencia arquitectónica vs. `OllamaBackend`: aquí NO hay parser de
//! tool-calls server-side. El backend produce **texto crudo** (tokens →
//! `TextDelta`) y la escalera de rescate del engine extrae las tool
//! calls — igual que el modo prompt-tools de Ollama, pero total. Por eso
//! la clase de bug #1/#17 (parser harmony de Ollama) no puede ocurrir.
//!
//! Fase 1: CPU, plantilla ChatML (familia qwen), streaming, `Usage` $0.
//! Fase 2: GPU/CUDA (`BRAZE_LOCAL_GPU_LAYERS`) + familia **Harmony**
//! (gpt-oss): plantilla nativa y parser de canales en `harmony.rs`. Los
//! marcadores de Harmony son tokens especiales que no sobreviven
//! `token_to_piece(special=false)`, así que a diferencia de qwen las
//! tool calls se parsean acá (por id de token) y se emiten como
//! `ToolCallRequested` — la escalera de rescate del engine queda de red
//! de seguridad para el texto visible.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use braze_types::{ContentBlock, Role};
use futures::Stream;
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::token::logit_bias::LlamaLogitBias;

use crate::args_repair::{ArgumentsOutcome, parse_arguments_with_repair};
use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;
use crate::gemma::{build_gemma_prompt, render_tools_preamble};
use crate::harmony::{
    HarmonyEvent, HarmonyMarker, HarmonyParser, build_harmony_prompt, utc_date_string,
};
use crate::stencil::{JsonCursor, ToolGrammarSpec, harmony_args_grammar, qwen_call_grammar};

// L-4 (docs/AUDITORIA-2026-07-v9.md): `local.rs` llegó a 2.128 líneas —
// la misma curva que engine.rs recorrió antes del P1.1 — y se repartió
// en submódulos ANTES de las features de Fase 2. Aquí quedan el struct,
// sus constructores, la resolución de GGUF y el `impl ModelBackend`.
mod cache;
mod decode;
mod family;
mod fit;
mod sampling;

pub use fit::{TuneReport, tune_model};
pub use sampling::LocalSampling;

#[allow(unused_imports)]
use cache::*;
#[allow(unused_imports)]
use decode::*;
#[allow(unused_imports)]
use family::*;
#[allow(unused_imports)]
use fit::*;
#[allow(unused_imports)]
use sampling::*;

/// Inferencia local sobre un GGUF cargado en el proceso. El modelo
/// (read-only) se comparte por `Arc`; cada `complete()` crea su propio
/// contexto en un hilo bloqueante (la inferencia de llama.cpp es
/// single-thread y CPU/GPU-bound, no async).
pub struct LocalBackend {
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    model_label: String,
    n_ctx: u32,
    family: ChatFamily,
    /// `Some` sólo para la familia Harmony.
    harmony: Option<HarmonyTokenIds>,
    /// Capas offloadeadas a GPU con las que se CARGÓ el modelo (convención
    /// de llama.cpp: 0 = CPU puro, negativo = todas). Se resuelve una vez al
    /// cargar y viaja al hilo de generación, que necesita el mismo número
    /// para decidir dónde vive el KV cache — releerlo del entorno ahí
    /// permitiría que el contexto se arme contra una realidad distinta a la
    /// que se midió al cargar.
    gpu_layers: i32,
    /// Dónde vive el KV cache, resuelto contra la VRAM medida al cargar.
    kv_placement: KvPlacement,
    /// Cómo samplea. Default greedy (ver [`LocalSampling`]).
    sampling: LocalSampling,
}

/// `LlamaBackend::init()` inicializa estado GLOBAL de llama.cpp y sólo
/// puede llamarse UNA vez por proceso (un segundo intento da
/// `BackendAlreadyInitialized`). braze-bench crea un `LocalBackend` nuevo
/// por tarea, así que la inicialización debe ser un singleton compartido.
/// El `Mutex` serializa la primera init; luego todos clonan el `Arc`.
fn shared_llama_backend() -> Result<Arc<LlamaBackend>, ModelError> {
    static BACKEND: Mutex<Option<Arc<LlamaBackend>>> = Mutex::new(None);
    let mut guard = BACKEND.lock().unwrap();
    if let Some(existing) = guard.as_ref() {
        return Ok(Arc::clone(existing));
    }
    // llama.cpp/ggml loggean a stderr por default — en la TUI eso pisa el
    // viewport de ratatui (verificado en vivo, qwen2.5:3b GPU 2026-07-21).
    // Rutearlos a `tracing` (cubre llama_log_set + ggml_log_set) los pone
    // bajo el mismo control de RUST_LOG que el resto del workspace.
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
    let backend = Arc::new(
        LlamaBackend::init()
            .map_err(|e| ModelError::Request(format!("llama backend init failed: {e}")))?,
    );
    *guard = Some(Arc::clone(&backend));
    Ok(backend)
}

/// Resuelve una referencia de modelo local al GGUF en disco: una ruta que
/// termina en `.gguf` (o que contiene `/`) se toma literal; cualquier otra
/// cosa se trata como ref de Ollama (`qwen2.5:3b`) y se busca en sus blobs.
/// Es la misma heurística que aplica el CLI al construir el backend.
///
/// # Errors
/// Devuelve error si la ref de Ollama no tiene manifest o su blob falta.
pub fn resolve_local_gguf(
    model_ref: &str,
    ollama_root: impl AsRef<Path>,
) -> Result<PathBuf, ModelError> {
    if model_ref.contains('/') || model_ref.ends_with(".gguf") {
        Ok(PathBuf::from(model_ref))
    } else {
        resolve_ollama_gguf(ollama_root.as_ref(), model_ref)
    }
}

impl LocalBackend {
    /// Carga un GGUF desde una ruta directa a `.gguf` o al blob de Ollama.
    /// `model_label` es solo para `name()`/trazas (precio local = $0).
    /// `gpu_layers` fija cuántas capas se ofloadean a GPU para ESTA
    /// instancia, ganándole a `BRAZE_LOCAL_GPU_LAYERS` y al auto-fit;
    /// `None` deja la resolución como siempre (env > auto-fit > CPU).
    /// Es por instancia y no por proceso porque un sweep de braze-bench
    /// corre dos precios de ronda como dos brazos de la misma corrida
    /// (`+ablate:gpu-layers=N`, línea round-economics).
    pub fn from_gguf_path(
        gguf: impl AsRef<Path>,
        model_label: impl Into<String>,
        n_ctx: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self, ModelError> {
        // llama-cpp-2 panickea (no devuelve Err) si la ruta no existe —
        // chequear antes convierte el panic en el error legible del
        // backend (papercut encontrado armando el wrapper braze-oss,
        // 2026-07-21).
        if !gguf.as_ref().exists() {
            return Err(ModelError::Request(format!(
                "el GGUF no existe: {}",
                gguf.as_ref().display()
            )));
        }
        let backend = shared_llama_backend()?;
        // Cuántas capas van a la GPU lo decide `resolve_model_params`:
        // auto-fit contra la VRAM libre por default, `BRAZE_LOCAL_GPU_LAYERS`
        // si el usuario fija el número a mano. En un binario sin CUDA no hay
        // devices GPU que medir y el fit devuelve 0 capas → CPU puro, el
        // mismo comportamiento que antes de la palanca.
        let (model, gpu_layers, layers_source, kv_placement) =
            load_model_cached(&backend, gguf.as_ref(), n_ctx, gpu_layers)?;
        let model_label = model_label.into();
        let family = detect_family(&model, &model_label);
        let harmony = match family {
            ChatFamily::Harmony => Some(HarmonyTokenIds::resolve(&model)?),
            ChatFamily::ChatMl | ChatFamily::Gemma => None,
        };
        tracing::info!(
            model = %model_label,
            ?family,
            gpu_layers,
            ?layers_source,
            ?kv_placement,
            "local backend loaded"
        );
        Ok(Self {
            backend,
            model,
            model_label,
            n_ctx,
            family,
            harmony,
            gpu_layers,
            kv_placement,
            sampling: LocalSampling::from_env(),
        })
    }

    /// Fija el sampling programáticamente. Mismo patrón que
    /// `AnthropicBackend::with_temperature`: el régimen vive en el backend,
    /// no en `CompletionRequest` (que es contrato congelado). Sin llamarlo,
    /// el backend toma [`LocalSampling::from_env`], cuyo default es greedy.
    #[must_use]
    pub fn with_sampling(mut self, sampling: LocalSampling) -> Self {
        self.sampling = sampling;
        self
    }

    /// Resuelve `modelo:tag` (p.ej. `qwen2.5:3b`) al blob GGUF que Ollama
    /// ya bajó, leyendo el manifest y ubicando la capa `…model`. Reusa
    /// los pesos sin re-download. `ollama_root` suele ser
    /// `/usr/share/ollama/.ollama` o `~/.ollama`.
    pub fn from_ollama_model(
        ollama_root: impl AsRef<Path>,
        model_ref: &str,
        model_label: impl Into<String>,
        n_ctx: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self, ModelError> {
        let gguf = resolve_ollama_gguf(ollama_root.as_ref(), model_ref)?;
        Self::from_gguf_path(gguf, model_label, n_ctx, gpu_layers)
    }
}

/// Ubica el blob GGUF de un modelo Ollama vía su manifest.
fn resolve_ollama_gguf(root: &Path, model_ref: &str) -> Result<PathBuf, ModelError> {
    let (name, tag) = model_ref.split_once(':').unwrap_or((model_ref, "latest"));
    let manifest_path = root
        .join("models/manifests/registry.ollama.ai/library")
        .join(name)
        .join(tag);
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| {
        ModelError::Request(format!(
            "no se pudo leer el manifest de Ollama '{}': {e}",
            manifest_path.display()
        ))
    })?;
    let json: serde_json::Value = serde_json::from_str(&manifest)
        .map_err(|e| ModelError::Decode(format!("manifest de Ollama inválido: {e}")))?;
    let digest = json
        .get("layers")
        .and_then(|l| l.as_array())
        .and_then(|layers| {
            layers.iter().find(|layer| {
                layer
                    .get("mediaType")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m.ends_with(".model"))
            })
        })
        .and_then(|layer| layer.get("digest"))
        .and_then(|d| d.as_str())
        .ok_or_else(|| {
            ModelError::Request(format!(
                "el manifest de '{model_ref}' no tiene capa de modelo"
            ))
        })?;
    // Ollama nombra los blobs con `-` en vez de `:`.
    let blob = root.join("models/blobs").join(digest.replace(':', "-"));
    if !blob.exists() {
        return Err(ModelError::Request(format!(
            "el blob GGUF no existe: {}",
            blob.display()
        )));
    }
    Ok(blob)
}

#[async_trait]
impl ModelBackend for LocalBackend {
    fn name(&self) -> &str {
        "local"
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let prompt = match self.family {
            ChatFamily::ChatMl => build_chatml_prompt(&req),
            ChatFamily::Gemma => build_gemma_prompt(&req),
            ChatFamily::Harmony => {
                // Esfuerzo de razonamiento del system message de gpt-oss.
                // Default `medium` = el default del template de Ollama
                // (paridad del A/B); override por env para el bench.
                let reasoning =
                    std::env::var("BRAZE_LOCAL_REASONING").unwrap_or_else(|_| "medium".to_string());
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
                    .unwrap_or(0);
                build_harmony_prompt(&req, &reasoning, Some(&utc_date_string(now)))
            }
        };
        tracing::info!(
            model = %self.model_label,
            n_ctx = self.n_ctx,
            family = ?self.family,
            prompt_chars = prompt.len(),
            "starting local (llama.cpp) completion turn"
        );

        let backend = Arc::clone(&self.backend);
        let model = Arc::clone(&self.model);
        let n_ctx = self.n_ctx;
        let max_tokens = req.max_tokens;
        let gpu_layers = self.gpu_layers;
        let placement = self.kv_placement;
        let sampling = self.sampling;
        // Para las gramáticas del stencil: nombre + input_schema de cada
        // tool del turno (los schemas derivan las gramáticas de args).
        let tools: Vec<ToolGrammarSpec> = req
            .tool_stubs
            .iter()
            .map(|s| ToolGrammarSpec {
                name: s.name.clone(),
                schema: s.input_schema.clone(),
            })
            .collect();
        let family_rt = match &self.harmony {
            Some(ids) => FamilyRuntime::Harmony {
                ids: ids.clone(),
                tools,
            },
            None => FamilyRuntime::ChatMl { tools },
        };

        // Canal acotado: la generación bloqueante empuja, el stream async
        // consume. Si el consumidor abandona, `blocking_send` falla y la
        // generación corta (cancelación cooperativa).
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<CompletionEvent, ModelError>>(32);
        tokio::task::spawn_blocking(move || {
            let gen_params = GenParams {
                n_ctx,
                max_tokens,
                gpu_layers,
                placement,
                sampling,
            };
            generate_blocking(&backend, &model, &prompt, gen_params, &family_rt, &tx);
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }
}
