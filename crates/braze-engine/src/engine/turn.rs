//! El loop de turno — P1.1 paso 4 (docs/AUDITORIA-2026-07-v8.md § 3).
//! Extraído VERBATIM de `engine/mod.rs` (2026-07-18): `run_turn` (el
//! corazón del motor: rondas completion↔dispatch hasta convergencia o
//! breaker), el estado por turno (`TurnDispatchState`, `TurnGuard`) y
//! la carga de skills mencionadas (`$skill`, D′ explicit-only).

use super::*;

/// Per-turn mutable state `dispatch_tool_calls` threads across every round
/// of one `run_turn` call — bundled into one struct rather than a growing
/// list of `&mut` parameters. Constructed fresh in `run_turn` and never
/// persisted or reused across turns.
pub(super) struct TurnDispatchState {
    /// Per-tool-name retry counter for the "one round of schema-repair
    /// context" mechanism in `dispatch_tool_calls`.
    pub(super) schema_retry_counts: HashMap<String, u32>,
    /// (tool name, canonical arguments) pairs already dispatched this
    /// turn — see `dispatch_tool_calls`'s repetition check (A5,
    /// docs/AUDITORIA-2026-07.md).
    pub(super) seen_calls: HashSet<(String, String)>,
    /// Every `tool_use` id already in this session's history plus every
    /// one minted so far this turn — see `ensure_unique_tool_call_id` and
    /// N-14, docs/AUDITORIA-2026-07-v2.md.
    pub(super) known_tool_call_ids: HashSet<String>,
}

/// RAII guard for [`Engine::turn_in_progress`] — see that field's doc
/// comment (N-17, docs/AUDITORIA-2026-07-v2.md). `acquire` fails if a
/// turn is already in flight; the flag is cleared on `Drop`, covering
/// every exit path from `run_turn` (success, any `?`-propagated error, or
/// an unwind) from one construction at the top instead of touching each
/// exit point individually.
struct TurnGuard<'a> {
    flag: &'a std::sync::atomic::AtomicBool,
}

impl<'a> TurnGuard<'a> {
    fn acquire(flag: &'a std::sync::atomic::AtomicBool) -> Result<Self, EngineError> {
        flag.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .map_err(|_| EngineError::ConcurrentTurn)?;
        Ok(Self { flag })
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}


impl Engine {
    /// Runs one complete turn: append the user's message, then loop
    /// model-completion <-> tool-dispatch rounds until the model responds
    /// with text and no further tool calls (or the safety cap is hit).
    /// `observer` receives each text fragment as it streams in
    /// ([`TurnObserver::on_text_delta`]) plus a live mirror of every
    /// [`AgentEvent`] persisted during the turn
    /// ([`TurnObserver::on_event`]) — the seam a frontend (plain CLI
    /// today, `braze-tui` next) renders from. Headless callers pass
    /// [`braze_events::NoopObserver`].
    ///
    /// A9 (docs/AUDITORIA-2026-07.md): `#[instrument]` wraps the whole
    /// call in an `info_span!`-equivalent "turn" span carrying `session`,
    /// so every log statement this turn produces — including ones nested
    /// several calls deep (`load_messages`, `dispatch_tool_calls`) — is
    /// automatically tagged with it. Diagnosing the "which turn produced
    /// this pathological sequence of tool calls" failure mode with
    /// `RUST_LOG=debug` was previously impossible; this is what makes it
    /// possible without threading `session` through every log call by
    /// hand.
    #[tracing::instrument(name = "turn", skip(self, user_input, observer), fields(session = %session))]
    pub async fn run_turn(
        &self,
        session: &SessionId,
        user_input: &str,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        // N-17 (docs/AUDITORIA-2026-07-v2.md): held for the rest of this
        // call via `TurnGuard`'s `Drop`, covering every exit path
        // (success, any `?`-propagated error, or an unwind) without
        // touching each one individually.
        let _turn_guard = TurnGuard::acquire(&self.turn_in_progress)?;

        // J-3/J-4 (docs/AUDITORIA-2026-07-v7.md): both pieces of
        // turn-scoped harness state start fresh — last turn's task list
        // (a new request is a new plan; stale pending entries re-injected
        // the summary forever and mixed unrelated plans) and last turn's
        // harness notes (a "answer now, stop calling tools" from turn 1
        // must not remain a live instruction in turn 2).
        self.task_list.lock().unwrap().clear();
        self.turn_harness_notes.lock().unwrap().clear();

        // N-4 (docs/AUDITORIA-2026-07-v2.md): repair any tool_use orphaned
        // by a crash/kill/power-loss in a *previous* run *before* this
        // turn's `UserMessage` is appended — `load_messages` also repairs
        // (so a direct caller of it still gets the invariant), but by
        // then the new `UserMessage` would already sit between the
        // orphaned tool_use and its synthesized result, producing a
        // sequence Anthropic rejects with a permanent 400 (the repair
        // itself would be the thing making the session unresumable).
        let existing_events = self.load_and_repair(session, observer).await?;

        // J-12 (docs/AUDITORIA-2026-07-v7.md): `loaded_skills` is
        // in-memory only — after a restart (`--resume`) or a `/model`
        // engine rebuild, the log's `SkillLoaded` events are the only
        // trace of guidance the conversation still references. Re-load
        // those bodies before this turn's own mentions resolve, so the
        // system prompt keeps carrying what the transcript assumes.
        self.rehydrate_skills_from_log(&existing_events);

        // D′: `$skill` mentions resolve before anything else this turn —
        // the study's point is loading the guidance BEFORE the executor's
        // first mistake, not after.
        self.load_mentioned_skills(session, user_input, observer)
            .await?;

        self.append_and_notify(
            session,
            &AgentEvent::UserMessage {
                text: user_input.to_string(),
            },
            observer,
        )
        .await?;

        // D5: the streak reflects *previous* turns only (this turn hasn't
        // run yet) — reusing the plain `UserMessage` event kind rather
        // than adding a new `AgentEvent` variant just for this.
        if self
            .consecutive_turns_without_tool_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            >= NARRATION_WITHOUT_ACTION_THRESHOLD
        {
            self.append_and_notify(
                session,
                &AgentEvent::UserMessage {
                    text: "[Reminder] Your last few responses described an intended action \
                           without actually calling the tool for it. If you're being asked \
                           to do something, call the appropriate tool now instead of \
                           describing or restating the plan."
                        .to_string(),
                },
                observer,
            )
            .await?;
        }

        let mut messages = self.load_messages(session, observer).await?;

        // PLAN.md § "Split planificador/ejecutor": optional one-shot
        // planning round before the executor loop. Doesn't count against
        // `MAX_TURN_ITERATIONS`, and can only *add* a persisted
        // `PlanCreated` (in which case messages are reloaded so the plan
        // reaches the executor's first request) — every planner failure
        // mode degrades to an unplanned turn instead of failing it.
        if self
            .attempt_planning_round(session, &messages, observer)
            .await?
        {
            messages = self.load_messages(session, observer).await?;
        }

        // Per-turn state threaded through `dispatch_tool_calls` across
        // every round of this call — schema-repair retry counts, the
        // repeated-call detector (A5), and the id-uniqueness guard (N-14,
        // docs/AUDITORIA-2026-07-v2.md, seeded from this session's
        // history so a collision with a *previous* turn — e.g. a
        // backend's synthetic-id fallback restarting its counter after
        // `--resume` — gets caught too, not just collisions within this
        // turn). Lives and dies with this `run_turn` call — none of it is
        // a field on `Engine` or persists across turns.
        let mut dispatch_state = TurnDispatchState {
            schema_retry_counts: HashMap::new(),
            seen_calls: HashSet::new(),
            known_tool_call_ids: Self::existing_tool_call_ids(&existing_events),
        };

        // D5: whether *any* round of this specific turn has dispatched a
        // tool call yet — decides, at every exit point below, whether
        // this turn counts toward `consecutive_turns_without_tool_calls`
        // or breaks the streak. Local to this call, unlike the `Engine`
        // field it eventually updates: a turn that calls a tool in an
        // early round and then converges with a plain-text answer in a
        // later round must NOT count as "narration only" just because its
        // *last* round happened to have no tool calls.
        let mut any_tool_calls_this_turn = false;
        // v4 P0.2: cumulative input+output across this turn's rounds —
        // the quantity `max_turn_total_tokens` breaks on. Local to the
        // call, like `any_tool_calls_this_turn`.
        let mut turn_total_tokens: u64 = 0;
        // A′.2: the budget warning fires at most ONCE per turn — a
        // repeated "you're over 80%" every round is noise competing with
        // the prompt budget it's trying to protect. (The iteration-cap
        // note needs no flag: `round` hits its threshold exactly once.)
        let mut budget_note_emitted = false;

        for round in 0..self.max_turn_iterations {
            // v4 P0.2 (docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3):
            // checked at the top of the NEXT iteration, not right after a
            // round — a round that converges to a final answer within
            // budget+ε must return normally, and one more model call
            // against an over-budget history is exactly what this breaker
            // exists to prevent. Same graceful degradation the iteration
            // cap gets below: summarize what was found instead of failing
            // outright.
            if let Some(budget) = self.max_turn_total_tokens
                && turn_total_tokens > budget
            {
                tracing::warn!(
                    round,
                    budget_tokens = budget,
                    spent_tokens = turn_total_tokens,
                    "turn blew its cumulative token budget; attempting a final tools-free summary \
                     round instead of continuing to re-send a growing history"
                );
                self.consecutive_turns_without_tool_calls
                    .store(0, std::sync::atomic::Ordering::SeqCst);
                if self
                    .attempt_tools_free_summary_round(session, &messages, observer)
                    .await?
                    == SummaryFallbackOutcome::Summarized
                {
                    return Ok(());
                }
                return Err(EngineError::TurnBudgetExhausted {
                    budget_tokens: budget,
                    spent_tokens: turn_total_tokens,
                });
            }

            // N-16 (docs/AUDITORIA-2026-07-v2.md): the lossy variant
            // degrades a provider that fails to list its stubs (e.g. an
            // MCP server that died mid-session) instead of aborting every
            // subsequent turn, including ones that only need local tools.
            let all_stubs = self.tools.all_stubs_lossy().await;
            // C′.1 (crate::tool_search): providers over the threshold
            // hide behind the `search_tools` meta-tool; activated hits
            // resurface. Recomputed per round so a search in round N
            // changes round N+1's inventory.
            let inventory = crate::tool_search::apply_deferral(
                all_stubs,
                self.tool_search_threshold,
                &self.activated_deferred_tools.lock().unwrap().clone(),
            );
            let mut tool_stubs = inventory.visible;
            let hidden_stubs = inventory.hidden;
            // C′.2: the task tools join the inventory only when the
            // lever is on, and the compact summary rides as an ephemeral
            // trailing user message — request-scoped like the inventory
            // itself, never persisted (persisting it every round would
            // be the prose-plan noise this lever exists to replace).
            let mut request_messages = messages.clone();
            // I.7: the explore tool joins the inventory only when the
            // lever is on — same opt-in posture as the task tools below.
            if self.exploration_enabled {
                tool_stubs.push(crate::exploration::explore_tool_stub());
            }
            if self.task_list_enabled {
                tool_stubs.extend(crate::task_list::task_tool_stubs());
                let task_list = self.task_list.lock().unwrap();
                if task_list.has_open_tasks() {
                    request_messages.push(Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: task_list.summary_line(),
                        }],
                    });
                }
            }
            // J-3: this turn's harness notes ride every later request of
            // the turn as ephemeral trailing user messages — same
            // request-scoped pattern as the task-list summary above; the
            // persisted events are audit-only (see the emission site).
            for note in self.turn_harness_notes.lock().unwrap().iter() {
                request_messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: format!("[harness] {note}"),
                    }],
                });
            }
            let req = CompletionRequest {
                messages: request_messages,
                tool_stubs: tool_stubs.clone(),
                system_prompt: self.system_prompt_with_skills(),
                max_tokens: self.max_tokens,
            };

            // B′: audit-only hooks see the request about to be sent
            // (`PromptBudgetAuditHook`'s attach point). Read-only by
            // construction — the request is sent regardless.
            if !self.hooks.is_empty() {
                for (id, reason) in self.dispatch_hooks_before_model_request(&req).await {
                    self.append_and_notify(
                        session,
                        &AgentEvent::HookErrored {
                            id,
                            point: crate::hooks::HookPoint::BeforeModelRequest
                                .as_str()
                                .to_string(),
                            reason,
                        },
                        observer,
                    )
                    .await?;
                }
            }

            // técnica G10 (docs/AUDITORIA-2026-07.md): `best_of_n <= 1`
            // takes the exact single-call path that existed before G10 —
            // `complete_once` is a straight extraction of what used to be
            // inline here, not new behavior.
            let RoundOutcome {
                text_buffer,
                tool_calls,
                usage,
                truncated,
                rescue_applied,
            } = if self.best_of_n > 1 {
                self.complete_with_best_of_n(&req, observer).await?
            } else {
                self.complete_once(req, observer, true).await?
            };

            // Persisted once per round (if the backend reported it) so
            // tooling like `braze-bench` can read per-round token usage
            // back out of the rollout log — see `AgentEvent::Usage`'s doc
            // comment. Order relative to the round's other events doesn't
            // matter: it's audit-only and never rendered into a `Message`
            // (see `history::event_to_message`). Under G10, this already
            // reflects the *summed* cost of every candidate this round
            // generated, not just the winner's — see
            // `complete_with_best_of_n`.
            if let Some(round_usage) = usage {
                // v4 P0.2: feed the turn's cumulative-token breaker (the
                // check at the top of the next iteration).
                turn_total_tokens +=
                    u64::from(round_usage.input_tokens) + u64::from(round_usage.output_tokens);
                // H-3 (docs/AUDITORIA-2026-07-v5.md): `AgentEvent::Usage`
                // itself gains no new field — the escalation fact gets its
                // own persisted event below instead, captured here before
                // `round_usage`'s other fields move into the `Usage`
                // literal.
                let escalation_trigger = round_usage.escalation_trigger;
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
                if let Some(trigger) = escalation_trigger {
                    self.append_and_notify(
                        session,
                        &AgentEvent::EscalationToLead { trigger },
                        observer,
                    )
                    .await?;
                }
            }

            // H-3 (docs/AUDITORIA-2026-07-v5.md): the action already
            // happened (inside `complete_once_with`'s rescue ladder,
            // logged via `tracing::info!`) — this just gives it a
            // persisted, bench-countable trail alongside the log line.
            if let Some(parser) = rescue_applied {
                self.append_and_notify(
                    session,
                    &AgentEvent::TextualRescueApplied { parser },
                    observer,
                )
                .await?;
            }

            // A9 (docs/AUDITORIA-2026-07.md): the round-level fact
            // `RUST_LOG=debug` previously had no way to see — which round
            // of this turn this was, and how many tool calls it produced
            // — nested under the "turn" span's `session` field.
            tracing::debug!(round, n_tool_calls = tool_calls.len(), "round completed");

            if tool_calls.is_empty() {
                // N-24 (docs/AUDITORIA-2026-07-v2.md): a truncated round
                // with no tool calls used to be persisted as a normal,
                // converged final answer — indistinguishable downstream
                // from a response the model actually finished on its own.
                // Surface it as an error instead of silently keeping (and
                // showing the user) a possibly mid-sentence answer.
                if truncated {
                    return Err(EngineError::TruncatedFinalResponse);
                }
                // Bajo (docs/AUDITORIA-2026-07-v2.md, "una completion
                // vacía termina el turno como éxito silencioso"): no text
                // and no tool calls is not a legitimate final answer —
                // under best-of-n several empty candidates can share the
                // same (empty) signature and win the vote outright.
                // Treat it as a failure to converge for this round rather
                // than a silent no-op success.
                if text_buffer.is_empty() {
                    // U-1 (docs/usability-log-template.md, hallado en vivo
                    // 2026-07-07 contra qwen3.5-coder/Nitro): a real session
                    // asked for a hardware report; the model called
                    // `shell_exec`×3 and `write_file` (all persisted, the
                    // file landed on disk), then its *next* round — asked
                    // to wrap up with no further tool calls pending — came
                    // back with neither text nor a tool call. Failing the
                    // whole turn here reported an error even though the
                    // actual task had already succeeded. If this turn
                    // already made real progress (dispatched at least one
                    // tool call), give it the same one-more-shot fallback
                    // `MAX_TURN_ITERATIONS` exhaustion gets below, instead
                    // of discarding that progress behind a hard failure. A
                    // turn whose *very first* round comes back empty (no
                    // progress at all) still fails immediately — nothing to
                    // summarize, and the best-of-n false-convergence risk
                    // this error exists for still applies in full.
                    if any_tool_calls_this_turn {
                        tracing::warn!(
                            round,
                            "round produced neither text nor a tool call after this turn already \
                             dispatched at least one; attempting a tools-free summary round \
                             instead of discarding that progress"
                        );
                        match self
                            .attempt_tools_free_summary_round(session, &messages, observer)
                            .await?
                        {
                            SummaryFallbackOutcome::Summarized => {
                                self.consecutive_turns_without_tool_calls
                                    .store(0, std::sync::atomic::Ordering::SeqCst);
                                return Ok(());
                            }
                            // Memory-distillation smoke 2026-07-16 against
                            // gpt-oss:20b/Nitro: both transfer tasks had
                            // already written the expected fix to disk when
                            // the model closed the turn — AND the summary
                            // fallback — with an empty content channel (a
                            // reasoning-model quirk: thinking arrives in a
                            // separate field, content can legitimately come
                            // back ""). The tool results are persisted and
                            // the fallback attempt is on record as
                            // `SummaryFallbackAttempted` + its `Usage`, so
                            // ending the turn beats reporting the whole
                            // thing as a hard failure. The best-of-n
                            // false-convergence risk `EmptyModelResponse`
                            // guards doesn't apply: this branch requires
                            // dispatched tool calls plus a paid fallback
                            // round, never a bare empty first round.
                            SummaryFallbackOutcome::Empty => {
                                tracing::warn!(
                                    round,
                                    "summary fallback also returned empty; ending the turn with \
                                     the already-persisted tool results instead of failing it"
                                );
                                self.consecutive_turns_without_tool_calls
                                    .store(0, std::sync::atomic::Ordering::SeqCst);
                                return Ok(());
                            }
                            // A dead fallback call may be a real backend
                            // failure — keep surfacing it as the error the
                            // turn would have raised without the fallback.
                            SummaryFallbackOutcome::CallFailed => {}
                        }
                    }
                    return Err(EngineError::EmptyModelResponse);
                }
                // Final response: no further tool calls requested.
                self.append_and_notify(
                    session,
                    &AgentEvent::AssistantText { text: text_buffer },
                    observer,
                )
                .await?;
                // D5: only a turn that *never* dispatched a tool call
                // (not just "this round didn't") counts toward the streak.
                if any_tool_calls_this_turn {
                    self.consecutive_turns_without_tool_calls
                        .store(0, std::sync::atomic::Ordering::SeqCst);
                } else {
                    self.consecutive_turns_without_tool_calls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                return Ok(());
            }

            // Text preceding this round's tool calls (if any) is persisted
            // first, preserving the order the model actually produced it
            // in, before the tool_use blocks that followed it.
            if !text_buffer.is_empty() {
                self.append_and_notify(
                    session,
                    &AgentEvent::AssistantText { text: text_buffer },
                    observer,
                )
                .await?;
            }

            self.dispatch_tool_calls(
                session,
                &tool_calls,
                &tool_stubs,
                &hidden_stubs,
                &mut dispatch_state,
                observer,
            )
            .await?;
            any_tool_calls_this_turn = true;

            // A′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md
            // § I.2): announce the deadline BEFORE the cut, not after —
            // TurnBudget/the iteration cap abort turns the model never
            // knew were budgeted; an announced deadline gives a small
            // model the chance to converge on its own. The event is
            // persisted for audit/bench counting, but what the model sees
            // is the ephemeral copy in `turn_harness_notes`, re-appended
            // to every later request of THIS turn only (J-3,
            // docs/AUDITORIA-2026-07-v7.md: rendering notes from history
            // kept "answer now, stop calling tools" alive as an
            // instruction in every subsequent turn of the session).
            if self.harness_notes_enabled {
                if let Some(budget) = self.max_turn_total_tokens
                    && !budget_note_emitted
                    && turn_total_tokens.saturating_mul(5) >= budget.saturating_mul(4)
                {
                    budget_note_emitted = true;
                    let text = format!(
                        "This turn has used {turn_total_tokens} of its {budget}-token \
                         budget (over 80%). Stop exploring and answer now with what you \
                         already have — the turn will be cut off at the budget."
                    );
                    self.append_and_notify(
                        session,
                        &AgentEvent::HarnessNote {
                            kind: "turn_budget".to_string(),
                            text: text.clone(),
                        },
                        observer,
                    )
                    .await?;
                    self.turn_harness_notes.lock().unwrap().push(text);
                }
                if self.max_turn_iterations >= 2 && round + 2 == self.max_turn_iterations {
                    let text = format!(
                        "The next round is this turn's last (round {} of {}). Answer now \
                         with what you already have instead of calling more tools.",
                        round + 2,
                        self.max_turn_iterations
                    );
                    self.append_and_notify(
                        session,
                        &AgentEvent::HarnessNote {
                            kind: "iteration_cap".to_string(),
                            text: text.clone(),
                        },
                        observer,
                    )
                    .await?;
                    self.turn_harness_notes.lock().unwrap().push(text);
                }
            }

            messages = self.load_messages(session, observer).await?;
        }

        // Fell through with `MAX_TURN_ITERATIONS` rounds exhausted — every
        // one of them had a non-empty `tool_calls` (any empty round would
        // have returned above), so this turn definitely isn't "narration
        // only"; D5's streak resets here too.
        self.consecutive_turns_without_tool_calls
            .store(0, std::sync::atomic::Ordering::SeqCst);

        tracing::warn!(
            max_iterations = self.max_turn_iterations,
            "turn did not converge; attempting a final tools-free summary round instead of failing outright"
        );
        if self
            .attempt_tools_free_summary_round(session, &messages, observer)
            .await?
            == SummaryFallbackOutcome::Summarized
        {
            return Ok(());
        }
        Err(EngineError::TurnDidNotConverge(self.max_turn_iterations))
    }

    /// The optional planning round (PLAN.md § "Split
    /// planificador/ejecutor"): asks `self.planner` — if configured — for
    /// a short plan of the turn, persists it as
    /// [`AgentEvent::PlanCreated`], and returns whether a plan was
    /// actually persisted (so `run_turn` knows to reload messages).
    ///
    /// Tools are *inlined as text* in the planning system prompt
    /// (name + summary from the stubs), with `tool_stubs` left empty on
    /// the request: the planner needs tool *awareness*, never invocation
    /// — the same only-names-in-context philosophy as deferred loading.
    ///
    /// Degradation, never failure (same philosophy as N-13's best-of-n
    /// fix — an optional enhancement must not kill the turn):
    /// 1. planner call errors ⇒ warn, proceed without a plan;
    /// 2. planner attempted tool calls ⇒ ignored with a warn, its text
    ///    is still used;
    /// 3. response truncated by the token budget ⇒ plan discarded (a
    ///    cut-off plan can mislead mid-step — espíritu N-24);
    /// 4. empty/whitespace text ⇒ plan discarded.
    ///
    /// The planner's `Usage` is persisted even when the plan is
    /// discarded — the cost was real either way, and hiding it would
    /// skew exactly the A/B accounting this feature exists to enable.
    ///
    /// Only `Err` for session-store failures (persisting `Usage`/
    /// `PlanCreated`) — those are real turn failures, not planner ones.
    /// D′: the base system prompt plus every loaded skill's addendum —
    /// rebuilt per request from in-memory state (the study's rule: never
    /// persist a body as conversation; the rollout log's trace is the
    /// `SkillLoaded` event).
    fn system_prompt_with_skills(&self) -> String {
        let loaded = self.loaded_skills.lock().unwrap();
        if loaded.is_empty() {
            return self.system_prompt.clone();
        }
        let mut prompt = self.system_prompt.clone();
        for skill in loaded.iter() {
            prompt.push_str(&skill.prompt_addendum());
        }
        prompt
    }

    /// J-12 (docs/AUDITORIA-2026-07-v7.md): re-loads every skill the
    /// session log records as loaded but that this `Engine` instance
    /// doesn't hold in memory — the `--resume`/`/model`-rebuild
    /// counterpart of `load_mentioned_skills`. Persists nothing (the log
    /// already records each original load; re-appending would double the
    /// bench's `SkillLoaded` counts on every resumed turn) and applies
    /// no per-turn cap (each body already passed the cap when it
    /// originally loaded). A body that became unreadable since — file
    /// deleted, registry paths changed — degrades to a warn and a system
    /// prompt without that addendum: same "an optional enhancement must
    /// not kill the turn" posture as the planner.
    fn rehydrate_skills_from_log(&self, events: &[AgentEvent]) {
        let Some(registry) = &self.skill_registry else {
            return;
        };
        for event in events {
            let AgentEvent::SkillLoaded { name, .. } = event else {
                continue;
            };
            if self
                .loaded_skills
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.name == *name)
            {
                continue;
            }
            match registry.load_body(name, self.skills_max_body_tokens) {
                Some(loaded) => {
                    tracing::info!(
                        skill = %loaded.name,
                        estimated_tokens = loaded.estimated_tokens,
                        "skill rehydrated from session log (J-12)"
                    );
                    self.loaded_skills.lock().unwrap().push(loaded);
                }
                None => {
                    tracing::warn!(
                        skill = %name,
                        "skill recorded in session log is no longer loadable — \
                         proceeding without its guidance"
                    );
                }
            }
        }
    }

    /// D′: resolves this turn's explicit `$skill` mentions against the
    /// registry, loading up to `skills_max_loaded_per_turn` bodies (cap
    /// crossings and unreadable files persist as `SkillLoadSkipped`).
    /// Already-loaded skills are skipped silently — re-mentioning is a
    /// no-op, not an error.
    async fn load_mentioned_skills(
        &self,
        session: &SessionId,
        user_input: &str,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        let Some(registry) = &self.skill_registry else {
            return Ok(());
        };
        let mentions = registry.explicit_mentions(user_input);
        let mut loaded_this_turn = 0usize;
        for name in mentions {
            if self
                .loaded_skills
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.name == name)
            {
                continue;
            }
            if loaded_this_turn >= self.skills_max_loaded_per_turn {
                self.append_and_notify(
                    session,
                    &AgentEvent::SkillLoadSkipped {
                        name,
                        reason: format!(
                            "per-turn cap ({}) reached",
                            self.skills_max_loaded_per_turn
                        ),
                    },
                    observer,
                )
                .await?;
                continue;
            }
            match registry.load_body(&name, self.skills_max_body_tokens) {
                Some(loaded) => {
                    tracing::info!(
                        skill = %loaded.name,
                        estimated_tokens = loaded.estimated_tokens,
                        truncated = loaded.truncated,
                        "skill loaded from explicit mention"
                    );
                    self.append_and_notify(
                        session,
                        &AgentEvent::SkillLoaded {
                            name: loaded.name.clone(),
                            trigger: "explicit_mention".to_string(),
                            estimated_tokens: loaded.estimated_tokens,
                            truncated: loaded.truncated,
                        },
                        observer,
                    )
                    .await?;
                    self.loaded_skills.lock().unwrap().push(loaded);
                    loaded_this_turn += 1;
                }
                None => {
                    self.append_and_notify(
                        session,
                        &AgentEvent::SkillLoadSkipped {
                            name,
                            reason: "body unreadable at load time".to_string(),
                        },
                        observer,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}
