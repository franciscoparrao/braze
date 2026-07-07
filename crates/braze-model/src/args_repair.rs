//! Escalera de reparación para argumentos de tool calls acumulados en
//! streaming (ítem 3 del backlog 2026-07-06, préstamo del
//! StreamingToolCallParser de OpenCode — docs/SOTA-2026-07.md § Adenda).
//!
//! Un buffer de argumentos puede llegar truncado (el stream se cortó a
//! media string), con una coma colgante, o directamente irreparable. La
//! política previa de ambos wire parsers (Anthropic y OpenRouter) era
//! **dropear la call en silencio** — la ronda "convergía" sin ejecutar
//! lo que el modelo pidió, indistinguible de una respuesta final sin
//! tools. La escalera la reemplaza: (1) parse directo, (2) reparación de
//! truncamiento (cerrar la string abierta, quitar la coma colgante,
//! balancear `{}`/`[]`), (3) colapso a `{}` — la call se despacha igual
//! y es la validación de schema / el error del tool el que informa al
//! modelo, una señal de reintento visible en vez de una omisión muda.

use serde_json::Value;

/// What the ladder had to do to produce a usable `arguments` value —
/// callers log anything above `Parsed` (it means the provider sent a
/// malformed buffer worth knowing about).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ArgumentsOutcome {
    /// Parsed as-is (or was empty — the documented "no-parameter call"
    /// normalization both wires already applied).
    Parsed,
    /// Direct parse failed; the truncation repair made it valid JSON.
    Repaired,
    /// Beyond repair — collapsed to `{}` so the call still dispatches.
    Collapsed,
}

/// Runs the ladder over one fully-accumulated arguments buffer. Never
/// fails: the worst case is `({}, Collapsed)`.
pub(crate) fn parse_arguments_with_repair(raw: &str) -> (Value, ArgumentsOutcome) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (empty_object(), ArgumentsOutcome::Parsed);
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return (value, ArgumentsOutcome::Parsed);
    }
    if let Ok(value) = serde_json::from_str::<Value>(&repair_truncated_json(trimmed)) {
        return (value, ArgumentsOutcome::Repaired);
    }
    (empty_object(), ArgumentsOutcome::Collapsed)
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Best-effort completion of a JSON buffer cut off mid-stream: walks the
/// text tracking string/escape state and an open-bracket stack, then (a)
/// drops a dangling trailing `\` (it would escape the quote we're about
/// to add), (b) closes an unterminated string, (c) strips one trailing
/// comma, and (d) closes the remaining brackets in reverse order. Only
/// truncation is repaired — anything structurally wrong *before* the cut
/// still fails the re-parse and falls through to the collapse rung.
fn repair_truncated_json(s: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in s.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }

    let mut out = s.to_string();
    if escaped {
        out.pop();
    }
    if in_string {
        out.push('"');
    }
    let trimmed_len = out.trim_end().len();
    if out[..trimmed_len].ends_with(',') {
        out.truncate(trimmed_len - 1);
    }
    while let Some(close) = stack.pop() {
        out.push(close);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_json_passes_through_untouched() {
        let (value, outcome) = parse_arguments_with_repair(r#"{"path": "x.txt"}"#);
        assert_eq!(value, serde_json::json!({"path": "x.txt"}));
        assert_eq!(outcome, ArgumentsOutcome::Parsed);
    }

    #[test]
    fn an_empty_buffer_is_a_no_parameter_call() {
        let (value, outcome) = parse_arguments_with_repair("   ");
        assert_eq!(value, serde_json::json!({}));
        assert_eq!(outcome, ArgumentsOutcome::Parsed);
    }

    #[test]
    fn a_string_cut_mid_value_is_repaired() {
        let (value, outcome) = parse_arguments_with_repair(r#"{"path": "src/mai"#);
        assert_eq!(value, serde_json::json!({"path": "src/mai"}));
        assert_eq!(outcome, ArgumentsOutcome::Repaired);
    }

    #[test]
    fn a_buffer_cut_after_a_complete_value_is_repaired() {
        let (value, outcome) = parse_arguments_with_repair(r#"{"a": 1, "b": [1, 2"#);
        assert_eq!(value, serde_json::json!({"a": 1, "b": [1, 2]}));
        assert_eq!(outcome, ArgumentsOutcome::Repaired);
    }

    #[test]
    fn a_trailing_comma_from_the_cut_is_stripped() {
        let (value, outcome) = parse_arguments_with_repair(r#"{"a": "x","#);
        assert_eq!(value, serde_json::json!({"a": "x"}));
        assert_eq!(outcome, ArgumentsOutcome::Repaired);
    }

    #[test]
    fn a_dangling_escape_does_not_poison_the_closing_quote() {
        let (value, outcome) = parse_arguments_with_repair(r#"{"path": "a\"#);
        assert_eq!(value, serde_json::json!({"path": "a"}));
        assert_eq!(outcome, ArgumentsOutcome::Repaired);
    }

    #[test]
    fn escaped_quotes_inside_strings_do_not_confuse_the_walker() {
        let (value, outcome) =
            parse_arguments_with_repair(r#"{"cmd": "echo \"hola\"", "dir": "/tm"#);
        assert_eq!(
            value,
            serde_json::json!({"cmd": "echo \"hola\"", "dir": "/tm"})
        );
        assert_eq!(outcome, ArgumentsOutcome::Repaired);
    }

    #[test]
    fn garbage_collapses_to_an_empty_object_instead_of_failing() {
        let (value, outcome) = parse_arguments_with_repair("not json at all }{");
        assert_eq!(value, serde_json::json!({}));
        assert_eq!(outcome, ArgumentsOutcome::Collapsed);
    }

    #[test]
    fn structural_damage_before_the_cut_is_not_papered_over() {
        // A missing colon isn't truncation — must collapse, not "repair"
        // into something the model never said.
        let (value, outcome) = parse_arguments_with_repair(r#"{"a" 1"#);
        assert_eq!(value, serde_json::json!({}));
        assert_eq!(outcome, ArgumentsOutcome::Collapsed);
    }
}
