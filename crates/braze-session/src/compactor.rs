use braze_events::AgentEvent;

use crate::error::SessionError;

/// The already-durable part of a session: information that's settled and
/// must never be re-summarized, only appended to (completed tool results,
/// decisions already folded into `summary`). Distinct from the tactical
/// window, which is raw and still subject to compaction.
#[derive(Debug, Clone, Default)]
pub struct DurableState {
    pub summary: String,
    pub durable_events: Vec<AgentEvent>,
}

/// Splits a raw event log into durable state (never re-summarized) and a
/// live tactical window (raw events, compacted only when the context
/// window fills up). This is what keeps compaction *differential* instead
/// of a blind truncation of the whole log.
pub trait ContextCompactor: Send + Sync {
    fn split(&self, events: &[AgentEvent]) -> (DurableState, Vec<AgentEvent>);

    /// Summarizes `tactical` into prose the model can read in place of the
    /// raw events it replaces. MVP: a real but simple split (last N raw
    /// turns), not a tuned summarizer.
    fn compact_tactical(&self, tactical: &[AgentEvent]) -> Result<String, SessionError>;
}
