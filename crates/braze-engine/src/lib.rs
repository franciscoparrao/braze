//! The agentic loop: orchestrates model calls, tool dispatch, background
//! tasks, context compaction and permission checks. Composition root.
//!
//! `Engine` (see [`engine::Engine`]) is the only crate in the workspace
//! that simultaneously depends on `braze-model`, `braze-tools-core`,
//! `braze-tools-local`, `braze-mcp-client`, `braze-session` and
//! `braze-events` — everything else in the workspace is a seam this crate
//! reconciles, per PLAN.md's dependency graph.

mod edit_fence;
mod editor;
mod engine;
mod error;
mod exploration;
mod history;
mod hooks;
mod project_memory_hook;
#[cfg(test)]
mod protocol_check;
mod rescue;
mod task_list;
mod tool_search;

pub use engine::{
    DEFAULT_TACTICAL_COMPACTION_THRESHOLD, Engine, VerificationConfig, synthesize_orphan_repairs,
};
pub use error::EngineError;
pub use hooks::{EngineHook, PromptBudgetAuditHook};
pub use project_memory_hook::ProjectMemoryHook;
pub use tool_search::initially_visible_stubs;
