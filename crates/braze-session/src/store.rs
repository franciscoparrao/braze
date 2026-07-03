use async_trait::async_trait;
use braze_events::AgentEvent;
use braze_types::SessionId;

use crate::error::SessionError;

/// Persists a session's raw event log (MVP: JSON-lines rollout file on
/// disk). Append-only — compaction (see [`ContextCompactor`](crate::ContextCompactor))
/// operates on the loaded log, it never rewrites what's on disk.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn append(&self, session: &SessionId, event: &AgentEvent) -> Result<(), SessionError>;
    async fn load(&self, session: &SessionId) -> Result<Vec<AgentEvent>, SessionError>;
    async fn list_sessions(&self) -> Result<Vec<SessionId>, SessionError>;
}
