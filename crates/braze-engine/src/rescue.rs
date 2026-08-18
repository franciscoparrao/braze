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
pub(crate) fn coerce_arguments_to_schema(
    arguments: &mut serde_json::Value,
    schema: &serde_json::Value,
) {
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


#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 resto (v9 L-5, 2026-08-18): tests unitarios de la escalera
    // movidos VERBATIM del `mod tests` de engine/mod.rs — la migración
    // que el module doc de arriba anticipaba ("migran cuando el split
    // llegue al módulo de tests"). Solo parsers puros; los de
    // integración async siguen con sus módulos del engine.

    // --- coerce_arguments_to_schema (hallazgo F2) ---

    fn limit_and_flag_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "integer"},
                "ratio": {"type": "number"},
                "recursive": {"type": "boolean"},
            },
        })
    }

    #[test]
    fn coerces_a_stringified_integer_to_a_number() {
        let mut args = serde_json::json!({"path": "x", "limit": "50"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["limit"], serde_json::json!(50));
    }

    #[test]
    fn coerces_a_stringified_float_to_a_number() {
        let mut args = serde_json::json!({"ratio": "0.5"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["ratio"], serde_json::json!(0.5));
    }

    #[test]
    fn coerces_stringified_booleans() {
        let mut args = serde_json::json!({"recursive": "true"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["recursive"], serde_json::json!(true));

        let mut args = serde_json::json!({"recursive": "false"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["recursive"], serde_json::json!(false));
    }

    #[test]
    fn an_unparseable_string_is_left_untouched_for_validation_to_reject() {
        let mut args = serde_json::json!({"limit": "not a number"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["limit"], serde_json::json!("not a number"));
    }

    #[test]
    fn a_json_object_is_re_serialized_to_a_string_when_the_schema_wants_one() {
        // The mirror-image mistake: `<parameter=path>{"a":1}</parameter>`
        // parses as a JSON object because the XML grammar treats any
        // value starting with `{`/`[` as structured — but the schema says
        // `path` is a string.
        let mut args = serde_json::json!({"path": {"a": 1}});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["path"], serde_json::json!(r#"{"a":1}"#));
    }

    #[test]
    fn already_correctly_typed_arguments_are_left_alone() {
        // The common case (wire-sourced calls): coercion must be a no-op.
        let mut args = serde_json::json!({
            "path": "x", "limit": 50, "ratio": 0.5, "recursive": true
        });
        let before = args.clone();
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args, before);
    }

    #[test]
    fn a_string_value_for_a_string_field_is_left_alone() {
        let mut args = serde_json::json!({"path": "src/main.rs"});
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args["path"], serde_json::json!("src/main.rs"));
    }

    #[test]
    fn a_non_object_schema_or_arguments_is_a_no_op() {
        let mut args = serde_json::json!("not an object");
        coerce_arguments_to_schema(&mut args, &limit_and_flag_schema());
        assert_eq!(args, serde_json::json!("not an object"));

        let mut args = serde_json::json!({"limit": "50"});
        coerce_arguments_to_schema(&mut args, &serde_json::json!({"type": "object"}));
        assert_eq!(args["limit"], serde_json::json!("50"));
    }

    // --- try_parse_textual_tool_call (hallazgo B5) ---

    #[test]
    fn parses_a_bare_json_tool_call() {
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "read_file", "arguments": {"path": "x.txt"}}"#)
                .expect("should parse");
        assert_eq!(rescued.name, "read_file");
        assert_eq!(rescued.arguments, serde_json::json!({"path": "x.txt"}));
    }

    #[test]
    fn parses_a_tool_call_fenced_in_json_code_block() {
        let text = "```json\n{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}\n```";
        let rescued = try_parse_textual_tool_call(text).expect("should parse");
        assert_eq!(rescued.name, "echo");
    }

    #[test]
    fn parses_a_tool_call_fenced_in_a_bare_code_block() {
        let text = "```\n{\"name\": \"echo\", \"arguments\": {}}\n```";
        let rescued = try_parse_textual_tool_call(text).expect("should parse");
        assert_eq!(rescued.name, "echo");
    }

    #[test]
    fn accepts_parameters_as_a_synonym_for_arguments() {
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "echo", "parameters": {"text": "hi"}}"#)
                .expect("should parse");
        assert_eq!(rescued.arguments, serde_json::json!({"text": "hi"}));
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_a_tool_call() {
        assert!(try_parse_textual_tool_call("El archivo tiene 3 lineas.").is_none());
    }

    #[test]
    fn json_without_a_name_field_is_not_a_tool_call() {
        assert!(try_parse_textual_tool_call(r#"{"arguments": {"path": "x.txt"}}"#).is_none());
    }

    #[test]
    fn non_object_arguments_are_rejected() {
        assert!(
            try_parse_textual_tool_call(r#"{"name": "echo", "arguments": "just a string"}"#)
                .is_none()
        );
    }

    // --- F1 (docs/AUDITORIA-2026-07-v3.md): reject OpenAI-style tool
    // *definitions* masquerading as a call via `parameters` ---

    #[test]
    fn an_openai_style_tool_definition_is_not_mistaken_for_a_call() {
        let text = r#"{"name": "get_weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}"#;
        assert!(
            try_parse_textual_tool_call(text).is_none(),
            "a JSON-Schema-shaped `parameters` must not be treated as arguments"
        );
    }

    #[test]
    fn a_genuine_object_typed_argument_named_type_is_still_accepted() {
        // Must not over-trigger: a real argument object that happens to
        // have a `type` field but no `properties` is not a schema.
        let rescued =
            try_parse_textual_tool_call(r#"{"name": "set_status", "arguments": {"type": "busy"}}"#)
                .expect("should still parse — no `properties` key present");
        assert_eq!(rescued.arguments, serde_json::json!({"type": "busy"}));
    }

    // --- F1: fenced examples are not real leaked tool calls ---

    #[test]
    fn a_tagged_call_inside_a_markdown_fence_is_not_executed() {
        let text = "Así es como Qwen emite tool calls:\n```\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"/etc/shadow\"}}\n</tool_call>\n```\n";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_bare_function_xml_inside_a_markdown_fence_is_not_executed() {
        let text = "Ejemplo:\n```\n<function=read_file>\n<parameter=path>\n/etc/shadow\n</parameter>\n</function>\n```\n";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    /// J-8 (docs/AUDITORIA-2026-07-v7.md): the pythonic rung was the only
    /// one missing the F1 fence check — a fenced `[func(...)]` example
    /// ("así emite Llama sus tool calls") got extracted and dispatched
    /// for real.
    #[test]
    fn a_pythonic_call_inside_a_markdown_fence_is_not_executed() {
        let text = "Así emite Llama sus tool calls:\n```\n[get_weather(city=\"SF\")]\n```\n";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty(), "a fenced example must not be dispatched");
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unfenced_pythonic_call_after_fenced_prose_is_still_rescued() {
        let text = "Ejemplo:\n```\nsolo texto\n```\n[echo(text=\"hi\")]";
        let (calls, _) = extract_pythonic_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "the real call after the fence must still rescue"
        );
    }

    #[test]
    fn an_unfenced_tagged_call_after_fenced_prose_is_still_rescued() {
        // The fence-toggle logic must correctly track state across
        // multiple fences, not just detect "any fence exists somewhere".
        let text = "Aquí un ejemplo:\n```\nesto es solo texto\n```\n<tool_call>\n{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}\n</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "the real call after the fence must still rescue"
        );
    }

    #[test]
    fn each_rescued_call_gets_a_distinct_id() {
        let a = try_parse_textual_tool_call(r#"{"name": "echo", "arguments": {}}"#).unwrap();
        let b = try_parse_textual_tool_call(r#"{"name": "echo", "arguments": {}}"#).unwrap();
        assert_ne!(a.id, b.id);
    }

    // --- extract_tagged_tool_calls (formato nativo Qwen/Hermes, ítem 2
    // del backlog 2026-07-06) ---

    #[test]
    fn extracts_a_single_qwen_tagged_tool_call() {
        let text = "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"x.txt\"}}\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert!(remaining.is_empty());
    }

    #[test]
    fn extracts_several_tagged_calls_from_one_response() {
        // Qwen emits one pair of tags per call for parallel calls.
        let text = concat!(
            "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a\"}}\n</tool_call>\n",
            "<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"b\"}}\n</tool_call>",
        );
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "a"}));
        assert_eq!(calls[1].arguments, serde_json::json!({"path": "b"}));
        assert!(remaining.is_empty());
        assert_ne!(calls[0].id, calls[1].id);
    }

    #[test]
    fn prose_around_a_tagged_call_is_preserved_as_the_round_text() {
        let text = "Voy a leer el archivo.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"x\"}}\n</tool_call>\nListo.";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "Voy a leer el archivo.\n\nListo.");
    }

    #[test]
    fn a_fenced_json_inside_the_tags_still_parses() {
        let text = "<tool_call>```json\n{\"name\": \"echo\", \"arguments\": {}}\n```</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn a_malformed_tagged_block_stays_in_the_text_instead_of_being_swallowed() {
        let text = "<tool_call>\n{\"name\": \"echo\", \"arguments\": no-es-json}\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_malformed_block_next_to_a_valid_one_keeps_only_the_malformed_text() {
        let text = concat!(
            "<tool_call>{broken</tool_call>",
            "<tool_call>{\"name\": \"echo\", \"arguments\": {}}</tool_call>",
        );
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "<tool_call>{broken</tool_call>");
    }

    #[test]
    fn an_unclosed_tag_rescues_nothing_and_keeps_the_text_intact() {
        // E.g. a round cut off mid-block: better visible than lost.
        let text = "algo de texto <tool_call>\n{\"name\": \"echo\"";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn a_valid_block_followed_by_an_unclosed_tag_keeps_the_dangling_tail() {
        let text =
            "<tool_call>{\"name\": \"echo\", \"arguments\": {}}</tool_call> y <tool_call>{\"na";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "y <tool_call>{\"na");
    }

    #[test]
    fn plain_prose_without_tags_is_not_rescued_by_the_tagged_extractor() {
        let (calls, remaining) = extract_tagged_tool_calls("El archivo tiene 3 lineas.");
        assert!(calls.is_empty());
        assert_eq!(remaining, "El archivo tiene 3 lineas.");
    }

    #[test]
    fn tagged_extraction_accepts_parameters_as_a_synonym_for_arguments() {
        let text =
            "<tool_call>{\"name\": \"echo\", \"parameters\": {\"text\": \"hi\"}}</tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"text": "hi"}));
    }

    // --- gramática XML <function=...> de qwen3-coder (extensión del
    // ítem 2, destrancada 2026-07-06 al haber qwen3.5-coder en Nitro) ---

    /// The exact shape qwen3-coder's chat template documents: XML-ish
    /// tags, parameter values on their own lines, wrapped in the same
    /// `<tool_call>` tags qwen2.5 uses around JSON.
    #[test]
    fn function_xml_inside_tool_call_wrapper_parses() {
        let text = "<tool_call>\n<function=read_file>\n<parameter=path>\nx.txt\n</parameter>\n</function>\n</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert!(remaining.is_empty());
    }

    #[test]
    fn bare_function_xml_with_prose_around_parses_and_preserves_the_prose() {
        let text = "Voy a leerlo.\n<function=read_file>\n<parameter=path>\nx.txt\n</parameter>\n</function>\ndespués te cuento";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert_eq!(remaining, "Voy a leerlo.\n\ndespués te cuento");
    }

    #[test]
    fn function_xml_with_several_parameters_collects_them_all() {
        let text = "<function=edit_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n<parameter=old_string>\nlet x = 1;\n</parameter>\n<parameter=new_string>\nlet x = 2;\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(call.name, "edit_file");
        assert_eq!(
            call.arguments,
            serde_json::json!({
                "path": "src/main.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;",
            })
        );
    }

    /// The whole point of the XML grammar: code-carrying values need no
    /// JSON escaping — inner quotes/braces arrive verbatim as a string.
    #[test]
    fn function_xml_keeps_code_carrying_values_as_verbatim_strings() {
        let text = "<function=write_file>\n<parameter=path>\na.json\n</parameter>\n<parameter=content>\nfn main() { println!(\"{:?}\", vec![1]); }\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(
            call.arguments["content"],
            serde_json::json!("fn main() { println!(\"{:?}\", vec![1]); }")
        );
    }

    /// Scalar-looking values stay strings ("42" must not become 42 —
    /// a `path: String` schema downstream would reject the number),
    /// while a clearly structured value (`{...}`) is parsed.
    #[test]
    fn function_xml_coerces_only_clearly_structured_values() {
        let text = "<function=echo>\n<parameter=text>\n42\n</parameter>\n<parameter=options>\n{\"deep\": true}\n</parameter>\n</function>";
        let call = parse_function_xml_tool_call(text).expect("should parse");
        assert_eq!(call.arguments["text"], serde_json::json!("42"));
        assert_eq!(call.arguments["options"], serde_json::json!({"deep": true}));
    }

    #[test]
    fn malformed_function_xml_stays_in_the_text() {
        // Missing </parameter> close.
        let text = "<function=echo>\n<parameter=text>\nhola\n</function>";
        let (calls, remaining) = extract_function_xml_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn function_xml_without_parameters_is_a_zero_argument_call() {
        let call =
            parse_function_xml_tool_call("<function=list_tools>\n</function>").expect("parses");
        assert_eq!(call.name, "list_tools");
        assert_eq!(call.arguments, serde_json::json!({}));
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_function_xml() {
        let (calls, remaining) =
            extract_function_xml_tool_calls("la función f(x) = x + 1 es creciente");
        assert!(calls.is_empty());
        assert_eq!(remaining, "la función f(x) = x + 1 es creciente");
    }

    // --- gramática <arg_key>/<arg_value> de z-ai/glm-5.2 (hallazgo U-15,
    // docs/usability-log-2026-07-07-si2.md — observada 2026-07-07 vía
    // OpenRouter) ---

    /// The exact shape observed leaking from `z-ai/glm-5.2`: no
    /// `<function=...>` wrapper, just the bare name followed by
    /// `<arg_key>`/`<arg_value>` pairs, all inside the same `<tool_call>`
    /// tags qwen2.5/qwen3-coder use.
    #[test]
    fn glm_arg_tags_inside_tool_call_wrapper_parses() {
        let text = "<tool_call>read_file<arg_key>limit</arg_key><arg_value>120</arg_value><arg_key>offset</arg_key><arg_value>63</arg_value><arg_key>path</arg_key><arg_value>crates/braze-bench/src/backend_spec.rs</arg_value></tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({
                "limit": "120",
                "offset": "63",
                "path": "crates/braze-bench/src/backend_spec.rs",
            })
        );
        assert!(remaining.is_empty());
    }

    #[test]
    fn glm_arg_tags_with_prose_around_are_preserved() {
        let text = "Voy a leerlo.\n<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>\ndespués te cuento";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x.txt"}));
        assert_eq!(remaining, "Voy a leerlo.\n\ndespués te cuento");
    }

    /// Scalar-looking values stay strings, same rule as the qwen3-coder
    /// XML rescue — a `path: String` schema downstream must not receive a
    /// JSON number just because the raw text looked numeric.
    #[test]
    fn glm_arg_tags_coerce_only_clearly_structured_values() {
        let text = "<tool_call>echo<arg_key>text</arg_key><arg_value>42</arg_value><arg_key>options</arg_key><arg_value>{\"deep\": true}</arg_value></tool_call>";
        let (calls, _) = extract_tagged_tool_calls(text);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("42"));
        assert_eq!(
            calls[0].arguments["options"],
            serde_json::json!({"deep": true})
        );
    }

    #[test]
    fn glm_arg_tags_without_any_arg_key_are_not_mistaken_for_the_grammar() {
        // No `<arg_key>` at all: indistinguishable from prose that merely
        // mentions a tool by name — must fall through unrescued rather
        // than being guessed at as a zero-argument call.
        assert!(parse_glm_arg_tag_tool_call("read_file").is_none());
    }

    #[test]
    fn malformed_glm_arg_tags_stay_in_the_text() {
        // Missing the closing </arg_value>.
        let text = "<tool_call>echo<arg_key>text</arg_key><arg_value>hola</tool_call>";
        let (calls, remaining) = extract_tagged_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    // --- extract_pythonic_tool_calls (hallazgo C2, Llama's native format) ---

    #[test]
    fn parses_a_single_pythonic_call() {
        let (calls, remaining) =
            extract_pythonic_tool_calls(r#"[get_weather(city="SF", metric="celsius")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"city": "SF", "metric": "celsius"})
        );
        assert_eq!(remaining, "");
    }

    #[test]
    fn pythonic_call_preserves_surrounding_prose() {
        let text = r#"Claro, reviso el clima.[get_weather(city="SF")]Listo."#;
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(remaining, "Claro, reviso el clima.Listo.");
    }

    #[test]
    fn pythonic_call_parses_numbers_and_booleans() {
        let (calls, _) =
            extract_pythonic_tool_calls("[read_file(path=\"a.txt\", offset=5, recursive=true)]");
        assert_eq!(calls[0].arguments["offset"], serde_json::json!(5));
        assert_eq!(calls[0].arguments["recursive"], serde_json::json!(true));
    }

    #[test]
    fn pythonic_call_parses_floats() {
        let (calls, _) = extract_pythonic_tool_calls("[set_ratio(value=0.5)]");
        assert_eq!(calls[0].arguments["value"], serde_json::json!(0.5));
    }

    #[test]
    fn several_pythonic_calls_in_one_bracket_are_all_parsed() {
        let (calls, remaining) = extract_pythonic_tool_calls(r#"[echo(text="a"), echo(text="b")]"#);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a"));
        assert_eq!(calls[1].arguments["text"], serde_json::json!("b"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn pythonic_call_without_arguments_is_a_zero_argument_call() {
        let (calls, _) = extract_pythonic_tool_calls("[list_tools()]");
        assert_eq!(calls[0].name, "list_tools");
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn a_comma_inside_a_quoted_argument_does_not_split_the_call() {
        let (calls, _) = extract_pythonic_tool_calls(r#"[echo(text="a, b, c")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a, b, c"));
    }

    #[test]
    fn a_bracket_inside_a_quoted_argument_does_not_close_the_block_early() {
        let (calls, remaining) = extract_pythonic_tool_calls(r#"[echo(text="a] b")]"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["text"], serde_json::json!("a] b"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn plain_prose_is_not_mistaken_for_a_pythonic_call() {
        // No literal brackets around the call — must not match (this is
        // exactly the ambiguity the bracket-wrapper requirement avoids).
        let text = "puedes llamar a leer(archivo) para revisar el contenido";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_ordinary_list_literal_is_not_mistaken_for_a_call() {
        // `[1, 2, 3]` has no `identifier(` right after the `[`.
        let text = "los valores son [1, 2, 3] en ese orden";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unrecognized_argument_shape_leaves_the_whole_block_in_the_text() {
        // A nested list value isn't in scope (string/number/bool only) —
        // the whole call must be left untouched, not partially rescued.
        let text = "[echo(items=[1, 2])]";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }

    #[test]
    fn an_unclosed_pythonic_bracket_stays_in_the_text() {
        let text = "[get_weather(city=\"SF\"";
        let (calls, remaining) = extract_pythonic_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(remaining, text);
    }
}
