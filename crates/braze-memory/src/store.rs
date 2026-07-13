//! `ProjectMemoryStore` — persistence for [`ProjectMemory`], and its
//! file-backed implementation.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::memory::ProjectMemory;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("failed to read project memory: {0}")]
    Read(String),
    #[error("failed to write project memory: {0}")]
    Write(String),
}

/// Persistence for one project's [`ProjectMemory`]. Deliberately not
/// keyed by an explicit `project_key` parameter on every call the way
/// `SessionStore` is keyed by `SessionId` — an implementation is
/// constructed already pointed at exactly one project's file (see
/// [`FileProjectMemoryStore::new`]), because unlike sessions (many per
/// process, arbitrary ids) a `braze` invocation only ever has one
/// current project.
#[async_trait]
pub trait ProjectMemoryStore: Send + Sync {
    /// Loads the persisted memory, or `Ok(None)` if nothing has been
    /// saved for this project yet (not an error — the normal state for
    /// a project's first `braze` session).
    async fn load(&self) -> Result<Option<ProjectMemory>, MemoryError>;

    /// Persists `memory` in full, replacing whatever was there. Callers
    /// are responsible for merging into an in-memory copy first (load,
    /// mutate, save) — this trait has no partial-update method, mirroring
    /// how `FileSessionStore::append` is the only write primitive
    /// `SessionStore` exposes and callers build on top of it.
    async fn save(&self, memory: &ProjectMemory) -> Result<(), MemoryError>;
}

/// Persists one project's memory as a single pretty-printed JSON file.
/// Unlike [`FileSessionStore`](../braze_session/struct.FileSessionStore.html)'s
/// append-only JSONL rollout log (many events, one file per session,
/// never rewritten), this is ONE small file, fully overwritten on every
/// `save` — there's exactly one current state to persist, not a log to
/// replay.
pub struct FileProjectMemoryStore {
    /// Full path to the memory file (typically
    /// `<project_root>/.braze/memory.json` — see
    /// [`crate::project_key::default_memory_path`]), not just a base
    /// directory: unlike sessions (many files under one dir, one per
    /// id), this store only ever touches one file, so there's nothing a
    /// second path parameter would disambiguate.
    path: PathBuf,
    /// Serializes concurrent `save` calls from the SAME process (e.g. a
    /// hook and an explicit save command racing) — cross-process safety
    /// is out of scope for V1 (a single interactive `braze chat` is the
    /// only writer in practice; `FileSessionStore`'s `fs2` advisory lock
    /// exists because multiple *sessions* can be live in one process,
    /// which has no analogue here — one project memory file per
    /// process, not per session).
    write_lock: Mutex<()>,
}

impl FileProjectMemoryStore {
    /// `path` is the exact file to read/write — construct it via
    /// [`crate::project_key::default_memory_path`] in the common case.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[async_trait]
impl ProjectMemoryStore for FileProjectMemoryStore {
    async fn load(&self) -> Result<Option<ProjectMemory>, MemoryError> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                let memory: ProjectMemory = serde_json::from_slice(&bytes)
                    .map_err(|e| MemoryError::Read(format!("{:?}: {e}", self.path)))?;
                Ok(Some(memory))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MemoryError::Read(format!("{:?}: {e}", self.path))),
        }
    }

    async fn save(&self, memory: &ProjectMemory) -> Result<(), MemoryError> {
        let _guard = self.write_lock.lock().await;

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| MemoryError::Write(format!("creating {parent:?}: {e}")))?;
        }

        // Pretty-printed and key-stable (serde_json's struct field order
        // follows declaration order, not a HashMap's) — the design doc's
        // own risk note flags noisy diffs on a versioned file as a real
        // cost; stable formatting is the cheap half of that mitigation
        // (the other half, not writing on every trivial event, is the
        // hook's job, not the store's).
        let json = serde_json::to_string_pretty(memory)
            .map_err(|e| MemoryError::Write(format!("serializing: {e}")))?;

        // Write to a temp file then rename — an interrupted write must
        // never leave a half-written JSON file that the next `load`
        // then fails to parse. Same directory as the target so the
        // rename is same-filesystem (atomic on POSIX).
        let tmp_path = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp_path, json.as_bytes())
            .await
            .map_err(|e| MemoryError::Write(format!("{tmp_path:?}: {e}")))?;
        tokio::fs::rename(&tmp_path, &self.path)
            .await
            .map_err(|e| MemoryError::Write(format!("renaming {tmp_path:?} -> {:?}: {e}", self.path)))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SignalSource;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "braze-memory-store-test-{:?}-{}",
            std::thread::current().id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("memory.json")
    }

    #[tokio::test]
    async fn load_returns_none_when_no_file_exists_yet() {
        let store = FileProjectMemoryStore::new(temp_path());
        let loaded = store.load().await.expect("load must not error on a missing file");
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn save_then_load_round_trips() {
        let path = temp_path();
        let store = FileProjectMemoryStore::new(&path);

        let mut memory = ProjectMemory::new("proj-key");
        memory.record_touched_file("src/main.rs", "write_file", "t1");
        memory.record_completed_signal("wrote the CLI", SignalSource::TaskListCompletion, "t2");

        store.save(&memory).await.expect("save must succeed");
        let loaded = store.load().await.expect("load must succeed").expect("must find what was saved");

        assert_eq!(loaded.project_key, "proj-key");
        assert_eq!(loaded.touched_files.len(), 1);
        assert_eq!(loaded.touched_files[0].path, "src/main.rs");
        assert_eq!(loaded.completed_signals.len(), 1);
        assert_eq!(loaded.completed_signals[0].description, "wrote the CLI");

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    #[tokio::test]
    async fn save_creates_the_parent_directory() {
        let path = temp_path(); // parent (.../braze-memory-store-test-.../) doesn't exist yet
        let store = FileProjectMemoryStore::new(&path);
        let memory = ProjectMemory::new("proj-key");

        store.save(&memory).await.expect("save must create missing parent dirs");
        assert!(path.exists());

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    #[tokio::test]
    async fn a_second_save_overwrites_rather_than_appending() {
        let path = temp_path();
        let store = FileProjectMemoryStore::new(&path);

        let mut first = ProjectMemory::new("proj-key");
        first.record_touched_file("a.rs", "write_file", "t1");
        store.save(&first).await.unwrap();

        let mut second = ProjectMemory::new("proj-key");
        second.record_touched_file("b.rs", "write_file", "t2");
        store.save(&second).await.unwrap();

        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.touched_files.len(), 1, "save replaces, it does not append");
        assert_eq!(loaded.touched_files[0].path, "b.rs");

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }
}
