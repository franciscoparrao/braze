//! La escalera de rescate textual de tool calls — P1.1 paso 1 del split
//! de `engine.rs` (docs/AUDITORIA-2026-07-v8.md § 3). Extraída VERBATIM
//! de `engine.rs` (2026-07-18): parsers puros, cero `&self`, cero
//! async — la frontera de menor riesgo del split.
//!
//! Qué vive aquí: el rescate de tool calls que un modelo chico emite
//! como texto en vez de `tool_calls` estructurado, por familia —
//! `<tool_call>{json}` (Qwen/Hermes), gramática XML `<function=...>`
//! (qwen3-coder), `<arg_key>/<arg_value>` (GLM), pythonic
//! `[func(a=1)]` (Llama), el envelope de prompt-tools (brazo B/C), el
//! JSON desnudo con o sin fences, y la coerción de argumentos
//! stringificados al schema (`coerce_arguments_to_schema`). El
//! principio compartido: nunca confundir prosa con una call, y ante
//! contenido malformado dejarlo en el texto en vez de inventar una
//! reparación (ver cada parser para su contrato exacto).
//!
//! Los tests unitarios de estos parsers siguen por ahora en el
//! `mod tests` de `engine.rs` (junto a los de integración async que se
//! entrelazan con ellos); migran cuando el split llegue al módulo de
//! tests — mismo criterio incremental que el resto del P1.1.

use braze_types::ToolCall;

/// Best-effort rescue of a tool call a model emitted as plain text instead
/// of a structured `tool_calls` entry — e.g. `{"name": "read_file",
/// "arguments": {"path": "x.txt"}}`, optionally wrapped in a ```json
/// fence. Returns `None` (not an error) for anything that doesn't parse as
/// such — most final text responses legitimately aren't JSON at all, and
/// this must never mistake prose for a tool call. The *whole* response
/// must be the JSON (modulo fences): with no explicit markers, prose
/// around a JSON-looking fragment is too ambiguous to touch. For the
/// explicitly tagged variant that does admit surrounding prose, see
/// [`extract_tagged_tool_calls`].
pub(crate) fn try_parse_textual_tool_call(text: &str) -> Option<ToolCall> {
    parse_tool_call_json(trim_json_fences(text))
}

/// A parsed prompt-tools *envelope* — the response format
/// `OllamaBackend`'s prompt-tools/constrained modes instruct the model to
/// emit (docs/constrained-decoding-ab-design.md § "Mecanismo mínimo"):
/// one JSON object that is either a tool call or the final answer, with
/// an optional in-schema `reasoning` field as the model's thinking space.
pub(crate) enum EnvelopeResponse {
    ToolCall {
        call: ToolCall,
        /// Preserved as the round's text (the model narrating before a
        /// call is the normal shape of a native round too).
        reasoning: Option<String>,
    },
    FinalAnswer {
        /// Replaces the raw envelope JSON as the round's text — the
        /// `reasoning` field is deliberately dropped here: it was the
        /// model's scratchpad, and the declared answer is `text`.
        text: String,
    },
}

/// Parses a whole response (modulo ```json fences) as an envelope.
/// `None` for anything else — prose, non-envelope JSON (which must stay
/// eligible for the rescue ladder), an unknown `action`, or an envelope
/// with the wrong field types. Lenient in exactly one place: a
/// `tool_call` without `arguments` gets `{}` — the unconstrained (B)
/// arm's models omit it for no-arg tools often enough that rejecting it
/// would measure strictness, not modality. The synthesized id mirrors the
/// rescue ladder's (unique within the session log; no backend id ever
/// existed), with its own prefix so a transcript reader can tell the
/// channels apart.
pub(crate) fn parse_envelope_response(text: &str) -> Option<EnvelopeResponse> {
    let value: serde_json::Value = serde_json::from_str(trim_json_fences(text)).ok()?;
    let reasoning = value
        .get("reasoning")
        .and_then(serde_json::Value::as_str)
        .filter(|reasoning| !reasoning.trim().is_empty())
        .map(str::to_string);
    match value.get("action")?.as_str()? {
        "tool_call" => {
            let name = value.get("name")?.as_str()?.to_string();
            let arguments = match value.get("arguments") {
                Some(arguments) if arguments.is_object() => arguments.clone(),
                Some(_) => return None,
                None => serde_json::json!({}),
            };
            Some(EnvelopeResponse::ToolCall {
                call: ToolCall {
                    id: format!("envelope-{}", uuid::Uuid::new_v4()),
                    name,
                    arguments,
                },
                reasoning,
            })
        }
        "final_answer" => Some(EnvelopeResponse::FinalAnswer {
            text: value.get("text")?.as_str()?.to_string(),
        }),
        _ => None,
    }
}

/// Strips an optional ```json / ``` fence (and surrounding whitespace)
/// from a candidate JSON fragment — shared by both textual-rescue
/// formats: some models fence the bare-JSON variant, and some fence the
/// JSON *inside* their `<tool_call>` tags too.
pub(crate) fn trim_json_fences(text: &str) -> &str {
    text.trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

/// The shared shape check for every textual-rescue format: a JSON object
/// with a string `name` and an object `arguments` (or `parameters`, the
/// synonym some templates use). `None` for anything else — never an
/// error, since the caller treats "doesn't parse" as "not a tool call".
///
/// The synthesized id only needs to be unique within this session's event
/// log (for `tool_use`/`tool_result` correlation) — a real backend id
/// never applies here since none was ever assigned.
pub(crate) fn parse_tool_call_json(candidate: &str) -> Option<ToolCall> {
    let value: serde_json::Value = serde_json::from_str(candidate).ok()?;
    let name = value.get("name")?.as_str()?.to_string();
    let arguments = value
        .get("arguments")
        .or_else(|| value.get("parameters"))?
        .clone();
    if !arguments.is_object() || looks_like_json_schema_definition(&arguments) {
        return None;
    }
    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name,
        arguments,
    })
}

/// `true` when `value` has the shape of a JSON-Schema object
/// (`{"type":"object","properties":{...}}`) rather than actual tool-call
/// arguments — F1 (docs/AUDITORIA-2026-07-v3.md): `parameters` doubles as
/// OpenAI's name for both a tool call's *arguments* and a tool
/// *definition*'s schema, so `{"name":"get_weather","parameters":
/// {"type":"object","properties":{...}}}` — the single most common shape
/// in tool-calling documentation, a very plausible response to "explain
/// how to define a tool" — would otherwise pass the shape check (an
/// object) and get despatched with the schema as if it were the
/// arguments. Real tool-call arguments essentially never carry both a
/// literal `type: "object"` field and a `properties` field at their own
/// top level.
fn looks_like_json_schema_definition(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    matches!(obj.get("type"), Some(serde_json::Value::String(t)) if t == "object")
        && obj.contains_key("properties")
}

/// Rescue for the tagged textual format the Qwen family (and other
/// Hermes-template models) emits natively when its tool-calling template
/// isn't honored end-to-end: `<tool_call>\n{"name": ..., "arguments":
/// ...}\n</tool_call>` — per the Qwen technical report, this is the
/// single highest-leverage textual format for small local models
/// (docs/SOTA-2026-07.md, técnica G6). Unlike the bare-JSON rescue, the
/// explicit tags make surrounding prose unambiguous, so this admits (and
/// preserves) text around the blocks and accepts *several* blocks in one
/// response (Qwen emits one pair of tags per call for parallel calls).
///
/// Returns the parsed calls plus the response text with the parsed
/// blocks removed — the model's surrounding prose is still its text for
/// the round (`run_turn` already persists round text before its tool
/// calls). A tagged block whose inner JSON doesn't parse as a tool call
/// stays in the text verbatim rather than being swallowed; an empty
/// `Vec` means no rescue at all, and the caller must leave the text
/// untouched.
pub(crate) fn extract_tagged_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    const OPEN_TAG: &str = "<tool_call>";
    const CLOSE_TAG: &str = "</tool_call>";

    let mut calls = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN_TAG) {
        let after_open = &rest[start + OPEN_TAG.len()..];
        let Some(inner_len) = after_open.find(CLOSE_TAG) else {
            // Unclosed tag (e.g. the round got cut off mid-block): stop
            // scanning; the final push below keeps everything from the
            // dangling tag onward in the text, visible instead of lost.
            break;
        };
        let block_end = start + OPEN_TAG.len() + inner_len + CLOSE_TAG.len();
        let inner = &after_open[..inner_len];

        // F1 (docs/AUDITORIA-2026-07-v3.md): a block sitting inside a
        // fenced code sample is the model *showing* the format (e.g.
        // answering "how does Qwen emit tool calls?"), not a real leaked
        // attempt — a genuine leak is never fenced, since fencing it
        // would be a successful, intentional act of formatting, not the
        // failure to honor the tool-call template this rescue exists to
        // recover from.
        let absolute_start = text.len() - rest.len() + start;
        if is_inside_code_fence(text, absolute_start) {
            remaining.push_str(&rest[..block_end]);
            rest = &rest[block_end..];
            continue;
        }

        // The wrapper admits three inner grammars: qwen2.5's JSON object,
        // qwen3-coder's `<function=...>` XML (which its template nests
        // inside the same `<tool_call>` tags), and z-ai/glm-5.2's
        // `name<arg_key>K</arg_key><arg_value>V</arg_value>...` tags
        // (docs/usability-log-2026-07-07-si2.md, hallazgo U-15 — observed
        // via OpenRouter when GLM's native tool-calling template isn't
        // honored end-to-end).
        match parse_tool_call_json(trim_json_fences(inner))
            .or_else(|| parse_function_xml_tool_call(inner))
            .or_else(|| parse_glm_arg_tag_tool_call(inner))
        {
            Some(call) => {
                calls.push(call);
                remaining.push_str(&rest[..start]);
            }
            // Malformed inner content: keep the whole tagged block in
            // the text — clearly *meant* as a tool call, but inventing a
            // repair here risks running something the model didn't say.
            None => remaining.push_str(&rest[..block_end]),
        }
        rest = &rest[block_end..];
    }
    if calls.is_empty() {
        return (calls, text.to_string());
    }
    remaining.push_str(rest);
    (calls, remaining.trim().to_string())
}

/// `true` when `offset` (a byte index into `text`) falls inside a
/// ``` ... ``` fenced region — toggled each time a literal "```" marker
/// is seen (doesn't require the fence to be alone on its own line; models
/// are consistent enough about this marker that the simpler check is
/// worth the tiny false-positive risk of a stray triple-backtick in
/// prose). Used to keep the tagged/XML rescues from firing on a fenced
/// example rather than a genuine leaked tool call (hallazgo F1).
pub(crate) fn is_inside_code_fence(text: &str, offset: usize) -> bool {
    let mut in_fence = false;
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find("```") {
        let idx = search_from + rel;
        if idx >= offset {
            break;
        }
        in_fence = !in_fence;
        search_from = idx + 3;
    }
    in_fence
}

/// Rescue for *bare* `<function=...>` blocks — qwen3-coder's XML grammar
/// emitted without its usual `<tool_call>` wrapper (observed leak mode
/// when the template isn't honored end-to-end). Same contract as
/// [`extract_tagged_tool_calls`]: parsed blocks are removed, surrounding
/// prose is preserved, malformed blocks stay in the text, empty `Vec`
/// means "leave the text untouched".
pub(crate) fn extract_function_xml_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    const OPEN_MARK: &str = "<function=";
    const CLOSE_TAG: &str = "</function>";

    let mut calls = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN_MARK) {
        let Some(inner_len) = rest[start..].find(CLOSE_TAG) else {
            break; // unclosed: the final push keeps everything visible
        };
        let block_end = start + inner_len + CLOSE_TAG.len();

        // F1 (docs/AUDITORIA-2026-07-v3.md): see the identical check in
        // `extract_tagged_tool_calls` — a fenced occurrence is the model
        // showing an example, not a real leaked call.
        let absolute_start = text.len() - rest.len() + start;
        if is_inside_code_fence(text, absolute_start) {
            remaining.push_str(&rest[..block_end]);
            rest = &rest[block_end..];
            continue;
        }

        match parse_function_xml_tool_call(&rest[start..block_end]) {
            Some(call) => {
                calls.push(call);
                remaining.push_str(&rest[..start]);
            }
            None => remaining.push_str(&rest[..block_end]),
        }
        rest = &rest[block_end..];
    }
    if calls.is_empty() {
        return (calls, text.to_string());
    }
    remaining.push_str(rest);
    (calls, remaining.trim().to_string())
}

/// Parses one qwen3-coder function-XML block (docs/SOTA-2026-07.md §
/// Adenda — the grammar designed to avoid JSON escaping in
/// code-carrying arguments):
///
/// ```text
/// <function=read_file>
/// <parameter=path>
/// x.txt
/// </parameter>
/// </function>
/// ```
///
/// Parameter values are kept as **strings** (trimmed of the
/// template's surrounding newlines) unless the whole value is clearly
/// structured (starts with `{`/`[` and parses as JSON) — this
/// project's tool arguments are overwhelmingly strings, and coercing a
/// scalar-looking value (`"42"`, `"true"`) into a JSON number/bool
/// would break a `path: String`-style schema downstream, the exact
/// kind of silent damage a rescue must not cause. `None` for anything
/// that doesn't match the grammar.
pub(crate) fn parse_function_xml_tool_call(block: &str) -> Option<ToolCall> {
    let trimmed = block.trim();
    let rest = trimmed.strip_prefix("<function=")?;
    let (name, body) = rest.split_once('>')?;
    let name = name.trim();
    if name.is_empty() || name.contains(['<', '\n']) {
        return None;
    }
    let body = body.strip_suffix("</function>")?;

    let mut arguments = serde_json::Map::new();
    let mut cursor = body;
    while let Some(param_start) = cursor.find("<parameter=") {
        let after_mark = &cursor[param_start + "<parameter=".len()..];
        let (key, after_key) = after_mark.split_once('>')?;
        let key = key.trim();
        let value_len = after_key.find("</parameter>")?;
        if key.is_empty() || key.contains(['<', '\n']) {
            return None;
        }
        let raw_value = after_key[..value_len].trim();
        let value = if raw_value.starts_with(['{', '[']) {
            serde_json::from_str(raw_value)
                .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()))
        } else {
            serde_json::Value::String(raw_value.to_string())
        };
        arguments.insert(key.to_string(), value);
        cursor = &after_key[value_len + "</parameter>".len()..];
    }

    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Parses `z-ai/glm-5.2`'s tool-call grammar as observed inside the
/// shared `<tool_call>...</tool_call>` wrapper (docs/usability-log-2026-07-07-si2.md,
/// hallazgo U-15): the tool name as bare text, followed by zero or more
/// `<arg_key>NAME</arg_key><arg_value>VALUE</arg_value>` pairs — distinct
/// from both qwen2.5's JSON payload and qwen3-coder's `<function=...>`
/// XML the same wrapper already covers:
///
/// ```text
/// <tool_call>read_file<arg_key>limit</arg_key><arg_value>120</arg_value><arg_key>offset</arg_key><arg_value>63</arg_value></tool_call>
/// ```
///
/// Requires at least one `<arg_key>` pair to fire — a bare name with no
/// tags at all is indistinguishable from ordinary prose (the same
/// ambiguity [`extract_pythonic_tool_calls`]'s doc comment calls out for
/// unbracketed `name(...)`), so a genuinely argument-less tool call in
/// this grammar isn't rescued; nothing in the observed leak shows that
/// shape yet. Argument values are kept as strings (trimmed) unless
/// clearly structured JSON (starts with `{`/`[` and parses) — same rule
/// [`parse_function_xml_tool_call`] uses, for the same reason: coercing a
/// scalar-looking value into a JSON number/bool would break a
/// `path: String`-style schema downstream. `None` for anything that
/// doesn't match the grammar, including a key/value pair that isn't
/// immediately adjacent (only whitespace between `</arg_key>` and the
/// next `<arg_value>`) — inventing a repair for a shape this rescue
/// wasn't built for risks running something the model didn't say.
pub(crate) fn parse_glm_arg_tag_tool_call(inner: &str) -> Option<ToolCall> {
    const KEY_OPEN: &str = "<arg_key>";
    const KEY_CLOSE: &str = "</arg_key>";
    const VALUE_OPEN: &str = "<arg_value>";
    const VALUE_CLOSE: &str = "</arg_value>";

    let first_tag = inner.find(KEY_OPEN)?;
    let name = inner[..first_tag].trim();
    if name.is_empty() || name.contains(['<', '\n']) {
        return None;
    }

    let mut arguments = serde_json::Map::new();
    let mut cursor = &inner[first_tag..];
    while let Some(key_start) = cursor.find(KEY_OPEN) {
        let after_key_open = &cursor[key_start + KEY_OPEN.len()..];
        let key_len = after_key_open.find(KEY_CLOSE)?;
        let key = after_key_open[..key_len].trim();
        if key.is_empty() || key.contains(['<', '\n']) {
            return None;
        }
        let after_key = &after_key_open[key_len + KEY_CLOSE.len()..];

        let value_start = after_key.find(VALUE_OPEN)?;
        if !after_key[..value_start].trim().is_empty() {
            return None;
        }
        let after_value_open = &after_key[value_start + VALUE_OPEN.len()..];
        let value_len = after_value_open.find(VALUE_CLOSE)?;
        let raw_value = after_value_open[..value_len].trim();
        let value = if raw_value.starts_with(['{', '[']) {
            serde_json::from_str(raw_value)
                .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()))
        } else {
            serde_json::Value::String(raw_value.to_string())
        };
        arguments.insert(key.to_string(), value);

        cursor = &after_value_open[value_len + VALUE_CLOSE.len()..];
    }

    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Rescue for Llama 3.x's native "pythonic" tool-call format — distinct
/// from Qwen's tagged/XML formats above: one or more comma-separated
/// `name(key=value, ...)` call expressions wrapped in a single pair of
/// square brackets, e.g. `[get_weather(city="SF", metric="celsius")]`.
/// See docs/AUDITORIA-2026-07-v3.md, hallazgo C2 — the rescue escalera
/// covered Qwen's two native formats but nothing for Llama, one of the
/// most commonly installed local model families via Ollama.
///
/// Same contract as [`extract_tagged_tool_calls`]/[`extract_function_xml_tool_calls`]:
/// parsed calls are removed from the returned text, surrounding prose is
/// preserved verbatim, and a bracketed block that doesn't parse cleanly
/// is left in the text rather than guessed at (`None` for anything not
/// unambiguously a call: an unrecognized argument shape fails the whole
/// call, not just that one argument). The bracket wrapper is what makes
/// this safe to scan for unprompted — plain prose describing a function
/// call rarely wraps it in literal `[...]`, unlike a bare `name(...)`
/// pattern which risks matching prose like "call read_file(path) to
/// check" — the exact ambiguity this project's other rescues are careful
/// to avoid.
pub(crate) fn extract_pythonic_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    let mut calls = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;

    while let Some(start) = find_pythonic_block_start(rest) {
        let Some(close_offset) = matching_bracket_end(&rest[start..]) else {
            break; // unclosed '[': stop scanning, leave the rest visible
        };
        let block_end = start + close_offset + 1; // include the ']'
        // J-8 (docs/AUDITORIA-2026-07-v7.md): same fence check the tagged
        // and `<function=` rungs apply (F1) — a `[get_weather(...)]`
        // QUOTED inside a markdown code fence is the model showing an
        // example ("así emite Llama sus tool calls"), not making a call.
        // This rung was the only one missing it, so a fenced example got
        // extracted and dispatched for real.
        let absolute_start = text.len() - rest.len() + start;
        if is_inside_code_fence(text, absolute_start) {
            remaining.push_str(&rest[..block_end]);
            rest = &rest[block_end..];
            continue;
        }
        let inner = &rest[start + 1..block_end - 1];
        match parse_pythonic_calls(inner) {
            Some(parsed) => {
                calls.extend(parsed);
                remaining.push_str(&rest[..start]);
            }
            // Malformed inner content: keep the whole bracketed block in
            // the text — clearly *meant* as a tool call, but inventing a
            // repair here risks running something the model didn't say.
            None => remaining.push_str(&rest[..block_end]),
        }
        rest = &rest[block_end..];
    }

    if calls.is_empty() {
        return (calls, text.to_string());
    }
    remaining.push_str(rest);
    (calls, remaining.trim().to_string())
}

/// Finds the byte offset of a `[` in `text` that's immediately (modulo
/// whitespace) followed by what looks like the start of a call
/// expression (`identifier(`) — the signal that this bracket is a
/// pythonic tool-call wrapper, not an unrelated list literal or markdown
/// link.
fn find_pythonic_block_start(text: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find('[') {
        let idx = search_from + rel;
        if looks_like_pythonic_call_start(&text[idx + 1..]) {
            return Some(idx);
        }
        search_from = idx + 1;
    }
    None
}

/// `true` when `s` (the text right after a candidate `[`) starts with an
/// identifier immediately followed by `(` — allowing leading whitespace
/// before the identifier, none between the identifier and `(`.
fn looks_like_pythonic_call_start(s: &str) -> bool {
    let s = s.trim_start();
    let ident_len: usize = s
        .char_indices()
        .take_while(|&(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic() || c == '_'
            } else {
                c.is_ascii_alphanumeric() || c == '_'
            }
        })
        .count();
    ident_len > 0 && s[ident_len..].starts_with('(')
}

/// Given `s` starting with `[`, finds the byte offset (within `s`) of the
/// matching `]` — tracking nested `[`/`]` depth and skipping
/// bracket-like characters inside quoted string arguments (`"..."`/
/// `'...'`, with backslash-escaped quotes). `None` if never closed.
fn matching_bracket_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits `s` on top-level occurrences of `sep` — one not nested inside
/// `(...)`/`[...]` or a quoted string. Shared by [`extract_pythonic_tool_calls`]'s
/// two split points: several calls inside the outer brackets, and
/// `key=value` pairs inside one call's parens.
pub(crate) fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;

    for (i, c) in s.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Parses `inner` (the content between the outer `[`/`]`) as one or more
/// top-level `name(args)` call expressions. `None` if any expression
/// doesn't unambiguously parse as a call — the whole bracketed block is
/// then left as text by the caller rather than partially rescued.
fn parse_pythonic_calls(inner: &str) -> Option<Vec<ToolCall>> {
    let mut calls = Vec::new();
    for expr in split_top_level(inner, ',') {
        let expr = expr.trim();
        if expr.is_empty() {
            continue;
        }
        calls.push(parse_pythonic_call(expr)?);
    }
    if calls.is_empty() { None } else { Some(calls) }
}

/// Parses one `name(key=value, ...)` call expression.
fn parse_pythonic_call(expr: &str) -> Option<ToolCall> {
    let open = expr.find('(')?;
    let name = expr[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = expr[open..].trim();
    let args_str = rest.strip_prefix('(')?.strip_suffix(')')?;

    let mut arguments = serde_json::Map::new();
    for pair in split_top_level(args_str, ',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (key, value_str) = pair.split_once('=')?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        arguments.insert(key.to_string(), parse_pythonic_value(value_str.trim())?);
    }

    Some(ToolCall {
        id: format!("rescued-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Parses one pythonic scalar literal: a quoted string, `true`/`false`
/// (Python or JSON casing), or a number. No lists/dicts/`None` —
/// deliberately scoped to hallazgo C2's ask (string/number/bool); an
/// argument shaped like anything else fails the whole call rather than
/// guessing at a representation.
fn parse_pythonic_value(s: &str) -> Option<serde_json::Value> {
    if let Some(unquoted) = strip_matching_quotes(s) {
        return Some(serde_json::Value::String(unquoted));
    }
    match s {
        "true" | "True" => return Some(serde_json::Value::Bool(true)),
        "false" | "False" => return Some(serde_json::Value::Bool(false)),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(serde_json::Value::Number(n.into()));
    }
    if let Ok(n) = s.parse::<f64>()
        && let Some(num) = serde_json::Number::from_f64(n)
    {
        return Some(serde_json::Value::Number(num));
    }
    None
}

/// Strips a matching pair of leading/trailing `"`/`'` quotes and
/// unescapes `\"`/`\'`/`\\` — the minimal escape set; anything else
/// (`\n`, etc.) passes through literally rather than guessing at Python
/// string-escape semantics. `None` if `s` isn't quoted (or the quotes
/// don't match).
fn strip_matching_quotes(s: &str) -> Option<String> {
    if s.len() < 2 {
        return None;
    }
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    if !s.ends_with(quote) {
        return None;
    }
    let inner = &s[quote.len_utf8()..s.len() - quote.len_utf8()];

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&next) if next == quote || next == '\\' => {
                    out.push(next);
                    chars.next();
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    Some(out)
}

/// Coerces `arguments` in place to match `schema`'s declared property
/// types, one level deep — narrowly targeted at
/// [`parse_function_xml_tool_call`]'s grammar, which has no native
/// number/boolean type, so every scalar param comes back as a JSON
/// string, and a code-carrying string value can come back mis-parsed as
/// a JSON object (docs/AUDITORIA-2026-07-v3.md, hallazgo F2). Two
/// directions:
/// - a string where the schema declares `integer`/`number`/`boolean` is
///   parsed into that type; left untouched if parsing fails — schema
///   validation, not this function, is what surfaces the real error to
///   the model;
/// - an object/array where the schema declares `string` is re-serialized
///   back to compact JSON text (the mirror-image mistake: the grammar
///   treats any value starting with `{`/`[` as structured, so
///   `<parameter=content>{"a":1}</parameter>` for a `content: string`
///   field parses as an object instead of the literal text).
///
/// A no-op for arguments that already match their schema's declared
/// types — the common case for wire-sourced tool calls, whose backend
/// already sends correctly-typed JSON — so this is safe to call
/// unconditionally before validating/dispatching any call, rescued or
/// not.
pub(crate) fn coerce_arguments_to_schema(arguments: &mut serde_json::Value, schema: &serde_json::Value) {
    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    let Some(args) = arguments.as_object_mut() else {
        return;
    };

    for (key, prop_schema) in properties {
        let Some(value) = args.get_mut(key) else {
            continue;
        };
        let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) else {
            continue;
        };

        if let Some(s) = value.as_str() {
            let s = s.trim().to_string();
            match expected_type {
                "integer" => {
                    if let Ok(n) = s.parse::<i64>() {
                        *value = serde_json::Value::Number(n.into());
                    }
                }
                "number" => {
                    if let Ok(n) = s.parse::<f64>()
                        && let Some(num) = serde_json::Number::from_f64(n)
                    {
                        *value = serde_json::Value::Number(num);
                    }
                }
                "boolean" => match s.as_str() {
                    "true" => *value = serde_json::Value::Bool(true),
                    "false" => *value = serde_json::Value::Bool(false),
                    _ => {}
                },
                _ => {}
            }
        } else if matches!(
            value,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
        ) && expected_type == "string"
            && let Ok(text) = serde_json::to_string(value)
        {
            *value = serde_json::Value::String(text);
        }
    }
}
