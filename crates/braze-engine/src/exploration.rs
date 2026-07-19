//! I.7 — el explorador de contexto aislado (`explore`), palanca de
//! AISLAMIENTO DE CONTEXTO, no de capacidad (diseño pre-registrado:
//! `docs/explorador-aislado-ab-design.md`, commit `4517b6d`; validación
//! de mercado 2026-07-19: Kimi Code envía un subagente `explore`
//! equivalente como feature core, sin medirlo).
//!
//! Mecanismo: una tool harness-owned `explore(question)` — mismo patrón
//! de intercepción que `search_tools`/`task_add` (sin registry schema,
//! sin permission guard: el hijo solo recibe tools read-only). El hijo
//! es un mini-loop sobre EL MISMO `ModelBackend` del executor (la
//! ganancia, si existe, no puede atribuirse a capacidad agregada, solo
//! al aislamiento), con `read_file`/`grep`/`glob`, un cap de
//! [`MAX_CHILD_ROUNDS`] rondas, y una historia desechable: al rollout
//! log del padre solo entran la tool call, su resultado, un
//! [`braze_events::AgentEvent::ExplorationDelegated`] de auditoría y un
//! `Usage` agregado con los tokens del hijo (cada llamada real se
//! contabiliza — misma regla que best-of-n).
//!
//! Off por default (`Config::enable_exploration` /
//! `+ablate:explore` en braze-bench) — el A/B pre-registrado decide su
//! adopción, este módulo solo lo hace medible. El hijo NO recibe la
//! tool `explore`: profundidad 1 por construcción.

use braze_types::ToolStub;

pub(crate) const EXPLORE_TOOL: &str = "explore";

/// Cap de rondas del loop hijo — bajo a propósito (el diseño lo fija en
/// 6): la exploración es "lee N archivos y concluye", no un turno
/// completo.
pub(crate) const MAX_CHILD_ROUNDS: u32 = 6;

/// Las únicas tools que el hijo puede invocar — read-only por
/// construcción, así el explorador nunca necesita permission guard.
/// `explore` no está en la lista: profundidad 1.
pub(crate) const CHILD_READ_ONLY_TOOLS: &[&str] = &["read_file", "grep", "glob"];

/// Addendum al system prompt del proyecto para el hijo — el formato que
/// el diseño fija: responder solo la pregunta, corto, sin proponer
/// acciones (el hijo informa; el padre decide).
pub(crate) const CHILD_PROMPT_ADDENDUM: &str = "\n\nYou are an exploration subagent. Answer \
     ONLY the question you were given, in at most 3 sentences. Do not \
     propose actions.";

/// El tool result recuperable cuando el hijo no converge — el modelo
/// padre puede seguir explorando por su cuenta, la palanca nunca mata
/// el turno.
pub(crate) const EXPLORATION_FAILED_RESULT: &str = "exploration failed; explore yourself with \
     read_file/grep";

pub(crate) fn explore_tool_stub() -> ToolStub {
    ToolStub {
        name: EXPLORE_TOOL.to_string(),
        summary: "Delegate a broad read-only exploration question (\"which of these files \
                  contains X?\") to an isolated helper that reads files for you and returns \
                  only the conclusion — keeping the file contents out of your own context. \
                  Use it for questions that need scanning several files; answer directly \
                  when you already know."
            .to_string(),
        source: "harness".to_string(),
        input_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The exploration question, self-contained (the helper \
                                    sees only this text, not your conversation)."
                }
            },
            "required": ["question"],
            "additionalProperties": false
        })),
    }
}

/// Lo que el mini-loop hijo le devuelve al dispatch del padre.
pub(crate) struct ExplorationOutcome {
    pub(crate) content: String,
    pub(crate) is_error: bool,
    pub(crate) child_rounds: u32,
    pub(crate) child_input_tokens: u64,
    pub(crate) child_output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_advertises_a_single_required_question_argument() {
        let stub = explore_tool_stub();
        assert_eq!(stub.name, EXPLORE_TOOL);
        assert_eq!(stub.source, "harness");
        let schema = stub.input_schema.expect("schema present");
        assert_eq!(schema["required"], serde_json::json!(["question"]));
    }

    /// Profundidad 1 por construcción: el hijo no puede delegar.
    #[test]
    fn the_child_tool_allowlist_excludes_explore_itself() {
        assert!(!CHILD_READ_ONLY_TOOLS.contains(&EXPLORE_TOOL));
        for tool in CHILD_READ_ONLY_TOOLS {
            assert!(
                ["read_file", "grep", "glob"].contains(tool),
                "solo lectura: {tool}"
            );
        }
    }
}
