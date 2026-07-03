use async_trait::async_trait;
use braze_types::{ToolCall, ToolResult, ToolStub};
use serde::{Deserialize, Serialize};

use crate::error::ToolError;

/// Full tool definition, resolved lazily — only fetched once the model is
/// about to call (or a search step is deciding whether to call) this exact
/// tool. See [`ToolStub`] for the cheap, always-in-context counterpart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// Raw JSON Schema, in the shape the model API expects.
    pub input_schema: serde_json::Value,
}

/// A source of tools: local built-ins ([`braze-tools-local`](../braze_tools_local))
/// or one MCP server connection ([`braze-mcp-client`](../braze_mcp_client)).
///
/// Both implementers are composed as siblings inside a single
/// [`ToolRegistry`](crate::ToolRegistry) — neither implementer depends on
/// the other, and `braze-engine` is the only crate that knows about both.
#[async_trait]
pub trait ToolProvider: Send + Sync {
    /// Stable identifier for this provider (e.g. "local", "mcp:filesystem").
    fn provider_id(&self) -> &str;

    /// Cheap: names + one-liners only, for the flat always-in-context list.
    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError>;

    /// Resolve the full schema for exactly one tool by name. Returns
    /// `Ok(None)` if this provider doesn't own `name` (the registry then
    /// tries the next provider).
    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError>;

    /// Execute a tool call. `call.name` is always a name this provider
    /// previously advertised via [`list_stubs`](ToolProvider::list_stubs).
    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError>;
}
