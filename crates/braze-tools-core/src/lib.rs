//! `ToolProvider` trait, `ToolRegistry` and deferred tool-schema loading.
//!
//! Frozen contract (PLAN.md): `braze-tools-local` and `braze-mcp-client`
//! both implement [`ToolProvider`] and are composed here into one flat
//! [`ToolRegistry`] that the engine talks to. Tool schemas are resolved
//! lazily via [`ToolRegistry::resolve`] — only [`ToolStub`]s (name + one-line
//! summary) are ever listed up front, keeping prompt size flat regardless
//! of how many tools/MCP servers are connected.

mod error;
mod provider;
mod registry;

pub use error::ToolError;
pub use provider::{ToolProvider, ToolSchema};
pub use registry::ToolRegistry;

// Re-exported for convenience: `ToolStub` is the shared vocabulary type
// (lives in `braze-types` so `braze-model` can reference it without
// depending on `braze-tools-core` — see braze-types/src/tool.rs).
pub use braze_types::ToolStub;
