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
//! Fase 1 (este archivo): CPU, plantilla ChatML, streaming, `Usage` $0.
//! GPU/CUDA y gpt-oss/harmony son fase 2.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use braze_types::{ContentBlock, Role};
use futures::Stream;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use braze_types::ToolStub;

use crate::backend::{CompletionEvent, CompletionRequest, ModelBackend};
use crate::error::ModelError;

/// Addendum de tools para el prompt del backend local. Instruye el
/// formato `<tool_call>{"name":…,"arguments":…}</tool_call>` — el que la
/// escalera de rescate del engine (`extract_tagged_tool_calls` +
/// `parse_tool_call_json`) captura. Deliberadamente NO es el envelope
/// `{"action":…}` de `render_prompt_tools_addendum` (ese sólo lo parsea
/// el engine en modo constrained-Ollama; el local pasa por la escalera).
fn render_local_tools_prompt(stubs: &[ToolStub]) -> String {
    let mut s = String::from(
        "You have access to the following tools. To call one, emit a line \
         with EXACTLY this shape and nothing else:\n\
         <tool_call>{\"name\": \"<tool>\", \"arguments\": {<args>}}</tool_call>\n\n\
         Available tools:\n",
    );
    for stub in stubs {
        s.push_str("- ");
        s.push_str(&stub.name);
        s.push_str(": ");
        s.push_str(&stub.summary);
        s.push('\n');
    }
    s.push_str(
        "\nCall a tool only when you need it. When you have the final answer, \
         reply in plain text without any tool_call.",
    );
    s
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
}

impl LocalBackend {
    /// Carga un GGUF desde una ruta directa a `.gguf` o al blob de Ollama.
    /// `model_label` es solo para `name()`/trazas (precio local = $0).
    pub fn from_gguf_path(
        gguf: impl AsRef<Path>,
        model_label: impl Into<String>,
        n_ctx: u32,
    ) -> Result<Self, ModelError> {
        let backend = LlamaBackend::init()
            .map_err(|e| ModelError::Request(format!("llama backend init failed: {e}")))?;
        let params = LlamaModelParams::default(); // n_gpu_layers = 0 (CPU, fase 1)
        let model = LlamaModel::load_from_file(&backend, gguf.as_ref(), &params).map_err(|e| {
            ModelError::Request(format!(
                "failed to load GGUF '{}': {e}",
                gguf.as_ref().display()
            ))
        })?;
        Ok(Self {
            backend: Arc::new(backend),
            model: Arc::new(model),
            model_label: model_label.into(),
            n_ctx,
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
        let addendum = render_local_tools_prompt(&req.tool_stubs);
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
                s.push_str(&format!(
                    "<tool_call>{{\"name\": \"{name}\", \"arguments\": {input}}}</tool_call>"
                ));
            }
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                if *is_error {
                    s.push_str("[tool error] ");
                }
                s.push_str(content);
            }
        }
        s.push('\n');
    }
    s
}

/// Genera de forma bloqueante y empuja eventos por `tx`. Corre en un hilo
/// de `spawn_blocking`. Convención de error: cualquier fallo se manda
/// como `Err` por el canal (el stream lo propaga como `StreamError`).
fn generate_blocking(
    backend: &LlamaBackend,
    model: &LlamaModel,
    prompt: &str,
    n_ctx: u32,
    max_tokens: u32,
    tx: &tokio::sync::mpsc::Sender<Result<CompletionEvent, ModelError>>,
) {
    macro_rules! bail {
        ($($arg:tt)*) => {{
            let _ = tx.blocking_send(Err(ModelError::StreamError(format!($($arg)*))));
            return;
        }};
    }

    let n_ctx = std::num::NonZeroU32::new(n_ctx.max(256));
    let ctx_params = LlamaContextParams::default().with_n_ctx(n_ctx);
    let mut ctx = match model.new_context(backend, ctx_params) {
        Ok(c) => c,
        Err(e) => bail!("local: no se pudo crear el contexto: {e}"),
    };

    let tokens = match model.str_to_token(prompt, AddBos::Always) {
        Ok(t) => t,
        Err(e) => bail!("local: tokenización falló: {e}"),
    };
    let input_tokens = tokens.len() as u32;

    let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
    let last = tokens.len() as i32 - 1;
    for (i, tok) in tokens.into_iter().enumerate() {
        if let Err(e) = batch.add(tok, i as i32, &[0], i as i32 == last) {
            bail!("local: batch.add falló: {e}");
        }
    }
    if let Err(e) = ctx.decode(&mut batch) {
        bail!("local: decode del prompt falló: {e}");
    }

    let mut sampler = LlamaSampler::greedy();
    let mut n_cur = batch.n_tokens();
    let mut output_tokens = 0u32;
    let budget = max_tokens.max(1);
    // Decoder UTF-8 persistente: un carácter multi-byte puede repartirse
    // entre tokens, y un decoder fresco por token lo rompería.
    let mut decoder = encoding_rs::UTF_8.new_decoder();

    // `n_cur` es la posición en el KV-cache, no un mero contador de
    // iteraciones (arranca en `batch.n_tokens()` y sólo avanza en tokens
    // que continúan la generación) — de ahí el allow.
    #[allow(clippy::explicit_counter_loop)]
    for _ in 0..budget {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        // `special = false`: no renderizar tokens especiales de plantilla
        // (p.ej. `<|im_end|>`) como texto — no deben filtrarse a la salida.
        match model.token_to_piece(token, &mut decoder, false, None) {
            Ok(piece) => {
                output_tokens += 1;
                if piece.is_empty() {
                    // token especial no-EOG o fragmento UTF-8 pendiente:
                    // sigue generando, no emite nada.
                } else if tx
                    .blocking_send(Ok(CompletionEvent::TextDelta(piece)))
                    .is_err()
                {
                    return; // el consumidor abandonó (cancelación)
                }
            }
            Err(e) => bail!("local: token_to_piece falló: {e}"),
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

    let _ = tx.blocking_send(Ok(CompletionEvent::Usage {
        input_tokens,
        output_tokens,
        stop_reason: Some("stop".to_string()),
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
        let prompt = build_chatml_prompt(&req);
        tracing::info!(
            model = %self.model_label,
            n_ctx = self.n_ctx,
            prompt_chars = prompt.len(),
            "starting local (llama.cpp) completion turn"
        );

        let backend = Arc::clone(&self.backend);
        let model = Arc::clone(&self.model);
        let n_ctx = self.n_ctx;
        let max_tokens = req.max_tokens;

        // Canal acotado: la generación bloqueante empuja, el stream async
        // consume. Si el consumidor abandona, `blocking_send` falla y la
        // generación corta (cancelación cooperativa).
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<CompletionEvent, ModelError>>(32);
        tokio::task::spawn_blocking(move || {
            generate_blocking(&backend, &model, &prompt, n_ctx, max_tokens, &tx);
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }
}
