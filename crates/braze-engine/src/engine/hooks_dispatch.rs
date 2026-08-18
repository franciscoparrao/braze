//! Persistencia + espejo a observer + dispatch de hooks — P1.1 paso 4
//! (v8 § 3). Extraído VERBATIM de `engine/mod.rs` (2026-07-18):
//! `append_and_notify` (la única puerta al rollout log durante un
//! turno: el observer se notifica solo DESPUÉS de un append exitoso) y
//! el dispatch de `EngineHook`s con timeout, streak por attach point y
//! auto-disable (Paquete B′).

use super::*;

impl Engine {
    /// Persists `event` to the session store and mirrors it into the
    /// turn's [`TurnObserver`] — the live seam frontends consume (see
    /// PLAN.md § "Fase TUI — diseño"). Persistence stays the source of
    /// truth: the observer is only notified *after* a successful append,
    /// so a frontend can never display an event the rollout log doesn't
    /// have.
    pub(super) async fn append_and_notify(
        &self,
        session: &SessionId,
        event: &AgentEvent,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        self.store.append(session, event).await?;
        observer.on_event(event);
        // B′ (docs/harness-engineering-hooks-skills-2026-07-10.md):
        // audit-only hooks see every persisted event — EXCEPT
        // `HookErrored` itself, so a failing hook can't feed back into
        // hook dispatch. A hook whose failure streak crosses the
        // threshold gets its disable recorded as a persisted event
        // (appended directly: the guard above makes re-dispatch moot,
        // but appending without dispatching keeps this non-recursive by
        // construction).
        if !self.hooks.is_empty() && !matches!(event, AgentEvent::HookErrored { .. }) {
            for (id, reason) in self.dispatch_hooks_on_event(event).await {
                let hook_event = AgentEvent::HookErrored {
                    id,
                    point: crate::hooks::HookPoint::OnEvent.as_str().to_string(),
                    reason,
                };
                self.store.append(session, &hook_event).await?;
                observer.on_event(&hook_event);
            }
        }
        Ok(())
    }

    /// Runs every enabled hook's `on_event` under the per-call timeout,
    /// warn-and-continue on failure. Returns the `(id, reason)` of each
    /// hook whose failure streak crossed the disable threshold on THIS
    /// call — the caller persists those as [`AgentEvent::HookErrored`].
    pub(super) async fn dispatch_hooks_on_event(
        &self,
        event: &AgentEvent,
    ) -> Vec<(String, String)> {
        let mut disabled_now = Vec::new();
        for registered in &self.hooks {
            if registered.is_disabled() {
                continue;
            }
            let outcome =
                tokio::time::timeout(crate::hooks::HOOK_TIMEOUT, registered.hook.on_event(event))
                    .await;
            if let Some((id, reason)) =
                Self::hook_failure(registered, crate::hooks::HookPoint::OnEvent, outcome)
            {
                disabled_now.push((id, reason));
            }
        }
        disabled_now
    }

    /// `before_model_request` twin of [`Engine::dispatch_hooks_on_event`].
    pub(super) async fn dispatch_hooks_before_model_request(
        &self,
        request: &CompletionRequest,
    ) -> Vec<(String, String)> {
        let mut disabled_now = Vec::new();
        for registered in &self.hooks {
            if registered.is_disabled() {
                continue;
            }
            let outcome = tokio::time::timeout(
                crate::hooks::HOOK_TIMEOUT,
                registered.hook.before_model_request(request),
            )
            .await;
            if let Some((id, reason)) = Self::hook_failure(
                registered,
                crate::hooks::HookPoint::BeforeModelRequest,
                outcome,
            ) {
                disabled_now.push((id, reason));
            }
        }
        disabled_now
    }

    /// Shared failure bookkeeping for both dispatchers: logs the
    /// warn-and-continue line, records the outcome on the hook's streak,
    /// and returns `Some((id, reason))` only on the call that crossed
    /// the disable threshold.
    pub(super) fn hook_failure(
        registered: &crate::hooks::RegisteredHook,
        point: crate::hooks::HookPoint,
        outcome: Result<Result<(), String>, tokio::time::error::Elapsed>,
    ) -> Option<(String, String)> {
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(reason)) => Some(reason),
            Err(_) => Some(format!(
                "timed out after {}ms",
                crate::hooks::HOOK_TIMEOUT.as_millis()
            )),
        };
        if let Some(reason) = &failure {
            tracing::warn!(
                hook = registered.hook.id(),
                point = point.as_str(),
                reason = %reason,
                "engine hook failed (warn_and_continue)"
            );
        }
        let crossed = registered.record_outcome(point, failure.is_some());
        if crossed {
            tracing::warn!(
                hook = registered.hook.id(),
                point = point.as_str(),
                "engine hook disabled after repeated failures"
            );
            return Some((
                registered.hook.id().to_string(),
                failure.unwrap_or_default(),
            ));
        }
        None
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 resto (v9 L-5, 2026-08-18): cluster completo del Paquete B′
    // (hooks audit-only) movido VERBATIM del `mod tests` de
    // engine/mod.rs — fixtures compartidas en engine/test_support.rs.
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    use async_trait::async_trait;
    use braze_events::{AgentEvent, NoopObserver};
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::{ToolError, ToolProvider, ToolRegistry};
    use braze_types::{SessionId, ToolResult};

    use crate::engine::Engine;
    use crate::engine::test_support::*;

    // --- Paquete B′: hooks audit-only
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte II) ---

    /// A recording hook: appends `(hook_id, what_it_saw)` to a shared
    /// vec — the ordering fixture for the stable-order test.
    struct RecordingHook {
        id: &'static str,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::hooks::EngineHook for RecordingHook {
        fn id(&self) -> &str {
            self.id
        }
        async fn on_event(&self, event: &AgentEvent) -> Result<(), String> {
            if matches!(event, AgentEvent::UserMessage { .. }) {
                self.log.lock().unwrap().push(format!("{}:user", self.id));
            }
            Ok(())
        }
        async fn before_model_request(
            &self,
            _request: &braze_model::CompletionRequest,
        ) -> Result<(), String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:request", self.id));
            Ok(())
        }
    }

    /// A hook that always fails — the degradation fixture.
    struct AlwaysFailingHook;

    #[async_trait::async_trait]
    impl crate::hooks::EngineHook for AlwaysFailingHook {
        fn id(&self) -> &str {
            "always-failing"
        }
        async fn on_event(&self, _event: &AgentEvent) -> Result<(), String> {
            Err("boom".to_string())
        }
    }

    /// Two hooks, registration order — both see the same points, in the
    /// order they were registered (acceptance criterion "orden de hooks
    /// estable y testeado"). Audit-only: the turn's outcome is untouched.
    #[tokio::test]
    async fn hooks_run_in_registration_order_and_see_events_and_requests() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola!".to_string()),
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
        )
        .with_hook(Arc::new(RecordingHook {
            id: "alpha",
            log: Arc::clone(&log),
        }))
        .with_hook(Arc::new(RecordingHook {
            id: "beta",
            log: Arc::clone(&log),
        }));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must succeed with hooks attached");

        let seen = log.lock().unwrap().clone();
        // The user message lands first (persisted before the round), the
        // request dispatch after — each point in registration order.
        assert_eq!(
            seen,
            vec!["alpha:user", "beta:user", "alpha:request", "beta:request"],
            "stable registration order at every point"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Acceptance criteria "hook que falla con warn_and_continue no mata
    /// el turno y emite evento": the turn succeeds, and the failing hook
    /// is disabled after its third consecutive failure with exactly one
    /// persisted `HookErrored`.
    #[tokio::test]
    async fn a_persistently_failing_hook_is_disabled_with_one_event_and_the_turn_survives() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Three persisted events (UserMessage, Usage, AssistantText) —
        // exactly enough on_event failures to cross the disable
        // threshold of 3 within one turn.
        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola!".to_string()),
            CompletionEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
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
        )
        .with_hook(Arc::new(AlwaysFailingHook));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("a failing audit-only hook must never kill the turn");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let hook_errors: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::HookErrored { .. }))
            .collect();
        assert_eq!(
            hook_errors.len(),
            1,
            "exactly one HookErrored — at the disable crossing, not per failure: {events:#?}"
        );
        match hook_errors[0] {
            AgentEvent::HookErrored { id, reason, .. } => {
                assert_eq!(id, "always-failing");
                assert_eq!(reason, "boom");
            }
            other => panic!("expected HookErrored, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `PromptBudgetAuditHook` on a real turn: pure smoke — read-only by
    /// construction, must never fail or alter the outcome.
    #[tokio::test]
    async fn the_prompt_budget_audit_hook_is_inert_on_a_normal_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola!".to_string()),
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
        )
        .with_hook(Arc::new(crate::hooks::PromptBudgetAuditHook));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("audit hook must be inert");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::HookErrored { .. })),
            "the audit hook must never error"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.2):
    /// crossing 80% of the turn budget injects ONE `HarnessNote` into the
    /// conversation — the announced deadline a small model needs to
    /// converge instead of exploring until the breaker kills the turn.
    #[tokio::test]
    async fn crossing_80_percent_of_the_token_budget_emits_one_harness_note() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Round 1: a tool call + usage at 85% of the budget.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 80_000,
                    output_tokens: 5_000,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // Round 2: converges with text, still under the budget.
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
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
        )
        .with_max_turn_total_tokens(Some(100_000));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge normally");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let notes: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::HarnessNote { .. }))
            .collect();
        assert_eq!(notes.len(), 1, "exactly one note, not one per round");
        match notes[0] {
            AgentEvent::HarnessNote { kind, text } => {
                assert_eq!(kind, "turn_budget");
                assert!(text.contains("85000"), "carries the real numbers: {text}");
                assert!(text.contains("100000"), "carries the budget: {text}");
            }
            other => panic!("expected HarnessNote, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The `no-harness-notes` ablation: same turn shape, no note — the
    /// A/B braze-bench runs to attribute any pass-rate delta to the
    /// deadline having been announced.
    #[tokio::test]
    async fn the_harness_notes_ablation_silences_the_budget_note() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Usage {
                    input_tokens: 80_000,
                    output_tokens: 5_000,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
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
        )
        .with_max_turn_total_tokens(Some(100_000))
        .with_harness_notes_enabled(false);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge normally");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::HarnessNote { .. })),
            "ablated engine must emit no notes"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Minimal `read_file` stand-in for the re-read nudge test: always
    /// succeeds, so the nudge is the only thing distinguishing the
    /// fourth result from the first three.
    struct StubReadFileProvider;

    #[async_trait]
    impl ToolProvider for StubReadFileProvider {
        fn provider_id(&self) -> &str {
            "stub-read"
        }
        async fn list_stubs(&self) -> Result<Vec<braze_types::ToolStub>, ToolError> {
            Ok(vec![braze_types::ToolStub {
                name: "read_file".to_string(),
                summary: "read a file".to_string(),
                source: "stub".to_string(),
                input_schema: None,
            }])
        }
        async fn resolve_schema(
            &self,
            name: &str,
        ) -> Result<Option<braze_tools_core::ToolSchema>, ToolError> {
            Ok((name == "read_file").then(|| braze_tools_core::ToolSchema {
                name: "read_file".to_string(),
                description: "read a file".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }))
        }
        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "linea a\nlinea b\n".to_string(),
                is_error: false,
            })
        }
    }

    /// Regression test for the roam #6 incident (2026-07-20): a model
    /// that re-reads the same file with slightly different windows
    /// evades the exact-args repeated-call guard. Observed twice in
    /// production: 5 and 10 reads of one 103-line file, zero edits,
    /// until the turn's cap killed it. The nudge must ride the FOURTH
    /// read's successful result — appended, never blocking (chunked
    /// reads of a big file are legitimate).
    #[tokio::test]
    async fn re_reading_one_file_without_editing_appends_a_nudge() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Four reads of the same path with DIFFERENT windows — exactly
        // the shape that slips past `seen_calls`.
        let mut rounds: Vec<Vec<CompletionEvent>> = (0..4)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({
                            "path": "notas.txt",
                            "offset": i * 3 + 1,
                            "limit": 10
                        }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]);

        let engine = Engine::new(
            Box::new(ScriptedModel::new(rounds)),
            ToolRegistry::new(vec![Box::new(StubReadFileProvider)]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "revisa notas.txt", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let nudged: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallCompleted { result, .. }
                    if result.content.contains("without editing it") =>
                {
                    Some(&result.content)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            nudged.len(),
            1,
            "exactly the fourth read must carry the nudge: {events:#?}"
        );
        assert!(
            nudged[0].contains("notas.txt") && !nudged[0].starts_with("[harness]"),
            "the nudge is appended to the real content, not a replacement: {}",
            nudged[0]
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the roam #5 incident (2026-07-20): with a
    /// realistic cap, a convergence note must arrive with REAL runway
    /// (past 70% of the cap), not only the one-round warning at the
    /// edge. In production a 20B model burned 17 rounds re-reading a
    /// file it had broken, got the last-round note at round 19 of 20,
    /// and spent it on a repeated grep.
    #[tokio::test]
    async fn a_convergence_note_arrives_with_runway_before_the_last_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // 10 tool-calling rounds then text: the cap is 10, so the
        // convergence note must fire at round 7 (70%) — three rounds of
        // runway — and the last-round note still at round 9.
        let mut rounds: Vec<Vec<CompletionEvent>> = (0..9)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("r{i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]);
        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(ScriptedModel::new(rounds)),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_iterations(10);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let kinds: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::HarnessNote { kind, .. } => Some(kind.clone()),
                _ => None,
            })
            .collect();
        assert!(
            kinds.iter().any(|k| k == "iteration_converge"),
            "expected a convergence note with runway, got: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| *k == "iteration_converge").count(),
            1,
            "the convergence note must fire exactly once"
        );
        // Y sigue llegando el aviso de última ronda: son dos funciones
        // distintas, no un reemplazo.
        assert!(
            kinds.iter().any(|k| k == "iteration_cap"),
            "the last-round note must still fire: {kinds:?}"
        );
        // Sin edición previa en el turno (echo no es mutante), el consejo
        // es el original: arregla con un edit decisivo.
        let convergence_text = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::HarnessNote { kind, text } if kind == "iteration_converge" => {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("convergence note present");
        assert!(
            convergence_text.contains("fix it with one decisive edit"),
            "a turn that never edited must get the decisive-edit advice: {convergence_text}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the roam #10 incident (2026-07-20, tarea 2 del
    /// testbed): when the turn has ALREADY landed a successful edit, the
    /// convergence note must not tell the model to "fix it with one
    /// decisive edit" — that advice sent gpt-oss:20b back to re-read a
    /// file it had already fixed, after an `old_string not found` error
    /// that only meant the change was already applied. With a prior edit
    /// the advice inverts: verify once and answer.
    #[tokio::test]
    async fn the_convergence_note_says_verify_when_the_turn_already_edited() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Round 0 writes (successful mutation), rounds 1..8 read, round 9
        // answers. Cap 10 ⇒ the convergence note fires at round 7, well
        // after the edit landed.
        let mut rounds: Vec<Vec<CompletionEvent>> = vec![vec![
            CompletionEvent::ToolCallRequested {
                id: "call-w".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": "a.rs" }),
            },
            CompletionEvent::Done,
        ]];
        rounds.extend((0..8).map(|i| {
            vec![
                CompletionEvent::ToolCallRequested {
                    id: format!("call-r{i}"),
                    name: "read_file".to_string(),
                    // Rutas distintas: aísla este test de la nota de
                    // relectura improductiva, que es otra palanca.
                    arguments: serde_json::json!({ "path": format!("f{i}.rs") }),
                },
                CompletionEvent::Done,
            ]
        }));
        rounds.push(vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]);

        let engine = Engine::new(
            Box::new(ScriptedModel::new(rounds)),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_iterations(10);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let convergence_text = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::HarnessNote { kind, text } if kind == "iteration_converge" => {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("convergence note present");
        assert!(
            convergence_text.contains("already applied at least one successful edit"),
            "a turn that edited must get the verify-and-close advice: {convergence_text}"
        );
        assert!(
            !convergence_text.contains("fix it with one decisive edit"),
            "the decisive-edit advice must NOT survive a landed edit: {convergence_text}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The iteration-cap note fires exactly once, right before the final
    /// round — "round N of N is your last" — so a model that would blow
    /// the cap gets one explicit chance to answer instead.
    #[tokio::test]
    async fn the_penultimate_round_emits_the_iteration_cap_note() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
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
        )
        .with_max_turn_iterations(2);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge in round 2");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let notes: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::HarnessNote { .. }))
            .collect();
        assert_eq!(notes.len(), 1);
        match notes[0] {
            AgentEvent::HarnessNote { kind, text } => {
                assert_eq!(kind, "iteration_cap");
                assert!(text.contains("round 2 of 2"), "got: {text}");
            }
            other => panic!("expected HarnessNote, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// J-3 (docs/AUDITORIA-2026-07-v7.md): a harness note reaches the
    /// remaining rounds of ITS OWN turn (as an ephemeral trailing user
    /// message) but is gone from the next turn's requests — before this,
    /// the persisted event rendered from history and a stale "answer now,
    /// stop calling tools" from turn 1 stayed a live instruction for the
    /// whole session (the single-turn bench never saw the failure mode).
    #[tokio::test]
    async fn a_harness_note_is_scoped_to_its_own_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                // Turn 1, round 1: a tool call + usage at 85% of budget →
                // the budget note fires.
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": "hola" }),
                    },
                    CompletionEvent::Usage {
                        input_tokens: 80_000,
                        output_tokens: 5_000,
                        stop_reason: Some("tool_use".to_string()),
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        escalation_trigger: None,
                    },
                    CompletionEvent::Done,
                ],
                // Turn 1, round 2: converges.
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
                // Turn 2, round 1: converges immediately.
                vec![
                    CompletionEvent::TextDelta("listo de nuevo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };
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
        )
        .with_max_turn_total_tokens(Some(100_000));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn 1 must converge");
        engine
            .run_turn(&session, "otra cosa", &mut NoopObserver)
            .await
            .expect("turn 2 must converge");

        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 3, "2 rounds in turn 1 + 1 in turn 2");
            assert!(
                any_message_text_contains(&requests[1], "[harness]"),
                "turn 1's round 2 must still see its own note (the A\u{2032}.2 mechanism)"
            );
            assert!(
                !any_message_text_contains(&requests[2], "[harness]"),
                "turn 2 must NOT see turn 1's stale note (J-3)"
            );
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// J-4 (docs/AUDITORIA-2026-07-v7.md): the task list is turn state —
    /// an entry left `pending` when its turn ends must not re-inject the
    /// summary into the next turn's requests (before this, plans of
    /// unrelated turns mixed and per-round cost grew monotonically with
    /// the session).
    #[tokio::test]
    async fn a_pending_task_does_not_leak_into_the_next_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                // Turn 1, round 1: adds a task (stays pending forever).
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "task_add".to_string(),
                        arguments: serde_json::json!({ "description": "leer el archivo" }),
                    },
                    CompletionEvent::Done,
                ],
                // Turn 1, round 2: converges without finishing the task.
                vec![
                    CompletionEvent::TextDelta("me rindo".to_string()),
                    CompletionEvent::Done,
                ],
                // Turn 2, round 1: converges immediately.
                vec![
                    CompletionEvent::TextDelta("tema nuevo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };
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
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn 1 must converge");
        engine
            .run_turn(&session, "otra cosa", &mut NoopObserver)
            .await
            .expect("turn 2 must converge");

        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 3, "2 rounds in turn 1 + 1 in turn 2");
            assert!(
                any_message_text_contains(&requests[1], "Task list:"),
                "turn 1's round 2 must see its own open task"
            );
            assert!(
                !any_message_text_contains(&requests[2], "Task list:"),
                "turn 2 must NOT inherit turn 1's abandoned pending task (J-4)"
            );
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `None` (the default) never trips, no matter how much a turn spends
    /// — zero behavior change for existing callers.
    #[tokio::test]
    async fn without_a_token_budget_an_expensive_turn_completes_normally() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("respuesta".to_string()),
            CompletionEvent::Usage {
                input_tokens: 5_000_000,
                output_tokens: 100,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
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

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(result.is_ok(), "got {result:?}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `+ablate:no-prune` (opencode ítem 2): with the collapse disabled,
    /// an old observation far beyond the full-observations window renders
    /// FULL — no "[old observation collapsed:" marker anywhere.
    #[tokio::test]
    async fn with_collapse_disabled_old_observations_render_full() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // 8 tool-call pairs (16 events) — with TACTICAL_FULL_OBSERVATIONS
        // = 5, the oldest 3 observations would normally collapse. Each
        // observation is large enough that collapsing saves space (the
        // collapse is skipped for tiny contents).
        for i in 0..8 {
            let id = format!("call-{i}");
            store
                .append(
                    &session,
                    &AgentEvent::AssistantToolCall {
                        id: id.clone(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": format!("f{i}.txt")}),
                    },
                )
                .await
                .expect("seed call");
            store
                .append(
                    &session,
                    &AgentEvent::ToolCallCompleted {
                        id: id.clone(),
                        result: braze_types::ToolResult {
                            tool_call_id: id,
                            content: format!("contenido {i}\n{}", "línea\n".repeat(100)),
                            is_error: false,
                        },
                    },
                )
                .await
                .expect("seed result");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_observation_collapse_enabled(false);

        let messages = engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        let rendered: String = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                braze_types::ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("[old observation collapsed:"),
            "collapse disabled: every observation must render full"
        );
        assert!(
            rendered.contains("contenido 0"),
            "the oldest observation's real content must be present"
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
