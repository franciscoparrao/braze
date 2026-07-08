//! Wire-format mapping for the Anthropic Messages API
//! (`POST https://api.anthropic.com/v1/messages`, `anthropic-version:
//! 2023-06-01`).
//!
//! Split out from `anthropic.rs` so the request-serialization types and the
//! hand-rolled SSE parser (byte-buffer framing + per-event state machine)
//! can be unit-tested directly, without needing a real HTTP connection.
//!
//! There is no SSE-parsing crate in the workspace (`eventsource-stream`
//! etc. are not workspace dependencies) and the task explicitly asked not
//! to add one unless genuinely necessary — the framing Anthropic uses
//! (`data: <json>\n\n`) is simple enough to parse by hand over
//! `reqwest::Response::bytes_stream()`, so no new dependency was added.

use std::collections::HashMap;

use braze_types::{ContentBlock, Message, Role, ToolStub};
use serde::Serialize;
use serde_json::Value;

use crate::backend::{CompletionEvent, CompletionRequest, permissive_fallback_schema};

pub(crate) const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

// ---------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub system: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AnthropicTool>,
    pub stream: bool,
    /// `None` (the default) omits the field entirely, leaving Anthropic's
    /// own provider default (~1.0) in effect — unchanged behavior for any
    /// existing caller. Set via
    /// [`AnthropicBackend::with_temperature`](crate::anthropic::AnthropicBackend::with_temperature),
    /// e.g. so `braze-bench` can give every backend in a sweep the same
    /// sampling temperature (N-34, docs/AUDITORIA-2026-07-v2.md). Note:
    /// the Anthropic Messages API has no `seed` parameter — unlike Ollama
    /// and OpenRouter's OpenAI-compatible surface, a run against Anthropic
    /// can never be made fully reproducible this way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicMessage {
    pub role: &'static str,
    pub content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Builds the Anthropic request body from the provider-agnostic
/// [`CompletionRequest`].
pub(crate) fn build_request(
    req: &CompletionRequest,
    model: &str,
    temperature: Option<f32>,
) -> AnthropicRequest {
    AnthropicRequest {
        model: model.to_string(),
        max_tokens: req.max_tokens,
        system: req.system_prompt.clone(),
        messages: req.messages.iter().map(to_anthropic_message).collect(),
        tools: build_tools(&req.tool_stubs),
        stream: true,
        temperature,
    }
}

fn to_anthropic_message(message: &Message) -> AnthropicMessage {
    // Anthropic only accepts "user"/"assistant" roles on the messages
    // array (the system prompt is the separate top-level `system` field).
    // `braze_types::Role::System` has no wire equivalent here; any such
    // message in history (e.g. an injected system-reminder turn) is folded
    // in as a "user" turn rather than dropped or panicking.
    let role = match message.role {
        Role::User | Role::System => "user",
        Role::Assistant => "assistant",
    };

    AnthropicMessage {
        role,
        content: message.content.iter().map(to_anthropic_block).collect(),
    }
}

fn to_anthropic_block(block: &ContentBlock) -> AnthropicContentBlock {
    match block {
        ContentBlock::Text { text } => AnthropicContentBlock::Text { text: text.clone() },
        ContentBlock::ToolUse { id, name, input } => AnthropicContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => AnthropicContentBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
            is_error: *is_error,
        },
    }
}

/// Builds the Anthropic `tools` array from deferred-loading stubs.
///
/// ## Política de schema de dos vías (D3, auditoría 2026-07)
///
/// `CompletionRequest.tool_stubs` no depende de `braze-tools-core` para
/// resolver un schema (braze-model y braze-tools-core son crates hermanos
/// de Nivel 1 que nunca se dependen entre sí, ver PLAN.md) — pero
/// `ToolStub` (en `braze-types`, Nivel 0) sí puede cargar opcionalmente el
/// `input_schema` real, poblado por el *provider* que lo produjo, no por
/// esta función. Para el set pequeño y estático de tools locales
/// (`braze-tools-local::schema::all_stubs`) el schema real ya viene en el
/// stub, así que se envía tal cual. Para tools MCP (set dinámico/no
/// acotado, `McpToolProvider::list_stubs`) el schema sigue diferido —
/// `input_schema` es `None` en el stub, y aquí se cae al schema
/// **permisivo/genérico** (`{"type":"object","additionalProperties":true}`),
/// resuelto para real recién en el dispatch (`braze-engine::Engine::dispatch_tool_calls`,
/// vía `braze-tools-core::ToolRegistry`).
fn build_tools(stubs: &[ToolStub]) -> Vec<AnthropicTool> {
    stubs
        .iter()
        .map(|stub| AnthropicTool {
            name: stub.name.clone(),
            description: stub.summary.clone(),
            input_schema: stub
                .input_schema
                .clone()
                .unwrap_or_else(permissive_fallback_schema),
        })
        .collect()
}

// ---------------------------------------------------------------------
// SSE byte-buffer framing
// ---------------------------------------------------------------------

/// Drains complete SSE events from `buf`, returning the concatenated
/// `data:` payload of the first event that has one. SSE events are
/// separated by a blank line (`\n\n`); an event may carry multiple `data:`
/// lines (joined with `\n` per spec), an `event:` line (ignored — we
/// dispatch on the JSON payload's own `"type"` field instead), and
/// `:`-prefixed comment lines (ignored, e.g. keep-alive pings).
///
/// Returns `None` when `buf` doesn't yet contain a full event boundary
/// (caller should read more bytes from the network) — this also covers
/// draining zero-or-more empty/comment-only events before running out of
/// buffered bytes.
///
/// Pure and allocation-light on purpose so it's directly unit-testable
/// without a network round-trip.
pub(crate) fn extract_next_sse_data(buf: &mut Vec<u8>) -> Option<String> {
    loop {
        let drain_len = find_event_boundary_end(buf)?;
        let event_bytes: Vec<u8> = buf.drain(..drain_len).collect();
        let event_str = String::from_utf8_lossy(&event_bytes);

        let mut data_lines: Vec<&str> = Vec::new();
        for raw_line in event_str.lines() {
            let line = raw_line.trim_end_matches('\r');
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.strip_prefix(' ').unwrap_or(data));
            }
            // "event:" lines and ":"-prefixed comments are intentionally
            // ignored — see doc comment above.
        }

        if data_lines.is_empty() {
            // Comment-only / heartbeat event — keep draining.
            continue;
        }

        return Some(data_lines.join("\n"));
    }
}

/// Returns the number of leading bytes of `buf` that make up one complete
/// SSE event (fields + the blank-line terminator), or `None` if `buf`
/// doesn't yet contain a full event. Tolerates both bare `\n\n` and
/// `\r\n\r\n` blank-line framing — the boundary is "a line terminator,
/// immediately followed by another blank line terminator", which covers
/// both without needing an exact 2-byte vs 3-byte pattern match.
fn find_event_boundary_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            if i + 1 < buf.len() && buf[i + 1] == b'\n' {
                return Some(i + 2);
            }
            if i + 2 < buf.len() && buf[i + 1] == b'\r' && buf[i + 2] == b'\n' {
                return Some(i + 3);
            }
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------
// Streaming event state machine
// ---------------------------------------------------------------------

enum BlockState {
    Text,
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
}

/// Accumulates streaming state across an Anthropic SSE session: per-block
/// tool-call argument fragments (never emitted until `content_block_stop`
/// gives us the complete JSON — see the task's framing on
/// `input_json_delta`), plus running token usage.
pub(crate) struct AnthropicStreamState {
    blocks: HashMap<u64, BlockState>,
    input_tokens: u32,
    output_tokens: u32,
    /// Captured from `message_delta.delta.stop_reason` — `"max_tokens"`
    /// means the response (including, possibly, a tool call's JSON
    /// arguments) was cut off by the `max_tokens` budget rather than the
    /// model finishing on its own. See `CompletionEvent::Usage`'s doc
    /// comment.
    stop_reason: Option<String>,
    pub done: bool,
    /// Set by a mid-stream `"error"` SSE event (e.g. `overloaded_error`).
    /// The caller (`drive_stream` in `anthropic.rs`) checks this after
    /// every `handle_event` call and, if set, yields it as
    /// `Err(ModelError::StreamError)` instead of silently ending the
    /// stream — see [`crate::ModelError::StreamError`]'s doc comment for why
    /// this used to be indistinguishable from a normal completion.
    pub stream_error: Option<String>,
}

impl AnthropicStreamState {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            done: false,
            stream_error: None,
        }
    }

    /// Processes one parsed SSE JSON payload, returning zero or more
    /// [`CompletionEvent`]s. Never panics — malformed/unexpected shapes are
    /// logged (by the caller, which has the tracing context) and skipped.
    pub fn handle_event(&mut self, json: &Value) -> Vec<CompletionEvent> {
        match json.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(tokens) = json
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_u64)
                {
                    self.input_tokens = tokens as u32;
                }
                Vec::new()
            }
            Some("content_block_start") => self.on_content_block_start(json),
            Some("content_block_delta") => self.on_content_block_delta(json),
            Some("content_block_stop") => self.on_content_block_stop(json),
            Some("message_delta") => {
                if let Some(tokens) = json
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64)
                {
                    self.output_tokens = tokens as u32;
                }
                if let Some(reason) = json
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(reason.to_string());
                }
                Vec::new()
            }
            Some("message_stop") => {
                self.done = true;
                vec![
                    CompletionEvent::Usage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                        stop_reason: self.stop_reason.clone(),
                        // Anthropic-native caching is out of scope for
                        // this pass (docs/usability-log-2026-07-07-si2.md
                        // — v1 targets the OpenRouter path, which is how
                        // this project actually gets used).
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                    },
                    CompletionEvent::Done,
                ]
            }
            Some("error") => {
                // Mid-stream server error (e.g. overloaded_error). Record
                // it in `stream_error` — `drive_stream` checks that after
                // this call and yields it as `Err(ModelError::StreamError)`
                // instead of a fabricated `Done`, so the caller can tell
                // this apart from a real, successful completion.
                let message = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                self.stream_error = Some(message);
                self.done = true;
                Vec::new()
            }
            // "ping" and any future/unknown event types are ignored.
            _ => Vec::new(),
        }
    }

    fn on_content_block_start(&mut self, json: &Value) -> Vec<CompletionEvent> {
        let Some(index) = json.get("index").and_then(Value::as_u64) else {
            return Vec::new();
        };
        let Some(block) = json.get("content_block") else {
            return Vec::new();
        };
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                self.blocks.insert(index, BlockState::Text);
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.blocks.insert(
                    index,
                    BlockState::ToolUse {
                        id,
                        name,
                        json_buf: String::new(),
                    },
                );
            }
            _ => {}
        }
        Vec::new()
    }

    fn on_content_block_delta(&mut self, json: &Value) -> Vec<CompletionEvent> {
        let Some(index) = json.get("index").and_then(Value::as_u64) else {
            return Vec::new();
        };
        let Some(delta) = json.get("delta") else {
            return Vec::new();
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                vec![CompletionEvent::TextDelta(text.to_string())]
            }
            Some("input_json_delta") => {
                let fragment = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some(BlockState::ToolUse { json_buf, .. }) = self.blocks.get_mut(&index) {
                    json_buf.push_str(fragment);
                }
                Vec::new()
            }
            // "thinking_delta", "signature_delta", etc. — not handled by
            // this MVP backend; ignored rather than treated as an error.
            _ => Vec::new(),
        }
    }

    fn on_content_block_stop(&mut self, json: &Value) -> Vec<CompletionEvent> {
        let Some(index) = json.get("index").and_then(Value::as_u64) else {
            return Vec::new();
        };
        let Some(state) = self.blocks.remove(&index) else {
            return Vec::new();
        };
        match state {
            BlockState::Text => Vec::new(),
            BlockState::ToolUse { id, name, json_buf } => {
                vec![finalize_tool_call(id, name, &json_buf)]
            }
        }
    }
}

/// Parses the fully-accumulated `input_json_delta` fragments for one
/// `tool_use` block into a [`CompletionEvent::ToolCallRequested`]. Pure,
/// directly unit-tested, and **infallible** (ítem 3 del backlog
/// 2026-07-06): a malformed buffer goes through the shared repair ladder
/// (`args_repair`) — truncation gets completed, garbage collapses to
/// `{}` — instead of the previous "drop the call silently", which made a
/// round "converge" without executing what the model requested. A
/// dispatched call with wrong/empty arguments fails schema validation or
/// the tool itself, and *that* error is fed back to the model as a
/// visible retry signal.
///
/// The empty-buffer → `{}` normalization (N-9,
/// docs/AUDITORIA-2026-07-v2.md — a no-argument `tool_use` streams zero
/// `input_json_delta` fragments; the official SDKs special-case it the
/// same way) is the ladder's first rung.
pub(crate) fn finalize_tool_call(id: String, name: String, json_buf: &str) -> CompletionEvent {
    let (arguments, outcome) = crate::args_repair::parse_arguments_with_repair(json_buf);
    if outcome != crate::args_repair::ArgumentsOutcome::Parsed {
        tracing::warn!(
            tool = %name,
            ?outcome,
            buffer = %json_buf,
            "anthropic stream: tool call arguments were not valid JSON — repaired/collapsed instead of dropping the call"
        );
    }
    CompletionEvent::ToolCallRequested {
        id,
        name,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::Role;

    #[test]
    fn build_tools_uses_permissive_generic_schema_when_stub_has_none() {
        let stubs = vec![ToolStub {
            name: "read_file".to_string(),
            summary: "Reads a file from disk".to_string(),
            source: "mcp:filesystem".to_string(),
            input_schema: None,
        }];
        let tools = build_tools(&stubs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[0].description, "Reads a file from disk");
        assert_eq!(
            tools[0].input_schema,
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
            summary: "Reads a file from disk".to_string(),
            source: "local".to_string(),
            input_schema: Some(real_schema.clone()),
        }];
        let tools = build_tools(&stubs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].input_schema, real_schema);
    }

    #[test]
    fn build_request_maps_system_role_message_to_user() {
        let req = CompletionRequest {
            messages: vec![Message::text(Role::System, "reminder")],
            tool_stubs: vec![],
            system_prompt: "you are helpful".to_string(),
            max_tokens: 100,
        };
        let wire = build_request(&req, "claude-opus-4-8", None);
        assert_eq!(wire.system, "you are helpful");
        assert_eq!(wire.messages.len(), 1);
        assert_eq!(wire.messages[0].role, "user");
        assert!(wire.stream);
        assert_eq!(wire.temperature, None);
    }

    #[test]
    fn build_request_forwards_an_explicit_temperature() {
        let req = CompletionRequest {
            messages: vec![Message::text(Role::User, "hi")],
            tool_stubs: vec![],
            system_prompt: String::new(),
            max_tokens: 100,
        };
        let wire = build_request(&req, "claude-opus-4-8", Some(0.2));
        assert_eq!(wire.temperature, Some(0.2));
    }

    #[test]
    fn extract_next_sse_data_returns_none_on_partial_buffer() {
        let mut buf = b"event: content_block_delta\ndata: {\"partial".to_vec();
        assert_eq!(extract_next_sse_data(&mut buf), None);
        // Nothing should have been drained — we're still waiting for more bytes.
        assert!(!buf.is_empty());
    }

    #[test]
    fn extract_next_sse_data_parses_single_event() {
        let mut buf = b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec();
        let data = extract_next_sse_data(&mut buf).expect("should parse one event");
        assert_eq!(data, r#"{"type":"message_stop"}"#);
        assert!(buf.is_empty());
    }

    #[test]
    fn extract_next_sse_data_skips_comment_only_events() {
        let mut buf = b": keep-alive\n\ndata: {\"type\":\"ping\"}\n\n".to_vec();
        let data = extract_next_sse_data(&mut buf).expect("should skip comment and find ping");
        assert_eq!(data, r#"{"type":"ping"}"#);
    }

    #[test]
    fn extract_next_sse_data_handles_bytes_arriving_in_two_pieces() {
        let mut buf = b"data: {\"type\":\"pi".to_vec();
        assert_eq!(extract_next_sse_data(&mut buf), None);
        buf.extend_from_slice(b"ng\"}\n\n");
        let data = extract_next_sse_data(&mut buf).expect("now complete");
        assert_eq!(data, r#"{"type":"ping"}"#);
    }

    #[test]
    fn stream_state_simple_text_completion() {
        let mut state = AnthropicStreamState::new();
        let events_json = [
            serde_json::json!({
                "type": "message_start",
                "message": {"usage": {"input_tokens": 10}}
            }),
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "Hello"}
            }),
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": ", world"}
            }),
            serde_json::json!({"type": "content_block_stop", "index": 0}),
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 5}
            }),
            serde_json::json!({"type": "message_stop"}),
        ];

        let mut all_events = Vec::new();
        for json in &events_json {
            all_events.extend(state.handle_event(json));
        }

        assert!(state.done);
        assert_eq!(all_events.len(), 4);
        match &all_events[0] {
            CompletionEvent::TextDelta(t) => assert_eq!(t, "Hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &all_events[1] {
            CompletionEvent::TextDelta(t) => assert_eq!(t, ", world"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &all_events[2] {
            CompletionEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                ..
            } => {
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 5);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(all_events[3], CompletionEvent::Done));
    }

    #[test]
    fn stream_state_tool_call_arguments_arrive_across_fragments() {
        let mut state = AnthropicStreamState::new();
        let events_json = [
            serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 20}}}),
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": {}}
            }),
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{\"locat"}
            }),
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "ion\": \"Pa"}
            }),
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "ris\"}"}
            }),
            serde_json::json!({"type": "content_block_stop", "index": 0}),
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 15}
            }),
            serde_json::json!({"type": "message_stop"}),
        ];

        let mut all_events = Vec::new();
        for json in &events_json {
            all_events.extend(state.handle_event(json));
        }

        // Exactly one ToolCallRequested — must not fire before content_block_stop.
        let tool_calls: Vec<_> = all_events
            .iter()
            .filter(|e| matches!(e, CompletionEvent::ToolCallRequested { .. }))
            .collect();
        assert_eq!(tool_calls.len(), 1);
        match tool_calls[0] {
            CompletionEvent::ToolCallRequested {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, &serde_json::json!({"location": "Paris"}));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn stream_state_ignores_unknown_event_types() {
        let mut state = AnthropicStreamState::new();
        let events = state.handle_event(&serde_json::json!({"type": "some_future_event"}));
        assert!(events.is_empty());
        assert!(!state.done);
    }

    #[test]
    fn stream_state_error_event_sets_stream_error_not_a_fabricated_done() {
        let mut state = AnthropicStreamState::new();
        let events = state.handle_event(&serde_json::json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"}
        }));
        assert!(state.done);
        // No events (in particular, no `Done`) — the caller must see this
        // as a stream error via `state.stream_error`, not as a successful
        // completion.
        assert!(events.is_empty());
        assert_eq!(state.stream_error.as_deref(), Some("Overloaded"));
    }

    /// Ítem 3 del backlog (2026-07-06): irreparable arguments collapse
    /// to `{}` and the call still dispatches — the previous behavior
    /// (return a Decode error, caller drops the call silently) made the
    /// round "converge" without executing what the model asked for.
    #[test]
    fn finalize_tool_call_collapses_irreparable_arguments_instead_of_failing() {
        let event = finalize_tool_call(
            "toolu_01".to_string(),
            "get_weather".to_string(),
            "{not valid json",
        );
        match event {
            CompletionEvent::ToolCallRequested {
                name, arguments, ..
            } => {
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, serde_json::json!({}));
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        }
    }

    /// Ítem 3: a buffer cut off mid-string (the stream died) is repaired
    /// rather than collapsed — the arguments the model actually produced
    /// survive.
    #[test]
    fn finalize_tool_call_repairs_a_truncated_buffer() {
        let event = finalize_tool_call(
            "toolu_01".to_string(),
            "read_file".to_string(),
            "{\"path\": \"src/mai",
        );
        match event {
            CompletionEvent::ToolCallRequested { arguments, .. } => {
                assert_eq!(arguments, serde_json::json!({"path": "src/mai"}));
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        }
    }

    /// Regression test for N-9 (docs/AUDITORIA-2026-07-v2.md): a
    /// no-argument tool call accumulates an empty `json_buf` (Anthropic
    /// streams zero `input_json_delta` fragments for `input: {}`) — this
    /// must resolve to `{}`, not a dropped tool call.
    #[test]
    fn finalize_tool_call_treats_an_empty_buffer_as_an_empty_object() {
        let event = finalize_tool_call("toolu_01".to_string(), "list_sessions".to_string(), "");
        match event {
            CompletionEvent::ToolCallRequested { arguments, .. } => {
                assert_eq!(arguments, serde_json::json!({}));
            }
            other => panic!("expected ToolCallRequested with empty arguments, got {other:?}"),
        }
    }

    /// Same as above, but for a buffer that's only whitespace — some
    /// providers emit a single `partial_json: " "`-shaped fragment
    /// instead of none at all.
    #[test]
    fn finalize_tool_call_treats_a_whitespace_only_buffer_as_an_empty_object() {
        let event = finalize_tool_call("toolu_01".to_string(), "list_sessions".to_string(), "  ");
        match event {
            CompletionEvent::ToolCallRequested { arguments, .. } => {
                assert_eq!(arguments, serde_json::json!({}));
            }
            other => panic!("expected ToolCallRequested with empty arguments, got {other:?}"),
        }
    }

    /// Ítem 3 (2026-07-06): a broken block used to be dropped — now it
    /// survives with collapsed (`{}`) arguments, so the engine dispatches
    /// it and the schema/tool error becomes the model's retry signal.
    #[test]
    fn stream_state_keeps_a_tool_call_with_invalid_json_with_collapsed_arguments() {
        let mut state = AnthropicStreamState::new();
        state.handle_event(&serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_01", "name": "bad", "input": {}}
        }));
        state.handle_event(&serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{not valid"}
        }));
        let events =
            state.handle_event(&serde_json::json!({"type": "content_block_stop", "index": 0}));
        assert_eq!(events.len(), 1);
        match &events[0] {
            CompletionEvent::ToolCallRequested { id, arguments, .. } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(arguments, &serde_json::json!({}));
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        }
    }
}
