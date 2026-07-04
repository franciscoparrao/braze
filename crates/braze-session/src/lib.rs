//! On-disk session persistence and differential context compaction.
//!
//! Frozen contract (PLAN.md): [`SessionStore`] persists the raw
//! [`AgentEvent`](braze_events::AgentEvent) log; [`ContextCompactor`]
//! never re-summarizes durable state, only appends to it — only the
//! tactical (live) window gets compacted. Bodies implemented in Fase 3
//! (Nivel 1, most novel/least-precedented piece — single agent, single PR).

mod compactor;
mod error;
mod file_store;
mod simple_compactor;
mod store;

pub use compactor::{ContextCompactor, DurableState};
pub use error::SessionError;
pub use file_store::FileSessionStore;
pub use simple_compactor::{DEFAULT_TACTICAL_WINDOW, SimpleContextCompactor};
pub use store::SessionStore;
