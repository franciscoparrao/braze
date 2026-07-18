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
            let seeded = self
                .task_list
                .lock()
                .unwrap()
                .seed_from_numbered_plan(plan);
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
            digit_count > 0
                && trimmed[digit_count..].starts_with(['.', ')'])
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
