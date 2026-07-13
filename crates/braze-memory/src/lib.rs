//! Cross-session project memory — `docs/project-memory-design.md`.
//!
//! Distinct from [`braze_session::SessionStore`] on purpose: that crate
//! persists ONE conversation for exact replay (`--resume <id>`); this
//! one persists a small, deterministic, capped summary of a PROJECT
//! that a brand-new session can read at startup, the same way
//! `PLAN.md`/`CLAUDE.md` already function as this very repo's
//! human-curated memory across work sessions. See the design doc's
//! "Por qué esto NO es lo que `SessionStore` ya hace" for the full
//! comparison.
//!
//! `Off by default` everywhere a composition root wires this in — same
//! posture as the typed task list (`Config::enable_task_list`) this
//! crate's `TaskCompleted` signal depends on: a new lever earns its
//! default via `braze-bench`'s `+ablate:project-memory`, not by
//! assumption.

mod memory;
mod project_key;
mod render;
mod store;

pub use memory::{CompletedSignal, MemoryMeta, ProjectMemory, SignalSource, TouchedFile};
pub use project_key::{default_memory_path, project_key_for, resolve_project_root};
pub use render::{DEFAULT_PROJECT_MEMORY_BUDGET_TOKENS, render_project_memory_section};
pub use store::{FileProjectMemoryStore, MemoryError, ProjectMemoryStore};
