use serde::{Deserialize, Serialize};

/// A request from the model to invoke a tool by name with arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// The outcome of executing a [`ToolCall`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// A cheap, prompt-sized descriptor: name + one-line summary, plus an
/// optional real JSON Schema.
///
/// This is what gets listed in-context for every connected tool source, on
/// every turn. Two policies coexist here: for a small, static set of tools
/// (the local built-ins) the real `input_schema` is cheap to include up
/// front — no I/O, no round-trip — so `input_schema` is `Some`. For an
/// unbounded/dynamic set (MCP servers) including every real schema on
/// every turn would bloat the prompt, so `input_schema` stays `None` and
/// the schema is resolved on demand by
/// `braze-tools-core::ToolRegistry::resolve`, only once the model is about
/// to invoke (or is deciding whether to invoke) this specific tool. Lives
/// in `braze-types` (not `braze-tools-core`) so that `braze-model` can
/// reference it without depending on `braze-tools-core` — the two are
/// siblings and must not depend on each other (see PLAN.md, dependency
/// graph).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStub {
    pub name: String,
    pub summary: String,
    pub source: String,
    pub input_schema: Option<serde_json::Value>,
}
