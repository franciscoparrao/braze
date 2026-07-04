use std::path::PathBuf;

use async_trait::async_trait;
use braze_events::AgentEvent;
use braze_types::SessionId;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::SessionError;
use crate::store::SessionStore;

/// [`SessionStore`] backed by a JSON-lines rollout file per session, on
/// disk, under a base directory supplied by the caller.
///
/// This crate is Nivel 1 (only depends on `braze-types` + `braze-events`,
/// per PLAN.md) and deliberately does not know about `braze-config` — the
/// base directory (which in the full system will come from
/// `braze-config::Config.session_dir`) is an explicit constructor
/// parameter, resolved by whoever composes this crate (`braze-engine` /
/// `braze-cli`, Fase 5).
///
/// File layout: `<base_dir>/<session_id>.jsonl`, one
/// `serde_json`-serialized [`AgentEvent`] per line, appended in event
/// order.
///
/// ## Concurrency
///
/// The MVP assumes a **single writer process** per base directory (no
/// cross-process file locking). Within a process, `append` is
/// synchronized by an internal [`tokio::sync::Mutex`] shared across *all*
/// sessions of this store instance, so concurrent `append` calls (even to
/// different session files) never interleave partial writes. This is
/// coarser than strictly necessary (a per-session lock would allow
/// concurrent writers on different sessions to proceed in parallel), but
/// it is simple and correct for the MVP's expected usage pattern (one
/// engine loop per process, appending sequentially as it processes
/// events). Revisit if/when multi-process access to the same session
/// directory becomes a real requirement.
#[derive(Debug)]
pub struct FileSessionStore {
    base_dir: PathBuf,
    write_lock: Mutex<()>,
}

impl FileSessionStore {
    /// Creates a store rooted at `base_dir`. The directory does not need
    /// to exist yet — it is created (recursively) on first `append`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    /// Base directory this store persists rollout files under.
    pub fn base_dir(&self) -> &std::path::Path {
        &self.base_dir
    }

    fn path_for(&self, session: &SessionId) -> PathBuf {
        self.base_dir.join(format!("{session}.jsonl"))
    }
}

#[async_trait]
impl SessionStore for FileSessionStore {
    async fn append(&self, session: &SessionId, event: &AgentEvent) -> Result<(), SessionError> {
        let mut line =
            serde_json::to_string(event).map_err(|e| SessionError::Write(e.to_string()))?;
        line.push('\n');

        let _guard = self.write_lock.lock().await;

        tokio::fs::create_dir_all(&self.base_dir)
            .await
            .map_err(|e| SessionError::Write(format!("creating {:?}: {e}", self.base_dir)))?;

        let path = self.path_for(session);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| SessionError::Write(format!("opening {path:?}: {e}")))?;

        file.write_all(line.as_bytes())
            .await
            .map_err(|e| SessionError::Write(format!("writing {path:?}: {e}")))?;
        file.flush()
            .await
            .map_err(|e| SessionError::Write(format!("flushing {path:?}: {e}")))?;

        Ok(())
    }

    async fn load(&self, session: &SessionId) -> Result<Vec<AgentEvent>, SessionError> {
        let path = self.path_for(session);
        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SessionError::NotFound(session.to_string())
            } else {
                SessionError::Read(format!("reading {path:?}: {e}"))
            }
        })?;

        let lines: Vec<&str> = content.lines().collect();
        let mut events = Vec::new();
        for (line_no, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<AgentEvent>(line) {
                Ok(event) => events.push(event),
                Err(e) => {
                    // A process kill/crash mid-`append` can leave the
                    // *last* line of the file partially written (the
                    // in-process write completed, but the bytes never
                    // made it to disk in full) — see
                    // docs/AUDITORIA-2026-07.md, hallazgo C5. Only the
                    // final line gets this tolerance: a malformed line
                    // anywhere else in the file is real corruption, not a
                    // truncated write, and must still fail loudly.
                    if line_no == lines.len() - 1 {
                        tracing::warn!(
                            path = ?path,
                            line = line_no + 1,
                            error = %e,
                            "discarding malformed final line in rollout log (likely a truncated write from a crash mid-append); session recovered up to this point"
                        );
                        break;
                    }
                    return Err(SessionError::Read(format!(
                        "{path:?}:{line}: malformed event: {e}",
                        line = line_no + 1
                    )));
                }
            }
        }
        Ok(events)
    }

    async fn list_sessions(&self) -> Result<Vec<SessionId>, SessionError> {
        let mut entries = match tokio::fs::read_dir(&self.base_dir).await {
            Ok(entries) => entries,
            // No directory yet means no sessions yet, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(SessionError::Read(format!(
                    "reading dir {:?}: {e}",
                    self.base_dir
                )));
            }
        };

        let mut sessions = Vec::new();
        loop {
            let entry = entries
                .next_entry()
                .await
                .map_err(|e| SessionError::Read(format!("iterating dir: {e}")))?;
            let Some(entry) = entry else { break };

            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                // Files that don't parse back into a SessionId aren't
                // sessions this store created (e.g. stray files dropped
                // into the directory by hand) — skip rather than error,
                // list_sessions enumerates *our* sessions.
                if let Ok(id) = stem.parse::<SessionId>() {
                    sessions.push(id);
                }
            }
        }
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::ToolResult;

    /// Creates a fresh, unique temp directory for a test and returns it
    /// alongside a store rooted there. No `tempfile` dependency (reported
    /// as a deliberate MVP decision) — cleaned up by hand at the end of
    /// each test via `remove_dir_all`.
    fn temp_store(test_name: &str) -> (FileSessionStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "braze-session-test-{test_name}-{}",
            SessionId::new()
        ));
        (FileSessionStore::new(dir.clone()), dir)
    }

    #[tokio::test]
    async fn append_then_load_roundtrips_events_in_order() {
        let (store, dir) = temp_store("roundtrip");
        let session = SessionId::new();

        let events = vec![
            AgentEvent::UserMessage {
                text: "hola".to_string(),
            },
            AgentEvent::AssistantText {
                text: "hola de vuelta".to_string(),
            },
            AgentEvent::ToolCallStarted {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                background: false,
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "contenido".to_string(),
                    is_error: false,
                },
            },
        ];

        for event in &events {
            store.append(&session, event).await.unwrap();
        }

        let loaded = store.load(&session).await.unwrap();
        assert_eq!(loaded.len(), events.len());
        match (&loaded[0], &events[0]) {
            (AgentEvent::UserMessage { text: a }, AgentEvent::UserMessage { text: b }) => {
                assert_eq!(a, b)
            }
            _ => panic!("unexpected event ordering/shape"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C5: a partially-written final line (what a
    /// crash mid-`write_all` would leave behind) must not fail the whole
    /// load — the events written before it are still recoverable.
    #[tokio::test]
    async fn load_tolerates_a_truncated_final_line() {
        let (store, dir) = temp_store("truncated-final-line");
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "hola".to_string(),
                },
            )
            .await
            .unwrap();
        store
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "hola de vuelta".to_string(),
                },
            )
            .await
            .unwrap();

        // Simulate a crash mid-write: append a truncated JSON fragment
        // with no closing brace/newline, exactly what an interrupted
        // `write_all` of a third event could leave on disk.
        let path = store.path_for(&session);
        let mut raw = tokio::fs::read_to_string(&path).await.unwrap();
        raw.push_str(r#"{"type":"user_message","text":"cor"#);
        tokio::fs::write(&path, raw).await.unwrap();

        let events = store.load(&session).await.expect("load should not fail");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A malformed line that is NOT the last one in the file is real
    /// corruption (not a truncated write) and must still fail loudly —
    /// the C5 tolerance is deliberately narrow.
    #[tokio::test]
    async fn load_still_fails_on_a_malformed_line_that_is_not_the_last_one() {
        let (store, dir) = temp_store("corrupt-middle-line");
        let session = SessionId::new();

        let path = store.path_for(&session);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let content = concat!(
            "{\"type\":\"user_message\",\"text\":\"hola\"}\n",
            "this is not valid json at all\n",
            "{\"type\":\"user_message\",\"text\":\"despues\"}\n",
        );
        tokio::fs::write(&path, content).await.unwrap();

        let err = store.load(&session).await.unwrap_err();
        assert!(matches!(err, SessionError::Read(_)));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn load_missing_session_returns_not_found() {
        let (store, dir) = temp_store("missing");
        let session = SessionId::new();

        let err = store.load(&session).await.unwrap_err();
        assert!(matches!(err, SessionError::NotFound(_)));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn list_sessions_on_missing_dir_is_empty_not_error() {
        let (store, _dir) = temp_store("no-such-dir");
        // Never created (no append happened), directory doesn't exist.
        let sessions = store.list_sessions().await.unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_enumerates_appended_sessions() {
        let (store, dir) = temp_store("list");
        let session_a = SessionId::new();
        let session_b = SessionId::new();

        store
            .append(
                &session_a,
                &AgentEvent::UserMessage {
                    text: "a".to_string(),
                },
            )
            .await
            .unwrap();
        store
            .append(
                &session_b,
                &AgentEvent::UserMessage {
                    text: "b".to_string(),
                },
            )
            .await
            .unwrap();

        let mut sessions = store.list_sessions().await.unwrap();
        sessions.sort_by_key(|s| s.to_string());
        let mut expected = vec![session_a, session_b];
        expected.sort_by_key(|s| s.to_string());

        assert_eq!(sessions, expected);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
