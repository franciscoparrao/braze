//! Plantilla **ChatML** (familia qwen, Fase 1) para el `LocalBackend` —
//! extraída VERBATIM de `local/family.rs` (2026-08-15) al patrón de
//! módulo puro de `harmony.rs`/`gemma.rs`: sin dependencia de llama.cpp,
//! compilada también sin el feature `local`, para que sus tests (y los
//! fixture tests contra el render de referencia, ver
//! `chat_template_fixtures.rs`) corran en el `cargo test` normal del
//! workspace. La lección es la de los 4 tests de `ModelCacheKey` rotos
//! en silencio (2026-08-08): lo que solo compila bajo `--features local`
//! no lo corre ni CI ni el flujo local.

use braze_types::{ContentBlock, Role};

use crate::backend::CompletionRequest;
use crate::gemma::render_tools_preamble;

/// Arma el prompt completo en ChatML (qwen family, fase 1): system (+
/// addendum de tools en el prompt) seguido de los turnos, y abre el turno
/// del assistant. Las tool calls van descritas EN EL PROMPT (la escalera
/// de rescate del engine parsea la salida), no en un campo `tools`.
pub(crate) fn build_chatml_prompt(req: &CompletionRequest) -> String {
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
pub(crate) fn render_blocks(blocks: &[ContentBlock]) -> String {
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
