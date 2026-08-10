//! Familias de plantilla de chat (ChatML/qwen, Harmony/gpt-oss,
//! Gemma): detección por arch GGUF/label/env, construcción del prompt
//! ChatML y el runtime por familia que el loop de decode consume. Las
//! plantillas Harmony y Gemma viven en sus módulos (`harmony.rs`,
//! `gemma.rs`); aquí se orquestan. L-4: extraído VERBATIM de `local.rs`.

use super::*;

/// Familia de plantilla de chat del modelo cargado. Decide qué prompt se
/// arma y cómo se interpreta la salida (texto plano + rescate del engine
/// para ChatML; parser de canales en el backend para Harmony).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatFamily {
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
pub(super) fn detect_family(model: &LlamaModel, label: &str) -> ChatFamily {
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
pub(super) struct HarmonyTokenIds {
    pairs: Vec<(LlamaToken, HarmonyMarker)>,
}

impl HarmonyTokenIds {
    pub(super) fn resolve(model: &LlamaModel) -> Result<Self, ModelError> {
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

    pub(super) fn marker_of(&self, token: LlamaToken) -> Option<HarmonyMarker> {
        self.pairs
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, m)| *m)
    }
}

/// Arma el prompt completo en ChatML (qwen family, fase 1): system (+
/// addendum de tools en el prompt) seguido de los turnos, y abre el turno
/// del assistant. Las tool calls van descritas EN EL PROMPT (la escalera
/// de rescate del engine parsea la salida), no en un campo `tools`.
pub(super) fn build_chatml_prompt(req: &CompletionRequest) -> String {
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
pub(super) fn render_blocks(blocks: &[ContentBlock]) -> String {
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
pub(super) static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Lo que el hilo de generación necesita saber de la familia del modelo:
/// los ids de marcadores (Harmony) y el inventario de tools con sus
/// schemas para las gramáticas del stencil. Agrupa lo que antes eran
/// parámetros sueltos de `generate_blocking`.
pub(super) enum FamilyRuntime {
    ChatMl {
        tools: Vec<ToolGrammarSpec>,
    },
    Harmony {
        ids: HarmonyTokenIds,
        tools: Vec<ToolGrammarSpec>,
    },
}
