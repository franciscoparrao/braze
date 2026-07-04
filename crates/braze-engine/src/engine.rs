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

/// Minimum number of raw tactical events always rendered verbatim to the
/// model, even in the same round a compaction just ran — see
/// [`Engine::load_messages`]. Without this, a compaction discarded the
/// *entire* tactical window (including the user's message for the current
/// turn, just appended in `run_turn`), so the model's next request
/// contained no trace of what was actually being asked.
const KEEP_RAW_TAIL: usize = 6;

/// The agentic loop. Orchestrates model calls, tool dispatch (via
/// background tasks + push notification), differential context
/// compaction, and session persistence.
pub struct Engine {
    model: Box<dyn ModelBackend>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn SessionStore>,
    compactor: Box<dyn ContextCompactor>,
    notifier: Box<dyn TaskNotifier>,
    system_prompt: String,
    max_tokens: u32,
    tactical_compaction_threshold: usize,
    /// Approximate token budget for the durable+tactical portion of the
    /// prompt (i.e. excluding `system_prompt`/tool schemas, which the
    /// caller should already have reserved headroom for when computing
    /// this). `None` (the default) means compaction is triggered purely
    /// by `tactical_compaction_threshold`'s raw event count, as before —
    /// set via [`Engine::with_context_budget`] for backends with a small,
    /// known context window (e.g. Ollama's `num_ctx`), where a single
    /// large tool result can blow the budget long before the event count
    /// does. See [`Engine::load_messages`].
    context_budget_tokens: Option<u32>,
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
        store: Arc<dyn SessionStore>,
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
            context_budget_tokens: None,
        }
    }

    /// Sets an approximate token budget for the durable+tactical portion
    /// of the prompt, above which a compaction triggers regardless of raw
    /// event count — see the field's doc comment and
    /// [`Engine::load_messages`]. Chainable, e.g.
    /// `Engine::new(...).with_context_budget(6000)`.
    pub fn with_context_budget(mut self, tokens: u32) -> Self {
        self.context_budget_tokens = Some(tokens);
        self
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

        // Per-turn, per-tool-name retry counter for the "one round of
        // repair context" mechanism in `dispatch_tool_calls` below. Lives
        // and dies with this `run_turn` call — it is not a field on
        // `Engine` and never persists across turns.
        let mut schema_retry_counts: HashMap<String, u32> = HashMap::new();

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
            let mut usage: Option<(u32, u32)> = None;

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
                        usage = Some((input_tokens, output_tokens));
                    }
                    CompletionEvent::Done => break,
                }
            }

            // Persisted once per round (if the backend reported it) so
            // tooling like `braze-bench` can read per-round token usage
            // back out of the rollout log — see `AgentEvent::Usage`'s doc
            // comment. Order relative to the round's other events doesn't
            // matter: it's audit-only and never rendered into a `Message`
            // (see `history::event_to_message`).
            if let Some((input_tokens, output_tokens)) = usage {
                self.store
                    .append(
                        session,
                        &AgentEvent::Usage {
                            input_tokens,
                            output_tokens,
                        },
                    )
                    .await?;
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

            self.dispatch_tool_calls(session, &tool_calls, &mut schema_retry_counts)
                .await?;

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
        retry_counts: &mut HashMap<String, u32>,
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

            // Real schema validation before dispatch (closes the gap noted
            // in Fase 3/5, see docs/SOTA-2026-07.md § 1): resolve the
            // tool's real schema and validate the model-produced arguments
            // against it. `ToolRegistry::resolve` returns
            // `Result<ToolSchema, ToolError>`, not
            // `Result<Option<ToolSchema>, ToolError>` — `ToolError::NotFound`
            // is exactly the "no provider advertises this tool" case (every
            // `ToolProvider::resolve_schema` implementation returns
            // `Ok(None)` for an unrecognized name; the registry only turns
            // that into `NotFound` once *no* provider claims the tool), so
            // it's handled like the "no schema to validate against" case
            // rather than a hard resolution failure.
            match self.tools.resolve(&call.name).await {
                Ok(schema) => {
                    if let Err(validation_err) =
                        jsonschema::validate(&schema.input_schema, &call.arguments)
                    {
                        // Retry counter is keyed by tool *name*, not by
                        // individual call id: if the model calls the same
                        // tool multiple times in one turn with different
                        // arguments, this can't distinguish "first real
                        // failure for this call" from "a different call to
                        // the same tool that also happens to fail". This is
                        // a deliberately simple, turn-bounded heuristic —
                        // not a precise per-call correlation mechanism.
                        let attempt_counter = retry_counts.entry(call.name.clone()).or_insert(0);
                        *attempt_counter += 1;
                        let attempt = *attempt_counter;

                        let repair_message = if attempt == 1 {
                            format!(
                                "Tool call '{}' failed schema validation: {validation_err}. \
                                 Expected input schema:\n{}\n\
                                 Retry this tool call with corrected arguments.",
                                call.name, schema.input_schema
                            )
                        } else {
                            format!(
                                "Tool call '{}' failed schema validation again: {validation_err}. \
                                 No further automatic repair hints will be given for this tool \
                                 this turn.",
                                call.name
                            )
                        };

                        tracing::warn!(
                            tool = %call.name,
                            attempt,
                            error = %validation_err,
                            "tool call arguments failed schema validation before dispatch"
                        );

                        self.store
                            .append(
                                session,
                                &AgentEvent::ToolCallCompleted {
                                    id: call.id.clone(),
                                    result: ToolResult {
                                        tool_call_id: call.id.clone(),
                                        content: repair_message,
                                        is_error: true,
                                    },
                                },
                            )
                            .await?;

                        continue;
                    }
                }
                Err(braze_tools_core::ToolError::NotFound(_)) => {
                    tracing::warn!(
                        tool = %call.name,
                        "no provider advertises this tool; dispatching anyway"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        tool = %call.name,
                        error = %err,
                        "failed to resolve tool schema before dispatch (MVP does not validate strictly)"
                    );
                }
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
    /// `tactical_compaction_threshold` **or** the estimated prompt size has
    /// grown past `context_budget_tokens` (whichever is configured; see
    /// that field's doc comment) — folds *all* of it into a fresh
    /// `CompactionOccurred` summary (persisted; see
    /// [`SimpleContextCompactor`](braze_session::SimpleContextCompactor)'s
    /// `last_compaction_index` logic for why folding the complete backlog,
    /// not a partial prefix, is what keeps repeated compaction
    /// differential instead of re-summarizing overlapping content every
    /// round) and builds messages from durable summary + that fresh
    /// summary, **plus the last [`KEEP_RAW_TAIL`] tactical events kept
    /// verbatim** — never the empty slice. Discarding the raw tail
    /// entirely would drop the user's just-appended message for the
    /// current turn (and any tool result from the round in progress) from
    /// the very request meant to act on it. Otherwise (below both
    /// thresholds) builds messages from durable summary + the full raw
    /// tactical window, unchanged.
    ///
    /// The event-count threshold alone is a poor proxy for prompt size —
    /// a single `read_file` of a large file counts the same as a
    /// two-word "ok" — so a caller targeting a small, fixed context
    /// window (e.g. Ollama's `num_ctx`) should also set
    /// `context_budget_tokens` via [`Engine::with_context_budget`].
    async fn load_messages(&self, session: &SessionId) -> Result<Vec<Message>, EngineError> {
        let events = match self.store.load(session).await {
            Ok(events) => events,
            Err(SessionError::NotFound(_)) => Vec::new(),
            Err(err) => return Err(err.into()),
        };

        let (durable, tactical) = self.compactor.split(&events);

        let over_event_count_threshold = tactical.len() > self.tactical_compaction_threshold;
        let over_token_budget = self
            .context_budget_tokens
            .is_some_and(|budget| estimate_prompt_tokens(&durable, &tactical) > budget);

        if over_event_count_threshold || over_token_budget {
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
            let keep = KEEP_RAW_TAIL.min(tactical.len());
            let live_tail = &tactical[tactical.len() - keep..];
            Ok(build_messages(&effective_durable, live_tail))
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

/// Rough token estimate for the *entire* durable+tactical portion of the
/// next model request — everything [`crate::history::build_messages`]
/// would turn into `Message`s, not just the tactical slice about to be
/// (maybe) compacted. Used by [`Engine::load_messages`] to decide whether
/// the prompt is approaching `context_budget_tokens`, since a raw event
/// *count* alone can't tell a two-word `AssistantText` apart from a
/// `ToolCallCompleted` carrying a 200KB file dump.
fn estimate_prompt_tokens(durable: &DurableState, tactical: &[AgentEvent]) -> u32 {
    let summary_tokens = (durable.summary.len() / 4) as u32;
    summary_tokens
        + estimate_dropped_tokens(&durable.durable_events)
        + estimate_dropped_tokens(tactical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use async_trait::async_trait;
    use braze_model::ModelError;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
    use braze_types::{ContentBlock, ToolStub};
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
    /// its `text` argument back verbatim. Its schema requires `text` (a
    /// real schema with a required field, not the generic permissive
    /// `{"type":"object"}` this provider originally had) so tests can
    /// exercise real validation failures. `invocations` is an `Arc` shared
    /// with the test that constructs it, so a test can assert `invoke` was
    /// never called for a call that should have been rejected by schema
    /// validation before ever reaching dispatch.
    struct EchoToolProvider {
        invocations: Arc<AtomicU32>,
    }

    impl EchoToolProvider {
        fn new(invocations: Arc<AtomicU32>) -> Self {
            Self { invocations }
        }
    }

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
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    }),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
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
            Arc::new(store),
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
    async fn run_turn_persists_usage_reported_by_the_backend() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Usage {
                input_tokens: 42,
                output_tokens: 7,
            },
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "hola", &mut |_| {})
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        match &events[1] {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(*input_tokens, 42);
                assert_eq!(*output_tokens, 7);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert!(matches!(events[2], AgentEvent::AssistantText { .. }));

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

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
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
        // Valid arguments: unchanged behavior — the real tool actually ran,
        // exactly once.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

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

    #[tokio::test]
    async fn invalid_args_get_one_round_of_schema_repair_context_then_the_retry_succeeds() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                // First attempt: missing the required `text` field.
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Second attempt (scripted as if the model read the repair
                // context and corrected itself): valid arguments.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut |_| {})
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        assert!(matches!(events[1], AgentEvent::AssistantToolCall { .. }));

        // The rejected call never gets a `ToolCallStarted` (it never
        // reaches dispatch) — its `ToolCallCompleted` follows the
        // `AssistantToolCall` directly, and carries the resolved schema so
        // the model has something concrete to correct itself with.
        match &events[2] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
                // "properties" only appears in the serialized schema dump,
                // never in `jsonschema`'s own error text (which reads
                // along the lines of `"text" is a required property`,
                // singular) — a reliable signal the schema was included.
                assert!(result.content.contains("properties"));
                assert!(result.content.contains("text"));
                // The real tool must never have run for the rejected call.
                assert_ne!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted for call-1, got {other:?}"),
        }

        assert!(matches!(events[3], AgentEvent::AssistantToolCall { .. }));
        assert!(matches!(events[4], AgentEvent::ToolCallStarted { .. }));
        match &events[5] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-2");
                assert!(!result.is_error);
                assert_eq!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted for call-2, got {other:?}"),
        }

        // `invoke` ran exactly once: only for the corrected, valid call.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_second_invalid_call_to_the_same_tool_in_one_turn_gets_no_more_schema_context() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same tool, still invalid — the model didn't correct
                // itself this time.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "wrong_field": 1 }),
                },
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut |_| {})
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let first_message = match &events[2] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert!(result.is_error);
                assert!(result.content.contains("properties"));
                result.content.clone()
            }
            other => panic!("expected ToolCallCompleted for call-1, got {other:?}"),
        };

        match &events[4] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-2");
                assert!(result.is_error);
                // Second failure of the same tool name this turn: no
                // schema dump this time, and a visibly shorter/different
                // message than the first repair-context one.
                assert!(!result.content.contains("properties"));
                assert_ne!(result.content, first_message);
                assert!(result.content.len() < first_message.len());
            }
            other => panic!("expected ToolCallCompleted for call-2, got {other:?}"),
        }

        // Both calls were rejected before dispatch — `invoke` never ran.
        assert_eq!(invocations.load(Ordering::SeqCst), 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A1/C1: when a compaction triggers,
    /// `load_messages` must never discard the live tail entirely — the
    /// user's just-appended message for the current turn (the newest
    /// event in the log) has to survive as a raw message, not be
    /// swallowed into the compaction summary with nothing concrete left
    /// for the model to act on.
    #[tokio::test]
    async fn load_messages_keeps_a_live_raw_tail_when_compaction_triggers() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Seed a backlog well past the default compaction threshold with
        // plain, non-durable-typed events (the orphan types that never
        // leave `tactical` on their own).
        for i in 0..(DEFAULT_TACTICAL_COMPACTION_THRESHOLD + 10) {
            store
                .append(
                    &session,
                    &AgentEvent::UserMessage {
                        text: format!("turno {i}"),
                    },
                )
                .await
                .expect("seed backlog event");
        }
        // The newest event — exactly what `run_turn` appends right before
        // calling `load_messages` for the current turn.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "pregunta actual del usuario".to_string(),
                },
            )
            .await
            .expect("seed current turn's message");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let messages = engine
            .load_messages(&session)
            .await
            .expect("load_messages should succeed");

        assert!(
            messages.iter().any(|m| matches!(
                &m.content[0],
                ContentBlock::Text { text } if text == "pregunta actual del usuario"
            )),
            "expected the live tail to include the just-appended user message, got: {messages:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "sanity check: a compaction should actually have been triggered"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C3: a single oversized event (e.g. a large
    /// `read_file` result) must trigger compaction via the token budget
    /// even when the raw event *count* is nowhere near
    /// `tactical_compaction_threshold` — the count alone can't tell a
    /// 200KB tool result apart from a two-word reply.
    #[tokio::test]
    async fn a_single_oversized_event_triggers_compaction_via_the_token_budget() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Just 2 events — nowhere near the default threshold of 40 — but
        // one of them is enormous.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "resume este archivo".to_string(),
                },
            )
            .await
            .expect("seed user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "x".repeat(20_000),
                },
            )
            .await
            .expect("seed oversized event");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_context_budget(1000); // ~4000 chars — the 20K-char event alone blows this.

        engine
            .load_messages(&session)
            .await
            .expect("load_messages should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "expected the token budget to trigger compaction despite the low event count"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Without a configured budget, a large event does NOT trigger
    /// compaction below the event-count threshold — confirms
    /// `context_budget_tokens: None` preserves the pre-C3 behavior
    /// exactly (event count is the only trigger).
    #[tokio::test]
    async fn without_a_configured_budget_only_event_count_triggers_compaction() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::AssistantText {
                    text: "x".repeat(20_000),
                },
            )
            .await
            .expect("seed oversized event");

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .load_messages(&session)
            .await
            .expect("load_messages should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "no budget configured: a single large event below the count threshold must not compact"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Confirms the generic permissive schema `braze-model` sends to the
    /// model today (`{"type":"object","additionalProperties":true}`) would
    /// not itself reject arbitrary arguments if it were ever validated
    /// against — the new validation in `dispatch_tool_calls` is exactly as
    /// strict as the real resolved schema says and no stricter, it doesn't
    /// introduce false rejections for that permissive case.
    #[test]
    fn generic_permissive_schema_accepts_arbitrary_arguments() {
        let schema = serde_json::json!({"type": "object", "additionalProperties": true});
        let instance = serde_json::json!({"cualquier_cosa": 123});
        assert!(jsonschema::validate(&schema, &instance).is_ok());
    }
}
