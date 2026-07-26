//! La ronda de planificación del split planificador/ejecutor — P1.1
//! paso 3 (docs/AUDITORIA-2026-07-v8.md § 3). Extraído VERBATIM de
//! `engine/mod.rs` (2026-07-18): `attempt_planning_round`, el system
//! prompt del planner y el conteo de pasos numerados que gobierna el
//! descarte de planes triviales. La evidencia del A/B pre-registrado
//! vive en docs/sweep-planlead-2026-07-11.md.

use super::*;

impl Engine {
    pub(super) async fn attempt_planning_round(
        &self,
        session: &SessionId,
        messages: &[Message],
        observer: &mut dyn TurnObserver,
    ) -> Result<bool, EngineError> {
        let Some(planner) = &self.planner else {
            return Ok(false);
        };

        // C′.1: the planner's prompt lists the tool inventory — the same
        // deferral applies (1.500 nombres en el prompt del planner es el
        // mismo problema que en el del executor).
        let tool_stubs = crate::tool_search::apply_deferral(
            self.tools.all_stubs_lossy().await,
            self.tool_search_threshold,
            &self.activated_deferred_tools.lock().unwrap().clone(),
        )
        .visible;
        let req = CompletionRequest {
            messages: messages.to_vec(),
            tool_stubs: Vec::new(),
            system_prompt: planning_system_prompt(&self.system_prompt, &tool_stubs),
            max_tokens: self.max_tokens.min(self.planner_max_tokens),
        };

        // `emit_deltas: false`: the plan reaches frontends once, as the
        // `PlanCreated` event mirror — streaming its text live too would
        // render it twice in the TUI (markdown preview + PlanCell).
        // `rescue_enabled: false` (F7, docs/AUDITORIA-2026-07-v3.md): a
        // plan step naming a tool in the planner's own native
        // tool-template syntax must survive as plan *text*, not be
        // extracted and discarded by the textual rescue before this
        // function even sees it.
        let outcome = match self
            .complete_once_with(planner.as_ref(), req, observer, false, false, false)
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "planner call failed; proceeding without a plan"
                );
                return Ok(false);
            }
        };

        if let Some(round_usage) = outcome.usage {
            self.append_and_notify(
                session,
                &AgentEvent::Usage {
                    input_tokens: round_usage.input_tokens,
                    output_tokens: round_usage.output_tokens,
                    stop_reason: round_usage.stop_reason,
                    cache_read_tokens: round_usage.cache_read_tokens,
                    cache_write_tokens: round_usage.cache_write_tokens,
                },
                observer,
            )
            .await?;
        }

        if !outcome.tool_calls.is_empty() {
            tracing::warn!(
                n_tool_calls = outcome.tool_calls.len(),
                "planner attempted tool calls despite the planning prompt; ignoring them"
            );
        }
        if outcome.truncated {
            tracing::warn!(
                "planner response was truncated by the token budget; discarding the partial plan"
            );
            return Ok(false);
        }
        let plan = outcome.text_buffer.trim();
        if plan.is_empty() {
            tracing::warn!("planner returned no usable text; proceeding without a plan");
            return Ok(false);
        }
        // Iteración pre-registrada del planner (PLAN.md § "Split
        // planificador/ejecutor"; ejecutada 2026-07-10): a plan with
        // fewer than two numbered steps is discarded instead of
        // persisted. The planning prompt itself asks for "just that
        // single step" on trivial requests — but a single-step plan adds
        // nothing the executor's first round wouldn't do anyway, costs
        // prompt tokens, and the matrix sweep
        // (docs/sweep-matriz-4brazos-2026-07-10.md) measured the
        // plan-in-prompt as the trigger of a degeneration artifact
        // precisely on trivial tasks (`no_tool` 15/15 → 6/15, all empty
        // responses in the round right after the plan). Prose with no
        // numbered steps counts as single-step: no structure worth
        // paying for.
        if count_numbered_steps(plan) < 2 {
            tracing::info!(
                "planner produced a single-step (or unstructured) plan; discarding it — \
                 the executor's first round covers it without the plan-in-prompt cost"
            );
            return Ok(false);
        }
        // C′.2 (crate::task_list): with the task list on, the plan
        // becomes TYPED STATE instead of prose — its numbered steps seed
        // the list (re-injected compactly every round) and no
        // `PlanCreated` prose enters the history at all. This is the
        // "planner→tasks" arm of the pre-registered A/B (PLAN.md § split
        // planificador/ejecutor): same planner call, different delivery.
        if self.task_list_enabled {
            let seeded = self.task_list.lock().unwrap().seed_from_numbered_plan(plan);
            tracing::info!(
                seeded,
                "plan delivered as typed tasks instead of prose (task list enabled)"
            );
            return Ok(false);
        }

        self.append_and_notify(
            session,
            &AgentEvent::PlanCreated {
                plan: plan.to_string(),
            },
            observer,
        )
        .await?;
        Ok(true)
    }
}

/// Counts lines that start (after leading whitespace) with `N.` or `N)`
/// — the numbered-step shape `planning_system_prompt` asks for. Used by
/// the single-step discard in [`Engine::attempt_planning_round`]; pure
/// and free-standing so the counting rule is unit-testable on its own.
pub(super) fn count_numbered_steps(plan: &str) -> usize {
    plan.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let digit_count = trimmed.chars().take_while(char::is_ascii_digit).count();
            digit_count > 0 && trimmed[digit_count..].starts_with(['.', ')'])
        })
        .count()
}

/// System prompt for the planning round (PLAN.md § "Split
/// planificador/ejecutor"): the base prompt plus planning instructions
/// and the tool list inlined as text — the planner sees the same working
/// context the executor will, plus what tools exist (names + summaries
/// only, deferred-loading style), minus the ability to call any of them.
/// The triviality clause keeps `no_tool`/`single_tool` requests from
/// being inflated with overhead plans.
///
/// (Doc reunido con su función en el paso 3 del split — en `engine.rs`
/// este bloque había quedado pegado sobre `count_numbered_steps`.)
fn planning_system_prompt(base: &str, stubs: &[ToolStub]) -> String {
    let mut tools_list = String::new();
    for stub in stubs {
        tools_list.push_str(&format!("- {}: {}\n", stub.name, stub.summary));
    }
    if tools_list.is_empty() {
        tools_list.push_str("(none)\n");
    }
    format!(
        "{base}\n\n\
         You are the planning step for this turn. Do NOT call any tool — none are \
         available in this request. Write a short numbered plan (3-7 steps) for how \
         to fulfill the user's latest request, naming the concrete tools you would \
         use from the list below and their key arguments where possible. If the \
         request is trivial (a single obvious action, or directly answerable), \
         reply with just that single step.\n\n\
         Available tools:\n{tools_list}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 paso 6: tests de integración movidos del mod tests de
    // engine/mod.rs — fixtures compartidas en engine/test_support.rs.
    use crate::engine::Engine;
    use crate::engine::test_support::*;
    use braze_events::NoopObserver;
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_types::{ContentBlock, SessionId};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Iteración pre-registrada del planner (2026-07-10): the numbered-
    /// step counting rule the single-step discard keys on — `N.`/`N)`
    /// after optional indentation; prose without numbers counts zero.
    #[test]
    fn count_numbered_steps_recognizes_dot_and_paren_forms_and_ignores_prose() {
        assert_eq!(count_numbered_steps("1. leer\n2. editar\n3. verificar"), 3);
        assert_eq!(count_numbered_steps("  1) leer\n  2) editar"), 2);
        assert_eq!(count_numbered_steps("1. único paso"), 1);
        assert_eq!(
            count_numbered_steps("primero leo el archivo y después respondo"),
            0
        );
        assert_eq!(count_numbered_steps("10. paso\n11. otro"), 2);
    }

    /// PLAN.md § "Split planificador/ejecutor", oleada 1: the shape a
    /// planned turn produces — `UserMessage`, `PlanCreated`, then the
    /// first round's tool calls — must render into a request the real
    /// Anthropic API accepts. The plan becomes an assistant Text message
    /// immediately before the round's assistant tool_use message
    /// (consecutive assistant messages — already the exact shape a
    /// text-before-tools round produces today), and the tool_use/result
    /// pairing must survive the plan sitting in between.
    #[tokio::test]
    async fn a_planned_turn_shape_renders_protocol_valid_messages() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        for event in [
            AgentEvent::UserMessage {
                text: "haz tres cosas".to_string(),
            },
            AgentEvent::PlanCreated {
                plan: "1. echo a\n2. echo b\n3. responder".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": "a" }),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "echoed: a".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "listo".to_string(),
            },
        ] {
            store.append(&session, &event).await.expect("seed event");
        }

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
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("load_messages should succeed");

        crate::protocol_check::check_anthropic_message_protocol(&messages)
            .expect("a planned turn's rendered request must be protocol-valid");

        assert!(
            messages
                .iter()
                .any(|m| m.role == braze_types::Role::User
                    && m.content.iter().any(
                        |b| matches!(b, ContentBlock::Text { text } if text.starts_with("Plan for this request"))
                    )),
            "the plan must reach the rendered request as user-role context, got: {messages:#?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// End-to-end happy path: the planner's text is persisted as
    /// `PlanCreated` (with its `Usage` before it), the executor's first
    /// request actually contains the rendered plan, and the whole planned
    /// turn stays protocol-valid.
    #[tokio::test]
    async fn a_planned_turn_persists_the_plan_and_the_executor_sees_it() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. echo hi\n2. responder".to_string()),
            CompletionEvent::Usage {
                input_tokens: 50,
                output_tokens: 12,
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: None,
                cache_write_tokens: None,
                escalation_trigger: None,
            },
            CompletionEvent::Done,
        ]]);

        let executor_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let executor = RequestCapturingModel {
            inner: ProtocolValidatingModel::new(ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": "hi" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ])),
            requests: Arc::clone(&executor_requests),
        };

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "haz echo de hi", &mut NoopObserver)
            .await
            .expect("planned turn should succeed");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        match &events[1] {
            AgentEvent::Usage { input_tokens, .. } => assert_eq!(*input_tokens, 50),
            other => panic!("expected the planner's Usage first, got {other:?}"),
        }
        match &events[2] {
            AgentEvent::PlanCreated { plan } => {
                assert_eq!(plan, "1. echo hi\n2. responder");
            }
            other => panic!("expected PlanCreated, got {other:?}"),
        }

        {
            let requests = executor_requests.lock().unwrap();
            assert!(
                requests[0].messages.iter().any(|m| m.content.iter().any(
                    |b| matches!(b, ContentBlock::Text { text } if text.contains("1. echo hi") && text.starts_with("Plan for this request"))
                )),
                "the executor's first request must contain the rendered plan"
            );
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 1: a planner whose call fails must not fail the
    /// turn — it proceeds unplanned.
    #[tokio::test]
    async fn a_failing_planner_degrades_to_an_unplanned_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(ErroringModel));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the turn must survive a failing planner");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "no plan must be persisted when the planner fails"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { text } if text == "hola")),
            "the executor's answer must still be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 4: an empty planner response degrades to an
    /// unplanned turn (contrast with the *executor*, where an empty
    /// completion on the turn's very first round is a hard
    /// `EmptyModelResponse` error — one occurring after the turn already
    /// dispatched a tool call instead gets one tools-free summary attempt,
    /// see `attempt_tools_free_summary_round`).
    #[tokio::test]
    async fn an_empty_planner_response_degrades_to_an_unplanned_turn() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![CompletionEvent::Done]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the turn must survive an empty planner response");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. }))
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Degradation rule 2: a planner that attempts tool calls despite the
    /// planning prompt has them ignored — never dispatched — while its
    /// text is still used as the plan.
    #[tokio::test]
    async fn a_planner_that_attempts_tool_calls_has_them_ignored_but_its_text_used() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::ToolCallRequested {
                id: "planner-call".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({ "text": "should never run" }),
            },
            CompletionEvent::TextDelta("1. hacer echo de hi\n2. responder".to_string()),
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the planner's tool call must never be dispatched"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::PlanCreated { plan } if plan == "1. hacer echo de hi\n2. responder"
            )),
            "the planner's text must still be used as the plan"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F7 (docs/AUDITORIA-2026-07-v3.md): `planning_system_prompt` asks
    /// the planner to name the concrete tools it would use. A local
    /// planner answering in its own native tool-template syntax (e.g.
    /// Qwen's `<tool_call>{...}</tool_call>`) must have that block survive
    /// as plain plan *text* — the textual rescue, shared with the
    /// executor by default before this fix, would otherwise extract and
    /// remove it from the plan before `attempt_planning_round` even
    /// looks at `outcome.tool_calls` (already ignored there regardless).
    #[tokio::test]
    async fn a_planners_native_tool_template_leak_survives_as_plan_text_not_rescued() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(
                "1. <tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"x\"}}</tool_call>\n\
                 2. responder"
                    .to_string(),
            ),
            CompletionEvent::Done,
        ]]);
        let executor = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
            CompletionEvent::Done,
        ]]);

        let invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the leaked block must never be dispatched as a real call"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::PlanCreated { .. }))
            .expect("expected a PlanCreated event")
        {
            AgentEvent::PlanCreated { plan } => {
                assert!(
                    plan.contains("<tool_call>"),
                    "the tagged block must survive as plan text, got: {plan}"
                );
                assert!(plan.contains("read_file"), "got: {plan}");
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
