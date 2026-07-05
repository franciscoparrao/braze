//! Wire-format mapping for OpenRouter's OpenAI-compatible chat completions
//! API (`POST {base_url}/chat/completions`).
//!
//! Mirrors `anthropic_wire.rs`/`ollama_wire.rs`'s split: request
//! serialization, framing, and the streaming state machine live here so
//! they're unit-testable without a network round-trip; `openrouter.rs` owns
//! the `OpenRouterBackend` struct and the actual HTTP/stream plumbing.
//!
//! OpenRouter's SSE framing is byte-identical to Anthropic's
//! (`data: <payload>\n\n`), reused here via
//! `crate::anthropic_wire::extract_next_sse_data` rather than duplicated.
//! The payload shape, however, is OpenAI's `chat.completion.chunk`: tool
//! calls arrive fragmented by index (like Anthropic's `input_json_delta`,
//! but without an explicit per-block "stop" event — the close is implicit
//! in a chunk's `finish_reason`), and the stream ends with the literal text
//! sentinel `data: [DONE]\n\n` (not a JSON payload) rather than a
//! terminating event type.

use braze_types::{ContentBlock, Message, Role, ToolStub};
use serde::Serialize;
use serde_json::Value;

use crate::backend::{CompletionEvent, CompletionRequest, permissive_fallback_schema};
use crate::error::ModelError;

pub(crate) const OPENROUTER_DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

// ---------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct OpenRouterRequest {
    pub model: String,
    pub messages: Vec<OpenRouterMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OpenRouterTool>,
    pub stream: bool,
    pub max_tokens: u32,
    pub stream_options: OpenRouterStreamOptions,
}

/// Requesting `include_usage` is what makes an OpenAI-compatible streaming
/// API emit a final chunk with real `usage` numbers — without it there is
/// no way to populate `CompletionEvent::Usage` with anything but zeros.
#[derive(Debug, Serialize)]
pub(crate) struct OpenRouterStreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct OpenRouterMessage {
    pub role: &'static str,
    /// Omitted entirely (not `null`, not `""`) when a message carries only
    /// tool calls — the OpenAI shape distinguishes "no content" from "empty
    /// content".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<OpenRouterToolCallOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenRouterToolCallOut {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: OpenRouterFunctionCallOut,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenRouterFunctionCallOut {
    pub name: String,
    /// JSON **serialized as a string**, not a raw `Value` — unlike Ollama's
    /// `arguments: Value`. This is the OpenAI wire shape and the easiest
    /// thing to get wrong by copying the Ollama pattern without thinking.
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenRouterTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: OpenRouterFunctionDef,
}

#[derive(Debug, Serialize)]
pub(crate) struct OpenRouterFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Builds the OpenRouter `/chat/completions` request body from the
/// provider-agnostic [`CompletionRequest`].
///
/// Like Ollama (and unlike Anthropic), OpenAI-compatible APIs have no
/// separate top-level "system" field — the system prompt is just the first
/// message in the array, with `role: "system"`.
pub(crate) fn build_request(req: &CompletionRequest, model: &str) -> OpenRouterRequest {
    let mut messages = Vec::new();

    if !req.system_prompt.is_empty() {
        messages.push(OpenRouterMessage {
            role: "system",
            content: Some(req.system_prompt.clone()),
            ..Default::default()
        });
    }

    for message in &req.messages {
        messages.extend(to_openrouter_messages(message));
    }

    OpenRouterRequest {
        model: model.to_string(),
        messages,
        tools: build_tools(&req.tool_stubs),
        stream: true,
        max_tokens: req.max_tokens,
        stream_options: OpenRouterStreamOptions {
            include_usage: true,
        },
    }
}

/// One internal [`Message`] can map to *multiple* OpenRouter messages, same
/// reasoning as `ollama_wire::to_ollama_messages`: the OpenAI shape has no
/// `tool_result`-style content block — a tool result is its own message
/// with `role: "tool"` and a mandatory `tool_call_id` (a field Ollama's
/// native API doesn't have). Text and `tool_use` blocks stay combined into
/// a single message under the mapped role; a `ToolResult` block always
/// flushes whatever text/tool_calls have accumulated so far and becomes its
/// own `role: "tool"` message.
fn to_openrouter_messages(message: &Message) -> Vec<OpenRouterMessage> {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    };

    let mut out = Vec::new();
    let mut text_buf = String::new();
    let mut tool_calls = Vec::new();

    let flush = |text_buf: &mut String,
                 tool_calls: &mut Vec<OpenRouterToolCallOut>,
                 out: &mut Vec<OpenRouterMessage>| {
        if !text_buf.is_empty() || !tool_calls.is_empty() {
            let content = if text_buf.is_empty() {
                None
            } else {
                Some(std::mem::take(text_buf))
            };
            out.push(OpenRouterMessage {
                role,
                content,
                tool_calls: std::mem::take(tool_calls),
                tool_call_id: None,
            });
        }
    };

    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                text_buf.push_str(text);
            }
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(OpenRouterToolCallOut {
                    id: id.clone(),
                    kind: "function",
                    function: OpenRouterFunctionCallOut {
                        name: name.clone(),
                        arguments: input.to_string(),
                    },
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                flush(&mut text_buf, &mut tool_calls, &mut out);
                out.push(OpenRouterMessage {
                    role: "tool",
                    // The OpenAI shape has no standard "is_error" field on
                    // tool messages either; surface it in the content so
                    // the model still sees it, same approach as Ollama.
                    content: Some(if *is_error {
                        format!("[error] {content}")
                    } else {
                        content.clone()
                    }),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool_use_id.clone()),
                });
            }
        }
    }
    flush(&mut text_buf, &mut tool_calls, &mut out);

    out
}

/// Builds the OpenRouter `tools` array from deferred-loading stubs.
///
/// Same two-tier schema policy as the other two backends (see
/// `anthropic_wire::build_tools` for the full rationale) and the same wire
/// shape Ollama already sends (`type:"function"`), duplicated here rather
/// than shared — each `*_wire.rs` is deliberately self-contained, per the
/// existing convention between `anthropic_wire.rs` and `ollama_wire.rs`.
fn build_tools(stubs: &[ToolStub]) -> Vec<OpenRouterTool> {
    stubs
        .iter()
        .map(|stub| OpenRouterTool {
            kind: "function",
            function: OpenRouterFunctionDef {
                name: stub.name.clone(),
                description: stub.summary.clone(),
                parameters: stub
                    .input_schema
                    .clone()
                    .unwrap_or_else(permissive_fallback_schema),
            },
        })
        .collect()
}

// ---------------------------------------------------------------------
// Streaming chunk -> CompletionEvent mapping
// ---------------------------------------------------------------------

/// One tool call's fragments, accumulated by `delta.tool_calls[].index`
/// until a chunk's `finish_reason` closes the round. `id`/`name` only
/// arrive on the fragment that first introduces that index; `arguments`
/// accumulates across every fragment that references it.
#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments_buf: String,
}

/// Accumulates streaming state across an OpenRouter SSE session. Unlike
/// Anthropic (`content_block_stop` closes one block) or Ollama (each
/// `tool_calls` entry arrives whole), OpenAI-style streaming closes *all*
/// pending tool calls implicitly, whenever a chunk's `finish_reason`
/// becomes non-null — there is no per-tool-call terminator event.
///
/// `done` is set only by [`OpenRouterStreamState::handle_done_sentinel`],
/// called on the literal `[DONE]` line — never inferred from
/// `finish_reason` alone, since one more chunk (carrying `usage`) is
/// expected to follow it before the stream actually ends. This mirrors
/// `message_stop`/`done:true` being the sole done-signals for the other two
/// backends.
pub(crate) struct OpenRouterStreamState {
    tool_calls: Vec<Option<PendingToolCall>>,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: Option<String>,
    pub done: bool,
    /// Set by a top-level `"error"` field in a chunk. The caller
    /// (`drive_stream` in `openrouter.rs`) checks this after every
    /// `handle_chunk` call and, if set, yields it as
    /// `Err(ModelError::StreamError)` instead of silently ending the
    /// stream.
    pub stream_error: Option<String>,
}

impl OpenRouterStreamState {
    pub fn new() -> Self {
        Self {
            tool_calls: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            done: false,
            stream_error: None,
        }
    }

    /// Processes one parsed chunk (never the `[DONE]` sentinel — that's
    /// [`Self::handle_done_sentinel`]). Never panics.
    pub fn handle_chunk(&mut self, json: &Value) -> Vec<CompletionEvent> {
        if let Some(error) = json.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("unknown error")
                .to_string();
            self.stream_error = Some(message);
            return Vec::new();
        }

        if let Some(usage) = json.get("usage") {
            if let Some(tokens) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = tokens as u32;
            }
            if let Some(tokens) = usage.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = tokens as u32;
            }
        }

        let Some(choice) = json
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            // Usage-only chunk (empty/absent `choices`) — nothing more to do.
            return Vec::new();
        };

        let mut events = Vec::new();

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
            {
                events.push(CompletionEvent::TextDelta(text.to_string()));
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for fragment in tool_calls {
                    self.accumulate_tool_call_fragment(fragment);
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(reason.to_string());
            events.extend(self.finalize_tool_calls());
        }

        events
    }

    fn accumulate_tool_call_fragment(&mut self, fragment: &Value) {
        let Some(index) = fragment.get("index").and_then(Value::as_u64) else {
            return;
        };
        let index = index as usize;
        if self.tool_calls.len() <= index {
            self.tool_calls.resize_with(index + 1, || None);
        }
        let slot = self.tool_calls[index].get_or_insert_with(PendingToolCall::default);

        if let Some(id) = fragment.get("id").and_then(Value::as_str) {
            slot.id = Some(id.to_string());
        }
        if let Some(function) = fragment.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                slot.name = Some(name.to_string());
            }
            if let Some(args_fragment) = function.get("arguments").and_then(Value::as_str) {
                slot.arguments_buf.push_str(args_fragment);
            }
        }
    }

    /// Drains all pending tool calls, parsing each one's accumulated
    /// argument fragments into a [`CompletionEvent::ToolCallRequested`].
    /// A tool call whose `arguments_buf` isn't valid JSON is logged and
    /// dropped, same precedent as `AnthropicStreamState::on_content_block_stop`
    /// — it does not fail the whole stream.
    fn finalize_tool_calls(&mut self) -> Vec<CompletionEvent> {
        std::mem::take(&mut self.tool_calls)
            .into_iter()
            .flatten()
            .filter_map(|pending| {
                let id = pending.id.unwrap_or_default();
                let name = pending.name.unwrap_or_default();
                match finalize_tool_call(id, name, &pending.arguments_buf) {
                    Ok(event) => Some(event),
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "openrouter stream: dropping tool call with unparseable arguments"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// Called only when `openrouter.rs::drive_stream` sees the literal
    /// `[DONE]` line — the sole reliable close signal for OpenAI-style
    /// streaming. Falls back to zero-valued usage if no `usage` chunk was
    /// ever seen (OpenRouter routes to heterogeneous upstream providers,
    /// some of which may not honor `stream_options.include_usage`) so the
    /// `ModelBackend` invariant (always end in `Done`) holds regardless.
    pub fn handle_done_sentinel(&mut self) -> Vec<CompletionEvent> {
        self.done = true;
        vec![
            CompletionEvent::Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                stop_reason: self.stop_reason.clone(),
            },
            CompletionEvent::Done,
        ]
    }
}

/// Parses one tool call's fully-accumulated `arguments` fragments into a
/// [`CompletionEvent::ToolCallRequested`]. Pure and directly unit-tested —
/// returns [`ModelError::Decode`] on invalid JSON, never panics.
pub(crate) fn finalize_tool_call(
    id: String,
    name: String,
    arguments_buf: &str,
) -> Result<CompletionEvent, ModelError> {
    let arguments: Value = serde_json::from_str(arguments_buf).map_err(|e| {
        ModelError::Decode(format!(
            "openrouter tool call arguments are not valid JSON ({e}): {arguments_buf:?}"
        ))
    })?;
    Ok(CompletionEvent::ToolCallRequested {
        id,
        name,
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use braze_types::Role;

    fn message_json(index: usize, delta: Value, finish_reason: Option<&str>) -> Value {
        let mut choice = serde_json::json!({"index": index, "delta": delta});
        choice["finish_reason"] = match finish_reason {
            Some(r) => Value::String(r.to_string()),
            None => Value::Null,
        };
        serde_json::json!({"choices": [choice]})
    }

    #[test]
    fn build_request_prepends_system_message_and_requests_usage() {
        let req = CompletionRequest {
            messages: vec![Message::text(Role::User, "hi")],
            tool_stubs: vec![],
            system_prompt: "be terse".to_string(),
            max_tokens: 100,
        };
        let wire = build_request(&req, "openai/gpt-4o-mini");
        assert_eq!(wire.messages.len(), 2);
        assert_eq!(wire.messages[0].role, "system");
        assert_eq!(wire.messages[0].content.as_deref(), Some("be terse"));
        assert_eq!(wire.messages[1].role, "user");
        assert!(wire.stream);
        assert!(wire.stream_options.include_usage);
        assert_eq!(wire.max_tokens, 100);
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
        assert_eq!(tools[0].function.parameters, real_schema);
    }

    #[test]
    fn to_openrouter_messages_splits_tool_result_into_tool_role_message_with_tool_call_id() {
        let message = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "sunny, 20C".to_string(),
                is_error: false,
            }],
        };
        let out = to_openrouter_messages(&message);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "tool");
        assert_eq!(out[0].content.as_deref(), Some("sunny, 20C"));
        assert_eq!(out[0].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn to_openrouter_messages_serializes_tool_call_arguments_as_json_string() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "get_weather".to_string(),
                input: serde_json::json!({"city": "Santiago"}),
            }],
        };
        let out = to_openrouter_messages(&message);
        assert_eq!(out.len(), 1);
        let serialized = serde_json::to_value(&out[0]).unwrap();
        let arguments = &serialized["tool_calls"][0]["function"]["arguments"];
        assert!(arguments.is_string(), "expected a JSON string, got {arguments:?}");
        let parsed: Value = serde_json::from_str(arguments.as_str().unwrap()).unwrap();
        assert_eq!(parsed, serde_json::json!({"city": "Santiago"}));
    }

    #[test]
    fn to_openrouter_messages_omits_content_when_only_tool_calls_present() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "get_weather".to_string(),
                input: serde_json::json!({}),
            }],
        };
        let out = to_openrouter_messages(&message);
        let serialized = serde_json::to_value(&out[0]).unwrap();
        assert!(
            serialized.get("content").is_none(),
            "expected no 'content' key, got {serialized:?}"
        );
    }

    #[test]
    fn to_openrouter_messages_keeps_text_and_tool_use_together() {
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
        let out = to_openrouter_messages(&message);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "assistant");
        assert_eq!(out[0].content.as_deref(), Some("Let me check."));
        assert_eq!(out[0].tool_calls.len(), 1);
        assert_eq!(out[0].tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn stream_state_simple_text_completion() {
        let mut state = OpenRouterStreamState::new();
        let mut all_events = Vec::new();
        all_events.extend(state.handle_chunk(&message_json(
            0,
            serde_json::json!({"content": "Hello"}),
            None,
        )));
        all_events.extend(state.handle_chunk(&message_json(
            0,
            serde_json::json!({"content": ", world"}),
            None,
        )));
        all_events.extend(state.handle_chunk(&message_json(0, serde_json::json!({}), Some("stop"))));
        all_events.extend(state.handle_chunk(
            &serde_json::json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5}}),
        ));
        assert!(!state.done);
        all_events.extend(state.handle_done_sentinel());

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
            } => {
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 5);
                assert_eq!(stop_reason.as_deref(), Some("stop"));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(all_events[3], CompletionEvent::Done));
    }

    #[test]
    fn stream_state_tool_call_fragmented_across_three_chunks_reassembles_correctly() {
        let mut state = OpenRouterStreamState::new();
        state.handle_chunk(&message_json(
            0,
            serde_json::json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{\"loc"}}]}),
            None,
        ));
        state.handle_chunk(&message_json(
            0,
            serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": "ation\": \"Pa"}}]}),
            None,
        ));
        state.handle_chunk(&message_json(
            0,
            serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": "ris\"}"}}]}),
            None,
        ));
        let events = state.handle_chunk(&message_json(0, serde_json::json!({}), Some("tool_calls")));

        assert_eq!(events.len(), 1);
        match &events[0] {
            CompletionEvent::ToolCallRequested {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, &serde_json::json!({"location": "Paris"}));
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        }
    }

    #[test]
    fn stream_state_two_interleaved_tool_calls_by_index_do_not_mix() {
        let mut state = OpenRouterStreamState::new();
        state.handle_chunk(&message_json(
            0,
            serde_json::json!({"tool_calls": [
                {"index": 0, "id": "call_a", "function": {"name": "a", "arguments": "{\"x\":"}},
                {"index": 1, "id": "call_b", "function": {"name": "b", "arguments": "{\"y\":"}},
            ]}),
            None,
        ));
        state.handle_chunk(&message_json(
            0,
            serde_json::json!({"tool_calls": [
                {"index": 1, "function": {"arguments": "2}"}},
                {"index": 0, "function": {"arguments": "1}"}},
            ]}),
            None,
        ));
        let events = state.handle_chunk(&message_json(0, serde_json::json!({}), Some("tool_calls")));

        assert_eq!(events.len(), 2);
        let mut by_name: HashMap<String, Value> = HashMap::new();
        for event in &events {
            if let CompletionEvent::ToolCallRequested {
                name, arguments, ..
            } = event
            {
                by_name.insert(name.clone(), arguments.clone());
            }
        }
        assert_eq!(by_name["a"], serde_json::json!({"x": 1}));
        assert_eq!(by_name["b"], serde_json::json!({"y": 2}));
    }

    #[test]
    fn stream_state_finish_reason_without_usage_chunk_falls_back_to_zero_usage() {
        let mut state = OpenRouterStreamState::new();
        state.handle_chunk(&message_json(0, serde_json::json!({"content": "hi"}), None));
        state.handle_chunk(&message_json(0, serde_json::json!({}), Some("stop")));
        let events = state.handle_done_sentinel();

        match &events[0] {
            CompletionEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
            } => {
                assert_eq!(*input_tokens, 0);
                assert_eq!(*output_tokens, 0);
                assert_eq!(stop_reason.as_deref(), Some("stop"));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(events[1], CompletionEvent::Done));
    }

    #[test]
    fn stream_state_done_sentinel_without_prior_finish_reason_still_terminates() {
        let mut state = OpenRouterStreamState::new();
        state.handle_chunk(&message_json(0, serde_json::json!({"content": "hi"}), None));
        let events = state.handle_done_sentinel();
        assert!(state.done);
        assert!(matches!(events[1], CompletionEvent::Done));
    }

    #[test]
    fn stream_state_top_level_error_chunk_sets_stream_error_not_fabricated_done() {
        let mut state = OpenRouterStreamState::new();
        let events = state.handle_chunk(&serde_json::json!({
            "error": {"message": "insufficient credits", "type": "insufficient_quota"}
        }));
        assert!(events.is_empty());
        assert_eq!(state.stream_error.as_deref(), Some("insufficient credits"));
        assert!(!state.done);
    }

    #[test]
    fn finalize_tool_call_returns_decode_error_on_invalid_json() {
        let result = finalize_tool_call(
            "call_1".to_string(),
            "get_weather".to_string(),
            "{not valid json",
        );
        assert!(matches!(result, Err(ModelError::Decode(_))));
    }

    #[test]
    fn finalize_tool_call_drops_unparseable_arguments_without_panicking() {
        let mut state = OpenRouterStreamState::new();
        state.handle_chunk(&message_json(
            0,
            serde_json::json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "bad", "arguments": "{not valid"}}]}),
            None,
        ));
        let events = state.handle_chunk(&message_json(0, serde_json::json!({}), Some("tool_calls")));
        assert!(events.is_empty());
    }
}
