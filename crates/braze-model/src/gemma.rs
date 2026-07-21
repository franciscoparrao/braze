//! Plantilla **Gemma** (familia `<start_of_turn>`) para el
//! `LocalBackend` — tercera familia de chat, junto a ChatML (qwen) y
//! Harmony (gpt-oss). Cubre gemma2/gemma3/gemma4 — el blob gemma4 de
//! Ollama declara `general.architecture = "gemma4"` y la llama.cpp
//! vendorizada lo soporta, así que acá SÍ se reusa el GGUF de Ollama
//! (a diferencia de gpt-oss).
//!
//! Módulo puro, compilado también sin el feature `local` (mismo patrón
//! que `harmony.rs`/`stencil.rs`): sus tests corren en el `cargo test`
//! normal del workspace.
//!
//! Decisiones de plantilla:
//! - Gemma **no tiene rol system**: las instrucciones (system prompt de
//!   braze + preámbulo de tools) se pliegan al primer turno `user`, la
//!   convención de los templates oficiales.
//! - Gemma tampoco tiene un formato de tool-calling entrenado con la
//!   fuerza del de qwen/gpt-oss — como somos dueños de la plantilla,
//!   instruimos la convención textual `<tool_call>{json}</tool_call>`
//!   que la escalera de rescate del engine y el stencil (trigger de
//!   tail + envelope schema-derivado) ya hablan. Si un A/B muestra que
//!   el formato del refresh de gemma4 rinde mejor, cambiar el preámbulo
//!   es una edición local a este módulo.
//! - El BOS lo agrega el tokenizer (`AddBos::Always` respeta la
//!   metadata del GGUF); `<end_of_turn>` está marcado EOG en el vocab y
//!   el loop de generación lo corta vía `is_eog_token`.

use braze_types::{ContentBlock, Role, ToolStub};

use crate::backend::CompletionRequest;

/// Addendum de tools compartido por las familias **textuales** (ChatML/
/// qwen y Gemma): sección `# Tools` con las firmas en `<tools></tools>`
/// y salida instruida en `<tool_call>{json}</tool_call>`. Es el formato
/// nativo de qwen2.5 reusado como convención instruida para Gemma — la
/// escalera de rescate lo parsea y el stencil lo estencila igual para
/// ambas. Con `input_schema` resuelto se incluye como `parameters`
/// (la ausencia del schema era el "format tax" de la Fase 1).
pub(crate) fn render_tools_preamble(stubs: &[ToolStub]) -> String {
    let mut s = String::from(
        "\n\n# Tools\n\n\
         You may call one or more functions to assist with the user query.\n\n\
         You are provided with function signatures within <tools></tools> XML tags:\n\
         <tools>\n",
    );
    for stub in stubs {
        let mut function = serde_json::json!({
            "name": stub.name,
            "description": stub.summary,
        });
        if let Some(schema) = &stub.input_schema {
            function["parameters"] = schema.clone();
        }
        let sig = serde_json::json!({ "type": "function", "function": function });
        s.push_str(&sig.to_string());
        s.push('\n');
    }
    s.push_str(
        "</tools>\n\n\
         For each function call, return a json object with function name and \
         arguments within <tool_call></tool_call> XML tags:\n\
         <tool_call>\n\
         {\"name\": <function-name>, \"arguments\": <args-json-object>}\n\
         </tool_call>",
    );
    s
}

/// Arma el prompt completo en la plantilla Gemma: system + tools
/// plegados al primer turno `user`, historial mapeado (ToolUse re-emitido
/// como `<tool_call>` en el turno `model`, ToolResult como
/// `<tool_response>` en turno `user`), y el turno `model` abierto.
pub(crate) fn build_gemma_prompt(req: &CompletionRequest) -> String {
    let mut preamble = req.system_prompt.clone();
    if !req.tool_stubs.is_empty() {
        let addendum = render_tools_preamble(&req.tool_stubs);
        if preamble.is_empty() {
            // El addendum arranca con "\n\n" para concatenar tras un
            // system prompt; solo, se recorta.
            preamble = addendum.trim_start().to_string();
        } else {
            preamble.push_str(&addendum);
        }
    }

    let mut out = String::new();
    let mut preamble_pending = !preamble.is_empty();

    for msg in &req.messages {
        let role = match msg.role {
            // Gemma no tiene system: un system mid-historial degrada a
            // turno user (mismo criterio que el plegado inicial).
            Role::User | Role::System => "user",
            Role::Assistant => "model",
        };
        out.push_str("<start_of_turn>");
        out.push_str(role);
        out.push('\n');
        if preamble_pending && role == "user" {
            out.push_str(&preamble);
            out.push_str("\n\n");
            preamble_pending = false;
        }
        out.push_str(&render_blocks(&msg.content));
        out.push_str("<end_of_turn>\n");
    }

    // Sin ningún turno user en el historial (borde raro), el preámbulo
    // igual tiene que llegar al modelo.
    if preamble_pending {
        out.push_str("<start_of_turn>user\n");
        out.push_str(&preamble);
        out.push_str("<end_of_turn>\n");
    }

    out.push_str("<start_of_turn>model\n");
    out
}

/// Aplana los bloques de un mensaje a texto (misma convención textual
/// que ChatML: `<tool_call>` re-emitido, `<tool_response>` etiquetado).
fn render_blocks(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => s.push_str(text),
            ContentBlock::ToolUse { name, input, .. } => {
                s.push_str(&format!(
                    "<tool_call>\n{{\"name\": \"{name}\", \"arguments\": {input}}}\n</tool_call>"
                ));
            }
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
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

#[cfg(test)]
mod tests {
    use braze_types::Message;

    use super::*;

    fn req(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            messages,
            tool_stubs: vec![],
            system_prompt: String::new(),
            max_tokens: 256,
        }
    }

    #[test]
    fn prompt_uses_gemma_turns_and_opens_model_turn() {
        let prompt = build_gemma_prompt(&req(vec![Message::text(Role::User, "hola")]));
        assert!(prompt.starts_with("<start_of_turn>user\nhola"));
        assert!(prompt.contains("<end_of_turn>\n"));
        assert!(prompt.ends_with("<start_of_turn>model\n"));
    }

    #[test]
    fn system_and_tools_fold_into_the_first_user_turn() {
        let mut r = req(vec![
            Message::text(Role::Assistant, "contexto previo"),
            Message::text(Role::User, "crea el archivo"),
        ]);
        r.system_prompt = "You are braze.".to_string();
        r.tool_stubs = vec![ToolStub {
            name: "write_file".to_string(),
            summary: "Write a file".to_string(),
            source: "local".to_string(),
            input_schema: None,
        }];
        let prompt = build_gemma_prompt(&r);
        // El preámbulo NO va en el turno model previo…
        let model_turn = prompt.find("<start_of_turn>model\ncontexto previo").unwrap();
        let folded = prompt.find("You are braze.").unwrap();
        assert!(folded > model_turn);
        // …sino plegado al primer turno user, antes de su contenido.
        assert!(prompt.contains("<start_of_turn>user\nYou are braze."));
        assert!(prompt.contains("# Tools"));
        assert!(prompt.contains("\"write_file\""));
        let user_content = prompt.find("crea el archivo").unwrap();
        assert!(folded < user_content);
    }

    #[test]
    fn system_role_messages_degrade_to_user_turns() {
        let prompt = build_gemma_prompt(&req(vec![Message::text(Role::System, "regla nueva")]));
        assert!(prompt.contains("<start_of_turn>user\nregla nueva"));
        assert!(!prompt.contains("<start_of_turn>system"));
    }

    #[test]
    fn tool_use_and_result_use_the_textual_convention() {
        let r = req(vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "c1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "x.txt"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "c1".to_string(),
                    content: "contenido".to_string(),
                    is_error: false,
                }],
            },
        ]);
        let prompt = build_gemma_prompt(&r);
        assert!(prompt.contains(
            "<start_of_turn>model\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\":\"x.txt\"}}\n</tool_call>"
        ));
        assert!(prompt.contains("<start_of_turn>user\n<tool_response>\ncontenido\n</tool_response>"));
    }

    #[test]
    fn preamble_without_user_turn_still_reaches_the_model() {
        let mut r = req(vec![]);
        r.system_prompt = "solo system".to_string();
        let prompt = build_gemma_prompt(&r);
        assert!(prompt.contains("<start_of_turn>user\nsolo system<end_of_turn>"));
        assert!(prompt.ends_with("<start_of_turn>model\n"));
    }

    #[test]
    fn tools_preamble_includes_schema_as_parameters() {
        let s = render_tools_preamble(&[ToolStub {
            name: "grep".to_string(),
            summary: "Search files".to_string(),
            source: "local".to_string(),
            input_schema: Some(serde_json::json!({"type": "object"})),
        }]);
        assert!(s.contains("\"parameters\":{\"type\":\"object\"}"));
        assert!(s.contains("<tool_call>"));
    }
}
