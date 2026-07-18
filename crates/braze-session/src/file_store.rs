use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use braze_events::AgentEvent;
use braze_types::SessionId;
use fs2::FileExt;
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
/// Within a process, `append` is synchronized by an internal
/// [`tokio::sync::Mutex`] shared across *all* sessions of this store
/// instance, so concurrent `append` calls (even to different session
/// files) never interleave partial writes. This is coarser than strictly
/// necessary (a per-session lock would allow concurrent writers on
/// different sessions to proceed in parallel), but it is simple and
/// correct for the MVP's expected usage pattern (one engine loop per
/// process, appending sequentially as it processes events).
///
/// Across processes: N-27 (docs/AUDITORIA-2026-07-v2.md) — a second
/// process appending to the *same* session concurrently (e.g. two
/// overlapping `braze chat --resume <id>` invocations) used to race
/// silently: each could independently decide the same orphaned
/// `tool_use` needs repairing and both append their own synthetic
/// `ToolCallCompleted`, corrupting the log with a duplicate result. This
/// process now takes an advisory exclusive lock (`fs2`, `flock` on Unix)
/// on the session's own file the first time it appends to that session,
/// held until this store drops — a second process's first `append` call
/// for the same session fails immediately and loudly instead of racing.
/// See the `session_locks` field below.
///
/// ## In-memory cache (C11, docs/AUDITORIA-2026-07.md)
///
/// `Engine::run_turn` calls `load` once per round, and a turn can run many
/// rounds — re-reading and re-parsing the *entire* on-disk log every round
/// is O(n²) I/O+parsing per session, competing with model inference for
/// CPU on a CPU-only box. `cache` holds the fully-parsed event list per
/// session once it has been read from disk at least once in this
/// process's lifetime; `append` keeps a *warm* cache entry up to date
/// in-memory (no disk re-read needed), and leaves a *cold* one (not yet
/// loaded in this process) alone — the next `load` call falls back to the
/// disk read, which already includes everything appended so far, and
/// warms the cache from that point on. This is only sound because of the
/// single-writer assumption above: nothing else can append to the same
/// session's file underneath this process and invalidate the cache.
#[derive(Debug)]
pub struct FileSessionStore {
    base_dir: PathBuf,
    write_lock: Mutex<()>,
    cache: Mutex<HashMap<SessionId, Vec<AgentEvent>>>,
    /// One dedicated, locked file handle per session this process has
    /// appended to — held purely to keep the advisory lock alive for the
    /// rest of the process's lifetime (never read/written through
    /// directly; the actual data writes go through their own handle in
    /// `append`). N-27, see the struct-level doc comment's "Concurrency"
    /// section. A `std::sync::Mutex` (not `tokio::sync::Mutex`): only ever
    /// locked for a synchronous map check/insert, already inside
    /// `append`'s own `write_lock` critical section.
    session_locks: std::sync::Mutex<HashMap<SessionId, std::fs::File>>,
}

impl FileSessionStore {
    /// Creates a store rooted at `base_dir`. The directory does not need
    /// to exist yet — it is created (recursively) on first `append`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            write_lock: Mutex::new(()),
            cache: Mutex::new(HashMap::new()),
            session_locks: std::sync::Mutex::new(HashMap::new()),
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

        // N-27 (docs/AUDITORIA-2026-07-v2.md): acquire (once per session,
        // per process) the advisory lock described in the struct's doc
        // comment — a dedicated handle kept alive purely to hold it.
        {
            let mut locks = self
                .session_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !locks.contains_key(session) {
                let lock_file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| {
                        SessionError::Write(format!("opening {path:?} for locking: {e}"))
                    })?;
                lock_file.try_lock_exclusive().map_err(|e| {
                    SessionError::Write(format!(
                        "another process already holds session {session}'s write lock \
                         ({path:?}): {e}"
                    ))
                })?;
                locks.insert(*session, lock_file);
            }
        }

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
        // Bajo (docs/AUDITORIA-2026-07-v2.md, "flush() sin sync_data()",
        // C14): `flush()` only ensures the write reaches the OS page
        // cache, not the physical disk — a power failure right after
        // `append` returns `Ok` could still lose the just-appended line.
        file.sync_data()
            .await
            .map_err(|e| SessionError::Write(format!("syncing {path:?}: {e}")))?;

        // Only extend an already-warm cache entry — a cold one means this
        // process has never `load`ed this session yet, so it may not
        // reflect events another process (or an earlier run) already
        // wrote to disk; blindly seeding a one-event cache here would
        // silently hide that prior history from the next `load`. The next
        // `load` call for a cold session does the full disk read (which
        // already includes this event) and warms the cache from there.
        let mut cache = self.cache.lock().await;
        if let Some(events) = cache.get_mut(session) {
            events.push(event.clone());
        }

        Ok(())
    }

    async fn load(&self, session: &SessionId) -> Result<Vec<AgentEvent>, SessionError> {
        {
            let cache = self.cache.lock().await;
            if let Some(events) = cache.get(session) {
                return Ok(events.clone());
            }
        }

        // N-7 (docs/AUDITORIA-2026-07-v2.md): the cold-cache disk read
        // below must not race a concurrent `append` — without holding the
        // same `write_lock` `append` holds for its entire write, a `load`
        // here could observe a half-written final line mid-`write_all`,
        // silently discard it via the C5 tolerance further down, and warm
        // the cache *without* an event that actually finished writing to
        // disk moments later. The next `load_messages` would then see the
        // matching `AssistantToolCall` with no result and synthesize a
        // *second* `ToolCallCompleted` for the same id — two
        // `tool_result`s for one `tool_use`, a permanent 400 on every
        // future resume. Cheap in the common case: only a cold load ever
        // reaches this point (the warm-cache check above already returned
        // for every other call).
        let _guard = self.write_lock.lock().await;

        // Re-check: another `load` could have warmed the cache while this
        // one waited for the lock.
        {
            let cache = self.cache.lock().await;
            if let Some(events) = cache.get(session) {
                return Ok(events.clone());
            }
        }

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
                        // N-5 (docs/AUDITORIA-2026-07-v2.md): C5 tolerated
                        // this on *read*, but never repaired the file —
                        // the next `append` would weld its event onto the
                        // truncated fragment (nothing separates them; the
                        // fragment itself ends in no newline), producing
                        // one malformed line that is no longer the *last*
                        // one after any further event is appended. `load`
                        // on the *next* resume would then fail hard
                        // instead of tolerating it — a one-turn hiccup
                        // turning into a permanently unresumable session.
                        //
                        // v8 K-5 (docs/AUDITORIA-2026-07-v8.md): the
                        // repair must hold the N-27 advisory lock. A
                        // read-only process (`braze permissions suggest`
                        // loads EVERY session) that catches a live
                        // writer mid-`write_all` would otherwise misread
                        // the partial line as a crash artifact and
                        // truncate the live process's log. If we already
                        // hold this session's lock we repair directly;
                        // otherwise we take it transiently — and if
                        // another process holds it, we tolerate in
                        // memory and leave the file alone (the writer's
                        // own flow finishes the line).
                        let already_held = {
                            let locks = self
                                .session_locks
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            locks.contains_key(session)
                        };
                        let transient_lock = if already_held {
                            None
                        } else {
                            match std::fs::OpenOptions::new().append(true).open(&path) {
                                Ok(lock_file) if lock_file.try_lock_exclusive().is_ok() => {
                                    Some(lock_file)
                                }
                                _ => {
                                    tracing::warn!(
                                        path = ?path,
                                        "another process holds this session's write lock; \
                                         tolerating the partial final line in memory without \
                                         repairing the file (v8 K-5)"
                                    );
                                    break;
                                }
                            }
                        };
                        let valid_prefix = if line_no == 0 {
                            String::new()
                        } else {
                            format!("{}\n", lines[..line_no].join("\n"))
                        };
                        if let Err(write_err) = tokio::fs::write(&path, &valid_prefix).await {
                            tracing::warn!(
                                path = ?path,
                                error = %write_err,
                                "failed to truncate rollout log after a truncated final line; \
                                 the fragment will remain on disk until the next successful append"
                            );
                        }
                        // A transiently-taken lock is released here (the
                        // file handle closes); a reader must not keep
                        // sessions locked after repairing them.
                        drop(transient_lock);
                        break;
                    }
                    return Err(SessionError::Read(format!(
                        "{path:?}:{line}: malformed event: {e}",
                        line = line_no + 1
                    )));
                }
            }
        }

        let mut cache = self.cache.lock().await;
        cache.insert(*session, events.clone());

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

    /// Regression test for N-5 (docs/AUDITORIA-2026-07-v2.md): C5 only
    /// tolerated a truncated final line *in memory* — the file itself was
    /// never repaired, so the next `append` would weld its event onto the
    /// leftover fragment (nothing separates them), producing one
    /// malformed line that's no longer the file's *last* line once a
    /// further event is appended — `load` on the *next* resume would then
    /// fail hard instead of tolerating it, turning a one-turn hiccup into
    /// a permanently unresumable session.
    #[tokio::test]
    async fn load_repairs_the_file_after_a_truncated_final_line_not_just_tolerates_it() {
        let (store, dir) = temp_store("truncated-final-line-repair");
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

        let path = store.path_for(&session);
        let mut raw = tokio::fs::read_to_string(&path).await.unwrap();
        raw.push_str(r#"{"type":"user_message","text":"cor"#);
        tokio::fs::write(&path, raw).await.unwrap();

        // Simulates the original process actually exiting (releasing its
        // N-27 advisory lock on the session file) before the next resume
        // — otherwise `store`'s still-open locked handle would make the
        // upcoming `fresh`/`second_resume` stores' appends fail exactly
        // as a genuinely concurrent second process's would.
        drop(store);

        // A fresh store (cold cache) pointed at the same directory, so
        // `load` genuinely reads — and, if the fix works, repairs — the
        // file on disk rather than serving from `store`'s own cache.
        let fresh = FileSessionStore::new(dir.clone());
        let events = fresh.load(&session).await.expect("load should not fail");
        assert_eq!(events.len(), 2);

        // The file on disk must now contain ONLY the two valid lines —
        // the fragment must be gone, not merely skipped in memory.
        let repaired_raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            repaired_raw.lines().count(),
            2,
            "the truncated fragment must have been removed from disk, not just \
             skipped while parsing: {repaired_raw:?}"
        );

        // Simulate the *next* resume (a brand new process/store instance):
        // appending a third event and loading again must round-trip
        // cleanly. Before the fix, the leftover fragment welded to this
        // new event would fail to parse, and — being no longer the file's
        // last line once a *fourth* event were appended — would error
        // hard on some future resume instead of tolerating it.
        fresh
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "tercero".to_string(),
                },
            )
            .await
            .unwrap();
        let second_resume = FileSessionStore::new(dir.clone());
        let events = second_resume
            .load(&session)
            .await
            .expect("the second resume must still load cleanly");
        assert_eq!(events.len(), 3);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-7 (docs/AUDITORIA-2026-07-v2.md): `load`'s
    /// cold-cache disk read must not race a concurrent `append` — without
    /// serializing on the same lock `append` holds for its entire write, a
    /// `load` could observe a half-written file, silently tolerate it as
    /// if it were a genuinely truncated crash artifact (C5), and warm the
    /// cache without an event that finishes writing moments later.
    #[tokio::test]
    async fn cold_load_waits_for_an_in_flight_append_instead_of_racing_it() {
        let (store, dir) = temp_store("cold-load-vs-append-race");
        let session = SessionId::new();
        let store = std::sync::Arc::new(store);

        // Hold the same lock `append` would hold while writing, to
        // simulate an in-flight append that hasn't finished yet.
        let guard = store.write_lock.lock().await;

        let store_clone = std::sync::Arc::clone(&store);
        let load_task = tokio::spawn(async move { store_clone.load(&session).await });

        // Give the spawned `load` a chance to reach (and block on) the
        // lock before we "finish the write" and release it.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = store.path_for(&session);
        tokio::fs::write(&path, "{\"type\":\"user_message\",\"text\":\"hola\"}\n")
            .await
            .unwrap();

        drop(guard);

        let events = load_task
            .await
            .unwrap()
            .expect("load should succeed once the lock is released");
        assert_eq!(
            events.len(),
            1,
            "load must observe the fully-written content, not race ahead of it"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-27 (docs/AUDITORIA-2026-07-v2.md): a second
    /// store instance (standing in for a second process — the lock is on
    /// the file, not any in-process state) appending to the same session
    /// must be rejected immediately instead of silently racing repair
    /// decisions with the first.
    #[tokio::test]
    async fn a_second_store_cannot_append_to_a_session_the_first_already_locked() {
        let (store_a, dir) = temp_store("cross-store-session-lock");
        let session = SessionId::new();

        store_a
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "hola".to_string(),
                },
            )
            .await
            .expect("first store should acquire the lock and append fine");

        let store_b = FileSessionStore::new(dir.clone());
        let result_b = store_b
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "hola de nuevo".to_string(),
                },
            )
            .await;
        assert!(
            matches!(result_b, Err(SessionError::Write(_))),
            "expected the second store to be rejected while the first holds the lock, got {result_b:?}"
        );

        // Once the first store (and its locked file handle) drops, the
        // lock releases and a fresh store can append normally.
        drop(store_a);
        let store_c = FileSessionStore::new(dir.clone());
        store_c
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "hola otra vez".to_string(),
                },
            )
            .await
            .expect("a third store should succeed once the lock is released");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A malformed line that is NOT the last one in the file is real
    /// corruption (not a truncated write) and must still fail loudly —
    /// the C5 tolerance is deliberately narrow.
    /// v8 K-5 (docs/AUDITORIA-2026-07-v8.md): the N-5 repair must not
    /// run while ANOTHER process holds the session's N-27 write lock — a
    /// read-only process (`braze permissions suggest` loads every
    /// session) catching a live writer mid-`write_all` would misread the
    /// partial line as a crash artifact and truncate the live process's
    /// log. With the lock held elsewhere: tolerate in memory, leave the
    /// file byte-for-byte alone. With the lock free: repair as before.
    #[tokio::test]
    async fn load_does_not_repair_while_another_process_holds_the_write_lock() {
        let (store, dir) = temp_store("k5-no-repair-under-foreign-lock");
        let session = SessionId::new();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = store.path_for(&session);

        // Log written raw (this store never appended → holds no lock):
        // one valid event plus a mid-write fragment, exactly what a
        // LIVE writer's `write_all` can look like from a reader racing it.
        let raw = format!(
            "{}\n{}",
            r#"{"type":"user_message","text":"hola"}"#,
            r#"{"type":"user_message","text":"cor"#
        );
        tokio::fs::write(&path, &raw).await.unwrap();

        // The "live writer": a separate handle holding the advisory lock.
        let writer_lock = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writer_lock.try_lock_exclusive().unwrap();

        let events = store.load(&session).await.expect("load must tolerate");
        assert_eq!(events.len(), 1, "the partial line is skipped in memory");
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            on_disk, raw,
            "the live writer's file must not be touched while its lock is held"
        );

        // Writer gone (lock released): a cold store's load now repairs.
        fs2::FileExt::unlock(&writer_lock).unwrap();
        drop(writer_lock);
        let fresh = FileSessionStore::new(dir.clone());
        let events = fresh.load(&session).await.expect("load should not fail");
        assert_eq!(events.len(), 1);
        let repaired = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            repaired.lines().count(),
            1,
            "with the lock free, the fragment must be repaired away: {repaired:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

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

    /// Regression test for C11: once a session has been `load`ed at least
    /// once (warming the cache), a subsequent `append` must keep serving
    /// `load` from memory instead of re-reading disk — proven here by
    /// corrupting the on-disk file *after* warming the cache and
    /// confirming `load` still succeeds with the correct, cache-backed
    /// contents instead of failing on the corruption a disk re-read would
    /// hit.
    #[tokio::test]
    async fn append_after_a_load_keeps_serving_from_cache_not_disk() {
        let (store, dir) = temp_store("cache-after-load");
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "primero".to_string(),
                },
            )
            .await
            .unwrap();

        // Warms the cache for this session.
        let events = store.load(&session).await.unwrap();
        assert_eq!(events.len(), 1);

        // A second append while the cache is warm.
        store
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "segundo".to_string(),
                },
            )
            .await
            .unwrap();

        // Corrupt the file directly on disk — if `load` fell back to
        // re-reading it now, this would surface as a `SessionError::Read`
        // (a malformed non-final line), not as the two well-formed events
        // already reflected in the cache.
        let path = store.path_for(&session);
        tokio::fs::write(&path, "not valid jsonl at all\n")
            .await
            .unwrap();

        let events = store.load(&session).await.expect("must serve from cache");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C11: an `append` before the session has ever
    /// been `load`ed (a cold cache) must not seed a partial cache entry —
    /// the very next `load` still needs to do the full disk read so it
    /// picks up everything on disk, not just what this process happened
    /// to append.
    #[tokio::test]
    async fn append_before_any_load_does_not_hide_prior_disk_state_on_the_next_load() {
        let (store, dir) = temp_store("cache-cold-append");
        let session = SessionId::new();

        // Simulate history already on disk from a previous process,
        // written without ever being `load`ed by this `store` instance.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "de un proceso anterior".to_string(),
                },
            )
            .await
            .unwrap();

        let events = store.load(&session).await.unwrap();
        assert_eq!(
            events.len(),
            1,
            "the cold-cache append must not have been silently skipped"
        );

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
