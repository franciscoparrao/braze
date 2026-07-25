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

use crate::args_repair::{ArgumentsOutcome, parse_arguments_with_repair};
use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;
use crate::gemma::{build_gemma_prompt, render_tools_preamble};
use crate::harmony::{
    HarmonyEvent, HarmonyMarker, HarmonyParser, build_harmony_prompt, utc_date_string,
};
use crate::stencil::{JsonCursor, ToolGrammarSpec, harmony_args_grammar, qwen_call_grammar};

/// Mapea el valor de `BRAZE_LOCAL_KV_TYPE` a un [`KvCacheType`]. Solo los
/// tipos que llama.cpp acepta para el KV cache (los k-quants por-bloque no
/// aplican al KV); `None` = desconocido → el caller deja el default `f16`.
fn parse_kv_cache_type(s: &str) -> Option<KvCacheType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "f16" => Some(KvCacheType::F16),
        "f32" => Some(KvCacheType::F32),
        "q8_0" => Some(KvCacheType::Q8_0),
        "q5_1" => Some(KvCacheType::Q5_1),
        "q5_0" => Some(KvCacheType::Q5_0),
        "q4_1" => Some(KvCacheType::Q4_1),
        "q4_0" => Some(KvCacheType::Q4_0),
        _ => None,
    }
}

/// Micro-batch FÍSICO del contexto (`BRAZE_LOCAL_UBATCH`). El buffer de
/// prompt-processing lo dimensiona `n_ubatch` × contexto y vive en VRAM; con
/// el default de llama.cpp (512) crece hasta reventar los 6GB al llenarse el
/// contexto. Ollama usa micro-batches chicos (VRAM plana) y lo replicamos.
fn ubatch_setting() -> u32 {
    std::env::var("BRAZE_LOCAL_UBATCH")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&u| u > 0)
        .unwrap_or(128)
}

/// ¿El KV cache vive en la RAM del host en vez de la VRAM? Sin esto, el KV
/// de las capas offloadeadas crece con el contexto y revienta la VRAM a
/// mitad de una sesión agéntica (OOM en la 2ª ronda, observado con
/// gpt-oss:20b en la RTX 3050 de 6GB). Kill-switch
/// `BRAZE_LOCAL_KV_OFFLOAD=gpu` restaura el default de llama.cpp.
fn kv_on_host(gpu_layers: i32) -> bool {
    gpu_layers != 0 && std::env::var("BRAZE_LOCAL_KV_OFFLOAD").as_deref() != Ok("gpu")
}

/// Arma los `LlamaContextParams` de generación. Lo usan DOS lados que deben
/// coincidir: la creación real del contexto y el probe del auto-fit. Si el
/// probe midiera con otros parámetros, el fit repartiría capas contra un
/// consumo de VRAM que no es el que la generación va a tener realmente.
fn build_ctx_params(
    n_ctx: u32,
    gpu_layers: i32,
    kv_type: Option<KvCacheType>,
) -> LlamaContextParams {
    let n_batch_max = n_ctx.max(256);
    let mut params =
        LlamaContextParams::default().with_n_ctx(std::num::NonZeroU32::new(n_batch_max));
    if kv_on_host(gpu_layers) {
        let ubatch = ubatch_setting();
        params = params
            .with_offload_kqv(false)
            .with_n_ubatch(ubatch)
            // `n_batch` (batch LÓGICO) cubre el prompt entero (braze lo
            // decodifica en chunks, si no `GGML_ASSERT n_tokens_all <= n_batch`).
            .with_n_batch(n_batch_max.max(ubatch));
    }
    if let Some(t) = kv_type {
        params = params.with_type_k(t).with_type_v(t);
    }
    params
}

/// `GGML_LOG_LEVEL_WARN`. `ggml_log_level` es un alias de `c_uint` en los
/// bindings y `llama-cpp-2` no re-exporta el crate `-sys`, así que el nivel
/// viaja como literal: los `LOG_INF`/`LOG_TRC` del fitting caen a debug y no
/// ensucian la TUI (nuestra propia traza del resultado dice lo que importa).
const FIT_LOG_LEVEL: u32 = 3;

/// Margen de memoria que el auto-fit deja libre por device.
/// Default = 1 GiB, el mismo de llama.cpp upstream (`fit_params_target`);
/// override con `BRAZE_LOCAL_VRAM_MARGIN_MB` para exprimir o ser más
/// conservador según la tarjeta.
fn fit_margin_bytes() -> usize {
    const DEFAULT_MARGIN_MIB: usize = 1024;
    let mib = std::env::var("BRAZE_LOCAL_VRAM_MARGIN_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MARGIN_MIB);
    mib * 1024 * 1024
}

/// ¿El binario tiene backend de GPU y hay uno disponible? Sin esto no se
/// puede interpretar el `-1` que devuelve el fit: en un build con CUDA y
/// tarjeta presente significa "todas las capas", y en un build CPU-only
/// significa cero.
fn backend_supports_gpu() -> bool {
    shared_llama_backend().is_ok_and(|b| b.supports_gpu_offload())
}

/// `common_fit_params` **no es thread-safe** (muta el logger global de
/// llama.cpp mientras corre). braze-bench crea un `LocalBackend` por tarea,
/// así que serializamos los fits entre sí.
static FIT_LOCK: Mutex<()> = Mutex::new(());

/// De dónde salió el `n_gpu_layers` con el que se cargó el modelo. Se traza
/// para que un sweep pueda distinguir "el auto-fit eligió 24" de "el usuario
/// pidió 24" sin releer el entorno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuLayersSource {
    /// `BRAZE_LOCAL_GPU_LAYERS` explícito — el usuario manda.
    Explicit,
    /// El auto-fit de llama.cpp repartió capas contra la VRAM libre.
    AutoFit,
    /// Auto-fit apagado (`BRAZE_LOCAL_AUTOFIT=off`) → CPU puro.
    Disabled,
    /// El auto-fit falló; se degradó a CPU en vez de crashear.
    FitFailed,
}

/// Resuelve `n_gpu_layers` y devuelve los `LlamaModelParams` listos para
/// cargar. El auto-fit (idea #1 de `docs/inference-runtimes-audit-2026-07-25.md`)
/// delega en `common_fit_params` de libcommon — el MISMO algoritmo que usa
/// `llama-cli` con `--fit` (default upstream: `fit_params = true`): mide la
/// VRAM libre por device con un probe `no_alloc`, llena capas densas
/// back-to-front dejando el margen, y manda los tensores MoE sobrantes a
/// RAM vía `tensor_buft_overrides`.
///
/// Sustituye al `BRAZE_LOCAL_GPU_LAYERS` adivinado a mano: ese número, si se
/// pasaba de largo, no daba un error legible sino un OOM de CUDA que mata el
/// proceso a mitad de sweep.
///
/// Precedencia: env explícito > auto-fit > CPU. Cualquier fallo del fit
/// degrada a CPU puro con un warn (filosofía degrade-not-crash del proyecto).
fn resolve_model_params(
    gguf: &Path,
    n_ctx: u32,
) -> (Pin<Box<LlamaModelParams>>, i32, GpuLayersSource) {
    // El env explícito gana siempre: es el escape hatch y el brazo de
    // ablación del bench.
    if let Some(n) = std::env::var("BRAZE_LOCAL_GPU_LAYERS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        let layers = i32::try_from(n).unwrap_or(i32::MAX);
        let params = Box::pin(LlamaModelParams::default().with_n_gpu_layers(n));
        tracing::info!(
            gpu_layers = layers,
            "local: n_gpu_layers explícito (BRAZE_LOCAL_GPU_LAYERS) — auto-fit omitido"
        );
        return (params, layers, GpuLayersSource::Explicit);
    }

    if std::env::var("BRAZE_LOCAL_AUTOFIT").as_deref() == Ok("off") {
        tracing::info!("local: auto-fit desactivado (BRAZE_LOCAL_AUTOFIT=off) — CPU puro");
        return (
            Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
            0,
            GpuLayersSource::Disabled,
        );
    }

    let Some(path_str) = gguf.to_str() else {
        tracing::warn!(
            path = %gguf.display(),
            "local: ruta del GGUF no es UTF-8 — auto-fit omitido, CPU puro"
        );
        return (
            Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
            0,
            GpuLayersSource::FitFailed,
        );
    };
    let Ok(c_path) = std::ffi::CString::new(path_str) else {
        tracing::warn!("local: ruta del GGUF con byte nulo — auto-fit omitido, CPU puro");
        return (
            Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
            0,
            GpuLayersSource::FitFailed,
        );
    };

    // El probe mide con los cparams REALES de generación. `gpu_layers = 1`
    // como sonda: le dice al fit "va a haber offload", que es lo que decide
    // si el KV vive en host. Si el fit termina asignando 0 capas la
    // generación tampoco usará VRAM, así que la cuenta sigue siendo válida.
    let kv_type = std::env::var("BRAZE_LOCAL_KV_TYPE")
        .ok()
        .and_then(|kv| parse_kv_cache_type(&kv));

    // Dos intentos: con el KV pedido y, si falla, con f16. El KV cuantizado
    // requiere flash-attn, que gpt-oss/Harmony NO soporta — sin este
    // reintento un `BRAZE_LOCAL_KV_TYPE` no soportado haría fracasar el fit
    // y perderíamos la GPU entera por una palanca ortogonal.
    let attempts: &[Option<KvCacheType>] = if kv_type.is_some() {
        &[kv_type, None]
    } else {
        &[None]
    };

    let margin = fit_margin_bytes();
    for (attempt, kv) in attempts.iter().enumerate() {
        let mut params = Box::pin(LlamaModelParams::default());
        let mut cparams = build_ctx_params(n_ctx, 1, *kv);
        let mut margins = vec![margin; llama_cpp_2::max_devices().max(1)];
        let result = {
            let _guard = FIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            params
                .as_mut()
                .fit_params(&c_path, &mut cparams, &mut margins, n_ctx, FIT_LOG_LEVEL)
        };
        match result {
            Ok(fit) => {
                // Sin GPU disponible el fit no toca `n_gpu_layers` y lo deja
                // en su default `-1`. Ese `-1` significa "todas las que
                // quepan", que sin device es CERO — pero tomado literal hace
                // que `kv_on_host` se active y le imponga a una corrida de
                // CPU puro el `n_ubatch` chico pensado para cuidar VRAM, que
                // ahí solo frena el prefill. Normalizarlo a 0 (bug cazado en
                // vivo con `braze tune` en la máquina sin GPU, 2026-07-25).
                let layers = if backend_supports_gpu() {
                    params.n_gpu_layers()
                } else {
                    0
                };
                tracing::info!(
                    gpu_layers = layers,
                    fitted_n_ctx = fit.n_ctx,
                    margin_mib = margin / (1024 * 1024),
                    kv_quantized = kv.is_some(),
                    "local: auto-fit resolvió el offload a GPU contra la VRAM libre"
                );
                return (params, layers, GpuLayersSource::AutoFit);
            }
            Err(e) if attempt + 1 < attempts.len() => {
                tracing::warn!(
                    error = %e,
                    "local: auto-fit falló con KV cuantizado; reintentando con f16"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "local: auto-fit falló — se carga en CPU puro (degradar, no crashear). \
                     Forzar capas con BRAZE_LOCAL_GPU_LAYERS si la GPU sí sirve."
                );
            }
        }
    }

    (
        Box::pin(LlamaModelParams::default().with_n_gpu_layers(0u32)),
        0,
        GpuLayersSource::FitFailed,
    )
}

// El preámbulo de tools de las familias textuales (formato nativo de
// qwen2.5, reusado como convención instruida para Gemma) vive en
// `gemma::render_tools_preamble` — módulo puro, compartido y testeado.

/// Familia de plantilla de chat del modelo cargado. Decide qué prompt se
/// arma y cómo se interpreta la salida (texto plano + rescate del engine
/// para ChatML; parser de canales en el backend para Harmony).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatFamily {
    /// ChatML + preámbulo de tools de qwen2.5 (Fase 1). Default.
    ChatMl,
    /// Harmony (gpt-oss): system/developer canónicos, canales
    /// analysis/commentary/final, tool calls por token especial.
    Harmony,
    /// Gemma (`<start_of_turn>`, gemma2/3/4): system plegado al primer
    /// turno user, misma convención textual de tools que ChatML (el
    /// GGUF de Ollama es compatible con llama.cpp — arch `gemma4`).
    Gemma,
}

/// Detecta la familia: override explícito por `BRAZE_LOCAL_FAMILY`
/// (`harmony`/`chatml`), si no la arquitectura del GGUF
/// (`general.architecture == "gpt-oss"`), si no el label del modelo
/// (`gpt-oss:20b` viene del ref de Ollama).
fn detect_family(model: &LlamaModel, label: &str) -> ChatFamily {
    match std::env::var("BRAZE_LOCAL_FAMILY").ok().as_deref() {
        Some("harmony") => return ChatFamily::Harmony,
        Some("chatml") => return ChatFamily::ChatMl,
        Some("gemma") => return ChatFamily::Gemma,
        Some(other) => {
            tracing::warn!(
                family = other,
                "BRAZE_LOCAL_FAMILY desconocida; autodetectando"
            );
        }
        None => {}
    }
    let arch = model
        .meta_val_str("general.architecture")
        .unwrap_or_default();
    if arch.replace('-', "") == "gptoss" || label.contains("gpt-oss") {
        ChatFamily::Harmony
    } else if arch.starts_with("gemma") || label.contains("gemma") {
        ChatFamily::Gemma
    } else {
        ChatFamily::ChatMl
    }
}

/// Ids de los tokens especiales de Harmony en el vocabulario del GGUF
/// cargado, resueltos una vez al construir el backend (tokenizar cada
/// literal debe dar exactamente un token — si no, el GGUF no es harmony
/// y el error temprano evita un run entero de salida ilegible).
#[derive(Clone)]
struct HarmonyTokenIds {
    pairs: Vec<(LlamaToken, HarmonyMarker)>,
}

impl HarmonyTokenIds {
    fn resolve(model: &LlamaModel) -> Result<Self, ModelError> {
        let mut pairs = Vec::with_capacity(HarmonyMarker::ALL.len());
        for marker in HarmonyMarker::ALL {
            let tokens = model
                .str_to_token(marker.literal(), AddBos::Never)
                .map_err(|e| {
                    ModelError::Request(format!(
                        "harmony: no se pudo tokenizar '{}': {e}",
                        marker.literal()
                    ))
                })?;
            let [token] = tokens.as_slice() else {
                return Err(ModelError::Request(format!(
                    "harmony: '{}' no es un token especial único en este vocabulario \
                     ({} tokens) — ¿el GGUF es realmente gpt-oss? \
                     (override: BRAZE_LOCAL_FAMILY=chatml)",
                    marker.literal(),
                    tokens.len()
                )));
            };
            pairs.push((*token, marker));
        }
        Ok(Self { pairs })
    }

    fn marker_of(&self, token: LlamaToken) -> Option<HarmonyMarker> {
        self.pairs
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, m)| *m)
    }
}

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

/// Lo que el auto-fit resolvió para un GGUF, **sin cargarlo para
/// inferencia**. Es la salida de [`tune_model`], que alimenta `braze tune`
/// (idea #8 de `docs/inference-runtimes-audit-2026-07-25.md`).
#[derive(Debug, Clone)]
pub struct TuneReport {
    /// El GGUF que se midió, con la ruta ya canonicalizada.
    pub gguf: PathBuf,
    /// Contexto contra el que se fiteó — el reparto depende de él.
    pub n_ctx: u32,
    /// Capas a GPU (convención llama.cpp: 0 = CPU, negativo = todas).
    pub n_gpu_layers: i32,
    /// Cómo se llegó al número: auto-fit, env explícito, o degradación.
    pub source: &'static str,
    /// Margen de VRAM por device que se dejó libre.
    pub margin_mib: usize,
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

/// Corre el auto-fit sobre un GGUF y reporta el reparto, sin cargar los
/// pesos para inferencia (el probe usa `no_alloc`). Sirve para **fitear una
/// vez y fijar el número**: el sweep siguiente exporta
/// `BRAZE_LOCAL_GPU_LAYERS` y se ahorra el fit por tarea, además de quedar
/// reproducible en vez de re-adivinado.
///
/// # Errors
/// Devuelve error si el GGUF no existe o si el backend de llama.cpp no
/// pudo inicializarse.
pub fn tune_model(gguf: impl AsRef<Path>, n_ctx: u32) -> Result<TuneReport, ModelError> {
    let gguf = gguf.as_ref();
    if !gguf.exists() {
        return Err(ModelError::Request(format!(
            "el GGUF no existe: {}",
            gguf.display()
        )));
    }
    // El fit carga un modelo con `no_alloc`, así que el backend global tiene
    // que estar inicializado (y esto además rutea los logs de llama.cpp a
    // `tracing`, para que el probe no escupa a stderr).
    let _backend = shared_llama_backend()?;
    let (_params, n_gpu_layers, source) = resolve_model_params(gguf, n_ctx);
    Ok(TuneReport {
        gguf: gguf.canonicalize().unwrap_or_else(|_| gguf.to_path_buf()),
        n_ctx,
        n_gpu_layers,
        source: match source {
            GpuLayersSource::Explicit => "explicit",
            GpuLayersSource::AutoFit => "auto-fit",
            GpuLayersSource::Disabled => "disabled",
            GpuLayersSource::FitFailed => "fit-failed",
        },
        margin_mib: fit_margin_bytes() / (1024 * 1024),
    })
}

impl TuneReport {
    /// Renderiza el reporte como TOML. Los comentarios mapean cada valor a
    /// su variable de entorno porque **esa es la vía de consumo**: braze no
    /// lee este archivo, se pega el `export` y el reparto queda fijado.
    #[must_use]
    pub fn to_toml(&self) -> String {
        format!(
            "# Generado por `braze tune` — reparto resuelto por el auto-fit.\n\
             # braze NO lee este archivo: es un registro reproducible del fit.\n\
             # Para fijarlo en un sweep, exportá las variables comentadas.\n\
             \n[local]\n\
             model = \"{}\"\n\
             n_ctx = {}\n\
             n_gpu_layers = {}      # BRAZE_LOCAL_GPU_LAYERS\n\
             vram_margin_mb = {}    # BRAZE_LOCAL_VRAM_MARGIN_MB\n\
             source = \"{}\"\n",
            self.gguf.display(),
            self.n_ctx,
            self.n_gpu_layers,
            self.margin_mib,
            self.source,
        )
    }
}

/// Identidad de un modelo YA cargado. Dos `LocalBackend` que pidan el mismo
/// GGUF con la misma configuración de offload pueden compartir el
/// `LlamaModel`: los pesos son read-only (por eso ya viajaban en `Arc`) y el
/// contexto se sigue creando fresco por generación, así que reusar el modelo
/// no comparte estado de inferencia entre tareas.
///
/// El entorno entra en la clave porque el auto-fit lo consulta: cambiar
/// `BRAZE_LOCAL_GPU_LAYERS` o el margen debe forzar una recarga, no devolver
/// el modelo repartido con la configuración anterior.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelCacheKey {
    path: PathBuf,
    n_ctx: u32,
    env: Vec<Option<String>>,
}

impl ModelCacheKey {
    fn new(gguf: &Path, n_ctx: u32) -> Self {
        const VARS: [&str; 6] = [
            "BRAZE_LOCAL_GPU_LAYERS",
            "BRAZE_LOCAL_AUTOFIT",
            "BRAZE_LOCAL_VRAM_MARGIN_MB",
            "BRAZE_LOCAL_KV_TYPE",
            "BRAZE_LOCAL_KV_OFFLOAD",
            "BRAZE_LOCAL_UBATCH",
        ];
        Self {
            // Canonicalizar para que `~/models/x.gguf` y una ruta relativa al
            // mismo archivo no carguen dos veces.
            path: gguf.canonicalize().unwrap_or_else(|_| gguf.to_path_buf()),
            n_ctx,
            env: VARS.iter().map(|v| std::env::var(v).ok()).collect(),
        }
    }
}

/// Modelo cacheado. Capacidad **1 a propósito**: braze-bench corre un backend
/// a la vez y los modelos de esta escala pesan 6-12GB — mantener dos vivos
/// revienta la RAM/VRAM de Nitro, que es justo el fallo que el auto-fit vino
/// a eliminar.
type CachedModel = (ModelCacheKey, Arc<LlamaModel>, i32, GpuLayersSource);
static MODEL_CACHE: Mutex<Option<CachedModel>> = Mutex::new(None);

/// Carga el GGUF reusando el modelo cacheado si la clave coincide.
///
/// **Por qué existe**: braze-bench crea un `LocalBackend` por tarea, así que
/// un sweep de 57 tareas pagaba 57 veces el probe del auto-fit (que carga el
/// modelo con `no_alloc`) **y** 57 recargas del GGUF entero con su re-subida
/// de capas a la GPU — medido en vivo el 2026-07-25: la VRAM caía a ~177 MiB
/// entre tareas y volvía a ~4.7GB en cada una.
///
/// **Caveat metodológico**: con el caché activo solo la primera tarea de un
/// brazo paga la carga, así que el `wall_time_ms` promedio deja de ser
/// comparable contra sweeps anteriores. Para reproducir números viejos está
/// `BRAZE_LOCAL_MODEL_CACHE=off`.
fn load_model_cached(
    backend: &Arc<LlamaBackend>,
    gguf: &Path,
    n_ctx: u32,
) -> Result<(Arc<LlamaModel>, i32, GpuLayersSource), ModelError> {
    let load_fresh = || -> Result<(Arc<LlamaModel>, i32, GpuLayersSource), ModelError> {
        let (params, gpu_layers, source) = resolve_model_params(gguf, n_ctx);
        let model = LlamaModel::load_from_file(backend, gguf, &params).map_err(|e| {
            ModelError::Request(format!("failed to load GGUF '{}': {e}", gguf.display()))
        })?;
        Ok((Arc::new(model), gpu_layers, source))
    };

    if std::env::var("BRAZE_LOCAL_MODEL_CACHE").as_deref() == Ok("off") {
        return load_fresh();
    }

    let key = ModelCacheKey::new(gguf, n_ctx);
    // El lock se sostiene durante la carga a propósito: si dos hilos piden el
    // mismo modelo a la vez, el segundo espera y reusa en vez de cargar un
    // duplicado de 12GB.
    let mut guard = MODEL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_key, model, gpu_layers, source)) = guard.as_ref()
        && *cached_key == key
    {
        tracing::debug!(path = %gguf.display(), "local: modelo reusado del caché");
        return Ok((Arc::clone(model), *gpu_layers, *source));
    }
    // Soltar el modelo viejo ANTES de cargar el nuevo. Si algún `LocalBackend`
    // vivo todavía tiene su `Arc`, el modelo sobrevive hasta que lo suelte —
    // no se puede liberar lo que está en uso.
    *guard = None;
    let (model, gpu_layers, source) = load_fresh()?;
    *guard = Some((key, Arc::clone(&model), gpu_layers, source));
    Ok((model, gpu_layers, source))
}

impl LocalBackend {
    /// Carga un GGUF desde una ruta directa a `.gguf` o al blob de Ollama.
    /// `model_label` es solo para `name()`/trazas (precio local = $0).
    pub fn from_gguf_path(
        gguf: impl AsRef<Path>,
        model_label: impl Into<String>,
        n_ctx: u32,
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
        let (model, gpu_layers, layers_source) = load_model_cached(&backend, gguf.as_ref(), n_ctx)?;
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
        })
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
    ) -> Result<Self, ModelError> {
        let gguf = resolve_ollama_gguf(ollama_root.as_ref(), model_ref)?;
        Self::from_gguf_path(gguf, model_label, n_ctx)
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

/// Arma el prompt completo en ChatML (qwen family, fase 1): system (+
/// addendum de tools en el prompt) seguido de los turnos, y abre el turno
/// del assistant. Las tool calls van descritas EN EL PROMPT (la escalera
/// de rescate del engine parsea la salida), no en un campo `tools`.
fn build_chatml_prompt(req: &CompletionRequest) -> String {
    let mut out = String::new();

    let mut system = req.system_prompt.clone();
    if !req.tool_stubs.is_empty() {
        let addendum = render_tools_preamble(&req.tool_stubs);
        if system.is_empty() {
            system = addendum;
        } else {
            system.push_str("\n\n");
            system.push_str(&addendum);
        }
    }
    if !system.is_empty() {
        out.push_str("<|im_start|>system\n");
        out.push_str(&system);
        out.push_str("<|im_end|>\n");
    }

    for msg in &req.messages {
        let role = match msg.role {
            // Un ToolResult se representa como turno `user` en ChatML
            // (qwen no tiene rol `tool` en la plantilla base).
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        out.push_str("<|im_start|>");
        out.push_str(role);
        out.push('\n');
        out.push_str(&render_blocks(&msg.content));
        out.push_str("<|im_end|>\n");
    }

    out.push_str("<|im_start|>assistant\n");
    out
}

/// Aplana los bloques de un mensaje a texto para ChatML. Los `ToolUse`
/// se re-emiten como el texto de tool-call que el modelo produjo, y los
/// `ToolResult` como el resultado etiquetado.
fn render_blocks(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => s.push_str(text),
            ContentBlock::ToolUse { name, input, .. } => {
                // Formato de qwen2.5 (saltos de línea incluidos).
                s.push_str(&format!(
                    "<tool_call>\n{{\"name\": \"{name}\", \"arguments\": {input}}}\n</tool_call>"
                ));
            }
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                // qwen espera los resultados dentro de <tool_response>.
                s.push_str("<tool_response>\n");
                if *is_error {
                    s.push_str("[tool error] ");
                }
                s.push_str(content);
                s.push_str("\n</tool_response>");
            }
        }
        s.push('\n');
    }
    s
}

/// Contador de tool calls emitidas por el proceso, para ids sintéticos
/// únicos (mismo esquema nonce+contador que los wires de Ollama/OpenRouter).
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Lo que el hilo de generación necesita saber de la familia del modelo:
/// los ids de marcadores (Harmony) y el inventario de tools con sus
/// schemas para las gramáticas del stencil. Agrupa lo que antes eran
/// parámetros sueltos de `generate_blocking`.
enum FamilyRuntime {
    ChatMl {
        tools: Vec<ToolGrammarSpec>,
    },
    Harmony {
        ids: HarmonyTokenIds,
        tools: Vec<ToolGrammarSpec>,
    },
}

/// Construye el sampler estencilado: gramática GBNF + greedy encadenados
/// (la gramática enmascara logits; greedy elige entre lo permitido). Una
/// gramática inválida es bug nuestro, no del modelo — se loguea y se
/// sigue sin constraint antes que brickear la generación.
fn constrained_sampler(model: &LlamaModel, grammar: &str) -> Option<LlamaSampler> {
    match LlamaSampler::grammar(model, grammar, "root") {
        Ok(g) => Some(LlamaSampler::chain_simple([g, LlamaSampler::greedy()])),
        Err(e) => {
            tracing::warn!(error = %e, "stencil: gramática inválida — generación sin constraint");
            None
        }
    }
}

/// Traduce un [`HarmonyEvent`] del parser a su `CompletionEvent` y lo
/// empuja. Devuelve `false` si el consumidor abandonó (cancelación).
fn emit_harmony_event(
    event: HarmonyEvent,
    tx: &tokio::sync::mpsc::Sender<Result<CompletionEvent, ModelError>>,
) -> bool {
    match event {
        HarmonyEvent::Visible(text) => tx
            .blocking_send(Ok(CompletionEvent::TextDelta(text)))
            .is_ok(),
        HarmonyEvent::ToolCall { name, raw_args } => {
            let (arguments, outcome) = parse_arguments_with_repair(&raw_args);
            if !matches!(outcome, ArgumentsOutcome::Parsed) {
                tracing::warn!(
                    tool = %name,
                    ?outcome,
                    "harmony: argumentos de tool call reparados/colapsados"
                );
            }
            let id = format!(
                "local-tool-call-{}-{}",
                crate::synth_id::process_nonce(),
                TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            tx.blocking_send(Ok(CompletionEvent::ToolCallRequested {
                id,
                name,
                arguments,
            }))
            .is_ok()
        }
    }
}

/// Los knobs numéricos del turno de generación. Agrupados por la misma
/// razón que [`FamilyRuntime`]: `generate_blocking` acumulaba parámetros
/// sueltos. `gpu_layers` viaja acá (y no se relee del entorno) para que el
/// contexto se arme contra el MISMO reparto de capas que midió el auto-fit
/// al cargar el modelo.
#[derive(Debug, Clone, Copy)]
struct GenParams {
    n_ctx: u32,
    max_tokens: u32,
    gpu_layers: i32,
}

/// Genera de forma bloqueante y empuja eventos por `tx`. Corre en un hilo
/// de `spawn_blocking`. Convención de error: cualquier fallo se manda
/// como `Err` por el canal (el stream lo propaga como `StreamError`).
///
/// Con `harmony: Some(_)` la salida se interpreta como mensajes Harmony:
/// los tokens especiales se matchean por id (nunca se renderizan), el
/// canal `final` fluye como `TextDelta`, `analysis` se traza y suprime, y
/// `<|call|>`/`<|return|>` cierran el turno con su `stop_reason` honesto
/// (`tool_use`/`stop`; presupuesto agotado = `length`).
fn generate_blocking(
    backend: &LlamaBackend,
    model: &LlamaModel,
    prompt: &str,
    gen_params: GenParams,
    family: &FamilyRuntime,
    tx: &tokio::sync::mpsc::Sender<Result<CompletionEvent, ModelError>>,
) {
    let GenParams {
        n_ctx,
        max_tokens,
        gpu_layers,
    } = gen_params;
    let (harmony, tools) = match family {
        FamilyRuntime::Harmony { ids, tools } => (Some(ids), tools.as_slice()),
        FamilyRuntime::ChatMl { tools } => (None, tools.as_slice()),
    };
    macro_rules! bail {
        ($($arg:tt)*) => {{
            let _ = tx.blocking_send(Err(ModelError::StreamError(format!($($arg)*))));
            return;
        }};
    }

    // KV cache cuantizado (`BRAZE_LOCAL_KV_TYPE=q8_0|q4_0|q5_0|q5_1|q4_1`, idea
    // #2 de `docs/inference-runtimes-audit-2026-07-25.md`): baja el footprint
    // del KV (RAM host, o VRAM con `BRAZE_LOCAL_KV_OFFLOAD=gpu` → más capas).
    // Default `f16` — palanca opt-in que gana su default por bench. **Verificado
    // en vivo 2026-07-25**: el KV cuantizado requiere flash-attn, que
    // gpt-oss/Harmony NO soportan (attention sinks) → `new_context` devuelve
    // null; qwen2.5:3b sí funciona. Por eso degradamos con gracia a f16 abajo si
    // falla, en vez de crashear (filosofía degrade-not-crash del proyecto).
    let requested_kv = std::env::var("BRAZE_LOCAL_KV_TYPE").ok().and_then(|kv| {
        parse_kv_cache_type(&kv).or_else(|| {
            tracing::warn!(kv_type = %kv, "BRAZE_LOCAL_KV_TYPE desconocido; se ignora (f16)");
            None
        })
    });
    // `gpu_layers` viene resuelto de la carga (auto-fit o env explícito), NO
    // del entorno: el contexto tiene que armarse contra el mismo reparto de
    // capas que se midió al cargar el modelo.
    let ctx_params = build_ctx_params(n_ctx, gpu_layers, requested_kv);
    if kv_on_host(gpu_layers) {
        tracing::info!(
            gpu_layers,
            ubatch = ubatch_setting(),
            "local: KV en host + micro-batch chico para mantener la VRAM plana"
        );
    }
    if requested_kv.is_some() {
        tracing::info!("local: KV cache cuantizado solicitado");
    }
    let n_ctx = std::num::NonZeroU32::new(n_ctx.max(256));
    let mut ctx = match model.new_context(backend, ctx_params) {
        Ok(c) => c,
        Err(e) if requested_kv.is_some() => {
            tracing::warn!(
                error = %e,
                "local: crear contexto con KV cuantizado falló; reintentando con f16 \
                 (el KV cuantizado requiere flash-attn, no soportado por gpt-oss/Harmony)"
            );
            let fb = build_ctx_params(
                n_ctx.map_or(256, std::num::NonZeroU32::get),
                gpu_layers,
                None,
            );
            match model.new_context(backend, fb) {
                Ok(c) => c,
                Err(e2) => bail!("local: no se pudo crear el contexto (ni con f16): {e2}"),
            }
        }
        Err(e) => bail!("local: no se pudo crear el contexto: {e}"),
    };

    // Harmony no lleva BOS: la conversación arranca directo en
    // `<|start|>system` (los GGUF de gpt-oss no definen add_bos).
    let add_bos = if harmony.is_some() {
        AddBos::Never
    } else {
        AddBos::Always
    };
    let tokens = match model.str_to_token(prompt, add_bos) {
        Ok(t) => t,
        Err(e) => bail!("local: tokenización falló: {e}"),
    };
    let input_tokens = tokens.len() as u32;

    // Guard explícito: un prompt que no cabe en el contexto debe ser un
    // error legible del backend, no un assert C++ que mata el proceso.
    let ctx_limit = n_ctx.map_or(256, std::num::NonZeroU32::get) as usize;
    if tokens.len() >= ctx_limit {
        bail!(
            "local: el prompt ({} tokens) no cabe en n_ctx ({ctx_limit}) — \
             la compactación del engine debería haber actuado antes",
            tokens.len()
        );
    }

    // El prompt se decodifica en chunks de n_batch: llama.cpp aborta el
    // proceso entero (GGML_ASSERT n_tokens_all <= n_batch) si un decode
    // excede el batch. Latente desde Fase 1 — los smokes usan prompts
    // cortos; lo expuso una tarea multi-ronda del sweep A/B del stencil
    // cuyo prompt de ronda superó los 2048 tokens (2026-07-21).
    const N_BATCH: usize = 2048; // default de llama_context_default_params
    let mut batch = LlamaBatch::new(N_BATCH, 1);
    let total = tokens.len();
    let mut fed = 0usize;
    while fed < total {
        batch.clear();
        let end = (fed + N_BATCH).min(total);
        for (i, tok) in tokens[fed..end].iter().enumerate() {
            let pos = fed + i;
            // Solo el último token del prompt pide logits.
            if let Err(e) = batch.add(*tok, pos as i32, &[0], pos + 1 == total) {
                bail!("local: batch.add falló: {e}");
            }
        }
        if let Err(e) = ctx.decode(&mut batch) {
            bail!("local: decode del prompt falló: {e}");
        }
        fed = end;
    }

    let mut sampler = LlamaSampler::greedy();
    // Posición del próximo token en el KV cache: el total del prompt
    // (no `batch.n_tokens()`, que tras el decode en chunks es solo el
    // tamaño del último chunk).
    let mut n_cur = total as i32;
    let mut output_tokens = 0u32;
    let budget = max_tokens.max(1);
    // Decoder UTF-8 persistente: un carácter multi-byte puede repartirse
    // entre tokens, y un decoder fresco por token lo rompería.
    let mut decoder = encoding_rs::UTF_8.new_decoder();

    let mut parser = HarmonyParser::new();
    // Presupuesto agotado sin cierre = "length" (mismo diagnóstico que
    // los wires: una tool call cortada por max_tokens no debe parecer un
    // stop limpio).
    let mut stop_reason = "length";

    // Stencil (Fase 3): constrained decoding GBNF con laziness manual —
    // el sampler se swapea a gramática+greedy exactamente cuando empieza
    // una tool call y vuelve a libre cuando el envelope se completa.
    // Kill-switch `BRAZE_LOCAL_GRAMMAR=off` (el brazo de ablación del
    // A/B; misma convención que BRAZE_CIRCUIT_BREAKER).
    let grammar_enabled = !matches!(
        std::env::var("BRAZE_LOCAL_GRAMMAR").as_deref(),
        Ok("off") | Ok("0")
    );
    // Precomputada por turno: el envelope qwen depende del inventario de
    // tools. Caveat compartido con la escalera de rescate: un
    // `<tool_call>` literal citado en texto libre (p.ej. dentro de un
    // fence) también gatilla — mismo trade-off, y el kill-switch cubre.
    let qwen_grammar = if harmony.is_none() && grammar_enabled {
        qwen_call_grammar(tools)
    } else {
        None
    };
    let mut constrained = false;
    let mut args_cursor = JsonCursor::new();
    let mut tail = String::new();

    // `n_cur` es la posición en el KV-cache, no un mero contador de
    // iteraciones (arranca en `batch.n_tokens()` y sólo avanza en tokens
    // que continúan la generación) — de ahí el allow.
    #[allow(clippy::explicit_counter_loop)]
    for _ in 0..budget {
        // OJO: `sample()` ya hace el accept internamente
        // (`llama_sampler_sample` → `llama_sampler_accept`). Un accept
        // explícito acá sería double-accept: inofensivo con greedy
        // (stateless), fatal con gramática (avanza el stack GBNF dos
        // veces → GGML_ASSERT(!stacks.empty()) — depurado en vivo,
        // 2026-07-21).
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        let marker = harmony.and_then(|ids| ids.marker_of(token));
        if let Some(m @ (HarmonyMarker::Call | HarmonyMarker::Return)) = marker {
            // Cierre del turno harmony (ambos son además EOG en el GGUF
            // de gpt-oss; el match por id decide el stop_reason honesto).
            output_tokens += 1;
            stop_reason = "stop";
            if let Some(event) = parser.feed_marker(m) {
                let is_call = matches!(event, HarmonyEvent::ToolCall { .. });
                if !emit_harmony_event(event, tx) {
                    return; // el consumidor abandonó (cancelación)
                }
                if is_call {
                    stop_reason = "tool_use";
                }
            }
            break;
        }
        if marker.is_none() && model.is_eog_token(token) {
            stop_reason = "stop";
            break;
        }
        output_tokens += 1;
        if let Some(m) = marker {
            // Marcador estructural intra-turno (<|channel|>, <|message|>,
            // <|end|>…): nunca se renderiza; puede cerrar una tool call
            // off-spec (lenidad de <|end|>) y la generación continúa —
            // eso habilita turnos multi-call.
            if let Some(event) = parser.feed_marker(m) {
                if matches!(event, HarmonyEvent::ToolCall { .. }) {
                    stop_reason = "tool_use";
                }
                if !emit_harmony_event(event, tx) {
                    return;
                }
            }
            if grammar_enabled {
                match m {
                    // El header fijó destinatario: lo que viene son los
                    // args — estencilarlos con la gramática derivada del
                    // schema de ESA tool (fallback: objeto JSON genérico).
                    HarmonyMarker::Message if parser.tool_call_in_progress() && !constrained => {
                        let tool = parser.pending_tool_name().unwrap_or_default();
                        let grammar = harmony_args_grammar(tool, tools);
                        if let Some(s) = constrained_sampler(model, &grammar) {
                            sampler = s;
                            constrained = true;
                            args_cursor = JsonCursor::new();
                            tracing::info!(tool, "stencil: constraint de args harmony activado");
                        }
                    }
                    // Cierre de mensaje con el constraint aún puesto
                    // (cierre off-spec): liberar antes de seguir.
                    HarmonyMarker::End | HarmonyMarker::Start if constrained => {
                        sampler = LlamaSampler::greedy();
                        constrained = false;
                    }
                    _ => {}
                }
            }
        } else {
            // `special = false`: no renderizar tokens especiales de
            // plantilla (p.ej. `<|im_end|>`) como texto — no deben
            // filtrarse a la salida.
            match model.token_to_piece(token, &mut decoder, false, None) {
                Ok(piece) => {
                    if piece.is_empty() {
                        // token especial no-EOG o fragmento UTF-8
                        // pendiente: sigue generando, no emite nada.
                    } else if harmony.is_some() {
                        if let Some(event) = parser.feed_text(&piece)
                            && !emit_harmony_event(event, tx)
                        {
                            return;
                        }
                        // Los args estencilados avanzan el cursor; al
                        // cerrar el objeto raíz se libera el sampler y
                        // el modelo emite su <|call|> libremente.
                        if constrained {
                            args_cursor.feed(&piece);
                            if args_cursor.complete() {
                                sampler = LlamaSampler::greedy();
                                constrained = false;
                                tracing::info!(
                                    "stencil: args JSON completos — constraint liberado"
                                );
                            }
                        }
                    } else {
                        if tx
                            .blocking_send(Ok(CompletionEvent::TextDelta(piece.clone())))
                            .is_err()
                        {
                            return; // el consumidor abandonó (cancelación)
                        }
                        if qwen_grammar.is_some() {
                            tail.push_str(&piece);
                            let excess = tail.len().saturating_sub(64);
                            if excess > 0 {
                                let cut = (excess..tail.len())
                                    .find(|i| tail.is_char_boundary(*i))
                                    .unwrap_or(0);
                                tail.drain(..cut);
                            }
                            if !constrained && tail.ends_with("<tool_call>") {
                                if let Some(s) = constrained_sampler(
                                    model,
                                    qwen_grammar.as_deref().unwrap_or_default(),
                                ) {
                                    sampler = s;
                                    constrained = true;
                                    tracing::info!("stencil: envelope qwen activado");
                                }
                            } else if constrained && tail.ends_with("</tool_call>") {
                                sampler = LlamaSampler::greedy();
                                constrained = false;
                                tracing::info!("stencil: envelope cerrado — constraint liberado");
                            }
                        }
                    }
                }
                Err(e) => {
                    // Un token de control no renderizable (p.ej. un
                    // `<|im_start|>` espurio: el modelo intentando abrir
                    // otro turno) terminaba el stream con error duro — 3
                    // fallos del brazo OFF del sweep A/B del stencil
                    // (2026-07-21). Es fin-de-turno de facto, no un error
                    // del backend: los stacks de chat suelen listar
                    // `<|im_start|>` como stop string. Cerrar limpio.
                    tracing::debug!(error = %e, "local: token no renderizable — fin de turno");
                    stop_reason = "stop";
                    break;
                }
            }
        }
        batch.clear();
        if let Err(e) = batch.add(token, n_cur, &[0], true) {
            bail!("local: batch.add (gen) falló: {e}");
        }
        n_cur += 1;
        if let Err(e) = ctx.decode(&mut batch) {
            bail!("local: decode (gen) falló: {e}");
        }
    }

    if stop_reason == "length" && parser.tool_call_in_progress() {
        tracing::warn!(
            "harmony: presupuesto de tokens agotado a mitad de una tool call — \
             la call se descarta (subir max_tokens)"
        );
    }

    let _ = tx.blocking_send(Ok(CompletionEvent::Usage {
        input_tokens,
        output_tokens,
        stop_reason: Some(stop_reason.to_string()),
        cache_read_tokens: None,
        cache_write_tokens: None,
        escalation_trigger: None,
    }));
    let _ = tx.blocking_send(Ok(CompletionEvent::Done));
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
            };
            generate_blocking(&backend, &model, &prompt, gen_params, &family_rt, &tx);
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_cache_type_parses_known_quant_types() {
        assert_eq!(parse_kv_cache_type("q8_0"), Some(KvCacheType::Q8_0));
        assert_eq!(parse_kv_cache_type("Q4_0"), Some(KvCacheType::Q4_0));
        assert_eq!(parse_kv_cache_type(" f16 "), Some(KvCacheType::F16));
        assert_eq!(parse_kv_cache_type("q5_1"), Some(KvCacheType::Q5_1));
    }

    #[test]
    fn kv_cache_type_unknown_falls_back_to_none() {
        assert_eq!(parse_kv_cache_type("q3_k"), None);
        assert_eq!(parse_kv_cache_type("nonsense"), None);
        assert_eq!(parse_kv_cache_type(""), None);
    }

    #[test]
    fn el_contexto_respeta_el_piso_de_256_tokens() {
        let chico = build_ctx_params(64, 0, None);
        assert_eq!(chico.n_ctx().map(std::num::NonZeroU32::get), Some(256));
        let normal = build_ctx_params(4096, 0, None);
        assert_eq!(normal.n_ctx().map(std::num::NonZeroU32::get), Some(4096));
    }

    #[test]
    fn sin_capas_en_gpu_el_kv_se_queda_donde_llama_cpp_lo_pone() {
        // 0 capas = CPU puro: no hay VRAM que cuidar, así que la palanca de
        // KV-en-host no se activa pase lo que pase con el entorno.
        assert!(!kv_on_host(0));
        assert!(build_ctx_params(2048, 0, None).offload_kqv());
    }

    #[test]
    fn la_clave_del_cache_distingue_modelo_y_contexto() {
        // Reusar un modelo cargado solo es correcto si la clave captura todo
        // lo que cambiaría su carga. Ruta y n_ctx son los dos ejes obvios; el
        // entorno entra en `ModelCacheKey::new` y no se testea acá porque
        // mutarlo es `unsafe` en la edición 2024 y contaminaría otros tests.
        let a = ModelCacheKey::new(Path::new("/models/x.gguf"), 8192);
        assert_eq!(a, ModelCacheKey::new(Path::new("/models/x.gguf"), 8192));
        assert_ne!(a, ModelCacheKey::new(Path::new("/models/x.gguf"), 4096));
        assert_ne!(a, ModelCacheKey::new(Path::new("/models/y.gguf"), 8192));
    }

    #[test]
    fn el_kv_en_host_se_activa_exactamente_cuando_hay_offload_a_gpu() {
        // La invariante que sostiene la palanca #1: el probe del auto-fit y
        // la creación real del contexto consultan la MISMA decisión sobre
        // dónde vive el KV. Si divergen, el fit reparte capas contra un
        // consumo de VRAM que no es el real y volvemos a los OOM.
        // `-1` (todas las capas, convención de llama.cpp) cuenta como
        // offload: el `> 0` que había antes lo dejaba pasar como CPU.
        for layers in [0, 1, 24, -1] {
            let params = build_ctx_params(4096, layers, None);
            assert_eq!(params.offload_kqv(), !kv_on_host(layers), "layers={layers}");
            assert!(params.n_batch() >= params.n_ubatch(), "layers={layers}");
        }
    }
}
