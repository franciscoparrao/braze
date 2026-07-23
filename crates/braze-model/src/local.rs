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
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;

use crate::args_repair::{parse_arguments_with_repair, ArgumentsOutcome};
use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;
use crate::gemma::{build_gemma_prompt, render_tools_preamble};
use crate::harmony::{build_harmony_prompt, utc_date_string, HarmonyEvent, HarmonyMarker, HarmonyParser};
use crate::stencil::{harmony_args_grammar, qwen_call_grammar, JsonCursor, ToolGrammarSpec};

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
            tracing::warn!(family = other, "BRAZE_LOCAL_FAMILY desconocida; autodetectando");
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
        // n_gpu_layers: 0 = CPU puro (default). Con el binario compilado
        // con el feature `cuda` y una GPU disponible,
        // `BRAZE_LOCAL_GPU_LAYERS=N` offloada N capas a la GPU (un valor
        // grande como 999 = todas las que quepan). Sin CUDA el valor se
        // ignora silenciosamente (llama.cpp corre en CPU igual).
        let gpu_layers = std::env::var("BRAZE_LOCAL_GPU_LAYERS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        let params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
        let model = LlamaModel::load_from_file(&backend, gguf.as_ref(), &params).map_err(|e| {
            ModelError::Request(format!(
                "failed to load GGUF '{}': {e}",
                gguf.as_ref().display()
            ))
        })?;
        let model_label = model_label.into();
        let family = detect_family(&model, &model_label);
        let harmony = match family {
            ChatFamily::Harmony => Some(HarmonyTokenIds::resolve(&model)?),
            ChatFamily::ChatMl | ChatFamily::Gemma => None,
        };
        tracing::info!(model = %model_label, ?family, "local backend loaded");
        Ok(Self {
            backend,
            model: Arc::new(model),
            model_label,
            n_ctx,
            family,
            harmony,
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
            ModelError::Request(format!("el manifest de '{model_ref}' no tiene capa de modelo"))
        })?;
    // Ollama nombra los blobs con `-` en vez de `:`.
    let blob = root
        .join("models/blobs")
        .join(digest.replace(':', "-"));
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
    n_ctx: u32,
    max_tokens: u32,
    family: &FamilyRuntime,
    tx: &tokio::sync::mpsc::Sender<Result<CompletionEvent, ModelError>>,
) {
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

    let n_batch_max = n_ctx.max(256);
    let n_ctx = std::num::NonZeroU32::new(n_batch_max);
    let mut ctx_params = LlamaContextParams::default().with_n_ctx(n_ctx);
    // Offload parcial a GPU: mantener el KV cache en el HOST (RAM), no en la
    // VRAM. Sin esto, el KV de las capas offloadeadas crece con el contexto y
    // revienta la VRAM a mitad de una sesión agéntica (OOM en la 2ª ronda,
    // observado con gpt-oss:20b en la RTX 3050 de 6GB). Ollama hace justo
    // esto —VRAM plana ~4,7GB a cualquier num_ctx— por eso corre el mismo
    // modelo en la misma máquina sin crashear. Kill-switch
    // `BRAZE_LOCAL_KV_OFFLOAD=gpu` restaura el default de llama.cpp (KV en
    // VRAM) para quien tenga VRAM de sobra y quiera el KV en GPU.
    let gpu_layers = std::env::var("BRAZE_LOCAL_GPU_LAYERS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if gpu_layers > 0 && std::env::var("BRAZE_LOCAL_KV_OFFLOAD").as_deref() != Ok("gpu") {
        ctx_params = ctx_params.with_offload_kqv(false);
        // Compute buffer chico. El buffer de prompt-processing lo dimensiona
        // `n_ubatch` (el batch FÍSICO) × contexto, y vive en VRAM; con el
        // default (n_ubatch=512) crece hasta reventar los 6GB cuando el
        // contexto se llena (aborta a mitad de una sesión agéntica, aun con el
        // KV ya en host). Ollama usa micro-batches chicos —por eso su VRAM
        // queda plana a cualquier num_ctx— y lo replicamos. `n_batch` (el batch
        // LÓGICO) queda en `n_ctx`: braze decodifica el prompt entero de una,
        // así que debe cubrirlo (si no, `GGML_ASSERT(n_tokens_all <= n_batch)`).
        // `BRAZE_LOCAL_UBATCH` ajusta el físico.
        let ubatch = std::env::var("BRAZE_LOCAL_UBATCH")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&u| u > 0)
            .unwrap_or(128);
        ctx_params = ctx_params
            .with_n_ubatch(ubatch)
            .with_n_batch(n_batch_max.max(ubatch));
        tracing::info!(
            gpu_layers,
            ubatch,
            "local: KV en host + micro-batch chico para mantener la VRAM plana"
        );
    }
    let mut ctx = match model.new_context(backend, ctx_params) {
        Ok(c) => c,
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
                            tracing::info!(
                                tool,
                                "stencil: constraint de args harmony activado"
                            );
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
                                tracing::info!(
                                    "stencil: envelope cerrado — constraint liberado"
                                );
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
                let reasoning = std::env::var("BRAZE_LOCAL_REASONING")
                    .unwrap_or_else(|_| "medium".to_string());
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
            generate_blocking(&backend, &model, &prompt, n_ctx, max_tokens, &family_rt, &tx);
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }
}
