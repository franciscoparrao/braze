//! The agentic loop: orchestrates model calls, tool dispatch, background
//! tasks, context compaction and permission checks. Composition root.
//!
//! `Engine` (see [`engine::Engine`]) is the only crate in the workspace
//! that simultaneously depends on `braze-model`, `braze-tools-core`,
//! `braze-tools-local`, `braze-mcp-client`, `braze-session` and
//! `braze-events` — everything else in the workspace is a seam this crate
//! reconciles, per PLAN.md's dependency graph.

mod engine;
mod hooks;
mod tool_search;
mod error;
mod history;
#[cfg(test)]
mod protocol_check;

pub use engine::{DEFAULT_TACTICAL_COMPACTION_THRESHOLD, Engine, synthesize_orphan_repairs};
pub use hooks::{EngineHook, PromptBudgetAuditHook};
pub use error::EngineError;
