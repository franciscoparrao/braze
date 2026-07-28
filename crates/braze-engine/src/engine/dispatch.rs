//! El despacho de tool calls de una ronda — P1.1 paso 3 (v8 § 3).
//! Extraído VERBATIM de `engine/mod.rs` (2026-07-18):
//! `dispatch_tool_calls` (registro, spawn en background vía
//! `TaskNotifier`, espera con timeout y abort real N-33) y el handler
//! in-process de las tools de task list (C′.2).

use super::*;

impl Engine {
    /// C′.2: applies one `task_add`/`task_update` call to the in-memory
    /// list and renders the tool result the model sees — `(content,
    /// is_error, completed_description)`. Malformed arguments come back
    /// as recoverable tool errors with the actionable detail (never a
    /// hard failure: the model can retry with fixed arguments).
    /// `completed_description` is `Some` only when this call transitioned
    /// a task to `Done` — the caller uses it to also persist
    /// `AgentEvent::TaskCompleted`.
    fn handle_task_tool_call(&self, call: &ToolCall) -> (String, bool, Option<String>) {
        let mut task_list = self.task_list.lock().unwrap();
        if call.name == crate::task_list::TASK_ADD_TOOL {
            let Some(description) = call
                .arguments
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|d| !d.trim().is_empty())
            else {
                return (
                    "task_add needs a non-empty 'description' string".to_string(),
                    true,
                    None,
                );
            };
            let id = task_list.add(description);
            return (
                format!("added task {id}. {}", task_list.summary_line()),
                false,
                None,
            );
        }
        let id = call.arguments.get("id").and_then(|v| v.as_u64());
        let status = call
            .arguments
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(crate::task_list::TaskStatus::parse);
        match (id, status) {
            (Some(id), Some(status)) => match task_list.update(id as usize, status) {
                Ok(completed) => (task_list.summary_line(), false, completed),
                Err(reason) => (reason, true, None),
            },
            _ => (
                "task_update needs an integer 'id' and a 'status' of pending/in_progress/done"
                    .to_string(),
                true,
                None,
            ),
        }
    }

    /// Records each requested tool call, spawns it as a background task via
    /// [`TaskNotifier`], and blocks until every task from this round has
    /// reported completion (persisting a `ToolCallCompleted` event for
    /// each), or times out — in which case every still-pending task is
    /// actually cancelled via [`TaskNotifier::abort`] (N-33,
    /// docs/AUDITORIA-2026-07-v2.md), not merely forgotten.
    pub(super) async fn dispatch_tool_calls(
        &self,
        session: &SessionId,
        tool_calls: &[ToolCall],
        available_tools: &[ToolStub],
        hidden_stubs: &[ToolStub],
        state: &mut TurnDispatchState,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        let TurnDispatchState {
            schema_retry_counts: retry_counts,
            seen_calls,
            known_tool_call_ids,
            reads_by_path,
        } = state;

        // Nota de relectura improductiva por id de llamada — ver
        // `TurnDispatchState::reads_by_path`. Se calcula al despachar
        // (donde se conocen nombre y argumentos) y se anexa al resultado
        // exitoso, sin bloquearlo.
        let mut reread_nudge: HashMap<String, String> = HashMap::new();
        let mut handle_to_id: HashMap<TaskHandle, String> = HashMap::new();
        let mut pending: HashSet<TaskHandle> = HashSet::new();
        // F6: resolves a completed call's id back to its tool name, so a
        // successful completion can be checked against
        // `MUTATING_TOOL_NAMES` without threading the name through the
        // background task machinery.
        let mut id_to_name: HashMap<String, String> = HashMap::new();
        // Resuelve el id de una llamada completada de vuelta a su clave
        // (nombre, argumentos), para poder guardar su resultado y servirlo
        // si el modelo repite la llamada más adelante en el turno.
        let mut id_to_key: HashMap<String, (String, String)> = HashMap::new();

        for call in tool_calls {
            // N-14 (docs/AUDITORIA-2026-07-v2.md): shadow `call` with an
            // owned copy whose id is guaranteed unique against every id
            // this session has ever used (history + this turn so far)
            // *before* anything below persists or dispatches it — every
            // remaining `call.id`/`call.clone()` use in this loop body
            // is unchanged and now operates on the deduped id. Without
            // this, a duplicate id (a backend's synthetic-id counter
            // restarting after `--resume`, or two calls the model itself
            // gave the same id) enters the append-only log as two
            // `tool_use`/`tool_result` pairs sharing one id — Anthropic
            // rejects that with a permanent 400 on every future request.
            let mut call = ToolCall {
                id: ensure_unique_tool_call_id(call.id.clone(), known_tool_call_ids),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            };

            self.append_and_notify(
                session,
                &AgentEvent::AssistantToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
                observer,
            )
            .await?;

            // J-9 (docs/AUDITORIA-2026-07-v7.md): a deferred tool is NOT
            // invocable until a search activates it — deferral is
            // invocability, not just prompt space. Before this gate, a
            // model that named a hidden tool exactly (remembered from
            // pre-compaction history, or guessed from the count the
            // search_tools stub announces) dispatched it directly,
            // silently bypassing the mechanism the search_tools A/B
            // claims to measure ("the model can only use what's listed
            // or searched for"). The error is recoverable and actionable
            // — one extra round through `search_tools` when it happens
            // (rare), in exchange for the mechanism meaning what it says.
            // Deliberately NOT auto-activate-and-dispatch: that would be
            // the same bypass with bookkeeping.
            //
            // Checked BEFORE the repeated-call nudge, and a blocked call
            // is deliberately NOT recorded in `seen_calls`: it never
            // produced a result, so the legitimate retry after the model
            // activates the tool via `search_tools` must dispatch — the
            // nudge's "the result has not changed" claim would be false
            // exactly there.
            if hidden_stubs.iter().any(|stub| stub.name == call.name) {
                tracing::info!(
                    tool = %call.name,
                    "blocked a direct call to a deferred tool that was never activated"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content: format!(
                                "Tool '{}' exists but is not loaded. Call search_tools with \
                                 keywords describing what you need (e.g. \"{}\") — matching \
                                 tools become available to call afterwards.",
                                call.name, call.name
                            ),
                            is_error: true,
                        },
                    },
                    observer,
                )
                .await?;
                continue;
            }

            // Exact repeat of a (name, arguments) pair already dispatched
            // earlier in this same turn — the dominant non-convergence
            // pattern for small/local models (they re-issue an identical
            // call instead of using the result they already got, or
            // giving up and answering in text). `arguments.to_string()` is
            // a canonical key: `serde_json::Value` objects serialize their
            // keys in sorted order by default (no `preserve_order`
            // feature), so structurally-identical arguments compare equal
            // regardless of the field order the model happened to emit
            // this time. Nudge instead of re-running the tool.
            let call_key = (call.name.clone(), call.arguments.to_string());
            if let Some(previous) = seen_calls.get(&call_key) {
                // La tool NO se re-ejecuta (esa es la intención anti-loop:
                // sin efectos secundarios ni costo repetido), pero si
                // tenemos el resultado anterior se lo devolvemos en vez de
                // negarnos.
                //
                // Por qué: el colapso ACI de observaciones viejas puede
                // haber borrado del contexto el resultado original, así que
                // "usá el que ya tenés" le pide al modelo algo que el propio
                // harness le quitó. Medido contra roam (2026-07-26):
                // gpt-oss leyó un archivo en 3 páginas, la observación de la
                // página 1 se colapsó, pidió releerla, y el nudge se la negó
                // cuatro veces hasta que abandonó el turno con el plan
                // correcto en la mano. Dos palancas correctas por separado
                // que se traban entre sí; devolver el contenido convierte la
                // trampa en un acierto de caché.
                let (content, is_error) = match previous {
                    Some(prev) => (
                        format!(
                            "(resultado en caché de la llamada idéntica anterior a '{}' \
                             en este turno — la tool no se volvió a ejecutar)\n{prev}",
                            call.name
                        ),
                        false,
                    ),
                    // Todavía sin resultado: dos llamadas idénticas en la
                    // MISMA ronda. No hay nada que servir, así que se
                    // mantiene el nudge original.
                    None => (
                        format!(
                            "You already called '{}' with these exact arguments \
                             earlier in this turn — the result has not changed. Do \
                             not repeat this call; either use the result you already \
                             have, or respond to the user with text instead of \
                             calling a tool.",
                            call.name
                        ),
                        true,
                    ),
                };
                tracing::warn!(
                    tool = %call.name,
                    servido_de_cache = !is_error,
                    "model repeated an identical tool call this turn; not re-dispatching"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content,
                            is_error,
                        },
                    },
                    observer,
                )
                .await?;
                continue;
            }
            seen_calls.insert(call_key.clone(), None);
            id_to_key.insert(call.id.clone(), call_key);

            // C′.1 (crate::tool_search): the `search_tools` meta-tool is
            // harness-owned — handled inline, before schema resolution
            // (the registry has no schema for it) and outside the
            // permission guard (read-only over an in-memory catalog).
            // Only intercepted while something is actually hidden, so a
            // real provider that happens to advertise this name isn't
            // shadowed when deferral is inactive.
            if call.name == crate::tool_search::SEARCH_TOOL_NAME && !hidden_stubs.is_empty() {
                let query = call
                    .arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let hits = crate::tool_search::search_stubs(
                    hidden_stubs,
                    query,
                    crate::tool_search::SEARCH_RESULTS_LIMIT,
                );
                let content = if hits.is_empty() {
                    format!(
                        "No tools matched '{query}'. Try different keywords — the catalog \
                         covers {} tools.",
                        hidden_stubs.len()
                    )
                } else {
                    let mut listing = String::from("Matching tools, now available to call:\n");
                    for hit in &hits {
                        listing.push_str(&format!("- {}: {}\n", hit.name, hit.summary));
                    }
                    listing
                };
                {
                    let mut activated = self.activated_deferred_tools.lock().unwrap();
                    for hit in &hits {
                        activated.insert(hit.name.clone());
                    }
                }
                tracing::info!(
                    query,
                    hits = hits.len(),
                    "search_tools activated deferred tools"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content,
                            is_error: false,
                        },
                    },
                    observer,
                )
                .await?;
                continue;
            }

            // I.7 (crate::exploration): the explore tool delegates a
            // read-only question to the isolated child loop — same
            // inline treatment as `search_tools` (no registry schema;
            // no permission guard: the child can only invoke read-only
            // tools by construction). Only intercepted while the lever
            // is on, so the name passes through untouched otherwise.
            if self.exploration_enabled && call.name == crate::exploration::EXPLORE_TOOL {
                let question = call
                    .arguments
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let outcome = if question.trim().is_empty() {
                    crate::exploration::ExplorationOutcome {
                        content: "explore needs a non-empty 'question' string".to_string(),
                        is_error: true,
                        child_rounds: 0,
                        child_input_tokens: 0,
                        child_output_tokens: 0,
                    }
                } else {
                    self.run_exploration(&question).await
                };
                let child_tokens = outcome.child_input_tokens + outcome.child_output_tokens;
                // The child's cost enters the rollout log twice on
                // purpose: an aggregate Usage (so every existing token
                // accounting — bench sums, budget audits — counts it
                // with zero new code) and the audit event (so the A/B
                // can attribute it to delegation specifically).
                if child_tokens > 0 {
                    self.append_and_notify(
                        session,
                        &AgentEvent::Usage {
                            input_tokens: outcome.child_input_tokens.min(u32::MAX as u64) as u32,
                            output_tokens: outcome.child_output_tokens.min(u32::MAX as u64) as u32,
                            stop_reason: Some("exploration_child".to_string()),
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                        },
                        observer,
                    )
                    .await?;
                }
                self.append_and_notify(
                    session,
                    &AgentEvent::ExplorationDelegated {
                        question,
                        child_rounds: outcome.child_rounds,
                        child_tokens,
                    },
                    observer,
                )
                .await?;
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content: outcome.content,
                            is_error: outcome.is_error,
                        },
                    },
                    observer,
                )
                .await?;
                continue;
            }

            // C′.2 (crate::task_list): the two task tools are
            // harness-owned state mutations — same inline treatment as
            // `search_tools` (no registry schema, no permission guard:
            // in-memory bookkeeping). Only intercepted while the lever
            // is on, so the names pass through untouched otherwise.
            if self.task_list_enabled
                && (call.name == crate::task_list::TASK_ADD_TOOL
                    || call.name == crate::task_list::TASK_UPDATE_TOOL)
            {
                let (content, is_error, completed) = self.handle_task_tool_call(&call);
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content,
                            is_error,
                        },
                    },
                    observer,
                )
                .await?;
                if let Some(description) = completed {
                    self.append_and_notify(
                        session,
                        &AgentEvent::TaskCompleted { description },
                        observer,
                    )
                    .await?;
                }
                continue;
            }

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
                    // F2 (docs/AUDITORIA-2026-07-v3.md): the qwen3-coder
                    // XML rescue (`parse_function_xml_tool_call`) has no
                    // native number/boolean grammar, so every scalar
                    // param comes back as a string, and a code-carrying
                    // string param can come back mis-parsed as a JSON
                    // object — both fail schema validation deterministically
                    // even though the model's *intent* was correct. A
                    // no-op for wire-sourced calls, whose backend already
                    // sends correctly-typed JSON.
                    coerce_arguments_to_schema(&mut call.arguments, &schema.input_schema);

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

                        // Trazar el fallo con los args: sin esto, una
                        // corrida de bench con schema_fail alto es
                        // indiagnosticable post-hoc (el error solo viaja
                        // al modelo como tool result). Lo pidió la
                        // anomalía del A/B v3 del stencil (2026-07-21:
                        // 8 fallos en una trayectoria que no reprodujo).
                        tracing::info!(
                            tool = %call.name,
                            attempt,
                            error = %validation_err,
                            arguments = %call.arguments,
                            "tool call failed schema validation"
                        );

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

                        self.append_and_notify(
                            session,
                            &AgentEvent::ToolCallCompleted {
                                id: call.id.clone(),
                                result: ToolResult {
                                    tool_call_id: call.id.clone(),
                                    content: repair_message,
                                    is_error: true,
                                },
                            },
                            observer,
                        )
                        .await?;

                        continue;
                    }
                }
                Err(braze_tools_core::ToolError::NotFound(_)) => {
                    // A hallucinated tool name — frequent with small
                    // models. Dispatching anyway would just fail identically
                    // and, worse, the error the model saw ("tool not
                    // found: X") gave it no way to self-correct. List the
                    // names that actually exist (already at hand from this
                    // round's stubs) so the model can retry with a valid
                    // one instead of repeating the same hallucination.
                    let available_names: Vec<&str> =
                        available_tools.iter().map(|s| s.name.as_str()).collect();
                    let available = available_names.join(", ");
                    tracing::warn!(
                        tool = %call.name,
                        "no provider advertises this tool; not dispatching"
                    );
                    self.append_and_notify(
                        session,
                        &AgentEvent::ToolCallCompleted {
                            id: call.id.clone(),
                            result: ToolResult {
                                tool_call_id: call.id.clone(),
                                content: format!(
                                    "Unknown tool '{}'.{} Available tools are: \
                                     {available}. Retry using one of these exact \
                                     names.",
                                    call.name,
                                    did_you_mean(&call.name, &available_names)
                                ),
                                is_error: true,
                            },
                        },
                        observer,
                    )
                    .await?;
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        tool = %call.name,
                        error = %err,
                        "failed to resolve tool schema before dispatch (MVP does not validate strictly)"
                    );
                }
            }

            // J-13 (docs/AUDITORIA-2026-07-v7.md): interactive tools wait
            // on a HUMAN, so they dispatch inline with no completion
            // timeout — under the background 120s clock, a slow human
            // answer to `ask_user` was cancelled (the model got a timeout
            // error and guessed anyway, defeating the tool's purpose) and
            // the line the human then typed was consumed by the chat loop
            // as a brand-new prompt. Blocking indefinitely is the correct
            // semantics here, exactly like the approval prompts.
            // Contabilidad de relectura improductiva (incidente roam):
            // una edición exitosa sobre una ruta reinicia su contador
            // (el modelo actuó); una lectura lo incrementa y, pasado el
            // umbral, prepara la nota que se anexará a su resultado.
            if let Some(path) = call
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                if MUTATING_TOOL_NAMES.contains(&call.name.as_str()) {
                    reads_by_path.remove(&path);
                } else if call.name == "read_file" {
                    let count = reads_by_path.entry(path.clone()).or_insert(0);
                    *count += 1;
                    if *count >= UNPRODUCTIVE_REREAD_THRESHOLD {
                        reread_nudge.insert(
                            call.id.clone(),
                            format!(
                                "\n\n[harness] You have read '{path}' {count} times this \
                                 turn without editing it. Re-reading is not making progress: \
                                 if you know what to change, make the edit now; if the file \
                                 is broken or you have lost track of its state, replace it \
                                 wholesale with write_file instead of reading it again."
                            ),
                        );
                    }
                }
            }

            if self.untimed_tools.contains(&call.name) {
                tracing::debug!(tool = %call.name, id = %call.id, "dispatching interactive tool call inline (no timeout)");
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        background: false,
                    },
                    observer,
                )
                .await?;
                let result = match self.tools.dispatch(&call).await {
                    Ok(result) => result,
                    Err(err) => ToolResult {
                        tool_call_id: call.id.clone(),
                        content: err.to_string(),
                        is_error: true,
                    },
                };
                // Mirrors the background completion path's F6 handling —
                // an interactive tool is not expected to mutate, but the
                // invariant shouldn't depend on that expectation.
                if !result.is_error && MUTATING_TOOL_NAMES.contains(&call.name.as_str()) {
                    seen_calls.clear();
                }
                if FILE_MUTATING_TOOL_NAMES.contains(&call.name.as_str()) {
                    self.turn_attempted_edit
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    if !result.is_error {
                        self.turn_did_edit
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result,
                    },
                    observer,
                )
                .await?;
                continue;
            }

            // A9 (docs/AUDITORIA-2026-07.md): per-tool-call visibility for
            // the ordinary/successful dispatch path — previously only
            // failures (schema rejection, unknown tool, timeout) logged
            // anything at all, so a `RUST_LOG=debug` trace of a healthy
            // turn showed no tool-call activity whatsoever.
            tracing::debug!(tool = %call.name, id = %call.id, "dispatching tool call");

            self.append_and_notify(
                session,
                &AgentEvent::ToolCallStarted {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    background: true,
                },
                observer,
            )
            .await?;

            id_to_name.insert(call.id.clone(), call.name.clone());

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

        // If `next_completed` times out, every still-pending task is
        // aborted (N-33, docs/AUDITORIA-2026-07-v2.md) and treated as
        // failed so the turn proceeds rather than hanging forever —
        // salvo que la ventana se haya ido esperando a un humano (ver el
        // branch `None`, incidente roam #3).
        let mut human_wait_at_window_start = braze_permissions::human_wait_accumulated();
        while !pending.is_empty() {
            match self
                .notifier
                .next_completed(self.tool_completion_timeout)
                .await
            {
                Some((handle, result)) => {
                    if !pending.remove(&handle) {
                        // Stale completion from a task that already timed
                        // out in a previous round: the MVP limitation
                        // above means a timed-out task is never cancelled,
                        // it keeps running unobserved and can eventually
                        // deliver its real result here, long after that
                        // earlier round already persisted a synthetic
                        // timeout `ToolCallCompleted` for the same
                        // `tool_call_id`. Persisting this one too would
                        // append a *second* `tool_result` for the same
                        // `tool_use_id` — Anthropic rejects that with a
                        // permanent 400 on every subsequent turn, since the
                        // rollout log is append-only. Discard it instead.
                        tracing::warn!(
                            ?handle,
                            tool_call_id = %result.tool_call_id,
                            "discarding a stale tool completion from a previous round"
                        );
                        continue;
                    }
                    let id = handle_to_id
                        .remove(&handle)
                        .unwrap_or_else(|| result.tool_call_id.clone());
                    tracing::debug!(
                        tool_call_id = %id,
                        is_error = result.is_error,
                        "tool call completed"
                    );
                    // F6 (docs/AUDITORIA-2026-07-v3.md): a successful
                    // mutating tool call invalidates the whole
                    // repeated-call streak — any prior `read_file`
                    // (or other) call recorded in `seen_calls` might now
                    // return something different, so a later identical
                    // repeat must be allowed to actually re-run instead
                    // of being nudged with a claim ("the result has not
                    // changed") that's no longer true.
                    if !result.is_error
                        && id_to_name
                            .get(&id)
                            .is_some_and(|name| MUTATING_TOOL_NAMES.contains(&name.as_str()))
                    {
                        seen_calls.clear();
                    }
                    // Guardar el resultado para poder servirlo si el
                    // modelo repite la llamada. Solo los exitosos: re-servir
                    // un error no ayuda y el modelo debe poder reintentar.
                    if !result.is_error
                        && let Some(key) = id_to_key.get(&id)
                        && let Some(slot) = seen_calls.get_mut(key)
                    {
                        *slot = Some(result.content.clone());
                    }
                    if id_to_name
                        .get(&id)
                        .is_some_and(|name| FILE_MUTATING_TOOL_NAMES.contains(&name.as_str()))
                    {
                        self.turn_attempted_edit
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        if !result.is_error {
                            self.turn_did_edit
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    // Nota de relectura improductiva: se anexa al
                    // resultado EXITOSO (nunca convierte una lectura
                    // válida en error — leer un archivo largo por trozos
                    // es legítimo; el bucle observado no lo era).
                    let mut result = result;
                    if !result.is_error
                        && let Some(nudge) = reread_nudge.remove(&id)
                    {
                        tracing::info!(id = %id, "appending unproductive-reread nudge");
                        result.content.push_str(&nudge);
                    }
                    self.append_and_notify(
                        session,
                        &AgentEvent::ToolCallCompleted { id, result },
                        observer,
                    )
                    .await?;
                }
                None => {
                    // Incidente roam #3 (2026-07-20): el timeout existe
                    // para matar una EJECUCIÓN desbocada, no para apurar
                    // a un humano. Si en esta ventana el harness estuvo
                    // bloqueado esperando una decisión de permiso — o si
                    // sigue esperándola ahora mismo — el reloj se
                    // reinicia por esa deliberación en vez de cancelar:
                    // en producción, un `shell_exec` que pedía aprobación
                    // moría a los 120s mientras la persona miraba el
                    // overlay, el modelo recibía "tool call timed out",
                    // reintentaba, y el usuario aprobaba dos veces la
                    // misma acción. Ver `braze_permissions::human_wait`.
                    let human_waited = braze_permissions::human_wait_accumulated();
                    if braze_permissions::human_is_waiting()
                        || human_waited > human_wait_at_window_start
                    {
                        tracing::info!(
                            pending = pending.len(),
                            waited_ms =
                                (human_waited - human_wait_at_window_start).as_millis() as u64,
                            still_waiting = braze_permissions::human_is_waiting(),
                            "tool completion window elapsed while blocked on a human                              decision; extending instead of cancelling"
                        );
                        human_wait_at_window_start = human_waited;
                        continue;
                    }
                    tracing::error!(
                        pending = pending.len(),
                        timeout_secs = self.tool_completion_timeout.as_secs(),
                        "timed out waiting for background tool task(s); aborting and treating remaining as failed"
                    );
                    for handle in pending.drain() {
                        self.notifier.abort(handle);
                        let id = handle_to_id.remove(&handle).unwrap_or_default();
                        self.append_and_notify(
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
                            observer,
                        )
                        .await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// I.7 (`crate::exploration`): the isolated child loop behind the
    /// `explore` tool. Runs the SAME `ModelBackend` as the executor —
    /// the point of the pre-registered design: any gain is attributable
    /// to context isolation, never to added capability — against a
    /// disposable in-memory transcript, with only the read-only tools
    /// (`CHILD_READ_ONLY_TOOLS`) dispatched directly (no permission
    /// guard needed by construction) and a low round cap. Every failure
    /// mode degrades to a recoverable tool result: the lever must never
    /// kill the parent turn.
    pub(super) async fn run_exploration(
        &self,
        question: &str,
    ) -> crate::exploration::ExplorationOutcome {
        use crate::exploration::{
            CHILD_PROMPT_ADDENDUM, CHILD_READ_ONLY_TOOLS, EXPLORATION_FAILED_RESULT,
            MAX_CHILD_ROUNDS,
        };

        let failed =
            |rounds: u32, input: u64, output: u64| crate::exploration::ExplorationOutcome {
                content: EXPLORATION_FAILED_RESULT.to_string(),
                is_error: true,
                child_rounds: rounds,
                child_input_tokens: input,
                child_output_tokens: output,
            };

        // The child's inventory: only the read-only subset the registry
        // actually has (a registry without `grep` just yields a smaller
        // child inventory, not an error).
        let child_stubs: Vec<ToolStub> = match self.tools.all_stubs().await {
            Ok(stubs) => stubs
                .into_iter()
                .filter(|s| CHILD_READ_ONLY_TOOLS.contains(&s.name.as_str()))
                .collect(),
            Err(_) => Vec::new(),
        };
        let system_prompt = format!("{}{}", self.system_prompt, CHILD_PROMPT_ADDENDUM);

        let mut messages = vec![Message::text(Role::User, question.to_string())];
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut rounds: u32 = 0;

        while rounds < MAX_CHILD_ROUNDS {
            rounds += 1;
            let req = CompletionRequest {
                messages: messages.clone(),
                tool_stubs: child_stubs.clone(),
                system_prompt: system_prompt.clone(),
                max_tokens: self.max_tokens,
            };
            let mut stream = match self.model.complete(req).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::warn!(error = %err, "exploration child: completion failed");
                    return failed(rounds, input_tokens, output_tokens);
                }
            };

            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut stream_failed = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(CompletionEvent::TextDelta(delta)) => text.push_str(&delta),
                    Ok(CompletionEvent::ToolCallRequested {
                        id,
                        name,
                        arguments,
                    }) => calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    }),
                    Ok(CompletionEvent::Usage {
                        input_tokens: it,
                        output_tokens: ot,
                        ..
                    }) => {
                        input_tokens += u64::from(it);
                        output_tokens += u64::from(ot);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "exploration child: stream failed");
                        stream_failed = true;
                        break;
                    }
                }
            }
            if stream_failed {
                return failed(rounds, input_tokens, output_tokens);
            }

            if calls.is_empty() {
                let conclusion = text.trim();
                if conclusion.is_empty() {
                    return failed(rounds, input_tokens, output_tokens);
                }
                return crate::exploration::ExplorationOutcome {
                    content: conclusion.to_string(),
                    is_error: false,
                    child_rounds: rounds,
                    child_input_tokens: input_tokens,
                    child_output_tokens: output_tokens,
                };
            }

            // Append the assistant round and dispatch the read-only
            // calls directly — anything outside the allowlist gets a
            // recoverable error result (depth 1: `explore` itself is
            // never in the allowlist).
            let mut assistant_content: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                assistant_content.push(ContentBlock::Text { text: text.clone() });
            }
            for call in &calls {
                assistant_content.push(ContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.arguments.clone(),
                });
            }
            messages.push(Message {
                role: Role::Assistant,
                content: assistant_content,
            });

            let mut result_content: Vec<ContentBlock> = Vec::new();
            for call in &calls {
                let result = if CHILD_READ_ONLY_TOOLS.contains(&call.name.as_str()) {
                    match self.tools.dispatch(call).await {
                        Ok(result) => result,
                        Err(err) => ToolResult {
                            tool_call_id: call.id.clone(),
                            content: err.to_string(),
                            is_error: true,
                        },
                    }
                } else {
                    ToolResult {
                        tool_call_id: call.id.clone(),
                        content: format!(
                            "'{}' is not available here — this exploration helper can only \
                             use read_file, grep, and glob",
                            call.name
                        ),
                        is_error: true,
                    }
                };
                result_content.push(ContentBlock::ToolResult {
                    tool_use_id: result.tool_call_id,
                    content: result.content,
                    is_error: result.is_error,
                });
            }
            messages.push(Message {
                role: Role::User,
                content: result_content,
            });
        }

        failed(rounds, input_tokens, output_tokens)
    }
}

/// Sugerencia "¿quisiste decir X?" para una tool inexistente — incidente
/// roam #9 (2026-07-20): gpt-oss:20b llamó tres veces en un mismo turno
/// a `search`, que no existe, quemando tres rondas; el error listaba las
/// tools válidas pero no señalaba la obvia (`grep`). Heurística
/// deliberadamente tonta —substring en cualquier dirección, y si no,
/// distancia de edición 1-2 sobre nombres cortos— porque el objetivo es
/// nombrar UN candidato evidente, no resolver búsqueda difusa. Devuelve
/// "" cuando no hay candidato claro, para no inventar pistas.
/// Hallucinated names that are *synonyms*, not typos — no edit-distance
/// bound can ever catch `search` → `grep`, and containment cannot either.
///
/// Measured over the 12 most recent LocalBackend sessions (2026-07-28,
/// `docs/roam-metrics-memoria-2026-07-28.md`): 11 calls to tools that do
/// not exist, `search` ×7 and a truncated `read...` ×4. In the turn that
/// failed, `search` alone consumed 5 of the 20 rounds — and when the
/// model finally reached for `grep`, it found what it needed on the
/// first try. The cost was never capability, only budget.
///
/// We suggest rather than silently dispatch the synonym: argument
/// schemas differ between the invented name and the real one (the
/// observed `search{path, pattern}` only *happens* to fit `grep`), and a
/// remap that guesses wrong executes the wrong thing instead of
/// returning a correctable error.
const TOOL_NAME_SYNONYMS: &[(&str, &str)] = &[
    ("search", "grep"),
    ("find", "glob"),
    ("bash", "shell_exec"),
    ("run", "shell_exec"),
    ("cat", "read_file"),
    ("ls", "glob"),
    ("str_replace", "edit_file"),
];

fn did_you_mean(requested: &str, available: &[&str]) -> String {
    // Small models emit truncated names with an ellipsis (`read...`,
    // the same tic already documented for paths); strip it before
    // matching, which alone lets the containment rule below resolve it.
    let req = requested
        .to_lowercase()
        .trim_end_matches(['.', '…', ' '])
        .to_string();
    if req.is_empty() {
        return String::new();
    }

    let synonym = TOOL_NAME_SYNONYMS
        .iter()
        .find(|(wrong, _)| *wrong == req)
        .and_then(|(_, right)| available.iter().find(|n| n.eq_ignore_ascii_case(right)));

    let best = synonym.or_else(|| {
        available.iter().find(|name| {
            let n = name.to_lowercase();
            n.contains(&req) || req.contains(&n) || edit_distance_at_most_2(&req, &n)
        })
    });
    match best {
        Some(name) => format!(" Did you mean '{name}'?"),
        None => String::new(),
    }
}

/// Distancia de Levenshtein acotada a 2 — suficiente para typos
/// (`read_fil`, `grepp`) sin traer una dependencia ni comparar nombres
/// que solo comparten longitud.
fn edit_distance_at_most_2(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 2 {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] <= 2
}

#[cfg(test)]
mod dispatch_tests {
    use super::{did_you_mean, edit_distance_at_most_2};

    /// Typos cercanos se resuelven por distancia de edición, y un nombre
    /// que no se parece a nada sigue sin pista inventada.
    #[test]
    fn suggests_a_close_name_and_stays_quiet_otherwise() {
        let tools = [
            "read_file",
            "write_file",
            "edit_file",
            "shell_exec",
            "grep",
            "glob",
        ];
        assert_eq!(did_you_mean("grepp", &tools), " Did you mean 'grep'?");
        assert_eq!(
            did_you_mean("read_fil", &tools),
            " Did you mean 'read_file'?"
        );
        assert_eq!(did_you_mean("xyzzy", &tools), "");
    }

    /// Reversión deliberada (2026-07-28): este test afirmaba que `search`
    /// no debía sugerir nada, con el argumento de que no se parece
    /// léxicamente a `grep` y la heurística no debe inventar. La medición
    /// lo desmintió — `search` no es un parecido dudoso, es el sinónimo
    /// que el modelo usa: 7 de las 11 llamadas a tools inexistentes en 12
    /// sesiones, y 5 rondas de 20 en el turno que falló. Ninguna cota de
    /// distancia de edición puede cubrirlo; hace falta la tabla.
    #[test]
    fn known_synonyms_resolve_even_without_lexical_similarity() {
        let tools = ["read_file", "shell_exec", "grep", "glob", "edit_file"];
        assert_eq!(did_you_mean("search", &tools), " Did you mean 'grep'?");
        assert_eq!(did_you_mean("find", &tools), " Did you mean 'glob'?");
        assert_eq!(did_you_mean("bash", &tools), " Did you mean 'shell_exec'?");
        assert_eq!(did_you_mean("cat", &tools), " Did you mean 'read_file'?");

        // El sinónimo solo se sugiere si la tool real está disponible en
        // esta ronda — si no, mejor callar que mandar a una tool ausente.
        assert_eq!(did_you_mean("search", &["read_file"]), "");
    }

    /// El otro tic medido: el nombre de la tool sale truncado con puntos
    /// suspensivos (`read...`), igual que ya pasaba con las rutas. Sacar
    /// la elipsis basta para que la regla de contención lo resuelva.
    #[test]
    fn truncated_tool_names_lose_their_ellipsis_before_matching() {
        let tools = ["read_file", "grep"];
        assert_eq!(
            did_you_mean("read...", &tools),
            " Did you mean 'read_file'?"
        );
        assert_eq!(did_you_mean("...", &tools), "");
    }

    /// Un nombre que CONTIENE a uno válido (o al revés) cuenta como
    /// candidato: `file_read`/`read` son errores típicos de un modelo
    /// que recuerda la familia pero no el nombre exacto.
    #[test]
    fn substring_matches_count_as_candidates() {
        let tools = ["read_file", "grep"];
        assert_eq!(did_you_mean("read", &tools), " Did you mean 'read_file'?");
    }

    #[test]
    fn edit_distance_is_bounded() {
        assert!(edit_distance_at_most_2("grep", "grepp"));
        assert!(!edit_distance_at_most_2("grep", "shell_exec"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 paso 7: tests de integración movidos del mod tests de
    // engine/mod.rs — fixtures compartidas en engine/test_support.rs.
    use crate::engine::Engine;
    use crate::engine::test_support::*;
    use braze_events::NoopObserver;
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_types::SessionId;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Regression test for N-33 (docs/AUDITORIA-2026-07-v2.md): when
    /// `dispatch_tool_calls`'s wait for a round's tool completions times
    /// out, every still-pending task must be genuinely cancelled via
    /// `TaskNotifier::abort`, not merely given up on while it keeps
    /// running unobserved.
    #[tokio::test]
    async fn a_tool_completion_timeout_actually_cancels_the_still_running_task() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let completed = Arc::new(AtomicBool::new(false));

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "slow".to_string(),
                    arguments: serde_json::json!({}),
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
            ToolRegistry::new(vec![Box::new(SlowToolProvider::new(
                Duration::from_millis(300),
                Arc::clone(&completed),
            ))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_completion_timeout(Duration::from_millis(20));

        engine
            .run_turn(&session, "please run the slow tool", &mut NoopObserver)
            .await
            .expect(
                "turn should still converge — the timeout is treated as a \
                 recoverable tool failure, not a hard error",
            );

        // Longer than the tool's own delay — if the background task
        // hadn't really been cancelled, the flag would be true by now.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !completed.load(Ordering::SeqCst),
            "the tool call kept running in the background after the engine \
             timed out waiting for it"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// J-13 (docs/AUDITORIA-2026-07-v7.md): a tool marked untimed
    /// (interactive — `ask_user` in production) dispatches inline and is
    /// exempt from `tool_completion_timeout`: the same slow tool that the
    /// N-33 test above proves gets CANCELLED under the 20ms clock must
    /// here run to completion and deliver its real result — a human
    /// answering slowly is not a hung tool.
    #[tokio::test]
    async fn an_untimed_tool_outlives_the_completion_timeout_and_delivers_its_result() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let completed = Arc::new(AtomicBool::new(false));

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "slow".to_string(),
                    arguments: serde_json::json!({}),
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
            ToolRegistry::new(vec![Box::new(SlowToolProvider::new(
                Duration::from_millis(300),
                Arc::clone(&completed),
            ))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_completion_timeout(Duration::from_millis(20))
        .with_untimed_tool("slow");

        engine
            .run_turn(&session, "please run the slow tool", &mut NoopObserver)
            .await
            .expect("turn must converge with the tool's real result");

        assert!(
            completed.load(Ordering::SeqCst),
            "the untimed tool must have actually run to completion"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let result = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { result, .. } => Some(result),
                _ => None,
            })
            .expect("the tool completion must be persisted");
        assert!(!result.is_error, "got: {}", result.content);
        assert_eq!(result.content, "done");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for docs/AUDITORIA-2026-07-v2.md hallazgo N-14.
    ///
    /// Nothing previously checked that a `ToolCallRequested`'s id was
    /// unique before persisting it as an `AssistantToolCall` — a model
    /// that (accidentally or via a buggy backend's synthetic-id fallback)
    /// issues two calls sharing one id would get two `tool_use`/
    /// `tool_result` pairs with the same id in the append-only log,
    /// which Anthropic rejects permanently on every future request.
    #[tokio::test]
    async fn duplicate_tool_use_ids_in_one_round_are_renamed_to_stay_unique() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ProtocolValidatingModel::new(ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "a" }),
                },
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "b" }),
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
            .run_turn(&session, "please echo two things", &mut NoopObserver)
            .await
            .expect(
                "turn should succeed despite the duplicate id — and every \
                     request built along the way must still pass protocol validation",
            );

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            2,
            "both calls have different arguments, so both must dispatch \
             (not be treated as an identical repeated call)"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let tool_use_ids: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_use_ids.len(), 2);
        assert_ne!(
            tool_use_ids[0], tool_use_ids[1],
            "the second call's id must have been renamed to stay unique"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
