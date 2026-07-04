//! [`Engine`]: the agentic loop. Composition root — this is the only crate
//! that talks to `braze-model`, `braze-tools-core`, `braze-session` and
//! `braze-events` at the same time (see PLAN.md, dependency graph).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use braze_events::{AgentEvent, BackgroundTask, TaskHandle, TaskNotifier};
use braze_model::{CompletionEvent, CompletionRequest, ModelBackend};
use braze_session::{ContextCompactor, DurableState, SessionError, SessionStore};
use braze_tools_core::ToolRegistry;
use braze_types::{Message, SessionId, ToolCall, ToolResult};

use crate::error::EngineError;
use crate::history::build_messages;

/// Default number of raw tactical events above which [`Engine::run_turn`]
/// triggers a compaction pass before building the next model request. See
/// [`Engine::new`].
pub const DEFAULT_TACTICAL_COMPACTION_THRESHOLD: usize = 40;

/// Safety cap on model/tool-call round trips within a single
/// [`Engine::run_turn`] call, so a model that never converges on a
/// text-only response can't hang the turn forever.
const MAX_TURN_ITERATIONS: usize = 20;

/// How long to wait for a single background tool task to complete before
/// treating it (and only it — sibling tasks keep waiting) as failed. See
/// the doc comment on the completion-collection loop in
/// [`Engine::run_turn`] for the documented MVP limitation this implies.
const TOOL_COMPLETION_TIMEOUT: Duration = Duration::from_secs(120);

/// The agentic loop. Orchestrates model calls, tool dispatch (via
/// background tasks + push notification), differential context
/// compaction, and session persistence.
pub struct Engine {
    model: Box<dyn ModelBackend>,
    tools: Arc<ToolRegistry>,
    store: Box<dyn SessionStore>,
    compactor: Box<dyn ContextCompactor>,
    notifier: Box<dyn TaskNotifier>,
    system_prompt: String,
    max_tokens: u32,
    tactical_compaction_threshold: usize,
}

impl Engine {
    /// Builds an `Engine` with [`DEFAULT_TACTICAL_COMPACTION_THRESHOLD`] as
    /// its compaction trigger. `tools` is wrapped internally in an `Arc` so
    /// it can be shared into the `'static` background-task futures
    /// [`TaskNotifier::spawn`] takes ownership of — `ToolRegistry` itself
    /// is not `Clone` (it owns a `Vec<Box<dyn ToolProvider>>`), so an `Arc`
    /// is the seam that lets the same registry be dispatched against from
    /// many concurrently-spawned tasks without cloning its providers.
    pub fn new(
        model: Box<dyn ModelBackend>,
        tools: ToolRegistry,
        store: Box<dyn SessionStore>,
        compactor: Box<dyn ContextCompactor>,
        notifier: Box<dyn TaskNotifier>,
        system_prompt: String,
        max_tokens: u32,
    ) -> Self {
        Self {
            model,
            tools: Arc::new(tools),
            store,
            compactor,
            notifier,
            system_prompt,
            max_tokens,
            tactical_compaction_threshold: DEFAULT_TACTICAL_COMPACTION_THRESHOLD,
        }
    }

    /// Runs one complete turn: append the user's message, then loop
    /// model-completion <-> tool-dispatch rounds until the model responds
    /// with text and no further tool calls (or the safety cap is hit).
    /// `on_text` is invoked with each text fragment as it streams in, for
    /// real-time display by the caller (CLI/TUI/etc).
    pub async fn run_turn(
        &self,
        session: &SessionId,
        user_input: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<(), EngineError> {
        self.store
            .append(
                session,
                &AgentEvent::UserMessage {
                    text: user_input.to_string(),
                },
            )
            .await?;

        let mut messages = self.load_messages(session).await?;

        for _ in 0..MAX_TURN_ITERATIONS {
            let tool_stubs = self.tools.all_stubs().await?;
            let req = CompletionRequest {
                messages: messages.clone(),
                tool_stubs,
                system_prompt: self.system_prompt.clone(),
                max_tokens: self.max_tokens,
            };

            let mut stream = self.model.complete(req).await?;

            let mut text_buffer = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();

            while let Some(event) = stream.next().await {
                match event {
                    CompletionEvent::TextDelta(delta) => {
                        on_text(&delta);
                        text_buffer.push_str(&delta);
                    }
                    CompletionEvent::ToolCallRequested {
                        id,
                        name,
                        arguments,
                    } => {
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                    CompletionEvent::Usage {
                        input_tokens,
                        output_tokens,
                    } => {
                        tracing::debug!(input_tokens, output_tokens, "model usage this round");
                    }
                    CompletionEvent::Done => break,
                }
            }

            if tool_calls.is_empty() {
                // Final response: no further tool calls requested.
                if !text_buffer.is_empty() {
                    self.store
                        .append(session, &AgentEvent::AssistantText { text: text_buffer })
                        .await?;
                }
                return Ok(());
            }

            // Text preceding this round's tool calls (if any) is persisted
            // first, preserving the order the model actually produced it
            // in, before the tool_use blocks that followed it.
            if !text_buffer.is_empty() {
                self.store
                    .append(session, &AgentEvent::AssistantText { text: text_buffer })
                    .await?;
            }

            self.dispatch_tool_calls(session, &tool_calls).await?;

            messages = self.load_messages(session).await?;
        }

        Err(EngineError::TurnDidNotConverge(MAX_TURN_ITERATIONS))
    }

    /// Records each requested tool call, spawns it as a background task via
    /// [`TaskNotifier`], and blocks until every task from this round has
    /// reported completion (persisting a `ToolCallCompleted` event for
    /// each), or times out.
    async fn dispatch_tool_calls(
        &self,
        session: &SessionId,
        tool_calls: &[ToolCall],
    ) -> Result<(), EngineError> {
        let mut handle_to_id: HashMap<TaskHandle, String> = HashMap::new();
        let mut pending: HashSet<TaskHandle> = HashSet::new();

        for call in tool_calls {
            self.store
                .append(
                    session,
                    &AgentEvent::AssistantToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                )
                .await?;

            // Best-effort schema resolution: known deferred-validation gap
            // from Fase 3 (see anthropic_wire.rs::build_tools) — the MVP
            // does not validate tool-call arguments against the resolved
            // schema, it just leaves evidence in the log that resolution
            // was attempted. A failure here never aborts the turn.
            if let Err(err) = self.tools.resolve(&call.name).await {
                tracing::warn!(
                    tool = %call.name,
                    error = %err,
                    "failed to resolve tool schema before dispatch (MVP does not validate strictly)"
                );
            }

            self.store
                .append(
                    session,
                    &AgentEvent::ToolCallStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        background: true,
                    },
                )
                .await?;

            let tools = Arc::clone(&self.tools);
            let call_owned = call.clone();
            let label = call.name.clone();
            let task = BackgroundTask {
                label,
                work: Box::pin(async move {
                    match tools.dispatch(&call_owned).await {
                        Ok(result) => result,
                        Err(err) => ToolResult {
                            tool_call_id: call_owned.id.clone(),
                            content: err.to_string(),
                            is_error: true,
                        },
                    }
                }),
            };

            let handle = self.notifier.spawn(task);
            handle_to_id.insert(handle, call.id.clone());
            pending.insert(handle);
        }

        // Known MVP limitation: if `next_completed` times out, remaining
        // handles are treated as failed and the turn proceeds rather than
        // hanging forever — there is no cancellation of the underlying
        // `tokio::spawn`ed work, it simply keeps running unobserved.
        while !pending.is_empty() {
            match self.notifier.next_completed(TOOL_COMPLETION_TIMEOUT).await {
                Some((handle, result)) => {
                    pending.remove(&handle);
                    let id = handle_to_id
                        .remove(&handle)
                        .unwrap_or_else(|| result.tool_call_id.clone());
                    self.store
                        .append(session, &AgentEvent::ToolCallCompleted { id, result })
                        .await?;
                }
                None => {
                    tracing::error!(
                        pending = pending.len(),
                        timeout_secs = TOOL_COMPLETION_TIMEOUT.as_secs(),
                        "timed out waiting for background tool task(s); treating remaining as failed"
                    );
                    for handle in pending.drain() {
                        let id = handle_to_id.remove(&handle).unwrap_or_default();
                        self.store
                            .append(
                                session,
                                &AgentEvent::ToolCallCompleted {
                                    id: id.clone(),
                                    result: ToolResult {
                                        tool_call_id: id,
                                        content: "tool call timed out waiting for completion"
                                            .to_string(),
                                        is_error: true,
                                    },
                                },
                            )
                            .await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Loads the full event log, splits it into durable/tactical via the
    /// compactor, and — if the tactical window has grown past
    /// `tactical_compaction_threshold` — folds it into a fresh
    /// `CompactionOccurred` summary (persisted) and builds messages from
    /// durable summary + that fresh summary only, discarding the raw
    /// tactical detail for this round's context. Otherwise builds messages
    /// from durable summary + the raw tactical window, unchanged.
    async fn load_messages(&self, session: &SessionId) -> Result<Vec<Message>, EngineError> {
        let events = match self.store.load(session).await {
            Ok(events) => events,
            Err(SessionError::NotFound(_)) => Vec::new(),
            Err(err) => return Err(err.into()),
        };

        let (durable, tactical) = self.compactor.split(&events);

        if tactical.len() > self.tactical_compaction_threshold {
            let summary = self.compactor.compact_tactical(&tactical)?;
            let dropped_tokens_estimate = estimate_dropped_tokens(&tactical);

            self.store
                .append(
                    session,
                    &AgentEvent::CompactionOccurred {
                        summary: summary.clone(),
                        dropped_tokens_estimate,
                    },
                )
                .await?;

            let effective_durable = merge_summary(durable, summary);
            Ok(build_messages(&effective_durable, &[]))
        } else {
            Ok(build_messages(&durable, &tactical))
        }
    }
}

/// Folds a freshly-compacted tactical summary into `durable.summary`,
/// preferring not to introduce a stray leading separator when the durable
/// summary was empty (e.g. the very first compaction of a session).
fn merge_summary(mut durable: DurableState, summary: String) -> DurableState {
    if durable.summary.is_empty() {
        durable.summary = summary;
    } else {
        durable.summary = format!("{} {summary}", durable.summary);
    }
    durable
}

/// Rough token estimate (~4 chars/token) for the tactical events about to
/// be dropped from raw context by a compaction pass — mirrors the same
/// heuristic `SimpleContextCompactor::compact_tactical` already uses
/// internally, applied here to fill `AgentEvent::CompactionOccurred`'s
/// `dropped_tokens_estimate` field from the engine's side.
fn estimate_dropped_tokens(events: &[AgentEvent]) -> u32 {
    let chars: usize = events.iter().map(|event| format!("{event:?}").len()).sum();
    (chars / 4) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;
    use braze_model::ModelError;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
    use braze_types::ToolStub;
    use futures::Stream;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::mpsc;

    /// Fixed sequence of "rounds" of `CompletionEvent`s: each call to
    /// `complete` pops and streams the next round, so a test can script a
    /// multi-round exchange (e.g. tool call round, then a final text-only
    /// round).
    struct ScriptedModel {
        rounds: AsyncMutex<std::collections::VecDeque<Vec<CompletionEvent>>>,
    }

    impl ScriptedModel {
        fn new(rounds: Vec<Vec<CompletionEvent>>) -> Self {
            Self {
                rounds: AsyncMutex::new(rounds.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl ModelBackend for ScriptedModel {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = CompletionEvent> + Send>>, ModelError> {
            let mut rounds = self.rounds.lock().await;
            let round = rounds
                .pop_front()
                .unwrap_or_else(|| vec![CompletionEvent::Done]);
            Ok(Box::pin(futures::stream::iter(round)))
        }
    }

    /// Minimal `TaskNotifier`: `tokio::spawn` per task + an mpsc
    /// completion channel, same shape `braze-cli::ChannelTaskNotifier`
    /// uses in the real binary — duplicated here (rather than depending on
    /// `braze-cli`, which would be a backwards dependency) purely so
    /// `Engine`'s tests don't need a real binary-level notifier.
    struct TestNotifier {
        tx: mpsc::UnboundedSender<(TaskHandle, ToolResult)>,
        rx: AsyncMutex<mpsc::UnboundedReceiver<(TaskHandle, ToolResult)>>,
        next: AtomicU64,
    }

    impl TestNotifier {
        fn new() -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            Self {
                tx,
                rx: AsyncMutex::new(rx),
                next: AtomicU64::new(0),
            }
        }
    }

    #[async_trait]
    impl TaskNotifier for TestNotifier {
        fn spawn(&self, task: BackgroundTask) -> TaskHandle {
            let handle = TaskHandle(self.next.fetch_add(1, Ordering::SeqCst));
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = task.work.await;
                let _ = tx.send((handle, result));
            });
            handle
        }

        async fn next_completed(&self, timeout: Duration) -> Option<(TaskHandle, ToolResult)> {
            let mut rx = self.rx.lock().await;
            tokio::time::timeout(timeout, rx.recv())
                .await
                .ok()
                .flatten()
        }
    }

    /// Fake `ToolProvider` owning exactly one tool, `echo`, which returns
    /// its `text` argument back verbatim.
    struct EchoToolProvider;

    #[async_trait]
    impl ToolProvider for EchoToolProvider {
        fn provider_id(&self) -> &str {
            "test:echo"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            Ok(vec![ToolStub {
                name: "echo".to_string(),
                summary: "echoes its input".to_string(),
                source: "test:echo".to_string(),
            }])
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name == "echo" {
                Ok(Some(ToolSchema {
                    name: "echo".to_string(),
                    description: "echoes its input".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            let text = call
                .arguments
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("echoed: {text}"),
                is_error: false,
            })
        }
    }

    fn temp_store() -> (FileSessionStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "braze-engine-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        (FileSessionStore::new(dir.clone()), dir)
    }

    #[tokio::test]
    async fn run_turn_with_no_tool_calls_streams_text_and_persists_it() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("Hola ".to_string()),
            CompletionEvent::TextDelta("mundo".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Box::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut streamed = String::new();
        engine
            .run_turn(&session, "hola", &mut |chunk| streamed.push_str(chunk))
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "Hola mundo");

        // Re-open the same on-disk store to verify persistence directly.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_turn_with_a_tool_call_round_trips_end_to_end() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider)]),
            Box::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let mut streamed = String::new();
        engine
            .run_turn(&session, "please echo hi", &mut |chunk| {
                streamed.push_str(chunk)
            })
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "done");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantToolCall { .. }));
        assert!(matches!(events[2], AgentEvent::ToolCallStarted { .. }));
        match &events[3] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.content, "echoed: hi");
                assert!(!result.is_error);
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }
        assert!(matches!(events[4], AgentEvent::AssistantText { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
