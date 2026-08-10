//! Fixtures compartidas de los tests del engine — P1.1: extraídas del
//! `mod tests` de `engine/mod.rs` al repartir sus ~8k líneas de tests
//! entre los módulos destino. Modelos scripteados (`ScriptedModel` y
//! variantes), providers de tools de juguete y el `temp_store` de
//! sesiones. Solo compila bajo `cfg(test)` (declarado así en `mod.rs`).

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use braze_events::{AgentEvent, BackgroundTask, TaskHandle, TaskNotifier, TurnObserver};
use braze_model::{CompletionEvent, CompletionRequest, ModelBackend, ModelError};
use braze_session::FileSessionStore;
use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
use braze_types::{SessionId, ToolCall, ToolResult, ToolStub};
use futures::{Stream, StreamExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

/// Fixed sequence of "rounds" of `CompletionEvent`s: each call to
/// `complete` pops and streams the next round, so a test can script a
/// multi-round exchange (e.g. tool call round, then a final text-only
/// round).
pub(crate) struct ScriptedModel {
    pub(crate) rounds: AsyncMutex<std::collections::VecDeque<Vec<CompletionEvent>>>,
}

impl ScriptedModel {
    pub(crate) fn new(rounds: Vec<Vec<CompletionEvent>>) -> Self {
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
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let mut rounds = self.rounds.lock().await;
        let round = rounds
            .pop_front()
            .unwrap_or_else(|| vec![CompletionEvent::Done]);
        Ok(Box::pin(futures::stream::iter(round.into_iter().map(Ok))))
    }
}

/// A `ModelBackend` whose stream yields some text then fails mid-round
/// with a `StreamError` — used to verify `run_turn` never persists the
/// partial text as if it were a complete response (see A3/B4).
pub(crate) struct ErroringModel;

#[async_trait]
impl ModelBackend for ErroringModel {
    fn name(&self) -> &str {
        "erroring"
    }

    async fn complete(
        &self,
        _req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let items = vec![
            Ok(CompletionEvent::TextDelta(
                "Voy a leer el archi".to_string(),
            )),
            Err(ModelError::StreamError("connection reset".to_string())),
        ];
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

/// A `ModelBackend` whose N-th call (0-indexed, `fail_on_attempt`)
/// errors and every other call succeeds with the same scripted round
/// — used to prove best-of-n (N-13, docs/AUDITORIA-2026-07-v2.md)
/// votes among whichever candidates succeeded instead of aborting the
/// whole round the instant any one of them fails.
pub(crate) struct FlakyBestOfNModel {
    pub(crate) fail_on_attempt: u32,
    pub(crate) calls: AtomicU32,
    pub(crate) good_round: Vec<CompletionEvent>,
}

#[async_trait]
impl ModelBackend for FlakyBestOfNModel {
    fn name(&self) -> &str {
        "flaky-best-of-n"
    }

    async fn complete(
        &self,
        _req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        if attempt == self.fail_on_attempt {
            return Err(ModelError::Request(
                "simulated transient failure".to_string(),
            ));
        }
        Ok(Box::pin(futures::stream::iter(
            self.good_round.clone().into_iter().map(Ok),
        )))
    }
}

/// A `ModelBackend` whose single scripted round only resolves after
/// `delay` — used to force two `run_turn` calls to genuinely overlap
/// in time (N-17, docs/AUDITORIA-2026-07-v2.md) instead of one
/// finishing before the other starts.
pub(crate) struct SlowModel {
    pub(crate) delay: Duration,
    pub(crate) round: Vec<CompletionEvent>,
}

#[async_trait]
impl ModelBackend for SlowModel {
    fn name(&self) -> &str {
        "slow"
    }

    async fn complete(
        &self,
        _req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        tokio::time::sleep(self.delay).await;
        Ok(Box::pin(futures::stream::iter(
            self.round.clone().into_iter().map(Ok),
        )))
    }
}

/// Como `ScriptedModel`, pero cada ronda lleva un flag `then_stall`: si
/// está encendido, el stream emite sus eventos scripteados y después se
/// queda MUDO para siempre (sin `Done`, sin `Err`) — la ronda desbocada
/// que `Engine::with_max_round_wall_clock` existe para acotar. Un
/// `ScriptedModel` no puede simularla: sus streams terminan al agotar el
/// guion, y el guardia `IncompleteStream` los convierte en error, que es
/// otra clase de fallo.
pub(crate) struct StallingModel {
    pub(crate) rounds: AsyncMutex<std::collections::VecDeque<(Vec<CompletionEvent>, bool)>>,
}

impl StallingModel {
    pub(crate) fn new(rounds: Vec<(Vec<CompletionEvent>, bool)>) -> Self {
        Self {
            rounds: AsyncMutex::new(rounds.into_iter().collect()),
        }
    }
}

#[async_trait]
impl ModelBackend for StallingModel {
    fn name(&self) -> &str {
        "stalling"
    }

    async fn complete(
        &self,
        _req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        let mut rounds = self.rounds.lock().await;
        let (round, then_stall) = rounds
            .pop_front()
            .unwrap_or_else(|| (vec![CompletionEvent::Done], false));
        let scripted = futures::stream::iter(round.into_iter().map(Ok));
        if then_stall {
            Ok(Box::pin(scripted.chain(futures::stream::pending())))
        } else {
            Ok(Box::pin(scripted))
        }
    }
}

/// Wraps any `ModelBackend` and validates every
/// `CompletionRequest.messages` against the Anthropic message-ordering
/// protocol (`crate::protocol_check`) before delegating to `inner` —
/// converts what would be a production `400` (or, on a backend that
/// doesn't validate, a silently wrong conversation) into an immediate,
/// precisely-diagnosed test failure at the exact call site that built
/// the bad `Vec<Message>`. Precondition for Grupo I,
/// docs/AUDITORIA-2026-07-v2.md: several context-pipeline fixes (A1/C1,
/// A2/C2, C4) had gaps (N-1, N-2, N-4) that no existing test caught,
/// because `ScriptedModel` never looks at the messages it's handed —
/// wrapping it in this turns those gaps into a red test right here.
pub(crate) struct ProtocolValidatingModel<M> {
    pub(crate) inner: M,
}

impl<M> ProtocolValidatingModel<M> {
    pub(crate) fn new(inner: M) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<M: ModelBackend> ModelBackend for ProtocolValidatingModel<M> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        if let Err(violation) =
            crate::protocol_check::check_anthropic_message_protocol(&req.messages)
        {
            panic!(
                "invalid message sequence would be rejected by the real Anthropic \
                 API: {violation}\nfull message list: {:#?}",
                req.messages
            );
        }
        self.inner.complete(req).await
    }
}

/// Wraps any `ModelBackend` and records every `CompletionRequest` it
/// receives — lets a test assert on what a backend was actually
/// *asked* (e.g. that the executor's first request contains the
/// plan the planner produced), which `ScriptedModel` alone can't:
/// it ignores its request entirely.
pub(crate) struct RequestCapturingModel<M> {
    pub(crate) inner: M,
    pub(crate) requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl<M: ModelBackend> ModelBackend for RequestCapturingModel<M> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>, ModelError>
    {
        self.requests.lock().unwrap().push(req.clone());
        self.inner.complete(req).await
    }
}

/// Minimal `TaskNotifier`: `tokio::spawn` per task + an mpsc
/// completion channel, same shape `braze-cli::ChannelTaskNotifier`
/// uses in the real binary — duplicated here (rather than depending on
/// `braze-cli`, which would be a backwards dependency) purely so
/// `Engine`'s tests don't need a real binary-level notifier.
pub(crate) struct TestNotifier {
    pub(crate) tx: mpsc::UnboundedSender<(TaskHandle, ToolResult)>,
    pub(crate) rx: AsyncMutex<mpsc::UnboundedReceiver<(TaskHandle, ToolResult)>>,
    pub(crate) next: AtomicU64,
    // Mirrors `braze_events::ChannelTaskNotifier`'s own tracking, so
    // tests can prove `dispatch_tool_calls`'s timeout path really
    // cancels a task instead of just forgetting about it (N-33).
    pub(crate) handles: std::sync::Mutex<HashMap<TaskHandle, tokio::task::JoinHandle<()>>>,
}

impl TestNotifier {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: AsyncMutex::new(rx),
            next: AtomicU64::new(0),
            handles: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Queues a completion for a handle that was never returned by
    /// `spawn` — simulating a task from an earlier round that finally
    /// finished after that round already gave up on it (timeout), so
    /// its handle is no longer in the current round's `pending` set.
    /// `TaskHandle(u64::MAX)` is guaranteed never to collide with a
    /// real handle from `spawn`'s monotonic counter, which starts at 0.
    pub(crate) fn inject_stale_completion(&self, tool_call_id: &str) {
        let stale = ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: "stale result that must never be persisted".to_string(),
            is_error: false,
        };
        let _ = self.tx.send((TaskHandle(u64::MAX), stale));
    }
}

#[async_trait]
impl TaskNotifier for TestNotifier {
    fn spawn(&self, task: BackgroundTask) -> TaskHandle {
        let handle = TaskHandle(self.next.fetch_add(1, Ordering::SeqCst));
        let tx = self.tx.clone();
        let join = tokio::spawn(async move {
            let result = task.work.await;
            let _ = tx.send((handle, result));
        });
        self.handles.lock().unwrap().insert(handle, join);
        handle
    }

    async fn next_completed(&self, timeout: Duration) -> Option<(TaskHandle, ToolResult)> {
        let mut rx = self.rx.lock().await;
        let completed = tokio::time::timeout(timeout, rx.recv())
            .await
            .ok()
            .flatten();
        if let Some((handle, _)) = &completed {
            self.handles.lock().unwrap().remove(handle);
        }
        completed
    }

    fn abort(&self, handle: TaskHandle) {
        if let Some(join) = self.handles.lock().unwrap().remove(&handle) {
            join.abort();
        }
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
pub(crate) struct EchoToolProvider {
    pub(crate) invocations: Arc<AtomicU32>,
}

impl EchoToolProvider {
    pub(crate) fn new(invocations: Arc<AtomicU32>) -> Self {
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
            input_schema: None,
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

/// Like `EchoToolProvider`, but `invoke` sleeps first — el reloj de
/// pared del turno tiene que avanzar de verdad para poder probar el
/// corte de `Engine::with_max_turn_wall_clock` (round-economics) en el
/// borde entre una ronda y la siguiente, y no solo el caso degenerado de
/// presupuesto cero.
pub(crate) struct SlowEchoToolProvider {
    pub(crate) invocations: Arc<AtomicU32>,
    pub(crate) delay: std::time::Duration,
}

impl SlowEchoToolProvider {
    pub(crate) fn new(invocations: Arc<AtomicU32>, delay: std::time::Duration) -> Self {
        Self { invocations, delay }
    }
}

#[async_trait]
impl ToolProvider for SlowEchoToolProvider {
    fn provider_id(&self) -> &str {
        "test:slow-echo"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![ToolStub {
            name: "echo".to_string(),
            summary: "echoes its input, slowly".to_string(),
            source: "test:slow-echo".to_string(),
            input_schema: None,
        }])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name == "echo" {
            Ok(Some(ToolSchema {
                name: "echo".to_string(),
                description: "echoes its input, slowly".to_string(),
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
        tokio::time::sleep(self.delay).await;
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: "echoed (slowly)".to_string(),
            is_error: false,
        })
    }
}

/// Like `EchoToolProvider`, but its schema declares an `integer`
/// field — used to test F2 (docs/AUDITORIA-2026-07-v3.md):
/// `coerce_arguments_to_schema` must turn a stringified integer from
/// the qwen3-coder XML rescue (whose grammar has no native number
/// type) into a real JSON number before validation/dispatch, instead
/// of failing schema validation and burning a repair round.
pub(crate) struct EchoWithLimitToolProvider {
    pub(crate) invocations: Arc<AtomicU32>,
    pub(crate) received_limit: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
}

impl EchoWithLimitToolProvider {
    pub(crate) fn new(
        invocations: Arc<AtomicU32>,
        received_limit: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    ) -> Self {
        Self {
            invocations,
            received_limit,
        }
    }
}

#[async_trait]
impl ToolProvider for EchoWithLimitToolProvider {
    fn provider_id(&self) -> &str {
        "test:echo_limit"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![ToolStub {
            name: "echo_limit".to_string(),
            summary: "echoes with a numeric limit".to_string(),
            source: "test:echo_limit".to_string(),
            input_schema: None,
        }])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name == "echo_limit" {
            Ok(Some(ToolSchema {
                name: "echo_limit".to_string(),
                description: "echoes with a numeric limit".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "limit": {"type": "integer"},
                    },
                    "required": ["text", "limit"],
                }),
            }))
        } else {
            Ok(None)
        }
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        *self.received_limit.lock().unwrap() = call.arguments.get("limit").cloned();
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: "ok".to_string(),
            is_error: false,
        })
    }
}

/// Mock provider offering tools named exactly like two of the real
/// local built-ins — used to test F6
/// (docs/AUDITORIA-2026-07-v3.md) against the actual names
/// `MUTATING_TOOL_NAMES` lists, not stand-ins.
pub(crate) struct ReadWriteToolProvider {
    pub(crate) read_invocations: Arc<AtomicU32>,
}

impl ReadWriteToolProvider {
    pub(crate) fn new(read_invocations: Arc<AtomicU32>) -> Self {
        Self { read_invocations }
    }
}

#[async_trait]
impl ToolProvider for ReadWriteToolProvider {
    fn provider_id(&self) -> &str {
        "test:read_write"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![
            ToolStub {
                name: "read_file".to_string(),
                summary: "reads a file".to_string(),
                source: "test:read_write".to_string(),
                input_schema: None,
            },
            ToolStub {
                name: "write_file".to_string(),
                summary: "writes a file".to_string(),
                source: "test:read_write".to_string(),
                input_schema: None,
            },
        ])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name != "read_file" && name != "write_file" {
            return Ok(None);
        }
        Ok(Some(ToolSchema {
            name: name.to_string(),
            description: name.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        }))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        match call.name.as_str() {
            "read_file" => {
                self.read_invocations.fetch_add(1, Ordering::SeqCst);
                Ok(ToolResult {
                    tool_call_id: call.id.clone(),
                    content: "contenido".to_string(),
                    is_error: false,
                })
            }
            "write_file" => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "wrote".to_string(),
                is_error: false,
            }),
            other => Err(ToolError::NotFound(other.to_string())),
        }
    }
}

/// Offers `write_file` but every call comes back as an error result
/// (`is_error: true`) — the shape of a real `edit_file` that failed
/// with `old_string not found` or a denied write. Used to test
/// incident roam #16: a turn that ATTEMPTED an edit and landed none.
pub(crate) struct FailingWriteToolProvider;

#[async_trait]
impl ToolProvider for FailingWriteToolProvider {
    fn provider_id(&self) -> &str {
        "test:failing_write"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![ToolStub {
            name: "write_file".to_string(),
            summary: "writes a file (always fails)".to_string(),
            source: "test:failing_write".to_string(),
            input_schema: None,
        }])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name != "write_file" {
            return Ok(None);
        }
        Ok(Some(ToolSchema {
            name: name.to_string(),
            description: name.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        }))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: "old_string not found (simulated edit failure)".to_string(),
            is_error: true,
        })
    }
}

/// Like `EchoToolProvider`, but the delay before resolving is taken
/// from the call's `delay_ms` argument — lets a test make a round's
/// concurrently-dispatched tool calls resolve in a chosen order
/// (e.g. reverse of dispatch order) via real `tokio::spawn`/`sleep`
/// scheduling, instead of only ever simulating that race by seeding
/// events directly in a specific order.
pub(crate) struct ReorderingEchoToolProvider {
    pub(crate) invocations: Arc<AtomicU32>,
}

impl ReorderingEchoToolProvider {
    pub(crate) fn new(invocations: Arc<AtomicU32>) -> Self {
        Self { invocations }
    }
}

#[async_trait]
impl ToolProvider for ReorderingEchoToolProvider {
    fn provider_id(&self) -> &str {
        "test:reordering-echo"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![ToolStub {
            name: "echo".to_string(),
            summary: "echoes its input after an id-dependent delay".to_string(),
            source: "test:reordering-echo".to_string(),
            input_schema: None,
        }])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name == "echo" {
            Ok(Some(ToolSchema {
                name: "echo".to_string(),
                description: "echoes its input".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "delay_ms": {"type": "number"},
                    },
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
        let delay_ms = call
            .arguments
            .get("delay_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: format!("echoed: {text}"),
            is_error: false,
        })
    }
}

/// A `ToolProvider` whose one tool sleeps a fixed delay then flips a
/// shared flag before returning — used to prove a tool-completion
/// timeout genuinely cancels the background task (the flag stays
/// `false`) rather than merely giving up on waiting for it (the flag
/// would eventually flip `true` regardless of what the engine decided
/// to do with it). See N-33, docs/AUDITORIA-2026-07-v2.md.
pub(crate) struct SlowToolProvider {
    pub(crate) delay: Duration,
    pub(crate) completed: Arc<AtomicBool>,
}

impl SlowToolProvider {
    pub(crate) fn new(delay: Duration, completed: Arc<AtomicBool>) -> Self {
        Self { delay, completed }
    }
}

#[async_trait]
impl ToolProvider for SlowToolProvider {
    fn provider_id(&self) -> &str {
        "test:slow"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![ToolStub {
            name: "slow".to_string(),
            summary: "sleeps then completes".to_string(),
            source: "test:slow".to_string(),
            input_schema: None,
        }])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name == "slow" {
            Ok(Some(ToolSchema {
                name: "slow".to_string(),
                description: "sleeps then completes".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }))
        } else {
            Ok(None)
        }
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        tokio::time::sleep(self.delay).await;
        self.completed.store(true, Ordering::SeqCst);
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: "done".to_string(),
            is_error: false,
        })
    }
}

/// Offers `edit_file` with the real tool's arg shape and records the
/// arguments of every call — the integration seam for the edit-fence
/// lever (A/B del impuesto JSON): a fence block must arrive here as a
/// normal, schema-valid `edit_file` call, indistinguishable from a
/// native one.
pub(crate) struct EditRecordingToolProvider {
    pub(crate) calls: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl EditRecordingToolProvider {
    pub(crate) fn new(calls: Arc<std::sync::Mutex<Vec<serde_json::Value>>>) -> Self {
        Self { calls }
    }
}

#[async_trait]
impl ToolProvider for EditRecordingToolProvider {
    fn provider_id(&self) -> &str {
        "test:edit_recording"
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        Ok(vec![ToolStub {
            name: "edit_file".to_string(),
            summary: "edits a file".to_string(),
            source: "test:edit_recording".to_string(),
            input_schema: None,
        }])
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if name != "edit_file" {
            return Ok(None);
        }
        Ok(Some(ToolSchema {
            name: "edit_file".to_string(),
            description: "edits a file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                },
                "required": ["path", "old_string", "new_string"],
            }),
        }))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        self.calls.lock().unwrap().push(call.arguments.clone());
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: "edited".to_string(),
            is_error: false,
        })
    }
}

pub(crate) fn temp_store() -> (FileSessionStore, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "braze-engine-test-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    (FileSessionStore::new(dir.clone()), dir)
}

/// Records everything the engine mirrors into it, for asserting the
/// live `TurnObserver` seam (PLAN.md § "Fase TUI — diseño", oleada 1)
/// sees exactly what gets persisted, in the same order.
pub(crate) struct RecordingObserver {
    pub(crate) deltas: Vec<String>,
    pub(crate) events: Vec<AgentEvent>,
}

impl TurnObserver for RecordingObserver {
    fn on_text_delta(&mut self, delta: &str) {
        self.deltas.push(delta.to_string());
    }
    fn on_event(&mut self, event: &AgentEvent) {
        self.events.push(event.clone());
    }
}
