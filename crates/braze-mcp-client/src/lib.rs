//! MCP client over `rmcp`, implementing [`braze_tools_core::ToolProvider`].
//!
//! Wraps exactly one MCP server connection, spawned as a stdio subprocess
//! (`name` + `command` + `args`). Sits as a sibling of `braze-tools-local`
//! behind the same [`ToolProvider`](braze_tools_core::ToolProvider) seam —
//! see PLAN.md, "carga diferida de herramientas": only cheap
//! [`ToolStub`](braze_types::ToolStub)s (name + one-line summary) go
//! in-context up front; the full JSON Schema for a tool is fetched from the
//! MCP server only when [`McpToolProvider::resolve_schema`] is actually
//! called, right before dispatch.

mod error;
mod negative_cache;
mod provider;
mod summary;

pub use provider::McpToolProvider;
