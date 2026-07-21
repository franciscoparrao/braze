//! `stencil` — constrained decoding GBNF para el `LocalBackend` (Fase 3
//! del design doc `docs/local-backend-design-2026-07-20.md`): la
//! gramática que enmascara logits para que la **sintaxis** de una tool
//! call sea imposible de generar mal. No *sobrevive* al error (como el
//! resample de #17) — lo hace **ingenerable**. Es la palanca que ningún
//! backend HTTP puede ofrecer: exige ser dueño del sampler.
//!
//! Módulo **puro** (grammar strings + cursor JSON), compilado también
//! sin el feature `local` para que sus tests corran en el `cargo test`
//! normal del workspace — mismo patrón que `harmony.rs`. La integración
//! (swap de sampler en el loop de generación) vive en `local.rs`.
//!
//! Estrategia de activación: **laziness manual**, no `grammar_lazy` de
//! llama.cpp. Somos dueños del loop de decode, así que el constraint se
//! engancha exactamente cuando el estado del turno lo dice — tras el
//! literal `<tool_call>` en ChatML/qwen, o al entrar el `HarmonyParser`
//! en modo args (`<|message|>` con destinatario) en gpt-oss — y se
//! suelta al completarse el envelope. El modelo escribe texto libre
//! antes y después; solo la call está estencilada.

/// Reglas JSON compartidas (adaptadas del `json.gbnf` de llama.cpp).
/// `value`/`object`/`array`/`string`/`number` + `ws`; los límites de
/// repetición acotan el backtracking del matcher de gramática.
const JSON_RULES: &str = r#"
value  ::= object | array | string | number | ("true" | "false" | "null") ws
object ::= "{" ws ( string ":" ws value ("," ws string ":" ws value)* )? "}" ws
array  ::= "[" ws ( value ("," ws value)* )? "]" ws
string ::= "\"" ( [^"\\\x7F\x00-\x1F] | "\\" (["\\bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]) )* "\"" ws
number ::= ("-"? ([0-9] | [1-9] [0-9]{0,15})) ("." [0-9]+)? ([eE] [-+]? [0-9] [1-9]{0,15})? ws
ws     ::= [ \t\n\r]{0,8}
"#;

/// Gramática del envelope de qwen2.5, activada tras el literal
/// `<tool_call>`: fuerza `{"name": <uno-de-los-tools>, "arguments":
/// <objeto JSON>}` seguido del tag de cierre — orden de claves fijo (el
/// formato entrenado), nombre restringido al inventario real (los
/// nombres alucinados mueren en el sampler, no en la validación), y el
/// `</tool_call>` garantizado (la clase de call-sin-cierre que la
/// escalera de rescate repara deja de existir).
///
/// `None` si no hay tools (sin inventario no hay call que estencilar).
pub(crate) fn qwen_call_grammar(tool_names: &[String]) -> Option<String> {
    if tool_names.is_empty() {
        return None;
    }
    let alternation = tool_names
        .iter()
        .map(|n| format!("\"\\\"{}\\\"\"", escape_gbnf_literal(n)))
        .collect::<Vec<_>>()
        .join(" | ");
    Some(format!(
        "root ::= ws \"{{\" ws \"\\\"name\\\"\" ws \":\" ws toolname ws \",\" ws \
         \"\\\"arguments\\\"\" ws \":\" ws object ws \"}}\" ws \"</tool_call>\"\n\
         toolname ::= {alternation}\n\
         {JSON_RULES}"
    ))
}

/// Gramática de los argumentos de una call Harmony, activada cuando el
/// header ya fijó el destinatario (`to=functions.X <|constrain|>json
/// <|message|>`): un objeto JSON válido, nada más. El nombre no se
/// estencila (ya quedó atrás, en el header); el `<|call|>` de cierre lo
/// emite el modelo al soltarse el constraint.
pub(crate) fn harmony_args_grammar() -> String {
    format!("root ::= ws object\n{JSON_RULES}")
}

/// Escapa un nombre de tool para un literal GBNF (`"` y `\`).
fn escape_gbnf_literal(name: &str) -> String {
    name.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Cursor incremental sobre un stream de texto JSON: detecta cuándo el
/// objeto raíz se cerró (profundidad 0 tras el primer `{`), respetando
/// strings y escapes. Es la señal de "soltar el constraint" — el
/// sampler vuelve a libre y el modelo cierra el turno a su manera
/// (`<|call|>` en Harmony). Puro y directamente testeado.
#[derive(Debug, Default)]
pub(crate) struct JsonCursor {
    depth: u32,
    started: bool,
    complete: bool,
    in_string: bool,
    escaped: bool,
}

impl JsonCursor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn feed(&mut self, piece: &str) {
        for ch in piece.chars() {
            if self.complete {
                return;
            }
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if ch == '\\' {
                    self.escaped = true;
                } else if ch == '"' {
                    self.in_string = false;
                }
                continue;
            }
            match ch {
                '"' if self.started => self.in_string = true,
                '{' => {
                    self.started = true;
                    self.depth += 1;
                }
                '}' if self.started => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 0 {
                        self.complete = true;
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn complete(&self) -> bool {
        self.complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen_grammar_lists_every_tool_and_fixes_the_envelope() {
        let g = qwen_call_grammar(&["read_file".to_string(), "write_file".to_string()])
            .expect("con tools hay gramática");
        assert!(g.contains(r#""\"read_file\"" | "\"write_file\"""#));
        // Orden de claves fijo (formato entrenado) y cierre garantizado.
        assert!(g.contains(r#""\"name\"""#));
        assert!(g.contains(r#""\"arguments\"""#));
        assert!(g.contains(r#""</tool_call>""#));
        assert!(g.contains("object ::="));
    }

    #[test]
    fn qwen_grammar_without_tools_is_none() {
        assert!(qwen_call_grammar(&[]).is_none());
    }

    #[test]
    fn qwen_grammar_escapes_hostile_names() {
        let g = qwen_call_grammar(&["a\"b\\c".to_string()]).unwrap();
        // El nombre queda escapado dentro del literal GBNF (`"`→`\"`,
        // `\`→`\\`), sin romper la sintaxis de la gramática.
        assert!(g.contains(r#"a\"b\\c"#));
    }

    #[test]
    fn harmony_grammar_is_a_bare_json_object() {
        let g = harmony_args_grammar();
        assert!(g.starts_with("root ::= ws object"));
        assert!(g.contains("string ::="));
    }

    #[test]
    fn cursor_completes_on_balanced_object() {
        let mut c = JsonCursor::new();
        c.feed("{\"path\": \"x.txt\", \"n\": {\"a\": 1}}");
        assert!(c.complete());
    }

    #[test]
    fn cursor_ignores_braces_inside_strings_and_escapes() {
        let mut c = JsonCursor::new();
        c.feed("{\"content\": \"}} \\\" {\"");
        assert!(!c.complete());
        c.feed("}");
        assert!(c.complete());
    }

    #[test]
    fn cursor_streams_across_arbitrary_piece_splits() {
        let mut c = JsonCursor::new();
        for piece in ["{\"pa", "th\": \"a", ".txt\"", "}"] {
            assert!(!c.complete());
            c.feed(piece);
        }
        assert!(c.complete());
    }

    #[test]
    fn cursor_tolerates_leading_whitespace() {
        let mut c = JsonCursor::new();
        c.feed("\n  {}");
        assert!(c.complete());
    }
}
