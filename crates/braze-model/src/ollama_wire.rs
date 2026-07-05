//! Wire-format mapping for Ollama's **native** chat API
//! (`POST http://localhost:11434/api/chat`) — NOT the OpenAI-compatible
//! surface. Streaming is NDJSON: one complete JSON object per line, not
//! SSE.
//!
//! Mirrors `anthropic_wire.rs`'s split: request-serialization types and the
//! line-framing/state-handling logic live here so they're unit-testable
//! without a network round-trip; `ollama.rs` owns the `OllamaBackend`
//! struct and the actual HTTP/stream plumbing.

use std::sync::atomic::{AtomicU64, Ordering};

use braze_types::{ContentBlock, Message, Role, ToolStub};
use serde::Serialize;
use serde_json::Value;

use crate::backend::{CompletionEvent, CompletionRequest};
use crate::error::ModelError;

// ---------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct OllamaRequest {
    pub model: String,
    pub messages: Vec<OllamaMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OllamaTool>,
    pub stream: bool,
    pub options: OllamaOptions,
}

/// Without an explicit `num_ctx`, Ollama falls back to its Modelfile
/// default (commonly 2048-4096 tokens) and **silently truncates** any
/// prompt that exceeds it from the front — the system prompt and tool
/// definitions are the first things to disappear, with no error surfaced
/// anywhere. `num_predict` is the native equivalent of
/// [`CompletionRequest::max_tokens`], which this backend previously
/// dropped on the floor entirely (Anthropic honored it, Ollama didn't) —
/// without it, a model stuck in a repetition loop generates unbounded
/// output. `temperature` low-but-not-zero balances deterministic
/// tool-call formatting against still allowing the model to recover from
/// a bad first attempt rather than repeating it verbatim.
#[derive(Debug, Serialize, Clone, Copy)]
pub(crate) struct OllamaOptions {
    pub num_ctx: u32,
    pub num_predict: i32,
    pub temperature: f32,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct OllamaMessage {
    pub role: &'static str,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<OllamaToolCallOut>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OllamaToolCallOut {
    pub function: OllamaFunctionCallOut,
}

#[derive(Debug, Serialize)]
pub(crate) struct OllamaFunctionCallOut {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct OllamaTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: OllamaFunctionDef,
}

#[derive(Debug, Serialize)]
pub(crate) struct OllamaFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Builds the Ollama `/api/chat` request body from the provider-agnostic
/// [`CompletionRequest`].
///
/// Ollama's native chat API has no separate top-level "system" field like
/// Anthropic's — the system prompt is just the first message in the array,
/// with `role: "system"`.
///
/// `num_ctx`/`temperature` are backend-level configuration (not part of
/// [`CompletionRequest`], which is provider-agnostic) — see
/// [`OllamaBackend`](crate::ollama::OllamaBackend)'s fields.
pub(crate) fn build_request(
    req: &CompletionRequest,
    model: &str,
    num_ctx: u32,
    temperature: f32,
) -> OllamaRequest {
    let mut messages = Vec::new();

    if !req.system_prompt.is_empty() {
        messages.push(OllamaMessage {
            role: "system",
            content: req.system_prompt.clone(),
            tool_calls: Vec::new(),
        });
    }

    for message in &req.messages {
        messages.extend(to_ollama_messages(message));
    }

    OllamaRequest {
        model: model.to_string(),
        messages,
        tools: build_tools(&req.tool_stubs),
        stream: true,
        options: OllamaOptions {
            num_ctx,
            // Ollama's own `num_predict` is `i32` with `-1` meaning
            // unbounded; `max_tokens` is realistically always small enough
            // to fit, but saturate defensively rather than panic/wrap on
            // an adversarial value.
            num_predict: req.max_tokens.min(i32::MAX as u32) as i32,
            temperature,
        },
    }
}

/// One internal [`Message`] can map to *multiple* Ollama messages: Ollama's
/// native API has no `tool_result`-style content block — a tool result is
/// its own message with `role: "tool"`. Text and `tool_use` blocks stay
/// combined into a single message under the mapped role (Ollama's
/// `tool_calls` sit alongside `content` on one assistant message); a
/// `ToolResult` block always flushes whatever text/tool_calls have
/// accumulated so far and becomes its own `role: "tool"` message.
fn to_ollama_messages(message: &Message) -> Vec<OllamaMessage> {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    };

    let mut out = Vec::new();
    let mut text_buf = String::new();
    let mut tool_calls = Vec::new();

    let flush = |text_buf: &mut String,
                 tool_calls: &mut Vec<OllamaToolCallOut>,
                 out: &mut Vec<OllamaMessage>| {
        if !text_buf.is_empty() || !tool_calls.is_empty() {
            out.push(OllamaMessage {
                role,
                content: std::mem::take(text_buf),
                tool_calls: std::mem::take(tool_calls),
            });
        }
    };

    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                text_buf.push_str(text);
            }
            ContentBlock::ToolUse { name, input, .. } => {
                tool_calls.push(OllamaToolCallOut {
                    function: OllamaFunctionCallOut {
                        name: name.clone(),
                        arguments: input.clone(),
                    },
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                flush(&mut text_buf, &mut tool_calls, &mut out);
                out.push(OllamaMessage {
                    role: "tool",
                    // Ollama's native API doesn't standardize an
                    // "is_error" field on tool messages; surface it in the
                    // content so the model still sees it.
                    content: if *is_error {
                        format!("[error] {content} (tool_use_id={tool_use_id})")
                    } else {
                        content.clone()
                    },
                    tool_calls: Vec::new(),
                });
            }
        }
    }
    flush(&mut text_buf, &mut tool_calls, &mut out);

    out
}

/// Builds the Ollama `tools` array from deferred-loading stubs.
///
/// Same two-tier schema policy as the Anthropic backend (see
/// `anthropic_wire::build_tools` for the full rationale): a stub that
/// already carries its real `input_schema` (the local built-ins, per
/// `braze-tools-local::schema::all_stubs`) sends it as-is; a stub that
/// doesn't (still-deferred MCP tools) falls back to the permissive
/// placeholder schema, resolved for real only on demand (Fase 5
/// deferred-validation note) — applied here too for consistency, even
/// though Ollama tends to be more tolerant of loose schemas in practice.
fn build_tools(stubs: &[ToolStub]) -> Vec<OllamaTool> {
    stubs
        .iter()
        .map(|stub| OllamaTool {
            kind: "function",
            function: OllamaFunctionDef {
                name: stub.name.clone(),
                description: stub.summary.clone(),
                parameters: stub.input_schema.clone().unwrap_or_else(|| {
                    serde_json::json!({
                        "type": "object",
                        "additionalProperties": true
                    })
                }),
            },
        })
        .collect()
}

// ---------------------------------------------------------------------
// NDJSON line framing
// ---------------------------------------------------------------------

/// Drains the first complete `\n`-terminated line from `buf` and returns
/// it (trimmed, with a leading `\r` stripped if present), skipping
/// blank lines. Returns `None` when `buf` has no complete line yet.
///
/// Pure and directly unit-tested, mirroring `anthropic_wire::extract_next_sse_data`.
pub(crate) fn extract_next_ndjson_line(buf: &mut Vec<u8>) -> Option<String> {
    loop {
        let newline_pos = buf.iter().position(|&b| b == b'\n')?;
        let line_bytes: Vec<u8> = buf.drain(..=newline_pos).collect();
        let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
        let trimmed = line.trim_end_matches('\r').trim();
        if trimmed.is_empty() {
            continue;
        }
        return Some(trimmed.to_string());
    }
}

// ---------------------------------------------------------------------
// Streaming line -> CompletionEvent mapping
// ---------------------------------------------------------------------

static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Ollama's native `/api/chat` streaming does not fragment tool-call
/// arguments across chunks the way Anthropic's `input_json_delta` does —
/// each `tool_calls` entry arrives as a complete JSON object in a single
/// line. This is the documented/observed behavior as of the model versions
/// available at the time this backend was written; if a future Ollama
/// version starts fragmenting tool call arguments, this function would
/// need the same accumulate-until-`content_block_stop`-style buffering as
/// the Anthropic backend.
pub(crate) struct OllamaStreamState {
    pub done: bool,
    /// Set when a line carries a top-level `"error"` field — Ollama emits
    /// this for a failed generation (e.g. the model crashed, ran out of
    /// memory), often without `"done": true` alongside it. The caller
    /// (`drive_stream` in `ollama.rs`) checks this after every
    /// `handle_line` call and yields it as
    /// `Err(ModelError::StreamError)` instead of silently ending the
    /// stream — see [`crate::ModelError::StreamError`]'s doc comment.
    pub stream_error: Option<String>,
}

impl OllamaStreamState {
    pub fn new() -> Self {
        Self {
            done: false,
            stream_error: None,
        }
    }

    /// Processes one parsed NDJSON line, returning zero or more
    /// [`CompletionEvent`]s. Never panics.
    pub fn handle_line(&mut self, json: &Value) -> Vec<CompletionEvent> {
        if let Some(message) = json.get("error").and_then(Value::as_str) {
            self.stream_error = Some(message.to_string());
            self.done = true;
            return Vec::new();
        }

        let mut events = Vec::new();

        if let Some(message) = json.get("message") {
            if let Some(content) = message.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                events.push(CompletionEvent::TextDelta(content.to_string()));
            }
            if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    if let Some(event) = tool_call_from_json(call) {
                        events.push(event);
                    }
                }
            }
        }

        if json.get("done").and_then(Value::as_bool) == Some(true) {
            let input_tokens = json
                .get("prompt_eval_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let output_tokens = json.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as u32;
            // "length" means output was cut off by the num_predict budget
            // rather than the model finishing on its own — see
            // `CompletionEvent::Usage`'s doc comment.
            let stop_reason = json
                .get("done_reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            events.push(CompletionEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
            });
            events.push(CompletionEvent::Done);
            self.done = true;
        }

        events
    }
}

fn tool_call_from_json(call: &Value) -> Option<CompletionEvent> {
    let function = call.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?.to_string();
    let arguments = function
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let id = format!(
        "ollama-tool-call-{}",
        TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    Some(CompletionEvent::ToolCallRequested {
        id,
        name,
        arguments,
    })
}

/// Parses one NDJSON line into a [`serde_json::Value`]. Pure and directly
/// unit-tested — returns [`ModelError::Decode`] on invalid JSON, never
/// panics.
pub(crate) fn parse_ndjson_line(line: &str) -> Result<Value, ModelError> {
    serde_json::from_str(line).map_err(|e| {
        ModelError::Decode(format!(
            "ollama NDJSON line is not valid JSON ({e}): {line:?}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::Role;

    #[test]
    fn build_request_prepends_system_message() {
        let req = CompletionRequest {
            messages: vec![Message::text(Role::User, "hi")],
            tool_stubs: vec![],
            system_prompt: "be terse".to_string(),
            max_tokens: 100,
        };
        let wire = build_request(&req, "llama3", 8192, 0.2);
        assert_eq!(wire.messages.len(), 2);
        assert_eq!(wire.messages[0].role, "system");
        assert_eq!(wire.messages[0].content, "be terse");
        assert_eq!(wire.messages[1].role, "user");
        assert_eq!(wire.messages[1].content, "hi");
        assert!(wire.stream);
        assert_eq!(wire.options.num_ctx, 8192);
        assert_eq!(wire.options.num_predict, 100);
        assert_eq!(wire.options.temperature, 0.2);
    }

    #[test]
    fn build_request_saturates_num_predict_instead_of_overflowing() {
        let req = CompletionRequest {
            messages: vec![],
            tool_stubs: vec![],
            system_prompt: String::new(),
            max_tokens: u32::MAX,
        };
        let wire = build_request(&req, "llama3", 8192, 0.2);
        assert_eq!(wire.options.num_predict, i32::MAX);
    }

    #[test]
    fn build_tools_uses_permissive_generic_schema_when_stub_has_none() {
        let stubs = vec![ToolStub {
            name: "read_file".to_string(),
            summary: "Reads a file".to_string(),
            source: "mcp:filesystem".to_string(),
            input_schema: None,
        }];
        let tools = build_tools(&stubs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].function.name, "read_file");
        assert_eq!(
            tools[0].function.parameters,
            serde_json::json!({"type": "object", "additionalProperties": true})
        );
    }

    #[test]
    fn build_tools_passes_through_stub_schema_when_present() {
        let real_schema = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        });
        let stubs = vec![ToolStub {
            name: "read_file".to_string(),
            summary: "Reads a file".to_string(),
            source: "local".to_string(),
            input_schema: Some(real_schema.clone()),
        }];
        let tools = build_tools(&stubs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.parameters, real_schema);
    }

    #[test]
    fn to_ollama_messages_splits_tool_result_into_separate_tool_message() {
        let message = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "sunny, 20C".to_string(),
                is_error: false,
            }],
        };
        let out = to_ollama_messages(&message);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "tool");
        assert_eq!(out[0].content, "sunny, 20C");
    }

    #[test]
    fn to_ollama_messages_keeps_text_and_tool_use_together() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Let me check.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "get_weather".to_string(),
                    input: serde_json::json!({"city": "Santiago"}),
                },
            ],
        };
        let out = to_ollama_messages(&message);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "assistant");
        assert_eq!(out[0].content, "Let me check.");
        assert_eq!(out[0].tool_calls.len(), 1);
        assert_eq!(out[0].tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn extract_next_ndjson_line_returns_none_on_partial_buffer() {
        let mut buf = b"{\"partial".to_vec();
        assert_eq!(extract_next_ndjson_line(&mut buf), None);
    }

    #[test]
    fn extract_next_ndjson_line_parses_one_line() {
        let mut buf = b"{\"a\":1}\n{\"b\":2}\n".to_vec();
        assert_eq!(
            extract_next_ndjson_line(&mut buf),
            Some(r#"{"a":1}"#.to_string())
        );
        assert_eq!(
            extract_next_ndjson_line(&mut buf),
            Some(r#"{"b":2}"#.to_string())
        );
        assert_eq!(extract_next_ndjson_line(&mut buf), None);
    }

    #[test]
    fn extract_next_ndjson_line_skips_blank_lines() {
        let mut buf = b"\n\n{\"a\":1}\n".to_vec();
        assert_eq!(
            extract_next_ndjson_line(&mut buf),
            Some(r#"{"a":1}"#.to_string())
        );
    }

    #[test]
    fn extract_next_ndjson_line_handles_bytes_arriving_in_two_pieces() {
        let mut buf = b"{\"a\":".to_vec();
        assert_eq!(extract_next_ndjson_line(&mut buf), None);
        buf.extend_from_slice(b"1}\n");
        assert_eq!(
            extract_next_ndjson_line(&mut buf),
            Some(r#"{"a":1}"#.to_string())
        );
    }

    #[test]
    fn parse_ndjson_line_returns_decode_error_on_invalid_json() {
        let result = parse_ndjson_line("{not valid json");
        assert!(matches!(result, Err(ModelError::Decode(_))));
    }

    #[test]
    fn stream_state_simple_text_completion() {
        let mut state = OllamaStreamState::new();
        let lines = [
            serde_json::json!({"model": "llama3", "message": {"role": "assistant", "content": "Hello"}, "done": false}),
            serde_json::json!({"model": "llama3", "message": {"role": "assistant", "content": ", world"}, "done": false}),
            serde_json::json!({
                "model": "llama3",
                "message": {"role": "assistant", "content": ""},
                "done": true,
                "prompt_eval_count": 12,
                "eval_count": 4
            }),
        ];

        let mut events = Vec::new();
        for line in &lines {
            events.extend(state.handle_line(line));
        }

        assert!(state.done);
        assert_eq!(events.len(), 4);
        match &events[0] {
            CompletionEvent::TextDelta(t) => assert_eq!(t, "Hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &events[1] {
            CompletionEvent::TextDelta(t) => assert_eq!(t, ", world"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &events[2] {
            CompletionEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(*input_tokens, 12);
                assert_eq!(*output_tokens, 4);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(events[3], CompletionEvent::Done));
    }

    #[test]
    fn stream_state_captures_done_reason_as_stop_reason() {
        let mut state = OllamaStreamState::new();
        let line = serde_json::json!({
            "model": "llama3",
            "message": {"role": "assistant", "content": ""},
            "done": true,
            "done_reason": "length",
            "prompt_eval_count": 12,
            "eval_count": 4
        });

        let events = state.handle_line(&line);
        let usage = events
            .iter()
            .find(|e| matches!(e, CompletionEvent::Usage { .. }))
            .expect("expected a Usage event");
        match usage {
            CompletionEvent::Usage { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("length"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn stream_state_emits_tool_call_and_done_together() {
        let mut state = OllamaStreamState::new();
        let line = serde_json::json!({
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {"function": {"name": "get_weather", "arguments": {"city": "Santiago"}}}
                ]
            },
            "done": true,
            "prompt_eval_count": 5,
            "eval_count": 2
        });

        let events = state.handle_line(&line);
        assert!(state.done);
        let tool_calls: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, CompletionEvent::ToolCallRequested { .. }))
            .collect();
        assert_eq!(tool_calls.len(), 1);
        match tool_calls[0] {
            CompletionEvent::ToolCallRequested {
                name, arguments, ..
            } => {
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, &serde_json::json!({"city": "Santiago"}));
            }
            _ => unreachable!(),
        }
        assert!(events.iter().any(|e| matches!(e, CompletionEvent::Done)));
    }

    #[test]
    fn stream_state_missing_usage_fields_default_to_zero_without_panicking() {
        let mut state = OllamaStreamState::new();
        let line = serde_json::json!({"done": true});
        let events = state.handle_line(&line);
        assert!(state.done);
        assert!(events.iter().any(|e| matches!(
            e,
            CompletionEvent::Usage {
                input_tokens: 0,
                output_tokens: 0,
                stop_reason: None
            }
        )));
    }
}
