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
    /// (tool name, argumentos canónicos) ya despachados en ESTE turno,
    /// mapeados al contenido de su resultado exitoso — ver la verificación
    /// de repetición en `dispatch_tool_calls` (A5,
    /// docs/AUDITORIA-2026-07.md).
    ///
    /// `None` = despachada pero todavía sin resultado (dos llamadas
    /// idénticas en la MISMA ronda). Guardar el contenido es lo que
    /// permite que una repetición se responda **con el resultado** en vez
    /// de con una negativa: el colapso ACI de observaciones viejas puede
    /// haber borrado del contexto el resultado original, y entonces
    /// negarse deja al modelo pidiendo algo que el propio harness le quitó
    /// (visto en vivo contra roam, 2026-07-26 — el modelo gastó 4
    /// llamadas y abandonó el turno).
    pub(super) seen_calls: HashMap<(String, String), Option<String>>,
    /// Every `tool_use` id already in this session's history plus every
    /// one minted so far this turn — see `ensure_unique_tool_call_id` and
    /// N-14, docs/AUDITORIA-2026-07-v2.md.
    pub(super) known_tool_call_ids: HashSet<String>,
    /// Cuántas veces se leyó cada ruta en ESTE turno sin haberla
    /// editado después — la palanca de "relectura improductiva"
    /// (incidente roam #5/#6, 2026-07-20). El guard de llamadas
    /// repetidas solo corta argumentos IDÉNTICOS; en producción un
    /// modelo chico esquivó ese corte variando (offset, limit) y
    /// releyó el mismo archivo de 103 líneas 5-10 veces en ventanas
    /// solapadas, sin editar, hasta agotar el cap del turno. Leer un
    /// archivo grande por trozos es legítimo, así que esto NO bloquea:
    /// anexa una nota accionable al resultado a partir del umbral.
    pub(super) reads_by_path: HashMap<String, u32>,
    /// Fallos de `edit_file` por ruta en ESTE turno, sin un `edit_file`
    /// exitoso posterior — el estado del interlock duro de `write_file`
    /// (v9 L-10). La rama de daño que cierra: un modelo que no puede
    /// REPRODUCIR el contenido de un archivo (hallazgo 2026-07-28:
    /// caracteres que entiende y no puede emitir — `U+1D62`, `≈`,
    /// comillas anidadas) falla `edit_file` repetidamente y cae a
    /// reescribir el archivo entero con `write_file`, donde la misma
    /// incapacidad corrompe en silencio TODO el archivo en vez de
    /// fallar la edición. La guarda de tamaño de `write_file` (28-jul)
    /// desincentiva esa rama; esto la cierra. Un `edit_file` exitoso
    /// sobre la ruta resetea su contador (el modelo recuperó la
    /// capacidad de editar dirigido).
    pub(super) edit_failures_by_path: HashMap<String, u32>,
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
        self.turn_did_edit
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.turn_attempted_edit
            .store(false, std::sync::atomic::Ordering::Relaxed);

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
        // Idem para los AGENTS.md descubiertos JIT en turnos previos: sus
        // bodies vuelven al system prompt de este turno resumido.
        self.rehydrate_agents_md_from_log(&existing_events);

        // D′: `$skill` mentions resolve before anything else this turn —
        // the study's point is loading the guidance BEFORE the executor's
        // first mistake, not after.
        self.load_mentioned_skills(session, user_input, observer)
            .await?;

        // SC-retention (docs/hypothesis-2026-08-13-sc-retention.md):
        // declared constraints enter the log BEFORE this turn's
        // `UserMessage` — the natural position (rules are stated up
        // front), and the position the route must survive: everything
        // this old dies by the digest tail-cap without it. Idempotent
        // against the log so `--resume` / multi-turn sessions don't
        // re-append the same declaration.
        for constraint in &self.session_constraints {
            let already_declared = existing_events.iter().any(|event| {
                matches!(
                    event,
                    AgentEvent::SessionConstraintDeclared { text } if text == constraint
                )
            });
            if !already_declared {
                self.append_and_notify(
                    session,
                    &AgentEvent::SessionConstraintDeclared {
                        text: constraint.clone(),
                    },
                    observer,
                )
                .await?;
            }
        }

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

        // round-economics: el reloj del turno arranca ANTES de la ronda de
        // planificación, no antes del loop. La ronda del planner no cuenta
        // contra `max_turn_iterations`, pero sí es una ronda de modelo y
        // sí cuesta tiempo — el proyecto ya decidió que su costo pertenece
        // a la comparación (ver `TaskResult::rounds` en braze-bench, que la
        // cuenta). Arrancar el reloj después le regalaría al brazo con
        // planner una ronda gratis medida en el mismo eje que este
        // presupuesto corta.
        //
        // `Instant` (monotónico) y no hora de pared: un salto de NTP no
        // debe cortar un turno ni regalarle tiempo.
        let turn_started = std::time::Instant::now();

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
            seen_calls: HashMap::new(),
            known_tool_call_ids: Self::existing_tool_call_ids(&existing_events),
            reads_by_path: HashMap::new(),
            edit_failures_by_path: HashMap::new(),
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
        // Verification gate (H2): extra rounds this turn has spent letting
        // the model fix a verification failure, bounded by
        // `VerificationConfig::max_rounds`.
        let mut verify_rounds_used = 0usize;
        // v4 P0.2: cumulative input+output across this turn's rounds —
        // the quantity `max_turn_total_tokens` breaks on. Local to the
        // call, like `any_tool_calls_this_turn`.
        let mut turn_total_tokens: u64 = 0;
        // A′.2: the budget warning fires at most ONCE per turn — a
        // repeated "you're over 80%" every round is noise competing with
        // the prompt budget it's trying to protect. (The iteration-cap
        // note needs no flag: `round` hits its threshold exactly once.)
        let mut budget_note_emitted = false;
        let mut convergence_note_emitted = false;

        // Cuántas rondas vacías se le perdonan al modelo por turno antes
        // de caer a las rutas de fallback/fallo. Dos: la primera nota
        // puede llegar tarde si la ronda ya estaba en vuelo, la segunda
        // confirma que el modelo no va a salir del pozo solo.
        const MAX_EMPTY_ROUND_RETRIES: u32 = 2;
        let mut empty_round_retries: u32 = 0;

        for round in 0..self.max_turn_iterations {
            // round-economics (docs/hypothesis-2026-07-28-round-economics.md):
            // presupuesto de wall-clock del turno, chequeado en el borde de
            // la ronda como los otros dos cortes — una ronda que converge
            // dentro del presupuesto+ε debe retornar normal, y lo que se
            // evita es EMPEZAR una ronda más cuando el tiempo ya se acabó.
            //
            // Sin ronda de resumen, a propósito: ver
            // `EngineError::TurnWallClockExhausted`. El costo de esa ronda
            // extra escala con el precio de la ronda, que es exactamente el
            // factor que esta línea manipula.
            if let Some(budget) = self.max_turn_wall_clock {
                let elapsed = turn_started.elapsed();
                if elapsed > budget {
                    tracing::warn!(
                        round,
                        budget_ms = budget.as_millis(),
                        elapsed_ms = elapsed.as_millis(),
                        "turn blew its wall-clock budget; stopping at the round boundary"
                    );
                    self.consecutive_turns_without_tool_calls
                        .store(0, std::sync::atomic::Ordering::SeqCst);
                    return Err(EngineError::TurnWallClockExhausted {
                        budget_ms: budget.as_millis(),
                        elapsed_ms: elapsed.as_millis(),
                        rounds_completed: round,
                    });
                }
            }

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
            // SWE-Edit #17: el editor entra al inventario solo con el
            // lever — misma postura opt-in que explore.
            if self.editor_enabled {
                tool_stubs.push(crate::editor::editor_tool_stub());
            }
            // A/B del impuesto JSON (`crate::edit_fence`): en el brazo
            // fence, `edit_file` sale del inventario — la edición viaja
            // como SEARCH/REPLACE textual (addendum más abajo). La tool
            // sigue existiendo en el provider: las calls sintetizadas
            // por el parser (y las fugas de un modelo que la llame por
            // nombre memorizado — contaminación que el A/B mide, no
            // supone) despachan igual.
            if self.edit_fence_enabled {
                tool_stubs.retain(|s| s.name != "edit_file");
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
            let mut request_system_prompt = self.system_prompt_with_skills();
            if self.edit_fence_enabled {
                request_system_prompt.push_str(crate::edit_fence::EDIT_FENCE_ADDENDUM);
            }
            let req = CompletionRequest {
                messages: request_messages,
                tool_stubs: tool_stubs.clone(),
                system_prompt: request_system_prompt,
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
                fence_edits,
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
            // Incidente roam #8: se recuerda cuántos tokens generó la
            // ronda para que un `EmptyModelResponse` pueda distinguir
            // "el modelo no dijo nada" de "dijo algo que el harness no
            // supo mapear" (canal de razonamiento/commentary no
            // expuesto, o tool call que no parseó y se descartó).
            let round_output_tokens = usage.as_ref().map(|u| u.output_tokens).unwrap_or(0);
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

            // Mismo patrón que el rescue de arriba, para el canal fence
            // (A/B del impuesto JSON): la acción ya ocurrió en
            // `complete_once_with`; esto la deja contable para el bench.
            if fence_edits > 0 {
                self.append_and_notify(
                    session,
                    &AgentEvent::EditFenceApplied {
                        blocks: fence_edits as u32,
                    },
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
                    // Antes de decidir entre el fallback y el fallo duro:
                    // DECIRLE al modelo qué pasó y darle la ronda de vuelta.
                    //
                    // Una ronda vacía no siempre es el modelo rindiéndose;
                    // muchas veces gastó la ronda entera en un canal que
                    // este harness no expone (el `analysis` de Harmony) y
                    // cerró el turno sin emitir nada mapeable. Medido con
                    // gpt-oss:20b en la suite discriminante (2026-07-26):
                    // dos de las tres tareas cuyo resultado oscilaba entre
                    // corridas idénticas fallaban así, con rondas de ~12
                    // tokens — o sea era la fuente dominante del ruido de
                    // medición, y era harness, no capacidad.
                    //
                    // El modelo no puede corregir lo que no sabe: sin esta
                    // nota, su siguiente request es idéntico al anterior y
                    // repetir la misma ronda es lo esperable. Es la misma
                    // lógica del mensaje de reparación de schema, que el
                    // A/B del stencil mostró que absorbe fallos río abajo.
                    //
                    // Acotado a `MAX_EMPTY_ROUND_RETRIES` por turno: si el
                    // modelo insiste, se cae a las rutas de siempre. Sin la
                    // cota, un modelo que solo emite razonamiento quemaría
                    // el turno entero en reintentos.
                    if empty_round_retries < MAX_EMPTY_ROUND_RETRIES {
                        empty_round_retries += 1;
                        let text = format!(
                            "Your last round produced no visible output: it generated \
                             {round_output_tokens} tokens but none of them reached the \
                             conversation — no text and no tool call. If you were \
                             reasoning, that channel is not shown to the user and does \
                             not count as an answer. Reply now with either a tool call \
                             or the final text answer."
                        );
                        tracing::warn!(
                            round,
                            round_output_tokens,
                            attempt = empty_round_retries,
                            "round produced nothing mappable; nudging the model instead of \
                             ending the turn"
                        );
                        self.append_and_notify(
                            session,
                            &AgentEvent::HarnessNote {
                                kind: "empty_round".to_string(),
                                text: text.clone(),
                            },
                            observer,
                        )
                        .await?;
                        self.turn_harness_notes.lock().unwrap().push(text);
                        continue;
                    }
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
                                // Incidente roam #16 (2026-07-20, sesión
                                // multi-turno): que el fallback devuelva
                                // texto NO prueba que el turno logró algo.
                                // En la cascada multi-turno, el turno 2
                                // intentó dos ediciones (ambas fallaron),
                                // aterrizó cero, y el fallback emitió su
                                // planificación como respuesta ("We need
                                // to add mean_speed()…"). Terminar en Ok
                                // con ese texto hueco envenenó al turno 3,
                                // que fue a buscar lo que el 2 no creó.
                                //
                                // Discriminador (ver `turn_attempted_edit`):
                                // "intentó editar y no aterrizó nada" =
                                // hueco, se falla; "nunca intentó editar"
                                // = quizá un Q&A read-only legítimo cuya
                                // respuesta quedó en el canal de
                                // razonamiento — que es exactamente lo que
                                // este fallback existe para rescatar, así
                                // que se respeta. `!turn_did_edit` a secas
                                // rompería ese caso legítimo; por eso el
                                // guard exige `attempted && !did`.
                                let attempted = self
                                    .turn_attempted_edit
                                    .load(std::sync::atomic::Ordering::Relaxed);
                                let landed = self
                                    .turn_did_edit
                                    .load(std::sync::atomic::Ordering::Relaxed);
                                if attempted && !landed {
                                    tracing::warn!(
                                        round,
                                        "summary fallback produced text but this turn attempted \
                                         a file edit and landed none; the 'answer' is salvaged \
                                         reasoning — failing instead of ending Ok with a hollow \
                                         result that would poison the next turn"
                                    );
                                    return Err(EngineError::EmptyModelResponse {
                                        generated_tokens: round_output_tokens,
                                    });
                                }
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
                                // Incidente roam #13 (2026-07-20): esta
                                // rama existe porque U-1 tenía trabajo
                                // REAL en disco (`write_file` aplicado) y
                                // fallar el turno lo habría reportado como
                                // pérdida total. Pero la condición que la
                                // gatilla — "despachó al menos una tool
                                // call" — también la cumple un turno que
                                // solo leyó archivos y falló todos sus
                                // edits, que es lo observado: dos
                                // `edit_file` rechazados, cero mutaciones,
                                // y el turno cerrando en Ok con la pantalla
                                // en blanco. Al usuario le quedó "y ahí
                                // quedó": ni respuesta ni error.
                                //
                                // Haber despachado tools no es haber hecho
                                // algo. Sin una mutación exitosa no hay
                                // nada que preservar, y el error honesto
                                // —que además ya reporta los tokens
                                // generados (incidente #8)— es más útil
                                // que el silencio.
                                if !self
                                    .turn_did_edit
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                {
                                    tracing::warn!(
                                        round,
                                        "summary fallback returned empty and this turn never \
                                         landed a successful edit; surfacing the empty response \
                                         instead of ending silently with nothing to show"
                                    );
                                    return Err(EngineError::EmptyModelResponse {
                                        generated_tokens: round_output_tokens,
                                    });
                                }
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
                    return Err(EngineError::EmptyModelResponse {
                        generated_tokens: round_output_tokens,
                    });
                }
                // Final response: no further tool calls requested.
                self.append_and_notify(
                    session,
                    &AgentEvent::AssistantText { text: text_buffer },
                    observer,
                )
                .await?;

                // Verification gate (H2,
                // docs/verification-lever-design-2026-07-22.md): before
                // accepting this claimed-done turn, run the configured
                // verification command. On failure, inject the real output
                // as an observation and give the model another round —
                // moving verification from the model's discretion (which
                // finding #15 shows it fakes) to the harness's guarantee.
                // Only for turns that actually dispatched tool calls (a
                // pure Q&A turn has nothing to verify), and bounded by
                // `max_rounds`. A command that is missing or times out
                // skips silently (see `run_verification`) — never blocks a
                // legitimate turn.
                if let Some(v) = self
                    .verification
                    .as_ref()
                    .filter(|v| any_tool_calls_this_turn && verify_rounds_used < v.max_rounds)
                    && let Err(output) = run_verification(v).await
                {
                    tracing::warn!(
                        round,
                        verify_round = verify_rounds_used + 1,
                        "verification gate failed after the model claimed done; \
                         injecting the real output and granting another round"
                    );
                    self.append_and_notify(
                        session,
                        &AgentEvent::VerificationFailed { output },
                        observer,
                    )
                    .await?;
                    verify_rounds_used += 1;
                    continue;
                }
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
                // Incidente roam #5 (2026-07-20, quinta sesión en
                // producción): la nota de abajo avisa con UNA ronda de
                // margen, y eso no es "antes de quedarse sin espacio" —
                // es el borde mismo. Observado: gpt-oss:20b rompió un
                // archivo con un edit, gastó 17 rondas releyéndolo en
                // ventanas solapadas, recibió el aviso en la ronda 19 de
                // 20 y la usó en un grep repetido. Un modelo chico
                // necesita margen REAL para cambiar de estrategia, así
                // que se avisa además a los ~70% del cap: no "es tu
                // última ronda" (que invita a rendirse) sino "vas a la
                // mitad larga, converge". Dos notas, dos funciones
                // distintas.
                if self.max_turn_iterations >= 6
                    && !convergence_note_emitted
                    && (round + 1).saturating_mul(10) >= self.max_turn_iterations.saturating_mul(7)
                    && round + 2 < self.max_turn_iterations
                {
                    convergence_note_emitted = true;
                    // Incidente roam #10 (2026-07-20, tarea 2 del
                    // testbed): el consejo "si algo está roto, arréglalo
                    // con UN edit decisivo" es correcto para un turno que
                    // aún no tocó nada, y contraproducente para uno que ya
                    // editó con éxito. Observado: el modelo terminó los
                    // edits en la ronda 5, un `edit_file` posterior falló
                    // con `old_string not found` (porque el cambio YA
                    // estaba aplicado), y la nota de la ronda 14 lo empujó
                    // a seguir "arreglando" — tres relecturas de lib.rs y
                    // un choque con el guard de duplicados antes de cerrar.
                    // Con edición previa el consejo correcto es el
                    // opuesto: verifica una vez y responde.
                    let text = if self
                        .turn_did_edit
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        format!(
                            "You are at round {} of this turn's {} (past 70%), and you have \
                             already applied at least one successful edit. Converge now: run \
                             your check (tests/build) once if you have not, then answer with \
                             what you have. Do not re-read files you already edited to \
                             reassure yourself — if an edit failed with 'old_string not \
                             found', the likeliest reason is that the change is already in \
                             place.",
                            round + 1,
                            self.max_turn_iterations
                        )
                    } else {
                        format!(
                            "You are at round {} of this turn's {} (past 70%). Converge now: \
                             prefer finishing with what you have over further exploration, \
                             and if something is broken, fix it with one decisive edit \
                             rather than re-reading.",
                            round + 1,
                            self.max_turn_iterations
                        )
                    };
                    self.append_and_notify(
                        session,
                        &AgentEvent::HarnessNote {
                            kind: "iteration_converge".to_string(),
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
        let agents_md = self.loaded_agents_md_bodies.lock().unwrap();
        if loaded.is_empty() && agents_md.is_empty() {
            return self.system_prompt.clone();
        }
        let mut prompt = self.system_prompt.clone();
        for skill in loaded.iter() {
            prompt.push_str(&skill.prompt_addendum());
        }
        // Carga JIT de AGENTS.md por subdirectorio
        // (docs/agents-md-jit-design-2026-08-11.md): los bodies
        // descubiertos se anexan en orden, bajo el mismo header que el
        // raíz usa en `braze_config::default_system_prompt`, para que el
        // modelo los lea como instrucciones del proyecto (que es lo que
        // son). El raíz NO está acá — vive en `self.system_prompt`.
        for body in agents_md.iter() {
            prompt.push_str(
                "\n\n## Project instructions (AGENTS.md, versioned in this repository):\n\n",
            );
            prompt.push_str(body);
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

    /// El espejo de [`Self::rehydrate_skills_from_log`] para la carga JIT
    /// de AGENTS.md (docs/agents-md-jit-design-2026-08-11.md): al resumir,
    /// re-siembra el set de dedup y recarga los bodies de cada
    /// `AgentsMdLoaded` del log, en orden. Persiste nada (el log ya los
    /// registra; re-emitir duplicaría el conteo). Un archivo que
    /// desapareció desde entonces degrada a warn y se sigue sin ese
    /// addendum — misma postura que el rehidratado de skills. No-op si la
    /// feature está apagada (`agents_md_root` None) o si un descubrimiento
    /// ya está en memoria (idempotente ante `/model`-rebuild).
    fn rehydrate_agents_md_from_log(&self, events: &[AgentEvent]) {
        if self.agents_md_root.is_none() {
            return;
        }
        for event in events {
            let AgentEvent::AgentsMdLoaded { path } = event else {
                continue;
            };
            let path = std::path::PathBuf::from(path);
            {
                let mut loaded = self.loaded_agents_md.lock().unwrap();
                if loaded.contains(&path) {
                    continue;
                }
                loaded.insert(path.clone());
            }
            let Some(dir) = path.parent() else { continue };
            match braze_config::load_agents_md_from(dir) {
                Some(body) => {
                    tracing::info!(path = %path.display(), "AGENTS.md JIT rehidratado del log");
                    self.loaded_agents_md_bodies.lock().unwrap().push(body);
                }
                None => {
                    tracing::warn!(
                        path = %path.display(),
                        "AGENTS.md registrado en el log ya no es legible — se sigue sin él"
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
    /// Invocación *call-time* (Recuris § 2.2.2 — ver
    /// [`Engine::with_call_time_skills`]): carga la skill declarada como
    /// guía de `tool`, si hay una y no está ya cargada.
    ///
    /// `Ok(Some(nombre))` = se cargó, y el caller **debe abortar la
    /// ejecución** de la call para que el modelo la re-emita con la guía
    /// delante. `Ok(None)` = seguir normalmente, y cubre los cuatro casos
    /// en que interceptar sería peor que no hacerlo: la palanca apagada,
    /// no hay skill para esa tool, la skill ya está cargada (así la
    /// segunda llamada a la misma herramienta sí ejecuta y no hay bucle),
    /// y el body dejó de ser legible.
    ///
    /// El cap por turno también degrada a `None` en vez de trabar la
    /// herramienta: pasado el cap, la acción se ejecuta sin guía, que es
    /// exactamente lo que pasaba antes de esta palanca.
    pub(super) async fn load_call_time_skill(
        &self,
        session: &SessionId,
        tool: &str,
        observer: &mut dyn TurnObserver,
    ) -> Result<Option<String>, EngineError> {
        if !self.call_time_skills_enabled {
            return Ok(None);
        }
        let Some(registry) = &self.skill_registry else {
            return Ok(None);
        };
        let Some(stub) = registry.for_tool(tool) else {
            return Ok(None);
        };
        let name = stub.name.clone();
        if self
            .loaded_skills
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.name == name)
        {
            return Ok(None);
        }
        if self.loaded_skills.lock().unwrap().len() >= self.skills_max_loaded_per_turn {
            self.append_and_notify(
                session,
                &AgentEvent::SkillLoadSkipped {
                    name,
                    reason: format!("per-turn cap ({}) reached", self.skills_max_loaded_per_turn),
                },
                observer,
            )
            .await?;
            return Ok(None);
        }
        match registry.load_body(&name, self.skills_max_body_tokens) {
            Some(loaded) => {
                tracing::info!(
                    skill = %loaded.name,
                    tool,
                    estimated_tokens = loaded.estimated_tokens,
                    truncated = loaded.truncated,
                    "skill loaded at call time; the drafted call was not executed"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::SkillLoaded {
                        name: loaded.name.clone(),
                        // El `trigger` distingue este camino del explícito
                        // en el rollout log, así que el A/B puede contar
                        // cuántas cargas fueron call-time sin instrumentar
                        // nada más.
                        trigger: "call_time".to_string(),
                        estimated_tokens: loaded.estimated_tokens,
                        truncated: loaded.truncated,
                    },
                    observer,
                )
                .await?;
                self.loaded_skills.lock().unwrap().push(loaded);
                Ok(Some(name))
            }
            None => {
                // Un body ilegible NO puede trabar la herramienta: sin
                // este `None` la call se interceptaría en cada ronda sin
                // que la skill llegue nunca, y el turno no avanzaría.
                tracing::warn!(
                    skill = %name,
                    tool,
                    "call-time skill is no longer loadable — executing the call without it"
                );
                Ok(None)
            }
        }
    }

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

/// Runs the verification gate's command (H2,
/// docs/verification-lever-design-2026-07-22.md). `Ok(())` = verified
/// (exit 0), or a "skip" case that must never block a legitimate turn:
/// the binary is missing, or the command times out (both trace-level,
/// same failure posture as the post-edit check). `Err(output)` = the
/// command ran and FAILED (exit ≠ 0); the string is its combined
/// stdout+stderr, length-capped, ready to inject as an observation.
async fn run_verification(config: &VerificationConfig) -> Result<(), String> {
    /// Cap on injected verifier output — enough for the first failures,
    /// not an unbounded dump that would blow up the tactical window.
    const OUTPUT_CAP: usize = 2000;

    let Some((program, args)) = config.command.split_first() else {
        return Ok(()); // empty command = nothing to verify
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // Run where the model's edits landed — the process cwd interactively,
    // an explicit sandbox dir in the bench (its tasks are NOT the process
    // cwd, so without this the command would verify the wrong tree).
    if let Some(dir) = &config.working_dir {
        cmd.current_dir(dir);
    }

    let run = cmd.output();
    let output = match tokio::time::timeout(config.timeout, run).await {
        Ok(Ok(output)) => output,
        // Timed out: skip (don't block the turn), like post-edit check.
        Err(_elapsed) => {
            tracing::warn!(command = ?config.command, "verification command timed out — skipping gate");
            return Ok(());
        }
        // Couldn't launch (missing binary, permission): skip.
        Ok(Err(e)) => {
            tracing::warn!(command = ?config.command, error = %e, "verification command failed to launch — skipping gate");
            return Ok(());
        }
    };

    if output.status.success() {
        return Ok(());
    }

    // Failed: combine stdout+stderr (tests print to both), cap, return.
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let combined = combined.trim();
    let capped = if combined.len() > OUTPUT_CAP {
        // Keep the TAIL — a test runner's verdict ("test result: FAILED")
        // and the failing assertions are at the end, not the top.
        let start = combined.len() - OUTPUT_CAP;
        let start = (start..combined.len())
            .find(|i| combined.is_char_boundary(*i))
            .unwrap_or(combined.len());
        format!(
            "[…output truncated to last {OUTPUT_CAP} chars…]\n{}",
            &combined[start..]
        )
    } else {
        combined.to_string()
    };
    Err(capped)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    use braze_events::{AgentEvent, NoopObserver};
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SessionStore, SimpleContextCompactor};
    use braze_tools_core::ToolRegistry;
    use braze_types::SessionId;

    use super::VerificationConfig;
    use crate::engine::Engine;
    use crate::engine::test_support::*;
    // P1.1 resto (v9 L-5): imports que el cluster movido de engine/mod.rs
    // trae consigo — el modelo helper del test de la ronda de resumen
    // implementa ModelBackend a mano (Pin/Stream/async_trait/AsyncMutex),
    // y los tests del ciclo de vida matchean EngineError y variantes.
    use super::MAX_TURN_ITERATIONS;
    use crate::EngineError;
    use async_trait::async_trait;
    use braze_events::TextDeltaObserver;
    use braze_model::{CompletionRequest, ModelBackend, ModelError};
    use braze_types::{ContentBlock, ToolResult};
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::atomic::Ordering;
    use tokio::sync::Mutex as AsyncMutex;

    fn tool_call_then_done(name: &str) -> Vec<CompletionEvent> {
        vec![
            CompletionEvent::ToolCallRequested {
                id: "call-1".to_string(),
                name: name.to_string(),
                arguments: serde_json::json!({ "text": "x" }),
            },
            CompletionEvent::Done,
        ]
    }

    fn final_text(text: &str) -> Vec<CompletionEvent> {
        vec![
            CompletionEvent::TextDelta(text.to_string()),
            CompletionEvent::Done,
        ]
    }

    fn verify(command: &[&str], max_rounds: usize) -> VerificationConfig {
        VerificationConfig {
            command: command.iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_secs(10),
            max_rounds,
            working_dir: None,
        }
    }

    /// The gate fires: the model claims done after a tool-call turn, the
    /// verification command FAILS, so a `VerificationFailed` observation is
    /// injected and the model gets another round — its second final answer
    /// is what ends the turn (max_rounds=1 stops the loop).
    #[tokio::test]
    async fn a_failing_verification_injects_an_observation_and_grants_another_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let model = ScriptedModel::new(vec![
            tool_call_then_done("echo"),
            final_text("all tests pass"), // the confabulated claim
            final_text("ok now really fixed"),
        ]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_verification(verify(&["sh", "-c", "echo boom >&2; exit 1"], 1));

        engine
            .run_turn(&session, "haz la tarea", &mut NoopObserver)
            .await
            .expect("turn should end (unverified) after exhausting verify rounds");

        let events = FileSessionStore::new(dir.clone())
            .load(&session)
            .await
            .expect("load");
        let vf: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::VerificationFailed { output } => Some(output.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(vf.len(), 1, "gate should fire exactly once (max_rounds=1)");
        assert!(
            vf[0].contains("boom"),
            "injected output carries the real verifier output"
        );
        // The model's SECOND answer (post-injection round) is persisted —
        // proof it got the extra round the gate granted.
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::AssistantText { text } if text.contains("really fixed"))
            ),
            "the post-verification round should have run"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// SC-retention (docs/hypothesis-2026-08-13-sc-retention.md): a
    /// configured session constraint is persisted as
    /// `SessionConstraintDeclared` BEFORE the first turn's `UserMessage`
    /// (rules are stated up front — the position the durable route must
    /// survive), and idempotently: a second turn on the same session
    /// does NOT re-append it.
    #[tokio::test]
    async fn a_session_constraint_is_declared_before_the_first_user_message_and_only_once() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let model = ScriptedModel::new(vec![final_text("listo"), final_text("listo de nuevo")]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_session_constraints(vec!["no borres config/produccion.toml".to_string()]);

        engine
            .run_turn(&session, "primera tarea", &mut NoopObserver)
            .await
            .expect("first turn");
        engine
            .run_turn(&session, "segunda tarea", &mut NoopObserver)
            .await
            .expect("second turn");

        let events = FileSessionStore::new(dir.clone())
            .load(&session)
            .await
            .expect("load");

        let declared: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                matches!(e, AgentEvent::SessionConstraintDeclared { text }
                    if text == "no borres config/produccion.toml")
                .then_some(i)
            })
            .collect();
        assert_eq!(
            declared.len(),
            1,
            "the declaration must be idempotent across turns: {events:#?}"
        );
        let first_user = events
            .iter()
            .position(|e| matches!(e, AgentEvent::UserMessage { .. }))
            .expect("a UserMessage must exist");
        assert!(
            declared[0] < first_user,
            "the constraint must precede the first UserMessage \
             (declared at {}, first user at {first_user})",
            declared[0]
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The gate passes: verification command succeeds, so no observation is
    /// injected and the turn ends on the model's first final answer.
    #[tokio::test]
    async fn a_passing_verification_does_not_inject_or_add_a_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let model = ScriptedModel::new(vec![tool_call_then_done("echo"), final_text("done")]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_verification(verify(&["true"], 2));

        engine
            .run_turn(&session, "haz la tarea", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let events = FileSessionStore::new(dir.clone())
            .load(&session)
            .await
            .expect("load");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::VerificationFailed { .. })),
            "a passing verification must not inject a failure"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// No verification configured = byte-identical to before: a tool-call
    /// turn ending in text just ends, no gate, no extra rounds.
    #[tokio::test]
    async fn without_verification_the_gate_is_inert() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let model = ScriptedModel::new(vec![tool_call_then_done("echo"), final_text("done")]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );
        engine
            .run_turn(&session, "haz la tarea", &mut NoopObserver)
            .await
            .expect("turn should succeed");
        let events = FileSessionStore::new(dir.clone())
            .load(&session)
            .await
            .expect("load");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::VerificationFailed { .. }))
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // P1.1 resto (v9 L-5): cluster run_turn_*/summary-round movido
    // VERBATIM del mod tests de engine/mod.rs (fixtures compartidas en
    // engine/test_support.rs) — ciclo de vida del turno: errores de
    // stream, rondas vacías y de resumen, observer, D5, presupuestos de
    // tokens y wall-clock.

    /// Regression test for A3/B4: a stream that fails mid-round (after
    /// delivering partial text) must propagate as an error from
    /// `run_turn`, and the partial text must never be persisted as if it
    /// were a complete `AssistantText` response.
    #[tokio::test]
    async fn a_mid_stream_error_propagates_and_does_not_persist_partial_text() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let engine = Engine::new(
            Box::new(ErroringModel),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::Model(_))),
            "expected the stream error to propagate as EngineError::Model, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "the partial text from the failed round must never be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-17 (docs/AUDITORIA-2026-07-v2.md): a second
    /// `run_turn` call on the same `Engine` while a first one is still in
    /// flight must be rejected explicitly instead of racing it over the
    /// shared `TaskNotifier` completion channel.
    #[tokio::test]
    async fn a_second_concurrent_run_turn_is_rejected_while_the_first_is_in_flight() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = SlowModel {
            delay: Duration::from_millis(200),
            round: vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        };

        let engine = Arc::new(Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        ));

        let engine_a = Arc::clone(&engine);
        let first = tokio::spawn(async move {
            let mut observer = NoopObserver;
            engine_a.run_turn(&session, "hola", &mut observer).await
        });

        // Give the first call time to acquire the guard before the second
        // one starts.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut observer_b = NoopObserver;
        let result_b = engine
            .run_turn(&session, "hola de nuevo", &mut observer_b)
            .await;
        assert!(
            matches!(result_b, Err(EngineError::ConcurrentTurn)),
            "expected the second call to be rejected, got {result_b:?}"
        );

        let result_a = first.await.expect("first turn's task should not panic");
        assert!(result_a.is_ok(), "the first turn should still succeed");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-24 (docs/AUDITORIA-2026-07-v2.md): a round
    /// with no tool calls whose `stop_reason` reports token-budget
    /// truncation must not be persisted as a normal converged answer.
    #[tokio::test]
    async fn a_truncated_final_response_is_reported_as_an_error_not_a_silent_success() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("partial answer cut off mid".to_string()),
            CompletionEvent::Usage {
                input_tokens: 10,
                output_tokens: 100,
                stop_reason: Some("max_tokens".to_string()),
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
        assert!(
            matches!(result, Err(EngineError::TruncatedFinalResponse)),
            "expected a truncated final response to be reported as an error, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "the truncated text must never be persisted as a normal final answer"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Una ronda vacía no siempre es el modelo rindiéndose: muchas veces
    /// gastó la ronda entera en un canal que el harness no expone (el
    /// `analysis` de Harmony) y cerró sin emitir nada mapeable. Medido con
    /// gpt-oss:20b en la suite discriminante (2026-07-26), era la fuente
    /// DOMINANTE del ruido de medición — dos de las tres tareas cuyo
    /// resultado oscilaba entre corridas idénticas fallaban así.
    ///
    /// El modelo no puede corregir lo que no sabe: sin la nota, su
    /// siguiente request es idéntico y repetir la ronda es lo esperable.
    #[tokio::test]
    async fn una_ronda_vacia_se_nudgea_y_el_modelo_puede_recuperarse() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Primera ronda: nada mapeable (todo se fue al canal oculto).
            vec![CompletionEvent::Done],
            // Tras el nudge, el modelo responde de verdad.
            vec![
                CompletionEvent::TextDelta("ahora sí, listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);

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
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("el turno debe recuperarse tras el nudge, no morir en la ronda vacía");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        // La nota se persiste, para que el turno sea auditable: sin ella,
        // el log mostraría una ronda que desaparece sin explicación.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::HarnessNote { kind, .. } if kind == "empty_round"
            )),
            "debe quedar registro de por qué se repitió la ronda"
        );
        // Y el turno termina con la respuesta real del modelo.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("ahora sí")
            )),
            "la respuesta posterior al nudge debe ser la que cierra el turno"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for the "una completion vacía termina el turno
    /// como éxito silencioso" bajo (docs/AUDITORIA-2026-07-v2.md): a
    /// round with no text and no tool calls at all must not be treated as
    /// a legitimate, silent convergence.
    #[tokio::test]
    async fn an_empty_completion_is_reported_as_an_error_not_a_silent_success() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![CompletionEvent::Done]]);

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
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "expected an empty completion to be reported as an error, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for U-1 (docs/usability-log-template.md, hallado en
    /// vivo 2026-07-07 contra qwen3.5-coder/Nitro): a turn that already
    /// dispatched a successful tool call, then gets a completely empty
    /// round (no text, no tool calls) — the exact shape a real session
    /// hit right after `write_file` succeeded — must recover via the
    /// tools-free summary round instead of discarding that already-done
    /// work behind a hard `EmptyModelResponse` error.
    #[tokio::test]
    async fn an_empty_round_after_a_dispatched_tool_call_recovers_via_the_summary_round() {
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
            // The round right after the tool call comes back with
            // nothing at all — no text, no further tool calls. Ahora el
            // engine responde con un nudge y le devuelve la ronda hasta
            // MAX_EMPTY_ROUND_RETRIES veces; este modelo insiste, que es
            // lo que lleva a la ruta de fallback que el test verifica.
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            // The tools-free summary attempt — reports Usage like any
            // real model round would (H-4: it used to be dropped).
            vec![
                CompletionEvent::TextDelta("listo, ya lo hice".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 900,
                    output_tokens: 15,
                    stop_reason: Some("end_turn".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
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

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected the turn to recover via the summary round, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "listo, ya lo hice"
            )),
            "expected the summary round's text to be persisted, got: {events:?}"
        );
        // The already-dispatched tool call's own result must still be
        // there too — this is the actual work the original bug threw away.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        );
        // H-3 (docs/AUDITORIA-2026-07-v5.md): the fallback being *reached
        // for* is now persisted, independent of whether it went on to
        // produce usable text.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "expected SummaryFallbackAttempted to be persisted, got: {events:?}"
        );
        // H-4 (docs/AUDITORIA-2026-07-v5.md): the fallback's own Usage is
        // persisted too — it re-sends the whole history as its prompt, so
        // dropping it under-reported cost exactly on degraded turns. The
        // fallback round is the only scripted round that reports Usage in
        // this test, so exactly one must land, with its numbers.
        let usage_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => Some((*input_tokens, *output_tokens)),
                _ => None,
            })
            .collect();
        assert_eq!(
            usage_events,
            vec![(900, 15)],
            "the summary fallback's Usage must be persisted, got: {events:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Same shape as the recovery test above, but the tools-free summary
    /// attempt *also* comes back empty. This used to surface
    /// `EmptyModelResponse` — until the memory-distillation smoke
    /// (2026-07-16, gpt-oss:20b/Nitro) showed reasoning models can close
    /// both the round AND the fallback with an empty content channel
    /// while the actual fix is already on disk. The turn must now end Ok
    /// with the tool results preserved and NO fabricated final answer —
    /// while a fallback whose *call itself* dies keeps failing (next
    /// test), so real backend errors don't hide behind this tolerance.
    ///
    /// El tool call es `write_file`, no `echo` (incidente roam #13): la
    /// tolerancia se justifica por trabajo REAL en disco, y con un
    /// stand-in no-mutante el test afirmaba algo más amplio de lo que el
    /// caso motivador sostiene — precisamente el hueco por el que un
    /// turno sin una sola mutación exitosa terminaba en Ok silencioso.
    #[tokio::test]
    async fn an_empty_summary_round_after_a_dispatched_tool_call_ends_the_turn_without_failing() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            // The tools-free summary attempt is ALSO empty.
            vec![CompletionEvent::Done],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected the turn to end Ok with its tool results preserved, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        // The real work the old hard failure threw away must be there.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. })),
            "expected the dispatched tool call's result to be persisted, got: {events:?}"
        );
        // The fallback attempt stays on record (H-3) …
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "expected SummaryFallbackAttempted to be persisted, got: {events:?}"
        );
        // … but nothing may be invented as a final answer: tolerating the
        // empty summary must not fabricate an `AssistantText`.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { .. })),
            "an empty summary must not persist a fabricated final answer, got: {events:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Incidente roam #13 (2026-07-20): el reverso del test de arriba.
    /// Un turno que despachó tool calls pero nunca aterrizó una mutación
    /// —observado en vivo: sólo lecturas, dos `edit_file` rechazados—
    /// no tiene nada que preservar. Terminar en `Ok` le dejaba al usuario
    /// una pantalla en blanco: ni respuesta ni error. Ahora sale el
    /// error honesto, que además reporta los tokens generados.
    #[tokio::test]
    async fn an_empty_summary_round_without_any_successful_edit_fails_instead_of_ending_silently() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    // Sólo lectura: el turno "hizo cosas" sin cambiar nada.
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "a turn that changed nothing and said nothing must not report success, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Incidente roam #16 (2026-07-20): el caso que el fix de #13 NO
    /// cubría. El turno intenta una edición que FALLA, aterriza cero, y
    /// el fallback de resumen devuelve texto NO-vacío (razonamiento
    /// disfrazado de respuesta). #13 solo fallaba con resumen vacío; con
    /// texto, el turno terminaba en Ok con un resultado hueco que
    /// envenenaba al siguiente. Ahora `attempted && !landed` lo falla.
    #[tokio::test]
    async fn a_nonempty_summary_after_a_failed_edit_and_no_landed_edit_fails() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Ronda 0: intenta write_file (el provider lo devuelve is_error).
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            // Ronda 1: sin texto ni tool calls → gatilla el fallback.
            // El engine ahora nudgea y devuelve la ronda hasta
            // MAX_EMPTY_ROUND_RETRIES veces; el modelo insiste.
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            vec![CompletionEvent::Done],
            // Fallback de resumen: texto NO-vacío (el razonamiento hueco).
            vec![
                CompletionEvent::TextDelta(
                    "We need to add the method. Let's insert it before the tests.".to_string(),
                ),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(FailingWriteToolProvider)]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "a turn that attempted an edit, landed none, and only produced salvaged \
             reasoning must fail, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El contrapeso de regresión de #16: un turno READ-ONLY legítimo que
    /// nunca intenta editar (solo lee) y cuya respuesta llega vía el
    /// fallback de resumen DEBE seguir terminando en Ok. Es exactamente
    /// el caso que un `!turn_did_edit` a secas habría roto — de ahí que
    /// el guard exija `attempted && !landed`, no solo `!landed`.
    #[tokio::test]
    async fn a_nonempty_summary_after_a_read_only_turn_still_ends_ok() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            // Ronda 0: solo lectura — nunca intenta editar.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            // Fallback: una respuesta real a una pregunta read-only.
            vec![
                CompletionEvent::TextDelta(
                    "The function computes the haversine distance in meters.".to_string(),
                ),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "a read-only turn that answered via the summary fallback must not be failed \
             by the #16 guard, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regresión de U-1: un turno que SÍ aterrizó una edición y luego
    /// cierra vía fallback de resumen con texto sigue en Ok — el guard de
    /// #16 exige que NO haya aterrizado ninguna edición, así que un edit
    /// exitoso lo desactiva por completo.
    #[tokio::test]
    async fn a_nonempty_summary_after_a_successful_edit_still_ends_ok() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "a.rs" }),
                },
                CompletionEvent::Done,
            ],
            vec![CompletionEvent::Done],
            vec![
                CompletionEvent::TextDelta("Added the method and it compiles.".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            // ReadWriteToolProvider's write_file succeeds → turn_did_edit.
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "a turn with a successful edit must stay Ok even if it closed via the summary \
             fallback, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A `ModelBackend` scripted per-call like `ScriptedModel`, except its
    /// N-th call (0-indexed) fails outright at the request level — used to
    /// prove the empty-summary tolerance above does NOT extend to a
    /// fallback whose own model call dies: that shape may be a real
    /// backend failure (auth, network, rate limit) and must keep
    /// surfacing as an error.
    struct ScriptedModelFailingOnCall {
        rounds: AsyncMutex<std::collections::VecDeque<Vec<CompletionEvent>>>,
        fail_on_attempt: u32,
        calls: AtomicU32,
    }

    #[async_trait]
    impl ModelBackend for ScriptedModelFailingOnCall {
        fn name(&self) -> &str {
            "scripted-failing-on-call"
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            if attempt == self.fail_on_attempt {
                return Err(ModelError::Request(
                    "simulated backend failure on the summary fallback".to_string(),
                ));
            }
            let mut rounds = self.rounds.lock().await;
            let round = rounds
                .pop_front()
                .unwrap_or_else(|| vec![CompletionEvent::Done]);
            Ok(Box::pin(futures::stream::iter(round.into_iter().map(Ok))))
        }
    }

    /// Same shape again, but the summary fallback's model call itself
    /// fails instead of returning empty — the turn must still surface
    /// `EmptyModelResponse`, not ride the empty-summary tolerance.
    #[tokio::test]
    async fn an_empty_round_still_fails_if_the_summary_rounds_call_itself_dies() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModelFailingOnCall {
            rounds: AsyncMutex::new(
                vec![
                    vec![
                        CompletionEvent::ToolCallRequested {
                            id: "call-1".to_string(),
                            name: "echo".to_string(),
                            arguments: serde_json::json!({ "text": "hola" }),
                        },
                        CompletionEvent::Done,
                    ],
                    // El engine ahora nudgea y devuelve la ronda hasta
                    // MAX_EMPTY_ROUND_RETRIES veces; el modelo insiste.
                    vec![CompletionEvent::Done],
                    vec![CompletionEvent::Done],
                    vec![CompletionEvent::Done],
                ]
                .into_iter()
                .collect(),
            ),
            // Call 0: ronda con tool call. Call 1: ronda vacia. Calls 2 y
            // 3: los dos reintentos con nudge, que el modelo tambien
            // devuelve vacios. Call 4: el summary fallback — muere a nivel
            // de request, que es lo que este test verifica.
            fail_on_attempt: 4,
            calls: AtomicU32::new(0),
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
        );

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::EmptyModelResponse { .. })),
            "expected a dead fallback call to keep surfacing as an error, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
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
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
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
                stop_reason: Some("end_turn".to_string()),
                cache_read_tokens: Some(30),
                cache_write_tokens: Some(5),
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

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
        match &events[1] {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                assert_eq!(*input_tokens, 42);
                assert_eq!(*output_tokens, 7);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(*cache_read_tokens, Some(30));
                assert_eq!(*cache_write_tokens, Some(5));
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
            .run_turn(
                &session,
                "please echo hi",
                &mut TextDeltaObserver(|chunk: &str| streamed.push_str(chunk)),
            )
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

    /// Smoke test for `ProtocolValidatingModel`: the exact same scenario as
    /// `run_turn_with_a_tool_call_round_trips_end_to_end` above, but with
    /// the model wrapped in the validator — proves the harness itself is
    /// usable (doesn't false-positive on a normal, well-formed exchange)
    /// before it's relied on by the regression tests below.
    #[tokio::test]
    async fn run_turn_with_a_tool_call_passes_protocol_validation() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![
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
        ]));

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
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed and every request should pass protocol validation");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-4.
    ///
    /// `load_messages_repairs_an_orphaned_tool_use_with_no_result` (below)
    /// proves the repair *happens*, but it seeds the orphan and calls
    /// `load_messages` directly with nothing in between — so the repaired
    /// `ToolCallCompleted` lands right after the orphaned `AssistantToolCall`
    /// and the sequence is trivially valid. That's not what actually
    /// happens in production: `run_turn` appends the turn's new
    /// `UserMessage` *before* calling `load_messages` (see `run_turn`'s
    /// first few lines), so the repair — which only runs inside
    /// `load_messages` — ends up appended *after* that `UserMessage`
    /// instead of immediately after the tool_use it repairs. The resulting
    /// log order (`tool_use`, unrelated `user` text, `tool_result`) is
    /// exactly what `ProtocolValidatingModel` is built to catch, and it
    /// persists to disk — every future resume repeats it.
    ///
    /// Fixed: `run_turn` now calls `repair_session` (which repairs and
    /// persists, discarding the loaded events) *before* appending the
    /// turn's `UserMessage` — see `Engine::run_turn`/`Engine::repair_session`.
    #[tokio::test]
    async fn resuming_after_a_crash_with_an_orphaned_tool_call_stays_protocol_valid() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Simulate a process that crashed between persisting the tool_use
        // and receiving its result: an `AssistantToolCall` with no
        // matching `ToolCallCompleted` anywhere in the log yet.
        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "please echo hi".to_string(),
                },
            )
            .await
            .expect("seed the original user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
            )
            .await
            .expect("seed an orphaned tool_use — process 'crashed' right here");

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("done".to_string()),
            CompletionEvent::Done,
        ]]));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        // Resuming the session and sending a new message must repair the
        // orphan into a protocol-valid sequence — `ProtocolValidatingModel`
        // panics inside `complete()` if it instead produces the
        // tool_use/user-text/tool_result order the bug creates.
        engine
            .run_turn(&session, "are you still there?", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Variant name only — the mirror test cares about kind and order,
    /// not payload equality.
    fn event_kind(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::UserMessage { .. } => "UserMessage",
            AgentEvent::AssistantText { .. } => "AssistantText",
            AgentEvent::AssistantToolCall { .. } => "AssistantToolCall",
            AgentEvent::ToolCallStarted { .. } => "ToolCallStarted",
            AgentEvent::ToolCallCompleted { .. } => "ToolCallCompleted",
            AgentEvent::CompactionOccurred { .. } => "CompactionOccurred",
            AgentEvent::Usage { .. } => "Usage",
            _ => "Other",
        }
    }

    /// The observer must receive a live mirror of every event the turn
    /// persists, in persistence order, plus the raw text deltas — the
    /// contract `braze-tui` will render from.
    #[tokio::test]
    async fn the_observer_mirrors_every_persisted_event_in_order() {
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
                CompletionEvent::TextDelta("do".to_string()),
                CompletionEvent::TextDelta("ne".to_string()),
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

        let mut observer = RecordingObserver {
            deltas: Vec::new(),
            events: Vec::new(),
        };
        engine
            .run_turn(&session, "please echo hi", &mut observer)
            .await
            .expect("turn should succeed");

        assert_eq!(observer.deltas, vec!["do", "ne"]);

        // The mirrored sequence must match the persisted log exactly —
        // same kinds, same order, nothing extra and nothing missing.
        let verify_store = FileSessionStore::new(dir.clone());
        let persisted = verify_store.load(&session).await.expect("load events");
        assert_eq!(
            observer.events.iter().map(event_kind).collect::<Vec<_>>(),
            persisted.iter().map(event_kind).collect::<Vec<_>>(),
        );
        assert_eq!(
            observer.events.iter().map(event_kind).collect::<Vec<_>>(),
            vec![
                "UserMessage",
                "AssistantToolCall",
                "ToolCallStarted",
                "ToolCallCompleted",
                "AssistantText",
            ],
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A4: a completion delivered for a handle that
    /// isn't part of the current round's `pending` set (simulating a task
    /// that finally finished after an earlier round already gave up on it
    /// via timeout) must be discarded, not persisted as a second
    /// `ToolCallCompleted` — which would otherwise corrupt the session
    /// with two `tool_result`s for a single `tool_use_id`.
    #[tokio::test]
    async fn stale_completion_from_an_earlier_round_is_discarded_not_persisted() {
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
        let notifier = TestNotifier::new();
        // Queued before dispatch even starts, so it is guaranteed to be
        // the first completion `next_completed` yields — exactly the
        // ordering a stale, previously-timed-out task's late delivery
        // would produce.
        notifier.inject_stale_completion("call-1");

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(notifier),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let completions: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
            .collect();
        // Exactly one `ToolCallCompleted` for call-1 — the real one — not
        // two.
        assert_eq!(completions.len(), 1);
        match completions[0] {
            AgentEvent::ToolCallCompleted { id, result } => {
                assert_eq!(id, "call-1");
                assert_eq!(result.content, "echoed: hi");
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- D5 (docs/AUDITORIA-2026-07-v3.md): narration-without-action
    // across several turns ---

    /// The nudge that already exists (A5) only fires in response to a
    /// *tool call* being repeated — it never catches a model that just
    /// keeps narrating across turns without ever calling a tool. After
    /// `NARRATION_WITHOUT_ACTION_THRESHOLD` (2) such turns, the 3rd turn
    /// must carry an extra reminder alongside the user's own message.
    #[tokio::test]
    async fn narration_without_action_across_several_turns_injects_a_reminder() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("ok, entiendo".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("voy a hacerlo".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("dale, ahora si".to_string()),
                CompletionEvent::Done,
            ],
        ]);

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
            .run_turn(&session, "guarda el archivo", &mut NoopObserver)
            .await
            .expect("turn 1 should succeed");
        engine
            .run_turn(&session, "hazlo ahora", &mut NoopObserver)
            .await
            .expect("turn 2 should succeed");
        engine
            .run_turn(&session, "por favor hazlo", &mut NoopObserver)
            .await
            .expect("turn 3 should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let user_texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::UserMessage { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec![
                "guarda el archivo",
                "hazlo ahora",
                "por favor hazlo",
                "[Reminder] Your last few responses described an intended action without \
                 actually calling the tool for it. If you're being asked to do something, \
                 call the appropriate tool now instead of describing or restating the plan.",
            ],
            "the reminder must appear exactly once, appended right after the 3rd turn's \
             own user message (2 prior narration-only turns crossed the threshold)"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A turn that dispatches a real tool call resets the streak to 0,
    /// even though the model still converges with a plain-text final
    /// answer *in that same turn* (a later round with no tool calls must
    /// not, on its own, count that whole turn as "narration only" — see
    /// `any_tool_calls_this_turn`'s doc comment). Sequence: 1 narration
    /// turn (streak→1), 1 turn that calls a tool then answers in text
    /// (streak must reset, not reach 2), then 2 more narration turns —
    /// without the reset, the 4th turn here would already be the 3rd
    /// *consecutive* narration-only turn and would wrongly carry the
    /// reminder.
    #[tokio::test]
    async fn a_dispatched_tool_call_resets_the_narration_streak() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let invocations = Arc::new(AtomicU32::new(0));
        let model = ScriptedModel::new(vec![
            // Turn 1: narration only (streak 0 -> 1).
            vec![
                CompletionEvent::TextDelta("ok".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 2: dispatches a tool call...
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            // ...then converges with a plain-text final answer in the
            // very same turn — must still reset the streak to 0, not
            // leave/advance it based on this last round alone.
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 3: narration only (streak 0 -> 1, thanks to turn 2's reset).
            vec![
                CompletionEvent::TextDelta("ok".to_string()),
                CompletionEvent::Done,
            ],
            // Turn 4: narration only (streak 1 -> 2) — still below
            // threshold, so this turn must NOT carry the reminder.
            vec![
                CompletionEvent::TextDelta("ok again".to_string()),
                CompletionEvent::Done,
            ],
        ]);

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
            .run_turn(&session, "a", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "b", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "c", &mut NoopObserver)
            .await
            .unwrap();
        engine
            .run_turn(&session, "d", &mut NoopObserver)
            .await
            .unwrap();

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "turn 2's tool call must have actually dispatched"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events.iter().any(
                |e| matches!(e, AgentEvent::UserMessage { text } if text.contains("[Reminder]"))
            ),
            "turn 2's dispatched tool call must have reset the streak — only 2 \
             consecutive narration-only turns (3, 4) followed it, below the threshold"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A7: a hallucinated tool name must not be
    /// dispatched, and the error the model sees must list the tools that
    /// actually exist so a small model has something concrete to
    /// self-correct with, instead of a bare "tool not found".
    #[tokio::test]
    async fn a_hallucinated_tool_name_is_not_dispatched_and_lists_valid_tools() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_files".to_string(), // hallucinated; only "echo" exists
                    arguments: serde_json::json!({}),
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

        engine
            .run_turn(&session, "please read a file", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        // The hallucinated call never reached `invoke`.
        assert_eq!(invocations.load(Ordering::SeqCst), 0);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-1"))
            .expect("expected a ToolCallCompleted for call-1")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(result.is_error);
                assert!(result.content.contains("read_files"));
                assert!(
                    result.content.contains("echo"),
                    "expected the available tool name to be listed, got: {}",
                    result.content
                );
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for A6: a turn that never converges within
    /// `MAX_TURN_ITERATIONS` must degrade gracefully — one final
    /// tools-free round asking the model to summarize — instead of
    /// failing outright with nothing to show for it.
    #[tokio::test]
    async fn a_turn_that_never_converges_gets_a_final_tools_free_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let mut rounds: Vec<Vec<CompletionEvent>> = (0..MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("attempt {i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        // One round beyond the cap: the tools-free summary attempt.
        rounds.push(vec![
            CompletionEvent::TextDelta("aqui esta lo que encontre".to_string()),
            CompletionEvent::Done,
        ]);

        let model = ScriptedModel::new(rounds);
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
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully instead of erroring");

        assert_eq!(streamed, "aqui esta lo que encontre");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text == "aqui esta lo que encontre"
        )));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `Engine::with_max_turn_iterations` (v4 P0.2/mitad rondas,
    /// `Config::max_turn_iterations` / `BRAZE_MAX_TURN_ITERATIONS`):
    /// reducing the cap from the default 20 to 2 must make `run_turn`
    /// abort after exactly 2 non-converging rounds and emit the
    /// tools-free summary fallback — instead of the default cap's
    /// 20 rounds. The scripted model below has only 2 tool-call rounds
    /// + 1 fallback round, so if the cap weren't honored the engine
    /// would exhaust its script and panic; that's the failure mode the
    /// test catches by construction.
    #[tokio::test]
    async fn a_lower_max_turn_iterations_caps_the_loop_instead_of_20() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Two tool-call rounds (the new cap), then one fallback summary
        // round — exactly the three rounds a correctly-capped engine
        // needs. Were `max_turn_iterations` still hardcoded at 20, the
        // engine would keep pulling from an exhausted script.
        let rounds: Vec<Vec<CompletionEvent>> = vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-0".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "r0"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "r1"}),
                },
                CompletionEvent::Done,
            ],
            // After the cap: the tools-free summary round that degrades
            // gracefully instead of failing outright.
            vec![
                CompletionEvent::TextDelta("fallback summary".to_string()),
                CompletionEvent::Done,
            ],
        ];

        let model = ScriptedModel::new(rounds);
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

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully to a summary");

        assert_eq!(streamed, "fallback summary");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        // Two tool-call rounds produced two `AssistantToolCall`s — the
        // cap must have been exactly 2, not the default 20, or else
        //ScriptedModel would have panicked pulling from an exhausted
        // vec and this assertion point would be unreachable.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::AssistantToolCall { .. }))
                .count(),
            2,
            "the cap must stop the loop after exactly 2 tool-call rounds"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::AssistantText { text } if text == "fallback summary"
        )));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for U-16 (docs/usability-log-2026-07-07-si2.md):
    /// `z-ai/glm-5.2` kept emitting its native (malformed) `<tool_call>`
    /// syntax even in the tools-free summary round, which explicitly tells
    /// the model no tool is available — before the fix, that leaked block
    /// was persisted verbatim as the turn's final answer instead of being
    /// stripped like it is in every other round.
    #[tokio::test]
    async fn a_leaked_tool_call_in_the_summary_round_is_stripped_not_shown_verbatim() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let mut rounds: Vec<Vec<CompletionEvent>> = (0..MAX_TURN_ITERATIONS)
            .map(|i| {
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: format!("call-{i}"),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({ "text": format!("attempt {i}") }),
                    },
                    CompletionEvent::Done,
                ]
            })
            .collect();
        rounds.push(vec![
            CompletionEvent::TextDelta(
                "Esto es lo que encontré.\n<tool_call>read_file<arg_key>path</arg_key>\
                 <arg_value>x.txt</arg_value></tool_call>"
                    .to_string(),
            ),
            CompletionEvent::Done,
        ]);

        let model = ScriptedModel::new(rounds);
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
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk| streamed.push_str(chunk)),
            )
            .await
            .expect("the turn should degrade gracefully instead of erroring");

        assert!(
            !streamed.contains("<tool_call>"),
            "the leaked block must never reach the user verbatim: {streamed}"
        );
        assert_eq!(streamed, "Esto es lo que encontré.");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for C4: an `AssistantToolCall` with no matching
    /// `ToolCallCompleted` anywhere in the log (what an interrupted
    /// process leaves behind — see `dispatch_tool_calls`, which persists
    /// the `tool_use` before dispatch) must be repaired with a synthetic
    /// error result on the next `load_messages`, not left dangling —
    /// otherwise Anthropic rejects every future request against this
    /// session with a permanent 400.
    #[tokio::test]
    async fn load_messages_repairs_an_orphaned_tool_use_with_no_result() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::UserMessage {
                    text: "lee el archivo".to_string(),
                },
            )
            .await
            .expect("seed user message");
        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-orphan".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "x.txt"}),
                },
            )
            .await
            .expect("seed orphaned tool_use — process 'crashed' right here");

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

        // The reconstructed history must be a valid tool_use/tool_result
        // pair, not a dangling tool_use — otherwise Anthropic's API
        // rejects the request outright.
        assert!(messages.iter().any(|m| matches!(
            &m.content[0],
            ContentBlock::ToolResult { tool_use_id, is_error, .. }
                if tool_use_id == "call-orphan" && *is_error
        )));

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let completions: Vec<_> = events
            .iter()
            .filter(
                |e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-orphan"),
            )
            .collect();
        assert_eq!(
            completions.len(),
            1,
            "expected exactly one synthetic repair to be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The repair must be idempotent: calling `load_messages` again after
    /// the first repair must not persist a second `ToolCallCompleted` for
    /// the same id (it now has one, so it's no longer an orphan).
    #[tokio::test]
    async fn repairing_an_orphaned_tool_use_twice_only_persists_one_completion() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        store
            .append(
                &session,
                &AgentEvent::AssistantToolCall {
                    id: "call-orphan".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "x.txt"}),
                },
            )
            .await
            .expect("seed orphaned tool_use");

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
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("first load_messages should repair the orphan");
        engine
            .load_messages(&session, &mut NoopObserver)
            .await
            .expect("second load_messages should be a no-op repair-wise");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let completions = events
            .iter()
            .filter(
                |e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-orphan"),
            )
            .count();
        assert_eq!(completions, 1, "the second call must not repair again");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-6.
    ///
    /// Once a large tool result has settled into `durable_events` (past
    /// the compactor's tactical window), the token-budget estimate must
    /// reflect the *cleared* render actually sent to the model — a short
    /// "[tool result cleared: N chars removed...]" placeholder — not the
    /// raw, uncleared payload. Before this fix, `estimate_prompt_tokens`
    /// measured `durable_events` via `format!("{event:?}")` over the raw
    /// event, so a 5000-char tool result kept counting as ~5000 chars
    /// forever: since compacting only ever folds `tactical` (never
    /// shrinks `durable_events`), `over_token_budget` would stay `true` on
    /// every single `load_messages` call no matter how many times it
    /// compacted — a `CompactionOccurred` appended on every round,
    /// indefinitely, for content that was already small once rendered.
    #[tokio::test]
    async fn a_large_settled_tool_result_does_not_permanently_blow_the_token_budget() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        for event in [
            AgentEvent::UserMessage {
                text: "lee el archivo grande".to_string(),
            },
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "big.txt" }),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "x".repeat(5_000),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "ok, lo leí".to_string(),
            },
            AgentEvent::UserMessage {
                text: "gracias".to_string(),
            },
            AgentEvent::AssistantText {
                text: "de nada".to_string(),
            },
        ] {
            store.append(&session, &event).await.expect("seed event");
        }

        let engine = Engine::new(
            Box::new(ScriptedModel::new(vec![])),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            // A small window (2) settles the tool call pair into
            // `durable_events` immediately, exactly like a real session
            // long past its first ~20 events.
            Box::new(SimpleContextCompactor::new(2)),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        // Far below the 5000-char raw content, comfortably above the
        // cleared render (a short placeholder plus a handful of small
        // events) — this is the whole point: the *rendered* size is what
        // must be compared against the budget.
        .with_context_budget(100);

        for _ in 0..3 {
            engine
                .load_messages(&session, &mut NoopObserver)
                .await
                .expect("load_messages should succeed");
        }

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::CompactionOccurred { .. })),
            "the cleared render of the settled tool result is well under budget — \
             compaction must not trigger (let alone repeatedly) just because the \
             raw, already-settled payload is large"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- full_observations_byte_budget (hallazgo U-17,
    // docs/usability-log-2026-07-07-si2.md) ---

    // --- tactical_cap_scale (I-2, docs/AUDITORIA-2026-07-v6.md): the
    // caps scale with the budget's VALUE, not its mere presence ---

    /// v4 P0.2 (docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3): a turn
    /// whose cumulative tokens blow the budget stops at the top of the
    /// next iteration — gracefully when the tools-free summary produces
    /// text, as here.
    #[tokio::test]
    async fn a_turn_over_its_token_budget_stops_gracefully_via_the_summary_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let expensive_usage = || CompletionEvent::Usage {
            input_tokens: 90_000,
            output_tokens: 500,
            stop_reason: Some("tool_use".to_string()),
            cache_read_tokens: None,
            cache_write_tokens: None,
            escalation_trigger: None,
        };
        let model = ScriptedModel::new(vec![
            // Round 1: a tool call whose usage alone blows the 50k budget.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hola" }),
                },
                expensive_usage(),
                CompletionEvent::Done,
            ],
            // The tools-free summary attempt (round 2 never runs as a
            // normal round — the breaker fires first).
            vec![
                CompletionEvent::TextDelta("resumen de lo hecho".to_string()),
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
        .with_max_turn_total_tokens(Some(50_000));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            result.is_ok(),
            "expected graceful summary recovery, got {result:?}"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "resumen de lo hecho"
            )),
            "the summary text must be persisted, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::SummaryFallbackAttempted)),
            "the breaker goes through the same instrumented fallback path"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Same breaker, but the summary attempt comes back empty — the turn
    /// surfaces `TurnBudgetExhausted` with the real numbers instead of
    /// pretending it converged.
    #[tokio::test]
    async fn a_turn_over_its_token_budget_with_an_empty_summary_errors_with_the_spent_count() {
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
                    input_tokens: 90_000,
                    output_tokens: 500,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // Empty summary attempt.
            vec![CompletionEvent::Done],
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
        .with_max_turn_total_tokens(Some(50_000));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        match result {
            Err(EngineError::TurnBudgetExhausted {
                budget_tokens,
                spent_tokens,
            }) => {
                assert_eq!(budget_tokens, 50_000);
                assert_eq!(spent_tokens, 90_500);
            }
            other => panic!("expected TurnBudgetExhausted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// round-economics (docs/hypothesis-2026-07-28-round-economics.md):
    /// el presupuesto de wall-clock corta en el BORDE de la ronda, no
    /// abortando la que está en vuelo. La ronda 0 pide un tool que tarda
    /// más que el presupuesto entero; el corte tiene que llegar recién al
    /// intentar la ronda 1, con `rounds_completed = 1` y el tool ya
    /// ejecutado — que es justo lo que un `tokio::time::timeout` de
    /// afuera NO puede dar (mata la ronda en vuelo y pierde su `Usage`,
    /// J-21/J-10).
    #[tokio::test]
    async fn a_turn_over_its_wall_clock_budget_stops_at_the_next_round_boundary() {
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
                    input_tokens: 10,
                    output_tokens: 5,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            // La ronda 2 existe en el guion pero no debe llegar a correr:
            // el presupuesto ya se agotó mientras el tool dormía.
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(
                super::test_support::SlowEchoToolProvider::new(
                    Arc::clone(&invocations),
                    Duration::from_millis(80),
                ),
            )]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_turn_wall_clock(Some(Duration::from_millis(30)));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        match result {
            Err(EngineError::TurnWallClockExhausted {
                budget_ms,
                elapsed_ms,
                rounds_completed,
            }) => {
                assert_eq!(budget_ms, 30);
                assert!(
                    elapsed_ms >= 80,
                    "el turno tiene que haber gastado al menos lo que durmió el tool, \
                     got {elapsed_ms} ms"
                );
                assert_eq!(
                    rounds_completed, 1,
                    "la ronda 0 completó entera — el corte es en el borde, no un abort"
                );
            }
            other => panic!("expected TurnWallClockExhausted, got {other:?}"),
        }
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "el tool de la ronda 0 corrió completo antes del corte"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El presupuesto no muerde a un turno que converge dentro de él: sin
    /// esto, el corte sería indistinguible de "toda corrida falla" y el
    /// brazo experimental no mediría nada.
    #[tokio::test]
    async fn a_turn_that_converges_within_its_wall_clock_budget_is_untouched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]]);
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
        .with_max_turn_wall_clock(Some(Duration::from_secs(60)));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("un turno dentro del presupuesto converge normal");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // P1.1 resto (v9 L-5, 2026-08-18): cluster D′ (skills locales
    // explicit-only) movido VERBATIM del `mod tests` de engine/mod.rs;
    // `temp_skills_dir` viaja con él.

    // --- D′: skills locales explicit-only
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte III) ---

    fn temp_skills_dir(label: &str, skills: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-engine-skills-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, body) in skills {
            let skill_dir = dir.join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: guidance for {name}\n---\n\n{body}"),
            )
            .unwrap();
        }
        dir
    }

    /// Como `temp_skills_dir` pero con `tools:` en el frontmatter — la
    /// declaración que habilita la invocación call-time.
    fn temp_skills_dir_for_tools(label: &str, skills: &[(&str, &str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-engine-skills-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, tools, body) in skills {
            let skill_dir = dir.join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: guidance for {name}\ntools: {tools}\n---\n\n{body}"
                ),
            )
            .unwrap();
        }
        dir
    }

    /// Recuris § 2.2.2 end to end. La primera llamada a `echo` se
    /// intercepta: NO se ejecuta (el contador del provider lo prueba), el
    /// modelo recibe un resultado de no-ejecución, y la skill entra al
    /// system prompt. La segunda llamada, ya con la guía delante, sí
    /// ejecuta — que es la mitad del contrato sin la cual esto sería un
    /// deadlock elegante.
    #[tokio::test]
    async fn a_call_time_skill_intercepts_the_first_call_and_lets_the_second_through() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let invocations = Arc::new(AtomicU32::new(0));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let skills_dir = temp_skills_dir_for_tools(
            "call-time",
            &[("echoing", "echo", "Echo twice, never once.")],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({"text": "hola"}),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-2".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({"text": "hola hola"}),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2)
        .with_call_time_skills(true);

        engine
            .run_turn(&session, "haz eco", &mut NoopObserver)
            .await
            .expect("turn must converge");

        // La herramienta corrió UNA vez: la segunda call, no la primera.
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "la primera call se intercepta y la segunda ejecuta"
        );

        let events = FileSessionStore::new(dir.clone())
            .load(&session)
            .await
            .expect("load");
        let first = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { id, result } if id == "call-1" => Some(result),
                _ => None,
            })
            .expect("la call interceptada igual reporta resultado");
        assert!(
            first.content.contains("NOT EXECUTED"),
            "el modelo tiene que saber que no pasó nada: {}",
            first.content
        );
        assert!(
            !first.is_error,
            "no es un fallo: no se ejecutó, que es distinto"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::SkillLoaded { name, trigger, .. }
                    if name == "echoing" && trigger == "call_time"
            )),
            "el trigger distingue este camino del explícito en el log"
        );

        // Y la guía llegó al modelo: la request posterior la lleva.
        {
            let seen = requests.lock().unwrap();
            assert!(
                seen.last()
                    .expect("hubo requests")
                    .system_prompt
                    .contains("Echo twice, never once."),
                "el body de la skill viaja en el system prompt de las rondas siguientes"
            );
        }

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Una tool sin skill declarada se ejecuta de una, con la palanca
    /// encendida: la intercepción es por herramienta, no un peaje global.
    #[tokio::test]
    async fn a_tool_without_a_declared_skill_is_not_intercepted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let invocations = Arc::new(AtomicU32::new(0));
        let skills_dir = temp_skills_dir_for_tools(
            "call-time-miss",
            &[("editing", "edit_file", "Read before editing.")],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "hola"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2)
        .with_call_time_skills(true);

        engine
            .run_turn(&session, "haz eco", &mut NoopObserver)
            .await
            .expect("turn must converge");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "sin skill para `echo`, la call corre de una"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Off por default: con la palanca apagada, una skill declarada para
    /// esa tool no cambia nada. Es el brazo de control del A/B.
    #[tokio::test]
    async fn with_the_lever_off_a_declared_skill_does_not_intercept() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let invocations = Arc::new(AtomicU32::new(0));
        let skills_dir =
            temp_skills_dir_for_tools("call-time-off", &[("echoing", "echo", "Echo twice.")]);
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({"text": "hola"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoToolProvider::new(Arc::clone(
                &invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);

        engine
            .run_turn(&session, "haz eco", &mut NoopObserver)
            .await
            .expect("turn must converge");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        let events = FileSessionStore::new(dir.clone())
            .load(&session)
            .await
            .expect("load");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::SkillLoaded { .. })),
            "con la palanca apagada no se carga ninguna skill sola"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The explicit-mention path end to end: `$testing` in the user's
    /// input loads that skill's body into the request's system prompt,
    /// persists `SkillLoaded`, and leaves unmentioned skills out.
    #[tokio::test]
    async fn a_skill_mention_loads_its_body_into_the_system_prompt() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let skills_dir = temp_skills_dir(
            "mention",
            &[
                ("testing", "Always run cargo test before claiming success."),
                ("review", "Check invariants first."),
            ],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ]]),
            requests: Arc::clone(&requests),
        };
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);

        engine
            .run_turn(&session, "usa $testing para esto", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let requests = requests.lock().unwrap().clone();
        assert!(
            requests[0].system_prompt.contains("Loaded skill: testing"),
            "got: {}",
            requests[0].system_prompt
        );
        assert!(
            requests[0]
                .system_prompt
                .contains("Always run cargo test before claiming success."),
            "the body itself must be injected"
        );
        assert!(
            !requests[0].system_prompt.contains("Loaded skill: review"),
            "unmentioned skills stay out"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::SkillLoaded { name, trigger, .. }
                    if name == "testing" && trigger == "explicit_mention"
            )),
            "the load must persist as the rollout log's trace"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessage { text } if text.contains("Always run cargo test")
            )),
            "the body is request-scoped, never persisted as conversation"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The per-turn cap: mentioning three skills with a cap of 2 loads
    /// two and persists a `SkillLoadSkipped` for the third — bounded
    /// context growth, visible in the log.
    #[tokio::test]
    async fn the_per_turn_cap_skips_the_excess_mention_with_an_event() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let skills_dir = temp_skills_dir(
            "cap",
            &[
                ("uno", "body uno"),
                ("dos", "body dos"),
                ("tres", "body tres"),
            ],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("listo".to_string()),
            CompletionEvent::Done,
        ]]);
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);

        engine
            .run_turn(&session, "usa $uno $dos $tres", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let loaded: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SkillLoaded { .. }))
            .collect();
        assert_eq!(loaded.len(), 2, "cap of 2: {events:#?}");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::SkillLoadSkipped { name, reason }
                    if name == "tres" && reason.contains("per-turn cap")
            )),
            "the third mention must be visibly skipped"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for J-12 (docs/AUDITORIA-2026-07-v7.md): a FRESH
    /// engine over the same session store — the `--resume` restart, or a
    /// `/model` rebuild — must re-load the bodies the log's `SkillLoaded`
    /// events record, without appending new `SkillLoaded` events (which
    /// would double the bench's counts on every resumed turn).
    #[tokio::test]
    async fn a_fresh_engine_rehydrates_previously_loaded_skills_from_the_log() {
        let (store, dir) = temp_store();
        let store = Arc::new(store);
        let session = SessionId::new();
        let skills_dir = temp_skills_dir(
            "rehydrate",
            &[("testing", "Always run cargo test before claiming success.")],
        );
        let registry = std::sync::Arc::new(braze_skills::SkillRegistry::discover(
            std::slice::from_ref(&skills_dir),
        ));

        // Session 1: the mention loads the skill and persists SkillLoaded.
        let first_engine = Engine::new(
            Box::new(ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ]])),
            ToolRegistry::new(vec![]),
            Arc::clone(&store) as Arc<dyn SessionStore>,
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(std::sync::Arc::clone(&registry), 1200, 2);
        first_engine
            .run_turn(&session, "usa $testing para esto", &mut NoopObserver)
            .await
            .expect("first turn must converge");
        drop(first_engine); // the restart: in-memory loaded_skills is gone

        // Session 2 (same store, fresh engine): no mention this turn —
        // the body must come back from the log alone.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let second_engine = Engine::new(
            Box::new(RequestCapturingModel {
                inner: ScriptedModel::new(vec![vec![
                    CompletionEvent::TextDelta("sigo".to_string()),
                    CompletionEvent::Done,
                ]]),
                requests: Arc::clone(&requests),
            }),
            ToolRegistry::new(vec![]),
            Arc::clone(&store) as Arc<dyn SessionStore>,
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "base prompt".to_string(),
            1024,
        )
        .with_skills(registry, 1200, 2);
        second_engine
            .run_turn(&session, "continúa con lo anterior", &mut NoopObserver)
            .await
            .expect("resumed turn must converge");

        let requests = requests.lock().unwrap().clone();
        assert!(
            requests[0]
                .system_prompt
                .contains("Always run cargo test before claiming success."),
            "the rehydrated body must reach the resumed turn's system prompt, got: {}",
            requests[0].system_prompt
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let loaded_count = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SkillLoaded { .. }))
            .count();
        assert_eq!(
            loaded_count, 1,
            "rehydration must NOT append a second SkillLoaded event: {events:#?}"
        );

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
