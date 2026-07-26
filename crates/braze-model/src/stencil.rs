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
/// `value`/`object`/`array`/`string`/`number` + escalares con nombre y
/// `ws`; los límites de repetición acotan el backtracking del matcher.
const JSON_RULES: &str = r#"
value  ::= object | array | string | number | ("true" | "false" | "null") ws
object ::= "{" ws ( string ":" ws value ("," ws string ":" ws value)* )? "}" ws
array  ::= "[" ws ( value ("," ws value)* )? "]" ws
string ::= "\"" ( [^"\\\x7F\x00-\x1F] | "\\" (["\\bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]) )* "\"" ws
number ::= ("-"? ([0-9] | [1-9] [0-9]{0,15})) ("." [0-9]+)? ([eE] [-+]? [0-9] [1-9]{0,15})? ws
integer ::= "-"? [0-9]{1,16} ws
boolean ::= ("true" | "false") ws
ws     ::= [ \t\n\r]{0,8}
"#;

/// Espec de una tool para las gramáticas del stencil: el nombre y su
/// `input_schema` (si el stub lo trae resuelto — los diferidos/MCP sin
/// resolver caen al objeto JSON genérico).
#[derive(Debug, Clone)]
pub(crate) struct ToolGrammarSpec {
    pub(crate) name: String,
    pub(crate) schema: Option<serde_json::Value>,
}

/// Conversor JSON Schema → reglas GBNF (subconjunto que los schemas de
/// tools de braze usan de verdad: object/properties/required, string,
/// integer, number, boolean, enum, array-de-X, objetos anidados). Lo no
/// soportado (anyOf/$ref/patterns…) degrada a la regla genérica `value`
/// — el sampler queda menos restringido, nunca más restringido de lo
/// correcto. Devuelve el nombre de la regla raíz del schema y acumula
/// las reglas generadas en `rules`; `fresh` numera reglas para evitar
/// colisiones.
///
/// Decisiones de forma (importan para no pelear con el modelo):
/// - Los campos requeridos se emiten en el orden de la LISTA `required`
///   (la intención del autor: `["path", "content"]`), no en el orden
///   alfabético del map de `properties`.
/// - Los opcionales van después, cada uno como `("," ws kv)?`
///   independiente; si NO hay requeridos, una cadena recursiva permite
///   cualquier subconjunto en orden sin coma inicial colgante.
/// - `additionalProperties` se trata como cerrado (los schemas de braze
///   declaran `false`): un arg no declarado es ingenerable.
fn fresh_id(fresh: &mut usize) -> String {
    *fresh += 1;
    format!("r{}", *fresh - 1)
}

fn schema_rule(schema: &serde_json::Value, rules: &mut Vec<String>, fresh: &mut usize) -> String {
    if let Some(vals) = schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .filter(|v| !v.is_empty())
    {
        let id = fresh_id(fresh);
        let alts = vals
            .iter()
            .map(|v| format!("\"{}\"", escape_gbnf_literal(&v.to_string())))
            .collect::<Vec<_>>()
            .join(" | ");
        rules.push(format!("{id} ::= ({alts}) ws"));
        return id;
    }
    if let Some(v) = schema.get("const") {
        let id = fresh_id(fresh);
        rules.push(format!(
            "{id} ::= \"{}\" ws",
            escape_gbnf_literal(&v.to_string())
        ));
        return id;
    }

    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => "string".to_string(),
        Some("number") => "number".to_string(),
        Some("integer") => "integer".to_string(),
        Some("boolean") => "boolean".to_string(),
        Some("null") => {
            let id = fresh_id(fresh);
            rules.push(format!("{id} ::= \"null\" ws"));
            id
        }
        Some("array") => {
            let item = schema
                .get("items")
                .map_or("value".to_string(), |s| schema_rule(s, rules, fresh));
            let id = fresh_id(fresh);
            rules.push(format!(
                "{id} ::= \"[\" ws ({item} (\",\" ws {item})*)? \"]\" ws"
            ));
            id
        }
        Some("object") => object_rule(schema, rules, fresh),
        _ => "value".to_string(),
    }
}

/// La regla de objeto con propiedades tipadas. Sin `properties` (o
/// vacías) cae al `object` genérico.
fn object_rule(schema: &serde_json::Value, rules: &mut Vec<String>, fresh: &mut usize) -> String {
    let Some(props) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return "object".to_string();
    };
    if props.is_empty() {
        return "object".to_string();
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|r| r.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();

    // kv por propiedad: `"clave" ws ":" ws <regla-del-valor>`.
    let mut kv_rule = |key: &str, prop: &serde_json::Value| -> String {
        let val = schema_rule(prop, rules, fresh);
        let id = fresh_id(fresh);
        rules.push(format!(
            "{id} ::= \"\\\"{}\\\"\" ws \":\" ws {val}",
            escape_gbnf_literal(key)
        ));
        id
    };

    // Requeridos en el orden de la lista `required`, luego los demás.
    let req_kvs: Vec<String> = required
        .iter()
        .filter_map(|k| props.get(*k).map(|p| kv_rule(k, p)))
        .collect();
    let opt_kvs: Vec<String> = props
        .iter()
        .filter(|(k, _)| !required.contains(&k.as_str()))
        .map(|(k, p)| kv_rule(k, p))
        .collect();

    let id = fresh_id(fresh);
    let mut body = String::from("\"{\" ws ");
    if req_kvs.is_empty() {
        // Todo-opcional: cadena recursiva que admite cualquier
        // subconjunto en orden, sin coma inicial colgante.
        if let Some(chain_root) = optional_chain(&opt_kvs, rules, fresh) {
            body.push_str(&format!("({chain_root})? "));
        }
    } else {
        body.push_str(&req_kvs.join(" \",\" ws "));
        for kv in &opt_kvs {
            body.push_str(&format!(" (\",\" ws {kv})?"));
        }
        body.push(' ');
    }
    body.push_str("\"}\" ws");
    rules.push(format!("{id} ::= {body}"));
    id
}

/// `c_i ::= kv_i ("," ws c_{i+1})? | c_{i+1}` — cualquier subconjunto
/// no-vacío de los opcionales, en orden. `None` si no hay opcionales.
fn optional_chain(
    opt_kvs: &[String],
    rules: &mut Vec<String>,
    fresh: &mut usize,
) -> Option<String> {
    let mut next: Option<String> = None;
    for kv in opt_kvs.iter().rev() {
        let id = fresh_id(fresh);
        match &next {
            None => rules.push(format!("{id} ::= {kv}")),
            Some(n) => rules.push(format!("{id} ::= {kv} (\",\" ws {n})? | {n}")),
        }
        next = Some(id);
    }
    next
}

/// Gramática del envelope de qwen2.5, activada tras el literal
/// `<tool_call>`: fuerza `{"name": <tool>, "arguments": <args>}` +
/// `</tool_call>`, con branch por tool — **el nombre elegido determina
/// la gramática de sus args**, derivada del `input_schema` (campos
/// requeridos forzados en el orden de la lista `required`, tipos y
/// enums cerrados, args no declarados ingenerables). Tools sin schema
/// caen al objeto JSON genérico. Los nombres alucinados y los args
/// no-conformes mueren en el sampler, no en la validación.
///
/// `None` si no hay tools (sin inventario no hay call que estencilar).
pub(crate) fn qwen_call_grammar(tools: &[ToolGrammarSpec]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let mut rules = Vec::new();
    let mut fresh = 0usize;
    let mut branches = Vec::new();
    for (i, tool) in tools.iter().enumerate() {
        let args = tool.schema.as_ref().map_or("object".to_string(), |s| {
            schema_rule(s, &mut rules, &mut fresh)
        });
        let branch = format!("c{i}");
        rules.push(format!(
            "{branch} ::= \"\\\"{}\\\"\" ws \",\" ws \"\\\"arguments\\\"\" ws \":\" ws {args}",
            escape_gbnf_literal(&tool.name)
        ));
        branches.push(branch);
    }
    Some(format!(
        "root ::= ws \"{{\" ws \"\\\"name\\\"\" ws \":\" ws ({}) ws \"}}\" ws \"</tool_call>\"\n{}\n{JSON_RULES}",
        branches.join(" | "),
        rules.join("\n"),
    ))
}

/// Gramática de los argumentos de una call Harmony, activada cuando el
/// header ya fijó el destinatario (`to=functions.X <|constrain|>json
/// <|message|>`). El nombre ya quedó atrás (en el header), así que acá
/// se selecciona la gramática de args **derivada del schema de esa
/// tool**; destinatario desconocido o sin schema cae al objeto JSON
/// genérico. El `<|call|>` de cierre lo emite el modelo al soltarse el
/// constraint.
pub(crate) fn harmony_args_grammar(tool_name: &str, tools: &[ToolGrammarSpec]) -> String {
    let mut rules = Vec::new();
    let mut fresh = 0usize;
    let args = tools
        .iter()
        .find(|t| t.name == tool_name)
        .and_then(|t| t.schema.as_ref())
        .map_or("object".to_string(), |s| {
            schema_rule(s, &mut rules, &mut fresh)
        });
    format!("root ::= ws {args}\n{}\n{JSON_RULES}", rules.join("\n"))
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

    fn spec(name: &str, schema: Option<serde_json::Value>) -> ToolGrammarSpec {
        ToolGrammarSpec {
            name: name.to_string(),
            schema,
        }
    }

    fn write_file_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "append": { "type": "boolean" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    #[test]
    fn qwen_grammar_branches_per_tool_with_schema_derived_args() {
        let g = qwen_call_grammar(&[
            spec("write_file", Some(write_file_schema())),
            spec("mystery", None),
        ])
        .expect("con tools hay gramática");
        // Branch por tool: el nombre elegido fija la gramática de args.
        assert!(g.contains("(c0 | c1)"));
        assert!(g.contains(r#"c0 ::= "\"write_file\"""#));
        // La tool sin schema cae al objeto genérico.
        assert!(g.contains(r#"c1 ::= "\"mystery\"" ws "," ws "\"arguments\"" ws ":" ws object"#));
        assert!(g.contains(r#""</tool_call>""#));
        // Requeridos en el orden de la lista `required`: path antes que
        // content (el orden alfabético del map diría content primero).
        let path_kv = g.find(r#"::= "\"path\"""#).expect("kv de path");
        let content_kv = g.find(r#"::= "\"content\"""#).expect("kv de content");
        assert!(path_kv < content_kv || g[..content_kv].contains("path"));
        // El opcional va como sufijo skippeable.
        assert!(g.contains(r#"("," ws"#));
    }

    #[test]
    fn schema_object_orders_required_by_list_and_makes_optionals_skippable() {
        let mut rules = Vec::new();
        let mut fresh = 0;
        let root = schema_rule(&write_file_schema(), &mut rules, &mut fresh);
        let all = rules.join("\n");
        let obj = rules
            .iter()
            .find(|r| r.starts_with(&format!("{root} ::=")))
            .expect("regla del objeto");
        // path (r1) requerido antes que content; append opcional con
        // `("," ws …)?`.
        assert!(obj.contains("\",\" ws"));
        assert!(obj.contains(")?"));
        assert!(all.contains(r#""\"append\"" ws ":" ws boolean"#));
        assert!(all.contains(r#""\"path\"" ws ":" ws string"#));
    }

    #[test]
    fn schema_all_optional_uses_the_chain_without_leading_comma() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "integer" } }
        });
        let mut rules = Vec::new();
        let mut fresh = 0;
        let root = schema_rule(&schema, &mut rules, &mut fresh);
        let obj = rules
            .iter()
            .find(|r| r.starts_with(&format!("{root} ::=")))
            .unwrap();
        // El objeto permite vacío y la cadena arranca sin coma.
        assert!(obj.contains(")? \"}\""));
        assert!(rules.iter().any(|r| r.contains("| r")));
    }

    #[test]
    fn schema_enum_and_scalars_map_to_closed_rules() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "enum": ["create", "append"] },
                "count": { "type": "integer" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "weird": { "anyOf": [{ "type": "string" }] }
            },
            "required": ["mode"]
        });
        let mut rules = Vec::new();
        let mut fresh = 0;
        schema_rule(&schema, &mut rules, &mut fresh);
        let all = rules.join("\n");
        assert!(all.contains(r#"("\"create\"" | "\"append\"") ws"#));
        assert!(all.contains(r#"":" ws integer"#));
        assert!(all.contains(r#""[" ws (string ("," ws string)*)? "]" ws"#));
        // Lo no soportado degrada al `value` genérico, nunca restringe mal.
        assert!(all.contains(r#"":" ws value"#));
    }

    #[test]
    fn qwen_grammar_without_tools_is_none() {
        assert!(qwen_call_grammar(&[]).is_none());
    }

    #[test]
    fn qwen_grammar_escapes_hostile_names() {
        let g = qwen_call_grammar(&[spec("a\"b\\c", None)]).unwrap();
        assert!(g.contains(r#"a\"b\\c"#));
    }

    #[test]
    fn harmony_grammar_selects_the_recipients_schema() {
        let tools = [
            spec("write_file", Some(write_file_schema())),
            spec("mystery", None),
        ];
        let g = harmony_args_grammar("write_file", &tools);
        assert!(g.contains(r#""\"path\"" ws ":" ws string"#));
        assert!(!g.starts_with("root ::= ws object"));
        // Destinatario sin schema (o desconocido) → objeto genérico.
        assert!(harmony_args_grammar("mystery", &tools).starts_with("root ::= ws object"));
        assert!(harmony_args_grammar("nope", &tools).starts_with("root ::= ws object"));
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
