//! `ProjectMemoryHook` — deterministic, zero-model-call capture of
//! cross-session project memory (`docs/project-memory-design.md`). An
//! `EngineHook` (Paquete B′, `crate::hooks`), the same audit-only
//! attach point `PromptBudgetAuditHook` uses — this hook OBSERVES
//! events and persists a side effect (the memory file); it never
//! influences the turn, per `EngineHook`'s own H0/H1 contract.
//!
//! Two signals, both already free in the event log:
//! - `AgentEvent::ToolCallCompleted` for a successful `write_file`/
//!   `edit_file`, correlated back to its `AssistantToolCall` by id (the
//!   same correlation pattern `SimpleContextCompactor::compact_tactical`
//!   already uses for tool errors) — the tool's `path` argument becomes
//!   a `TouchedFile`.
//! - `AgentEvent::TaskCompleted` (the task list's `done` transition,
//!   `crate::task_list`) — becomes a `CompletedSignal` with
//!   `SignalSource::TaskListCompletion`, the only source this hook ever
//!   writes (see `braze_memory::SignalSource`'s doc comment for why
//!   the other two variants stay unpopulated in V1).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use braze_events::AgentEvent;
use braze_memory::{ProjectMemory, ProjectMemoryStore, SignalSource};

use crate::hooks::EngineHook;

/// Tool names whose `path` argument counts as a file touched — deliberately
/// narrow (write/edit only): `read_file`/`grep`/`glob` don't change
/// anything the memory is for.
const FILE_WRITING_TOOLS: &[&str] = &["write_file", "edit_file"];

pub struct ProjectMemoryHook {
    /// Accumulates in memory across the session; `on_event` is `&self`
    /// (no mutable engine access — `EngineHook`'s contract), so interior
    /// mutability here mirrors `Engine::task_list`'s own
    /// `std::sync::Mutex`.
    memory: Mutex<ProjectMemory>,
    /// `AssistantToolCall.id -> (name, arguments)`, kept only long enough
    /// to correlate the matching `ToolCallCompleted` — same shape as
    /// `SimpleContextCompactor::compact_tactical`'s `tool_names_by_id`,
    /// extended to carry `arguments` too (needed for the `path` field
    /// `ToolCallCompleted` doesn't itself carry).
    pending_calls: Mutex<HashMap<String, (String, serde_json::Value)>>,
    /// Queue into the dedicated saver task that owns the store (v8 K-8):
    /// `on_event` must do NO disk I/O — it runs under `hooks.rs`'s
    /// 250ms `HOOK_TIMEOUT`, which was designed for pure observers. The
    /// first draft awaited `store.save()` inline, with two failure
    /// modes on a slow disk: three slow saves auto-disabled the hook for
    /// the rest of the session (silent capture death), and a
    /// timed-out-but-still-running `tokio::fs` rename could land AFTER
    /// a later save and regress `memory.json`. A single consumer task
    /// serializes saves, so neither can happen.
    saver: tokio::sync::mpsc::UnboundedSender<SaveMsg>,
}

enum SaveMsg {
    /// A full-state snapshot to persist. Bursts coalesce in the saver
    /// task: every snapshot is the complete memory, so only the newest
    /// queued one needs to hit the disk.
    Snapshot(ProjectMemory),
    /// Ack once everything queued before this point is on disk — see
    /// [`ProjectMemoryHook::flush`].
    Flush(tokio::sync::oneshot::Sender<()>),
}

impl ProjectMemoryHook {
    /// Loads any existing memory for this project (via `store.load()`)
    /// to seed the in-memory copy — a fresh `braze` session continues
    /// accumulating onto what earlier sessions already captured, rather
    /// than starting blank each time. A load failure is logged and
    /// treated as "nothing saved yet" (same posture as
    /// `warm_up_ollama_model`'s best-effort failures elsewhere in this
    /// workspace): a broken memory file must never block a session from
    /// starting. A loaded memory whose `project_key` doesn't match the
    /// one asked for is discarded the same way (v8 K-7): a store pointed
    /// at the wrong file must fail safe, not inject another project's
    /// notes into this project's system prompt.
    ///
    /// Spawns the saver task (see `SaveMsg`) — requires a tokio runtime,
    /// which this `async fn` already implies.
    pub async fn new(
        store: std::sync::Arc<dyn ProjectMemoryStore>,
        project_key: impl Into<String>,
    ) -> Self {
        let project_key = project_key.into();
        let memory = match store.load().await {
            Ok(Some(memory)) if memory.project_key == project_key => memory,
            Ok(Some(memory)) => {
                tracing::warn!(
                    expected = %project_key,
                    found = %memory.project_key,
                    "project memory: loaded memory belongs to a different project; \
                     discarding it and starting fresh (v8 K-7)"
                );
                ProjectMemory::new(project_key)
            }
            Ok(None) => ProjectMemory::new(project_key),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "project memory: failed to load existing memory, starting fresh this session"
                );
                ProjectMemory::new(project_key)
            }
        };

        let (saver, mut save_rx) = tokio::sync::mpsc::unbounded_channel::<SaveMsg>();
        tokio::spawn(async move {
            while let Some(msg) = save_rx.recv().await {
                match msg {
                    SaveMsg::Snapshot(mut snapshot) => {
                        // Coalesce whatever queued up behind this one:
                        // each snapshot is the full state, so only the
                        // newest matters. A Flush drained here acks
                        // after the save that covers it.
                        let mut flush_acks = Vec::new();
                        loop {
                            match save_rx.try_recv() {
                                Ok(SaveMsg::Snapshot(newer)) => snapshot = newer,
                                Ok(SaveMsg::Flush(ack)) => flush_acks.push(ack),
                                Err(_) => break,
                            }
                        }
                        if let Err(err) = store.save(&snapshot).await {
                            // Best-effort by design: a failing disk must
                            // not fail turns (the hook already returned
                            // Ok) — but it must be visible.
                            tracing::warn!(
                                error = %err,
                                "project memory: background save failed"
                            );
                        }
                        for ack in flush_acks {
                            let _ = ack.send(());
                        }
                    }
                    SaveMsg::Flush(ack) => {
                        // Nothing queued before it — already settled.
                        let _ = ack.send(());
                    }
                }
            }
        });

        Self {
            memory: Mutex::new(memory),
            pending_calls: Mutex::new(HashMap::new()),
            saver,
        }
    }

    /// Resolves once every save queued so far is on disk. Composition
    /// roots call this after the last turn (and tests use it to make the
    /// asynchronous save observable) — without it, a process that exits
    /// right after a turn could drop the final queued save.
    pub async fn flush(&self) {
        let (ack, done) = tokio::sync::oneshot::channel();
        if self.saver.send(SaveMsg::Flush(ack)).is_ok() {
            let _ = done.await;
        }
    }

    /// A snapshot of the current in-memory state — for injecting into a
    /// fresh session's system prompt (`crate::project_memory_hook::render`)
    /// without going back to the store (this hook's own copy is already
    /// current: loaded once at construction, updated on every relevant
    /// event since).
    pub fn snapshot(&self) -> ProjectMemory {
        self.memory.lock().unwrap().clone()
    }

    fn now() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".to_string())
    }

    /// Mutates the in-memory copy and returns an owned clone to persist
    /// — a plain sync fn, not inlined into `on_event`, so the
    /// `MutexGuard` it creates is dropped when this function returns,
    /// nowhere near the `.await` the caller does with the result. A
    /// guard held across an await point makes the whole future
    /// non-`Send`, which `dyn EngineHook: Send + Sync` requires.
    fn record_touched_and_snapshot(&self, path: &str, tool: &str) -> ProjectMemory {
        let mut memory = self.memory.lock().unwrap();
        memory.record_touched_file(path, tool, Self::now());
        memory.clone()
    }

    /// Same reasoning as [`Self::record_touched_and_snapshot`].
    fn record_completed_and_snapshot(&self, description: &str) -> ProjectMemory {
        let mut memory = self.memory.lock().unwrap();
        memory.record_completed_signal(description.to_string(), SignalSource::TaskListCompletion, Self::now());
        memory.clone()
    }

    /// Hands a snapshot to the saver task — instant, no I/O (v8 K-8).
    /// `Err` only if the saver task is gone (runtime shutting down),
    /// which the hook dispatch layer treats like any other hook error.
    fn queue_save(&self, snapshot: ProjectMemory) -> Result<(), String> {
        self.saver
            .send(SaveMsg::Snapshot(snapshot))
            .map_err(|_| "project memory saver task is gone".to_string())
    }
}

#[async_trait]
impl EngineHook for ProjectMemoryHook {
    fn id(&self) -> &str {
        "project-memory"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), String> {
        match event {
            AgentEvent::AssistantToolCall {
                id,
                name,
                arguments,
            } if FILE_WRITING_TOOLS.contains(&name.as_str()) => {
                self.pending_calls
                    .lock()
                    .unwrap()
                    .insert(id.clone(), (name.clone(), arguments.clone()));
            }
            AgentEvent::ToolCallCompleted { id, result } if !result.is_error => {
                // The `pending_calls` lock must be fully released before
                // the `.await` below — a `MutexGuard` held across an
                // await point makes the whole future non-`Send`, which
                // `EngineHook`'s `dyn EngineHook: Send + Sync` requires.
                // Extracting into an owned `pending` on its own statement
                // (not inline in an `if let` condition, whose temporary
                // would otherwise live for the entire block) keeps the
                // guard's lifetime scoped to just this one line.
                let pending = self.pending_calls.lock().unwrap().remove(id);
                if let Some((name, arguments)) = pending
                    && let Some(path) = arguments.get("path").and_then(|v| v.as_str())
                {
                    let snapshot = self.record_touched_and_snapshot(path, &name);
                    self.queue_save(snapshot)?;
                }
            }
            // A failed tool call never touched anything worth
            // remembering — drop its pending entry so it doesn't leak
            // for the rest of the session (ids aren't reused, but there's
            // no reason to hold it either).
            AgentEvent::ToolCallCompleted { id, .. } => {
                self.pending_calls.lock().unwrap().remove(id);
            }
            AgentEvent::TaskCompleted { description } => {
                let snapshot = self.record_completed_and_snapshot(description);
                self.queue_save(snapshot)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_memory::{FileProjectMemoryStore, ProjectMemoryStore};
    use braze_types::ToolResult;

    fn temp_store_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "braze-memory-hook-test-{:?}-{}",
            std::thread::current().id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )).join("memory.json")
    }

    #[tokio::test]
    async fn a_successful_write_file_call_is_recorded_as_touched() {
        let path = temp_store_path();
        let store = std::sync::Arc::new(FileProjectMemoryStore::new(&path));
        let hook = ProjectMemoryHook::new(store, "proj").await;

        hook.on_event(&AgentEvent::AssistantToolCall {
            id: "call-1".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": "src/main.rs", "content": "..."}),
        })
        .await
        .unwrap();

        hook.on_event(&AgentEvent::ToolCallCompleted {
            id: "call-1".to_string(),
            result: ToolResult {
                tool_call_id: "call-1".to_string(),
                content: "wrote 42 bytes".to_string(),
                is_error: false,
            },
        })
        .await
        .unwrap();

        let snapshot = hook.snapshot();
        assert_eq!(snapshot.touched_files.len(), 1);
        assert_eq!(snapshot.touched_files[0].path, "src/main.rs");
        assert_eq!(snapshot.touched_files[0].last_tool, "write_file");

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    #[tokio::test]
    async fn a_failed_write_file_call_is_not_recorded() {
        let path = temp_store_path();
        let store = std::sync::Arc::new(FileProjectMemoryStore::new(&path));
        let hook = ProjectMemoryHook::new(store, "proj").await;

        hook.on_event(&AgentEvent::AssistantToolCall {
            id: "call-1".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": "src/main.rs", "content": "..."}),
        })
        .await
        .unwrap();

        hook.on_event(&AgentEvent::ToolCallCompleted {
            id: "call-1".to_string(),
            result: ToolResult {
                tool_call_id: "call-1".to_string(),
                content: "permission denied".to_string(),
                is_error: true,
            },
        })
        .await
        .unwrap();

        assert!(hook.snapshot().touched_files.is_empty());
        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    #[tokio::test]
    async fn a_read_only_tool_call_is_not_recorded() {
        let path = temp_store_path();
        let store = std::sync::Arc::new(FileProjectMemoryStore::new(&path));
        let hook = ProjectMemoryHook::new(store, "proj").await;

        hook.on_event(&AgentEvent::AssistantToolCall {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        })
        .await
        .unwrap();
        hook.on_event(&AgentEvent::ToolCallCompleted {
            id: "call-1".to_string(),
            result: ToolResult {
                tool_call_id: "call-1".to_string(),
                content: "fn main() {}".to_string(),
                is_error: false,
            },
        })
        .await
        .unwrap();

        assert!(hook.snapshot().touched_files.is_empty());
        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    #[tokio::test]
    async fn a_task_completed_event_is_recorded_as_a_completed_signal() {
        let path = temp_store_path();
        let store = std::sync::Arc::new(FileProjectMemoryStore::new(&path));
        let hook = ProjectMemoryHook::new(store, "proj").await;

        hook.on_event(&AgentEvent::TaskCompleted {
            description: "leer notas.txt".to_string(),
        })
        .await
        .unwrap();

        let snapshot = hook.snapshot();
        assert_eq!(snapshot.completed_signals.len(), 1);
        assert_eq!(snapshot.completed_signals[0].description, "leer notas.txt");
        assert_eq!(
            snapshot.completed_signals[0].source,
            SignalSource::TaskListCompletion
        );

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    /// The hook must persist to the store, not just accumulate in
    /// memory — a later session's fresh `ProjectMemoryHook::new` (which
    /// loads from the store) must see what an earlier session recorded.
    #[tokio::test]
    async fn recorded_signals_survive_into_a_new_hook_instance() {
        let path = temp_store_path();
        let store: std::sync::Arc<dyn ProjectMemoryStore> =
            std::sync::Arc::new(FileProjectMemoryStore::new(&path));

        {
            let hook = ProjectMemoryHook::new(std::sync::Arc::clone(&store), "proj").await;
            hook.on_event(&AgentEvent::TaskCompleted {
                description: "first session's work".to_string(),
            })
            .await
            .unwrap();
            // Saves are queued to the background saver task (v8 K-8) —
            // flush() is the contract for "everything is on disk now".
            hook.flush().await;
        }

        let second_hook = ProjectMemoryHook::new(store, "proj").await;
        let snapshot = second_hook.snapshot();
        assert_eq!(snapshot.completed_signals.len(), 1);
        assert_eq!(snapshot.completed_signals[0].description, "first session's work");

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    /// v8 K-7: a store pointed at another project's file must fail safe
    /// — the loaded memory is discarded, not injected into this
    /// project's system prompt.
    #[tokio::test]
    async fn a_memory_with_a_mismatched_project_key_is_discarded() {
        let path = temp_store_path();
        let store: std::sync::Arc<dyn ProjectMemoryStore> =
            std::sync::Arc::new(FileProjectMemoryStore::new(&path));

        // Seed the file with ANOTHER project's memory, complete with a
        // signal that must not leak across.
        let mut foreign = braze_memory::ProjectMemory::new("other-project");
        foreign.record_completed_signal("other project's secret work", SignalSource::TaskListCompletion, "t1");
        store.save(&foreign).await.unwrap();

        let hook = ProjectMemoryHook::new(std::sync::Arc::clone(&store), "proj").await;
        let snapshot = hook.snapshot();
        assert_eq!(snapshot.project_key, "proj");
        assert!(
            snapshot.completed_signals.is_empty(),
            "the foreign memory must not survive the key check"
        );

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    /// v8 K-6 (capa del hook): eventos TaskCompleted duplicados — misma
    /// descripción — no acumulan señales duplicadas en la memoria.
    #[tokio::test]
    async fn duplicate_task_completed_events_do_not_duplicate_signals() {
        let path = temp_store_path();
        let store = std::sync::Arc::new(FileProjectMemoryStore::new(&path));
        let hook = ProjectMemoryHook::new(store, "proj").await;

        for _ in 0..3 {
            hook.on_event(&AgentEvent::TaskCompleted {
                description: "leer notas.txt".to_string(),
            })
            .await
            .unwrap();
        }

        assert_eq!(hook.snapshot().completed_signals.len(), 1);
        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }

    #[tokio::test]
    async fn a_load_failure_starts_fresh_instead_of_blocking_the_session() {
        // Point the store at a path that exists but is NOT valid JSON —
        // `load` must surface an error internally, and `new` must
        // recover from it rather than panicking or propagating.
        let path = temp_store_path();
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, b"not valid json").await.unwrap();

        let store = std::sync::Arc::new(FileProjectMemoryStore::new(&path));
        let hook = ProjectMemoryHook::new(store, "proj").await;

        assert!(hook.snapshot().touched_files.is_empty());
        assert_eq!(hook.snapshot().project_key, "proj");

        tokio::fs::remove_dir_all(path.parent().unwrap()).await.ok();
    }
}
