//! El subagente `editor` (SWE-Edit, arXiv 2604.26102) — la mitad
//! ESCRITORA del par Viewer/Editor de contexto angosto. Su hermano
//! read-only es `explore` (`crate::exploration`); este módulo es su
//! espejo con capacidad de mutación. Diseño completo:
//! `docs/editor-subagent-design-2026-08-10.md`.
//!
//! Mecanismo: una tool harness-owned `editor(path, instruction)`
//! interceptada en dispatch (mismo patrón que `explore`/`search_tools`).
//! El padre delega UNA edición autocontenida sobre un archivo; el hijo
//! es un mini-loop sobre EL MISMO `ModelBackend` del executor (la
//! ganancia, si existe, es de aislamiento de contexto, no de capacidad)
//! que corre el ciclo read→edit→verify y devuelve solo un resumen de
//! estado. El churn —ediciones fallidas, contenido del archivo, salida
//! del `cargo check`— se queda en el transcript desechable del hijo, que
//! es la ganancia entera.
//!
//! A diferencia de `explore`, el hijo MUTA, así que:
//! - despacha `edit_file`/`write_file` por `self.tools.dispatch` (hereda
//!   el permission guard, la resolución de workdir, el post-edit check y
//!   el sandbox Landlock — todo dentro del provider, cero código nuevo);
//! - mantiene su PROPIO interlock L-10 (`edit_failures_by_path` fresco
//!   por delegación, reusando `EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD`),
//!   porque el del padre vive en `TurnDispatchState` y un hijo que
//!   despacha directo lo evitaría — dejando la clase de daño de L-10
//!   (reescribir el archivo entero para tapar una edición imposible)
//!   suelta dentro de un loop aún menos observable;
//! - devuelve un [`EditorOutcome`] ESTRUCTURADO (no solo texto): el padre
//!   necesita saber si la edición aterrizó y si compila SIN releer el
//!   archivo (releer derrota el aislamiento).
//!
//! Off por default (`Config::enable_editor` / `+ablate:editor`). El hijo
//! no recibe `editor` ni `explore`: profundidad 1 por construcción.

use braze_types::ToolStub;

pub(crate) const EDITOR_TOOL: &str = "editor";

/// Cap de rondas del loop hijo: read → edit → ver el verdict del
/// post-edit check → un fix → re-check → un fix más. Bajo a propósito
/// (más que las 6 de explore no hace falta; el hijo hace UNA edición,
/// no un turno completo).
pub(crate) const MAX_EDITOR_CHILD_ROUNDS: u32 = 6;

/// Las únicas tools que el hijo puede invocar. `read_file` para ubicar
/// el `old_string`, `edit_file` para el cambio dirigido, `write_file`
/// como rescate (gobernado por el interlock propio del hijo). NO
/// `grep`/`glob` (el `path` se da), NO `shell_exec` (verify = post-edit
/// check, que ya viaja gratis), NO `editor`/`explore` (profundidad 1).
pub(crate) const CHILD_EDIT_TOOLS: &[&str] = &["read_file", "edit_file", "write_file"];

/// Addendum al system prompt del proyecto para el hijo. Fuerza el
/// formato de reporte de ESTADO (no narrativa) del que el padre lee
/// ground truth sin releer, y la honestidad sobre la que se construye el
/// diseño: reportar que no se pudo antes que corromper.
pub(crate) const CHILD_PROMPT_ADDENDUM: &str = "\n\nYou are an edit subagent. Apply ONLY the \
     requested change to the given file. After each edit, read the `[post-edit check]` block in \
     your tool result: if it says the code COMPILES you are done. If an edit fails, retry \
     edit_file with a shorter old_string copied EXACTLY from the file — do NOT rewrite the whole \
     file to paper over a mismatch. When done, reply with ONE line in this form: \
     `State: <fully edited|partially edited|unchanged>. Compiles: <yes|no|n/a>. Change: <one \
     line>.` If you cannot make the change safely, say so honestly instead of corrupting the file.";

/// Tool result recuperable cuando el hijo no converge — el padre puede
/// editar por su cuenta; el lever nunca mata el turno (misma postura que
/// `EXPLORATION_FAILED_RESULT`).
pub(crate) const EDITOR_FAILED_RESULT: &str =
    "edit delegation did not converge; make the edit yourself";

/// Estado de compilación del archivo tal como quedó en disco, derivado
/// del bloque `[post-edit check]` del resultado de la última edición
/// exitosa — verdict, no el churn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompileStatus {
    /// El post-edit check dijo `... the code COMPILES`.
    Pass,
    /// El post-edit check dijo `... (exit N) ...`.
    Fail,
    /// No hubo edición exitosa, o su resultado no traía bloque de check
    /// (extensión sin formatter, archivo huérfano, check no ejecutable).
    Unknown,
}

/// Lo que el mini-loop hijo le devuelve al dispatch del padre. A
/// diferencia de `ExplorationOutcome` (que nunca muta), lleva ground
/// truth estructurado — ver el module doc y el diseño.
pub(crate) struct EditorOutcome {
    pub(crate) content: String,
    pub(crate) is_error: bool,
    /// ¿Alguna edición del hijo tuvo éxito? Derivado de los resultados
    /// reales del dispatch, NO del auto-reporte del hijo. Maneja el
    /// bookkeeping del padre (`turn_did_edit`, `seen_calls.clear()`).
    pub(crate) landed: bool,
    /// Estado de compilación del archivo en disco tras la delegación.
    pub(crate) compiles: CompileStatus,
    pub(crate) child_rounds: u32,
    pub(crate) child_input_tokens: u64,
    pub(crate) child_output_tokens: u64,
}

pub(crate) fn editor_tool_stub() -> ToolStub {
    ToolStub {
        name: EDITOR_TOOL.to_string(),
        summary: "Delegate ONE self-contained edit to a single file to an isolated helper that \
                  owns the read→edit→verify loop and returns only a one-line state summary — \
                  keeping failed edits, full file contents and post-edit-check output out of \
                  your context. Use when you already know the change to make; edit directly for \
                  trivial one-liners."
            .to_string(),
        source: "harness".to_string(),
        input_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The single file to edit (relative to the working directory)."
                },
                "instruction": {
                    "type": "string",
                    "description": "A self-contained description of the change to make (the \
                                    helper sees only this text and the file, not your \
                                    conversation)."
                }
            },
            "required": ["path", "instruction"],
            "additionalProperties": false
        })),
    }
}

/// Deriva [`CompileStatus`] del contenido de un tool result de edición
/// exitosa, matcheando los markers que `braze_tools_local::post_edit_check`
/// emite. Acoplado a esos strings a propósito: es el único lugar donde el
/// harness lee el verdict del check de forma robusta en vez de confiar en
/// el auto-reporte del hijo.
pub(crate) fn compile_status_from_result(content: &str) -> CompileStatus {
    if !content.contains("[post-edit check]") {
        return CompileStatus::Unknown;
    }
    if content.contains("the code COMPILES") {
        CompileStatus::Pass
    } else if content.contains("(exit ") {
        CompileStatus::Fail
    } else {
        CompileStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_advertises_required_path_and_instruction() {
        let stub = editor_tool_stub();
        assert_eq!(stub.name, EDITOR_TOOL);
        assert_eq!(stub.source, "harness");
        let schema = stub.input_schema.expect("schema present");
        assert_eq!(
            schema["required"],
            serde_json::json!(["path", "instruction"])
        );
    }

    /// Profundidad 1 y superficie acotada: el hijo no delega ni explora,
    /// y no toca la shell.
    #[test]
    fn the_child_tool_allowlist_is_edit_only_and_excludes_delegation() {
        assert!(!CHILD_EDIT_TOOLS.contains(&EDITOR_TOOL));
        assert!(!CHILD_EDIT_TOOLS.contains(&"explore"));
        assert!(!CHILD_EDIT_TOOLS.contains(&"shell_exec"));
        for tool in CHILD_EDIT_TOOLS {
            assert!(
                ["read_file", "edit_file", "write_file"].contains(tool),
                "solo edición: {tool}"
            );
        }
    }

    #[test]
    fn compile_status_reads_the_post_edit_check_markers() {
        assert_eq!(
            compile_status_from_result(
                "edited\n\n[post-edit check] `cargo check` passed in 2s — the code COMPILES."
            ),
            CompileStatus::Pass
        );
        assert_eq!(
            compile_status_from_result(
                "edited\n\n[post-edit check] `cargo check` (exit 101) in 3s after this edit\nerror[E0308]"
            ),
            CompileStatus::Fail
        );
        assert_eq!(
            compile_status_from_result("edited (no check ran)"),
            CompileStatus::Unknown
        );
    }
}
