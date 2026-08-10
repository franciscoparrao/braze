//! El canal SEARCH/REPLACE textual del A/B del impuesto JSON
//! (docs/hypothesis-2026-08-10-json-tax-edit-fence.md).
//!
//! Con el lever `edit_fence_enabled` prendido, `edit_file` sale del
//! inventario de tools y el modelo recibe (vía
//! [`EDIT_FENCE_ADDENDUM`]) la instrucción de emitir sus ediciones como
//! bloques SEARCH/REPLACE en el texto de la respuesta. Este módulo es
//! el parser de vuelta: cada bloque bien formado se sintetiza como una
//! `ToolCall` de `edit_file`, así la APLICACIÓN reusa entera la
//! herramienta real (escalera fuzzy, gate sintáctico, post-edit check,
//! mensajes de error pedagógicos) y el A/B mide SOLO el transporte.
//!
//! Es un canal *primario* (instruido), no un rescate — precedente del
//! envelope de prompt-tools (`rescue::parse_envelope_response`):
//! contarlo como rescue contaminaría `rescued_tool_calls`, la métrica
//! de mecanismo del A/B. Corre en `complete_once_with` ANTES del
//! envelope y de la escalera, y sin la condición
//! `tool_calls.is_empty()`: una respuesta puede llamar `read_file`
//! nativo Y emitir un fence de edición.
//!
//! Contrato del parser, compartido con la escalera: ante un bloque
//! malformado (sin path resoluble, marcadores incompletos) el texto se
//! deja INTACTO en vez de inventar una reparación — el modelo verá su
//! propia salida sin efecto y reintentará con el feedback del loop.
//! Limitación aceptada (misma que aider): una línea de `=======` DENTRO
//! de la sección SEARCH se toma como divisor; el `old_string` resultante
//! no matcheará y el error de `edit_file` guiará la corrección.

use braze_types::ToolCall;

/// Addendum al system prompt del brazo edit-fence — la gramática que el
/// modelo debe emitir en lugar de llamar `edit_file`. En inglés, como el
/// resto de los prompts del harness.
pub(crate) const EDIT_FENCE_ADDENDUM: &str = "\n\n## Editing files\n\
To edit an existing file, do NOT call a tool. Instead, write a \
SEARCH/REPLACE block directly in your reply text, in exactly this form:\n\
\n\
path/to/file.ext\n\
<<<<<<< SEARCH\n\
exact text currently in the file\n\
=======\n\
replacement text\n\
>>>>>>> REPLACE\n\
\n\
Rules:\n\
- The file path goes on its own line immediately before the block.\n\
- The SEARCH section must reproduce the current file text exactly, \
character for character, including whitespace.\n\
- Emit one block per change; several blocks in one reply are fine.\n\
- Every other tool is still called normally. To create a NEW file, use \
the write_file tool.\n";

/// ¿Es esta línea el marcador de apertura `<<<<<<< SEARCH`? Tolerante en
/// el conteo (5+ `<`) — los modelos chicos pierden caracteres de
/// ceremonia con facilidad y el marcador sigue siendo inconfundible.
fn is_search_marker(line: &str) -> bool {
    let t = line.trim();
    let angles = t.chars().take_while(|&c| c == '<').count();
    angles >= 5 && t[angles..].trim() == "SEARCH"
}

/// ¿Divisor `=======`? Solo signos `=` (5+), nada más en la línea.
fn is_divider(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 5 && t.chars().all(|c| c == '=')
}

/// ¿Cierre `>>>>>>> REPLACE`? Mismo contrato que [`is_search_marker`].
fn is_replace_marker(line: &str) -> bool {
    let t = line.trim();
    let angles = t.chars().take_while(|&c| c == '>').count();
    angles >= 5 && t[angles..].trim() == "REPLACE"
}

/// Normaliza la línea de path: fuera backticks/comillas/asteriscos
/// envolventes y un `:` final. `None` si lo que queda no parece un path
/// (vacío, o con espacios internos — prosa, no un path de este bench).
fn clean_path_line(line: &str) -> Option<String> {
    // Iterativo: las decoraciones se anidan en cualquier orden
    // (`` `path`: `` termina en `:` DESPUÉS del backtick) — una sola
    // pasada de trim dejaría la de adentro.
    let mut cleaned = line.trim();
    loop {
        let next = cleaned
            .trim_matches(|c| c == '`' || c == '*' || c == '"' || c == '\'')
            .trim_end_matches(':')
            .trim();
        if next == cleaned {
            break;
        }
        cleaned = next;
    }
    if cleaned.is_empty() || cleaned.contains(char::is_whitespace) {
        return None;
    }
    Some(cleaned.to_string())
}

/// Extrae todos los bloques SEARCH/REPLACE bien formados de `text` como
/// `ToolCall`s de `edit_file`, y devuelve el texto restante con los
/// bloques consumidos (path y fence envolvente incluidos). Bloques
/// malformados quedan en el texto tal cual — ver el module doc.
pub(crate) fn extract_edit_fence_calls(text: &str) -> (Vec<ToolCall>, String) {
    let lines: Vec<&str> = text.lines().collect();
    let mut consumed = vec![false; lines.len()];
    let mut calls = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        if !is_search_marker(lines[i]) {
            i += 1;
            continue;
        }
        // Busca el divisor y el cierre hacia adelante.
        let Some(div) = (i + 1..lines.len()).find(|&j| is_divider(lines[j])) else {
            i += 1;
            continue;
        };
        let Some(end) = (div + 1..lines.len()).find(|&j| is_replace_marker(lines[j])) else {
            i += 1;
            continue;
        };
        // Path: la línea no vacía inmediatamente anterior, saltando (y
        // consumiendo) una apertura de fence de código si el modelo
        // envolvió el bloque en ```.
        let mut path_idx = None;
        let mut fence_open_idx = None;
        let mut j = i;
        while j > 0 {
            j -= 1;
            let t = lines[j].trim();
            if t.is_empty() {
                break;
            }
            if t.starts_with("```") && fence_open_idx.is_none() && path_idx.is_none() {
                fence_open_idx = Some(j);
                continue;
            }
            path_idx = Some(j);
            break;
        }
        let Some(path) = path_idx.and_then(|j| clean_path_line(lines[j])) else {
            // Sin path resoluble: el bloque queda como texto.
            i = end + 1;
            continue;
        };

        let old_string = lines[i + 1..div].join("\n");
        let new_string = lines[div + 1..end].join("\n");
        calls.push(ToolCall {
            id: format!("fence-{}", uuid::Uuid::new_v4()),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "old_string": old_string,
                "new_string": new_string,
            }),
        });

        consumed[path_idx.unwrap()..=end].fill(true);
        if let Some(f) = fence_open_idx {
            consumed[f] = true;
            // El cierre del fence envolvente, si sigue inmediatamente.
            if lines.get(end + 1).map(|l| l.trim()) == Some("```") {
                consumed[end + 1] = true;
            }
        }
        i = end + 1;
    }

    if calls.is_empty() {
        return (Vec::new(), text.to_string());
    }
    let remaining = lines
        .iter()
        .enumerate()
        .filter(|(idx, _)| !consumed[*idx])
        .map(|(_, l)| *l)
        .collect::<Vec<_>>()
        .join("\n");
    (calls, remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(call: &ToolCall) -> (&str, &str, &str) {
        (
            call.arguments["path"].as_str().unwrap(),
            call.arguments["old_string"].as_str().unwrap(),
            call.arguments["new_string"].as_str().unwrap(),
        )
    }

    #[test]
    fn parses_a_single_block_with_surrounding_prose() {
        let text = "I'll fix the bug now.\n\nsrc/lib.rs\n<<<<<<< SEARCH\nlet x = 1;\n=======\nlet x = 2;\n>>>>>>> REPLACE\n\nDone.";
        let (calls, remaining) = extract_edit_fence_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "edit_file");
        assert!(calls[0].id.starts_with("fence-"));
        let (path, old, new) = args(&calls[0]);
        assert_eq!(path, "src/lib.rs");
        assert_eq!(old, "let x = 1;");
        assert_eq!(new, "let x = 2;");
        assert!(remaining.contains("I'll fix the bug now."));
        assert!(remaining.contains("Done."));
        assert!(!remaining.contains("SEARCH"));
    }

    #[test]
    fn parses_multiple_blocks() {
        let text = "a.rs\n<<<<<<< SEARCH\nfoo\n=======\nbar\n>>>>>>> REPLACE\n\nb.rs\n<<<<<<< SEARCH\nbaz\n=======\nqux\n>>>>>>> REPLACE";
        let (calls, _) = extract_edit_fence_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(args(&calls[0]).0, "a.rs");
        assert_eq!(args(&calls[1]).0, "b.rs");
    }

    #[test]
    fn parses_a_block_wrapped_in_a_code_fence() {
        let text = "src/main.rs\n```rust\n<<<<<<< SEARCH\nold\n=======\nnew\n>>>>>>> REPLACE\n```\ntrailing";
        let (calls, remaining) = extract_edit_fence_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(args(&calls[0]).0, "src/main.rs");
        assert_eq!(remaining.trim(), "trailing");
        assert!(!remaining.contains("```"));
    }

    #[test]
    fn tolerates_sloppy_marker_ceremony_and_decorated_paths() {
        // 5 `<` en vez de 7, path con backticks y dos puntos.
        let text = "`src/lib.rs`:\n<<<<< SEARCH\nfoo\n=========\nbar\n>>>>>>>> REPLACE";
        let (calls, _) = extract_edit_fence_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(args(&calls[0]).0, "src/lib.rs");
    }

    #[test]
    fn multiline_sections_preserved_verbatim() {
        let text = "f.rs\n<<<<<<< SEARCH\nfn a() {\n    1\n}\n=======\nfn a() {\n    2\n}\n>>>>>>> REPLACE";
        let (calls, _) = extract_edit_fence_calls(text);
        let (_, old, new) = args(&calls[0]);
        assert_eq!(old, "fn a() {\n    1\n}");
        assert_eq!(new, "fn a() {\n    2\n}");
    }

    #[test]
    fn empty_search_section_still_synthesizes_the_call() {
        // edit_file rechaza old_string vacío con su mensaje pedagógico
        // ("use write_file") — el loop de feedback corrige, no el parser.
        let text = "f.rs\n<<<<<<< SEARCH\n=======\ncontenido nuevo\n>>>>>>> REPLACE";
        let (calls, _) = extract_edit_fence_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(args(&calls[0]).1, "");
    }

    #[test]
    fn block_without_a_resolvable_path_is_left_as_text() {
        let text = "Here is the change:\n\n<<<<<<< SEARCH\nfoo\n=======\nbar\n>>>>>>> REPLACE";
        let (calls, remaining) = extract_edit_fence_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn unterminated_block_is_left_as_text() {
        let text = "f.rs\n<<<<<<< SEARCH\nfoo\n=======\nbar";
        let (calls, remaining) = extract_edit_fence_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn prose_with_equals_ruler_is_not_a_block() {
        let text = "Título\n=======\n\nTexto normal sin marcadores.";
        let (calls, remaining) = extract_edit_fence_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn path_line_that_is_prose_rejects_the_block() {
        let text = "I will edit the file now\n<<<<<<< SEARCH\nfoo\n=======\nbar\n>>>>>>> REPLACE";
        let (calls, remaining) = extract_edit_fence_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn addendum_mentions_the_grammar_and_write_file() {
        assert!(EDIT_FENCE_ADDENDUM.contains("<<<<<<< SEARCH"));
        assert!(EDIT_FENCE_ADDENDUM.contains(">>>>>>> REPLACE"));
        assert!(EDIT_FENCE_ADDENDUM.contains("write_file"));
    }
}
