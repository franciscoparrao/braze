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
            edit_failures_by_path,
        } = state;

        // Nota de relectura improductiva por id de llamada — ver
        // `TurnDispatchState::reads_by_path`. Se calcula al despachar
        // (donde se conocen nombre y argumentos) y se anexa al resultado
        // exitoso, sin bloquearlo.
        let mut reread_nudge: HashMap<String, String> = HashMap::new();
        // Interlock L-10: resuelve el id de un `edit_file` completado de
        // vuelta a su ruta, para poder acreditar el fallo (o el éxito que
        // resetea) a `edit_failures_by_path` cuando el resultado llega por
        // el camino background, donde ya no está la llamada a mano.
        let mut id_to_edit_path: HashMap<String, String> = HashMap::new();
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

            // Interlock duro de `write_file` (v9 L-10): un `write_file`
            // sobre una ruta cuyo `edit_file` ya falló
            // `EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD` veces en este turno
            // se bloquea sin despachar. La rama que cierra: el modelo que
            // no puede REPRODUCIR el contenido (caracteres que entiende y
            // no puede emitir, hallazgo 2026-07-28) cae de la edición
            // dirigida que falla honesto a la reescritura total que
            // corrompe en silencio. El error es accionable y deja las
            // salidas legítimas abiertas: reintentar la edición con un
            // old_string más corto, o reportar el bloqueo — que es el
            // rechazo honesto que la verificación en vivo mostró como el
            // buen desenlace (deadlock de 20 rondas → rechazo en 4).
            //
            // Como el gate J-9: la llamada bloqueada NO se registra en
            // `seen_calls` — un `write_file` legítimo posterior (tras un
            // `edit_file` exitoso que resetea el contador) debe poder
            // despachar sin que el nudge de repetición mienta.
            if call.name == "write_file"
                && let Some(path) = call.arguments.get("path").and_then(|v| v.as_str())
                && edit_failures_by_path
                    .get(path)
                    .is_some_and(|n| *n >= EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD)
            {
                tracing::warn!(
                    path,
                    "blocked write_file after repeated edit_file failures on the same path \
                     (hard interlock, v9 L-10)"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content: format!(
                                "write_file on '{path}' is blocked for the rest of this turn: \
                                 edit_file already failed on that file {} times, and rewriting \
                                 the whole file after failed targeted edits is how content gets \
                                 silently corrupted (the same mismatch that broke the edits \
                                 would be written over the entire file). Either retry edit_file \
                                 with a shorter old_string copied EXACTLY from the latest \
                                 read_file output, or stop and report honestly that you cannot \
                                 make this edit.",
                                EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD
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
                // Interlock L-10: un repeat IDÉNTICO de un `edit_file` que
                // no tiene resultado exitoso guardado cuenta como fallo
                // hacia el umbral. Es exactamente el caso de producción
                // que motivó el interlock — un modelo que no puede emitir
                // un carácter reproduce la MISMA llamada corrupta cada
                // vez, así que sus reintentos caen todos acá (nudgeados,
                // nunca re-despachados) y sin este conteo el contador se
                // quedaría en 1 mientras el modelo se descuelga a
                // write_file.
                if call.name == "edit_file"
                    && previous.is_none()
                    && let Some(path) = call.arguments.get("path").and_then(|v| v.as_str())
                {
                    *edit_failures_by_path.entry(path.to_string()).or_insert(0) += 1;
                }
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

            // SWE-Edit #17 (crate::editor): el subagente editor — mismo
            // patrón de intercepción inline que explore, pero MUTA, así
            // que además del doble-entry hay bookkeeping del padre
            // (turn flags + seen_calls) que explore, read-only, no hace.
            if self.editor_enabled && call.name == crate::editor::EDITOR_TOOL {
                let path = call
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let instruction = call
                    .arguments
                    .get("instruction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let outcome = if path.trim().is_empty() || instruction.trim().is_empty() {
                    crate::editor::EditorOutcome {
                        content: "editor needs non-empty 'path' and 'instruction' strings"
                            .to_string(),
                        is_error: true,
                        landed: false,
                        compiles: crate::editor::CompileStatus::Unknown,
                        child_rounds: 0,
                        child_input_tokens: 0,
                        child_output_tokens: 0,
                    }
                } else {
                    self.run_editor(&path, &instruction).await
                };

                // Bookkeeping del padre: una mutación pasó FUERA de su
                // dispatch, así que hay que replicar lo que
                // `dispatch_tool_calls` hace tras una edición directa —
                // sin esto, la salvage de ronda vacía y las notas de
                // convergencia de turn.rs malinterpretan el turno.
                self.turn_attempted_edit
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                if outcome.landed {
                    self.turn_did_edit
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    // F6: una mutación exitosa invalida la caché de
                    // repetición del turno.
                    seen_calls.clear();
                    // L-10: una edición que aterrizó resetea el contador
                    // del padre para esa ruta (el modelo recuperó la
                    // edición dirigida, aunque haya sido vía el hijo).
                    edit_failures_by_path.remove(&path);
                }

                let child_tokens = outcome.child_input_tokens + outcome.child_output_tokens;
                let compiles = match outcome.compiles {
                    crate::editor::CompileStatus::Pass => "pass",
                    crate::editor::CompileStatus::Fail => "fail",
                    crate::editor::CompileStatus::Unknown => "unknown",
                };
                // Doble entrada como explore: Usage agregado (para que
                // toda contabilidad de tokens lo cuente gratis) + evento
                // de auditoría con el ground truth que el A/B lee.
                if child_tokens > 0 {
                    self.append_and_notify(
                        session,
                        &AgentEvent::Usage {
                            input_tokens: outcome.child_input_tokens.min(u32::MAX as u64) as u32,
                            output_tokens: outcome.child_output_tokens.min(u32::MAX as u64) as u32,
                            stop_reason: Some("editor_child".to_string()),
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                        },
                        observer,
                    )
                    .await?;
                }
                self.append_and_notify(
                    session,
                    &AgentEvent::EditorDelegated {
                        path,
                        instruction,
                        landed: outcome.landed,
                        compiles: compiles.to_string(),
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
                if call.name == "edit_file" {
                    id_to_edit_path.insert(call.id.clone(), path.clone());
                }
                // Carga JIT de AGENTS.md por subdirectorio
                // (docs/agents-md-jit-design-2026-08-11.md): el tool tocó
                // `path`; descubrir el AGENTS.md más cercano subiendo
                // hasta el techo del proyecto e inyectarlo para las
                // rondas siguientes. Solo con el lever prendido
                // (`agents_md_root.is_some()`); no-op barato si no.
                self.maybe_discover_agents_md(session, &path, observer)
                    .await?;
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
                // Espejo del interlock L-10 del camino background — un
                // edit_file no es interactivo hoy, pero el invariante no
                // debe depender de esa expectativa (mismo argumento que
                // el F6 de arriba).
                if let Some(path) = id_to_edit_path.remove(&call.id) {
                    if result.is_error {
                        *edit_failures_by_path.entry(path).or_insert(0) += 1;
                    } else {
                        edit_failures_by_path.remove(&path);
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
                    // Interlock L-10: acreditar el desenlace del edit_file
                    // a su ruta — un fallo suma hacia el umbral que
                    // bloquea write_file; un éxito resetea el contador (el
                    // modelo recuperó la edición dirigida).
                    if let Some(path) = id_to_edit_path.remove(&id) {
                        if result.is_error {
                            let n = edit_failures_by_path.entry(path.clone()).or_insert(0);
                            *n += 1;
                            if *n >= EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD {
                                tracing::info!(
                                    path,
                                    failures = *n,
                                    "edit_file failure threshold reached — write_file on this \
                                     path is now blocked for the rest of the turn"
                                );
                            }
                        } else {
                            edit_failures_by_path.remove(&path);
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

    /// SWE-Edit #17 — el mini-loop del subagente `editor`
    /// (`docs/editor-subagent-design-2026-08-10.md`). Espejo de
    /// [`Engine::run_exploration`] con capacidad de MUTACIÓN, así que:
    /// despacha `edit_file`/`write_file` por `self.tools.dispatch` (hereda
    /// guard + workdir + post-edit check + Landlock del provider),
    /// mantiene su PROPIO interlock L-10 (el del padre vive en
    /// `TurnDispatchState`, inalcanzable acá), y devuelve un
    /// [`EditorOutcome`] estructurado con ground truth (`landed`,
    /// `compiles`) para que el padre sepa el estado del workspace sin
    /// releer. Toda falla degrada a un resultado recuperable: el lever
    /// nunca mata el turno del padre.
    pub(super) async fn run_editor(
        &self,
        path: &str,
        instruction: &str,
    ) -> crate::editor::EditorOutcome {
        use crate::editor::{
            CHILD_EDIT_TOOLS, CHILD_PROMPT_ADDENDUM, CompileStatus, EDITOR_FAILED_RESULT,
            EditorOutcome, MAX_EDITOR_CHILD_ROUNDS, compile_status_from_result,
        };

        // Estado del hijo, acumulado a través de sus rondas.
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut rounds: u32 = 0;
        let mut landed = false;
        let mut compiles = CompileStatus::Unknown;
        // Interlock L-10 propio del hijo (fresco por delegación).
        let mut edit_failures: u32 = 0;

        // No-convergencia: el archivo puede haber quedado a medias si ya
        // hubo una edición exitosa — se lo decimos al padre para que
        // relea en vez de asumir estado limpio (el peor caso del diseño).
        let not_converged =
            |rounds: u32, input: u64, output: u64, landed: bool, compiles: CompileStatus| {
                let mut content = EDITOR_FAILED_RESULT.to_string();
                if landed {
                    content.push_str(&format!(
                        "; the file '{path}' was left partially modified — read it before retrying."
                    ));
                }
                EditorOutcome {
                    content,
                    is_error: true,
                    landed,
                    compiles,
                    child_rounds: rounds,
                    child_input_tokens: input,
                    child_output_tokens: output,
                }
            };

        let child_stubs: Vec<ToolStub> = match self.tools.all_stubs().await {
            Ok(stubs) => stubs
                .into_iter()
                .filter(|s| CHILD_EDIT_TOOLS.contains(&s.name.as_str()))
                .collect(),
            Err(_) => Vec::new(),
        };
        let system_prompt = format!("{}{}", self.system_prompt, CHILD_PROMPT_ADDENDUM);
        let seed = format!("File to edit: {path}\n\nInstruction: {instruction}");
        let mut messages = vec![Message::text(Role::User, seed)];

        while rounds < MAX_EDITOR_CHILD_ROUNDS {
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
                    tracing::warn!(error = %err, "editor child: completion failed");
                    return not_converged(rounds, input_tokens, output_tokens, landed, compiles);
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
                        tracing::warn!(error = %err, "editor child: stream failed");
                        stream_failed = true;
                        break;
                    }
                }
            }
            if stream_failed {
                return not_converged(rounds, input_tokens, output_tokens, landed, compiles);
            }

            // Convergencia: el hijo respondió sin tool calls — su texto es
            // el resumen de estado. Un árbol roto (compiles==Fail) es un
            // error aunque el hijo diga "listo": nunca devolver limpio
            // sobre código que no compila.
            if calls.is_empty() {
                let conclusion = text.trim();
                if conclusion.is_empty() {
                    return not_converged(rounds, input_tokens, output_tokens, landed, compiles);
                }
                let is_error = compiles == CompileStatus::Fail;
                let mut content = conclusion.to_string();
                if is_error {
                    content.push_str(
                        "\n\n[harness] the edit was applied but the file does NOT compile — \
                         read it and fix or revert before continuing.",
                    );
                }
                return EditorOutcome {
                    content,
                    is_error,
                    landed,
                    compiles,
                    child_rounds: rounds,
                    child_input_tokens: input_tokens,
                    child_output_tokens: output_tokens,
                };
            }

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
                let result = if !CHILD_EDIT_TOOLS.contains(&call.name.as_str()) {
                    ToolResult {
                        tool_call_id: call.id.clone(),
                        content: format!(
                            "'{}' is not available here — this edit helper can only use \
                             read_file, edit_file, and write_file",
                            call.name
                        ),
                        is_error: true,
                    }
                } else if call.name == "write_file"
                    && edit_failures >= EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD
                {
                    // Interlock L-10 propio del hijo: reescribir el archivo
                    // entero tras ediciones fallidas es donde el contenido
                    // se corrompe en silencio.
                    tracing::warn!(
                        path,
                        "editor child: write_file blocked after repeated edit_file failures"
                    );
                    ToolResult {
                        tool_call_id: call.id.clone(),
                        content: format!(
                            "write_file is blocked: edit_file already failed {edit_failures} \
                             times here, and rewriting the whole file after failed edits is how \
                             content gets silently corrupted. Retry edit_file with a shorter \
                             old_string copied exactly, or stop and report you cannot make the \
                             edit."
                        ),
                        is_error: true,
                    }
                } else {
                    match self.tools.dispatch(call).await {
                        Ok(result) => result,
                        Err(err) => ToolResult {
                            tool_call_id: call.id.clone(),
                            content: err.to_string(),
                            is_error: true,
                        },
                    }
                };

                // Ground truth desde el dispatch, no del auto-reporte:
                // acredita landed/compiles y el interlock del hijo.
                if call.name == "edit_file" || call.name == "write_file" {
                    if result.is_error {
                        edit_failures += 1;
                    } else {
                        landed = true;
                        edit_failures = 0;
                        compiles = compile_status_from_result(&result.content);
                    }
                }

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

        not_converged(rounds, input_tokens, output_tokens, landed, compiles)
    }

    /// Carga JIT de AGENTS.md por subdirectorio
    /// (docs/agents-md-jit-design-2026-08-11.md). Un tool tocó `raw_path`;
    /// si la feature está prendida (`agents_md_root.is_some()`), descubre
    /// el `AGENTS.md` más cercano subiendo hasta el techo del proyecto,
    /// y —si es nuevo y no se pasó del tope de la sesión— lo carga en
    /// `loaded_agents_md_bodies` (inyectado por
    /// `system_prompt_with_skills` en las rondas siguientes) y persiste
    /// `AgentsMdLoaded` para auditoría y rehidratación en `--resume`.
    ///
    /// Barato y silencioso cuando no hay nada que descubrir: el caso
    /// común (proyecto sin AGENTS.md de subdir) es un walk-up que no
    /// encuentra archivo y no toca estado.
    pub(super) async fn maybe_discover_agents_md(
        &self,
        session: &SessionId,
        raw_path: &str,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        let Some(root) = &self.agents_md_root else {
            return Ok(());
        };
        // El directorio del archivo tocado — el "piso" del walk-up.
        // Resuelto contra el techo si es relativo (el `path` de una tool
        // puede venir relativo al workdir, que ES el proyecto).
        let path = std::path::Path::new(raw_path);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let Some(from_dir) = abs.parent() else {
            return Ok(());
        };
        let Some(found) = braze_config::find_nearest_agents_md(from_dir, root) else {
            return Ok(());
        };
        // Dedup por path canónico; el raíz ya está sembrado.
        {
            let mut loaded = self.loaded_agents_md.lock().unwrap();
            if loaded.contains(&found) {
                return Ok(());
            }
            // Tope de cuenta por sesión: no inflar el prompt sin límite —
            // el prompt chico es el punto de la feature. `len()-1` porque
            // el raíz sembrado ocupa un slot que no es un body inyectado.
            if self.loaded_agents_md_bodies.lock().unwrap().len() >= AGENTS_MD_JIT_MAX_FILES {
                // Se marca como visto para no reintentar el mismo cada
                // ronda, pero no se inyecta.
                loaded.insert(found.clone());
                tracing::warn!(
                    path = %found.display(),
                    max = AGENTS_MD_JIT_MAX_FILES,
                    "AGENTS.md JIT alcanzó el tope de archivos por sesión; se ignora este"
                );
                return Ok(());
            }
            loaded.insert(found.clone());
        }
        // Leer el body (fuera del lock — I/O). Si desapareció entre el
        // find y ahora, se descarta silencioso (ya está en el set, no se
        // reintenta).
        let Some(dir) = found.parent() else {
            return Ok(());
        };
        let Some(body) = braze_config::load_agents_md_from(dir) else {
            return Ok(());
        };
        self.loaded_agents_md_bodies.lock().unwrap().push(body);
        self.append_and_notify(
            session,
            &AgentEvent::AgentsMdLoaded {
                path: found.to_string_lossy().into_owned(),
            },
            observer,
        )
        .await?;
        tracing::info!(path = %found.display(), "AGENTS.md de subdirectorio cargado JIT");
        Ok(())
    }
}

/// Tope de `AGENTS.md` de subdirectorio que la carga JIT inyecta por
/// sesión (docs/agents-md-jit-design-2026-08-11.md): una sesión que barre
/// medio repo no puede inflar el system prompt sin límite — el prompt
/// chico es justo lo que la feature protege. Descubrimientos más allá de
/// este tope se marcan como vistos (no se reintentan) pero no se inyectan.
const AGENTS_MD_JIT_MAX_FILES: usize = 8;

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
    use async_trait::async_trait;
    use braze_events::NoopObserver;
    use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
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

    // P1.1 resto (v9 L-5): schema-repair y repeated-call movidos
    // VERBATIM del mod tests de engine/mod.rs.

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
            .run_turn(&session, "please echo hi", &mut NoopObserver)
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
            vec![
                CompletionEvent::TextDelta("giving up".to_string()),
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
            .run_turn(&session, "please echo hi", &mut NoopObserver)
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

    /// Regression test for A5: the model repeating an identical
    /// (name, arguments) tool call within the same turn must be nudged
    /// instead of re-dispatched — the dominant non-convergence pattern for
    /// small/local models, which otherwise burn a round (and, in Ollama's
    /// case, real CPU time) re-running a call whose result can't change.
    #[tokio::test]
    async fn an_identical_repeated_tool_call_is_served_from_cache_not_re_dispatched() {
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
                // Same tool, same arguments, different id — a small model
                // re-issuing the identical call instead of using the
                // result it already has.
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
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

        engine
            .run_turn(&session, "please echo hi twice", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        // La invariante que de verdad protege esta palanca: la tool REAL
        // corrió una sola vez. La repetición no se re-despacha (sin efectos
        // secundarios, sin costo repetido).
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let first = match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-1"))
            .expect("expected a ToolCallCompleted for call-1")
        {
            AgentEvent::ToolCallCompleted { result, .. } => result.content.clone(),
            _ => unreachable!(),
        };
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-2"))
            .expect("expected a ToolCallCompleted for call-2")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                // La repetición se responde CON el resultado anterior, no con
                // una negativa. Negarse dejaba al modelo pidiendo algo que el
                // colapso ACI ya le había borrado del contexto: medido contra
                // roam (2026-07-26), gastó 4 llamadas y abandonó el turno.
                assert!(
                    !result.is_error,
                    "servir el resultado cacheado no es un error"
                );
                assert!(
                    result.content.contains(&first),
                    "la repetición debe traer el contenido del resultado original"
                );
                assert!(
                    result.content.contains("caché"),
                    "y debe decir que viene de caché, para que el modelo no crea que re-ejecutó"
                );
            }
            _ => unreachable!(),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F6 (docs/AUDITORIA-2026-07-v3.md): `read_file(x)` → `write_file(x)`
    /// → `read_file(x)` again is a legitimate re-verification pattern —
    /// the second `read_file` must actually re-run (the write may have
    /// changed what it returns), not get nudged with a now-false "the
    /// result has not changed" claim.
    #[tokio::test]
    async fn a_repeated_read_after_a_mutating_call_actually_redispatches() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let read_args = serde_json::json!({ "path": "x.txt" });
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: read_args.clone(),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "x.txt", "content": "new" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                // Same (name, arguments) as call-1 — but a write happened
                // in between, so this must actually re-run.
                CompletionEvent::ToolCallRequested {
                    id: "call-3".to_string(),
                    name: "read_file".to_string(),
                    arguments: read_args,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let read_invocations = Arc::new(AtomicU32::new(0));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::clone(
                &read_invocations,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "read x, write x, read x again", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(
            read_invocations.load(Ordering::SeqCst),
            2,
            "the second read_file, after an intervening write_file, must actually re-run"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. } if id == "call-3"))
            .expect("expected a ToolCallCompleted for call-3")
        {
            AgentEvent::ToolCallCompleted { result, .. } => {
                assert!(!result.is_error, "must not be nudged: {result:?}");
                assert_eq!(result.content, "contenido");
            }
            other => panic!("expected ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Provider de juguete para el interlock L-10: `edit_file` SIEMPRE
    /// falla (el patrón "no puedo reproducir el contenido") y
    /// `write_file` cuenta sus despachos reales — el assert central es
    /// que ese contador NO avanza cuando el interlock bloquea.
    struct EditFailingProvider {
        write_invocations: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl braze_tools_core::ToolProvider for EditFailingProvider {
        fn provider_id(&self) -> &str {
            "test:edit-failing"
        }

        async fn list_stubs(
            &self,
        ) -> Result<Vec<braze_types::ToolStub>, braze_tools_core::ToolError> {
            Ok(["edit_file", "write_file"]
                .iter()
                .map(|name| braze_types::ToolStub {
                    name: name.to_string(),
                    summary: format!("{name} de juguete"),
                    source: "test:edit-failing".to_string(),
                    input_schema: None,
                })
                .collect())
        }

        async fn resolve_schema(
            &self,
            name: &str,
        ) -> Result<Option<braze_tools_core::ToolSchema>, braze_tools_core::ToolError> {
            Ok(Some(braze_tools_core::ToolSchema {
                name: name.to_string(),
                description: format!("{name} de juguete"),
                input_schema: serde_json::json!({"type": "object"}),
            }))
        }

        async fn invoke(
            &self,
            call: &braze_types::ToolCall,
        ) -> Result<braze_types::ToolResult, braze_tools_core::ToolError> {
            match call.name.as_str() {
                "edit_file" => Ok(braze_types::ToolResult {
                    tool_call_id: call.id.clone(),
                    content: "edit_file failed: old_string not found (first divergence at byte 3)"
                        .to_string(),
                    is_error: true,
                }),
                _ => {
                    self.write_invocations.fetch_add(1, Ordering::SeqCst);
                    Ok(braze_types::ToolResult {
                        tool_call_id: call.id.clone(),
                        content: "written".to_string(),
                        is_error: false,
                    })
                }
            }
        }
    }

    fn edit_call(id: &str, path: &str) -> CompletionEvent {
        edit_call_with(id, path, "a")
    }

    fn edit_call_with(id: &str, path: &str, old: &str) -> CompletionEvent {
        CompletionEvent::ToolCallRequested {
            id: id.to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({ "path": path, "old_string": old, "new_string": "b" }),
        }
    }

    fn write_call(id: &str, path: &str) -> CompletionEvent {
        CompletionEvent::ToolCallRequested {
            id: id.to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": path, "content": "todo el archivo" }),
        }
    }

    /// Interlock L-10: tras dos fallos de `edit_file` sobre una ruta, un
    /// `write_file` sobre ESA ruta se bloquea sin despachar (el provider
    /// nunca lo ve) con un error accionable — y una ruta distinta queda
    /// fuera del interlock, que es por-archivo y no global.
    #[tokio::test]
    async fn write_file_is_blocked_after_two_edit_failures_on_the_same_path() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![edit_call("e1", "src/lib.rs"), CompletionEvent::Done],
            vec![edit_call("e2", "src/lib.rs"), CompletionEvent::Done],
            vec![write_call("w1", "src/lib.rs"), CompletionEvent::Done],
            vec![write_call("w2", "src/otro.rs"), CompletionEvent::Done],
            vec![
                CompletionEvent::TextDelta("me detengo".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let write_invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EditFailingProvider {
                write_invocations: Arc::clone(&write_invocations),
            })]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "edita src/lib.rs", &mut NoopObserver)
            .await
            .expect("el turno converge: el interlock devuelve un error de tool, no mata el turno");

        assert_eq!(
            write_invocations.load(Ordering::SeqCst),
            1,
            "el write_file sobre la ruta bloqueada NO debe llegar al provider; el de la otra ruta sí"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let blocked = events.iter().find_map(|e| match e {
            AgentEvent::ToolCallCompleted { id, result } if id == "w1" => Some(result.clone()),
            _ => None,
        });
        let blocked = blocked.expect("el write_file bloqueado persiste su ToolCallCompleted");
        assert!(blocked.is_error);
        assert!(
            blocked.content.contains("blocked"),
            "el error debe explicar el bloqueo, got: {}",
            blocked.content
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Un `edit_file` exitoso sobre la ruta resetea el contador del
    /// interlock: el fallo posterior queda en 1 (< umbral) y el
    /// `write_file` despacha normal. Sin el reset, un tropiezo temprano
    /// bloquearía la reescritura legítima de un modelo que SÍ puede
    /// editar.
    #[tokio::test]
    async fn a_successful_edit_resets_the_interlock_counter() {
        struct FlakyEditProvider {
            edit_calls: AtomicU32,
            write_invocations: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl braze_tools_core::ToolProvider for FlakyEditProvider {
            fn provider_id(&self) -> &str {
                "test:flaky-edit"
            }

            async fn list_stubs(
                &self,
            ) -> Result<Vec<braze_types::ToolStub>, braze_tools_core::ToolError> {
                Ok(["edit_file", "write_file"]
                    .iter()
                    .map(|name| braze_types::ToolStub {
                        name: name.to_string(),
                        summary: format!("{name} de juguete"),
                        source: "test:flaky-edit".to_string(),
                        input_schema: None,
                    })
                    .collect())
            }

            async fn resolve_schema(
                &self,
                name: &str,
            ) -> Result<Option<braze_tools_core::ToolSchema>, braze_tools_core::ToolError>
            {
                Ok(Some(braze_tools_core::ToolSchema {
                    name: name.to_string(),
                    description: format!("{name} de juguete"),
                    input_schema: serde_json::json!({"type": "object"}),
                }))
            }

            async fn invoke(
                &self,
                call: &braze_types::ToolCall,
            ) -> Result<braze_types::ToolResult, braze_tools_core::ToolError> {
                match call.name.as_str() {
                    // Falla el 1º y el 3º; el 2º (el del medio) tiene éxito.
                    "edit_file" => {
                        let n = self.edit_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(braze_types::ToolResult {
                            tool_call_id: call.id.clone(),
                            content: if n == 1 { "edited" } else { "edit_file failed" }.to_string(),
                            is_error: n != 1,
                        })
                    }
                    _ => {
                        self.write_invocations.fetch_add(1, Ordering::SeqCst);
                        Ok(braze_types::ToolResult {
                            tool_call_id: call.id.clone(),
                            content: "written".to_string(),
                            is_error: false,
                        })
                    }
                }
            }
        }

        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Argumentos DISTINTOS por intento — el camino del reintento
        // real que ajusta el old_string; el repeat idéntico (nudgeado)
        // se cubre en el test anterior.
        let model = ScriptedModel::new(vec![
            vec![
                edit_call_with("e1", "src/lib.rs", "a"),
                CompletionEvent::Done,
            ], // falla (1)
            vec![
                edit_call_with("e2", "src/lib.rs", "bb"),
                CompletionEvent::Done,
            ], // éxito → reset
            vec![
                edit_call_with("e3", "src/lib.rs", "ccc"),
                CompletionEvent::Done,
            ], // falla (1 de nuevo)
            vec![write_call("w1", "src/lib.rs"), CompletionEvent::Done], // bajo el umbral → despacha
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let write_invocations = Arc::new(AtomicU32::new(0));

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(FlakyEditProvider {
                edit_calls: AtomicU32::new(0),
                write_invocations: Arc::clone(&write_invocations),
            })]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "edita src/lib.rs", &mut NoopObserver)
            .await
            .expect("turno normal");

        assert_eq!(
            write_invocations.load(Ordering::SeqCst),
            1,
            "con el contador reseteado por el éxito, el write_file debe despachar"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Carga JIT de AGENTS.md por subdir
    /// (docs/agents-md-jit-design-2026-08-11.md): un `read_file` sobre un
    /// archivo en un subdir con AGENTS.md hace que (a) el request de la
    /// ronda siguiente lleve ese AGENTS.md en su system prompt, (b) se
    /// persista `AgentsMdLoaded`, (c) el raíz NUNCA se re-inyecte, y (d)
    /// un segundo touch no duplique.
    #[tokio::test]
    async fn a_subdir_agents_md_is_discovered_and_injected_next_round() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Proyecto: raíz con AGENTS.md + subdir crates/foo con el suyo.
        let root = dir.join("proj");
        let sub = root.join("crates/foo");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap(); // techo = git root
        std::fs::write(root.join("AGENTS.md"), "REGLA RAIZ").unwrap();
        std::fs::write(sub.join("AGENTS.md"), "REGLA-DE-FOO-SUBDIR").unwrap();

        // Ronda 1: read_file de crates/foo/bar.rs → dispara descubrimiento.
        // Ronda 2: texto final (su request debe llevar el addendum).
        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "r1".to_string(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({ "path": "crates/foo/bar.rs" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        let requests = Arc::clone(&model.requests);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(ReadWriteToolProvider::new(Arc::new(
                AtomicU32::new(0),
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            // El system prompt YA lleva el raíz (como en producción).
            "system prompt\n\nREGLA RAIZ".to_string(),
            1024,
        )
        .with_agents_md_jit(root.clone(), Some(root.join("AGENTS.md")));

        engine
            .run_turn(&session, "lee crates/foo/bar.rs", &mut NoopObserver)
            .await
            .expect("turno converge");

        let reqs = requests.lock().unwrap().clone();
        assert!(reqs.len() >= 2, "debe haber al menos dos rondas");
        // (a) El request de la ronda 2 lleva el AGENTS.md del subdir.
        assert!(
            reqs[1].system_prompt.contains("REGLA-DE-FOO-SUBDIR"),
            "el AGENTS.md del subdir debe entrar al system prompt de la ronda 2"
        );
        // (c) El raíz aparece UNA vez (el sembrado del dedup evita
        // re-inyectarlo como si fuera un descubrimiento).
        assert_eq!(
            reqs[1].system_prompt.matches("REGLA RAIZ").count(),
            1,
            "el AGENTS.md raíz no debe duplicarse"
        );

        // (b) Se persistió AgentsMdLoaded, exactamente una vez (d: dedup).
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let loads: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::AgentsMdLoaded { .. }))
            .collect();
        assert_eq!(loads.len(), 1, "un solo AgentsMdLoaded para el subdir");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Con la feature apagada (sin `with_agents_md_jit`), tocar un subdir
    /// con AGENTS.md NO inyecta nada — el default es no-op estricto.
    #[tokio::test]
    async fn without_the_lever_no_subdir_agents_md_is_loaded() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let sub = dir.join("proj/crates/foo");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "REGLA-DE-FOO-SUBDIR").unwrap();

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "r1".to_string(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({ "path": "proj/crates/foo/bar.rs" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        let requests = Arc::clone(&model.requests);

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

        engine
            .run_turn(&session, "lee bar.rs", &mut NoopObserver)
            .await
            .expect("turno converge");

        let reqs = requests.lock().unwrap().clone();
        assert!(
            !reqs.iter().any(|r| r.system_prompt.contains("REGLA-DE-FOO-SUBDIR")),
            "sin el lever, ningún AGENTS.md de subdir debe cargarse"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // P1.1 resto (v9 L-5, 2026-08-18): clusters I.7 (explorador),
    // C′.2 (task list tipada) y C′.1 (search_tools) movidos VERBATIM
    // del `mod tests` de engine/mod.rs.

    // --- I.7: explorador de contexto aislado
    // (docs/explorador-aislado-ab-design.md) ---

    /// The delegation round-trip: the parent calls `explore`, the child
    /// loop (same scripted backend) answers in one tools-free round, the
    /// conclusion comes back as the explore call's tool result, and the
    /// rollout log records the audit event + aggregate child usage —
    /// but NONE of the child's own transcript (isolation is the lever).
    #[tokio::test]
    async fn an_exploration_delegation_round_trips_and_stays_isolated() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Script order: parent round 1 (calls explore) → child round
        // (answers, no tools) → parent round 2 (final answer).
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-explore".to_string(),
                    name: "explore".to_string(),
                    arguments: serde_json::json!({
                        "question": "which file defines parse_header?"
                    }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("parse_header is defined in src/header.rs.".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 120,
                    output_tokens: 30,
                    stop_reason: Some("stop".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("Está en src/header.rs.".to_string()),
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
        )
        .with_exploration_enabled(true);

        engine
            .run_turn(
                &session,
                "¿dónde se define parse_header?",
                &mut NoopObserver,
            )
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        // The audit event carries the question and the child's cost.
        let delegated = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ExplorationDelegated {
                    question,
                    child_rounds,
                    child_tokens,
                } => Some((question.clone(), *child_rounds, *child_tokens)),
                _ => None,
            })
            .expect("ExplorationDelegated must be persisted");
        assert_eq!(delegated.0, "which file defines parse_header?");
        assert_eq!(delegated.1, 1, "the child answered in one round");
        assert_eq!(delegated.2, 150, "child input+output tokens summed");

        // The conclusion is the explore call's tool result…
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallCompleted { result, .. }
                    if !result.is_error
                        && result.content.contains("src/header.rs")
            )),
            "the child's conclusion must come back as the tool result"
        );
        // …and the child's transcript is NOT in the parent's log: the
        // only AssistantText is the parent's final answer.
        let assistant_texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            assistant_texts,
            vec!["Está en src/header.rs."],
            "isolation: the child's own text must never enter the parent log"
        );
        // The aggregate child usage rides as a Usage event so every
        // existing token accounting counts it.
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Usage { stop_reason: Some(reason), input_tokens: 120, output_tokens: 30, .. }
                    if reason == "exploration_child"
            )),
            "aggregate child usage must be persisted: {events:#?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The lever must never kill the turn: a child that produces nothing
    /// usable degrades to a recoverable tool error the parent can act on.
    #[tokio::test]
    async fn a_failed_exploration_degrades_to_a_recoverable_tool_error() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-explore".to_string(),
                    name: "explore".to_string(),
                    arguments: serde_json::json!({ "question": "¿algo?" }),
                },
                CompletionEvent::Done,
            ],
            // Child round: empty text, no tool calls → exploration fails.
            vec![CompletionEvent::Done],
            vec![
                CompletionEvent::TextDelta("Exploro yo mismo.".to_string()),
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
        )
        .with_exploration_enabled(true);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("the failed exploration must not kill the turn");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallCompleted { result, .. }
                    if result.is_error && result.content.contains("exploration failed")
            )),
            "expected the recoverable error result: {events:#?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- C′.2: task list tipada
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § I.4) ---

    /// With the lever ON: the two task tools join the inventory, an add
    /// + update round-trips through the harness-owned handler, and the
    /// compact summary rides the NEXT round's request as an ephemeral
    /// user message (never persisted).
    #[tokio::test]
    async fn task_tools_round_trip_and_the_summary_rides_the_next_request() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "task_add".to_string(),
                        arguments: serde_json::json!({ "description": "leer notas.txt" }),
                    },
                    CompletionEvent::ToolCallRequested {
                        id: "call-2".to_string(),
                        name: "task_add".to_string(),
                        arguments: serde_json::json!({ "description": "responder" }),
                    },
                    CompletionEvent::Done,
                ],
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-3".to_string(),
                        name: "task_update".to_string(),
                        arguments: serde_json::json!({ "id": 1, "status": "done" }),
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
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "haz dos cosas", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let requests = requests.lock().unwrap().clone();
        // Round 1: tools listed, no summary yet (empty list).
        let round1_names: Vec<&str> = requests[0]
            .tool_stubs
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(round1_names.contains(&"task_add"));
        assert!(round1_names.contains(&"task_update"));
        let round1_text: String = requests[0]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !round1_text.contains("Task list:"),
            "no summary before any task exists"
        );

        // Round 2: the summary reflects the adds.
        let round2_text: String = requests[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            round2_text.contains("1 [pending] leer notas.txt"),
            "got: {round2_text}"
        );
        assert!(round2_text.contains("2 [pending] responder"));

        // Round 3: the update shows.
        let round3_text: String = requests[2]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            round3_text.contains("1 [done] leer notas.txt"),
            "got: {round3_text}"
        );

        // The ephemeral summary must NOT be persisted as events.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessage { text } if text.contains("Task list:")
            )),
            "the summary is request-scoped, never persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// With the lever OFF (the default): no task tools in the inventory
    /// — existing measurements see zero change.
    #[tokio::test]
    async fn the_task_list_is_absent_by_default() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("hola".to_string()),
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
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let requests = requests.lock().unwrap().clone();
        assert!(
            !requests[0]
                .tool_stubs
                .iter()
                .any(|s| s.name == "task_add" || s.name == "task_update"),
            "off by default"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The planner bridge (the pre-registered A/B's planner→tasks arm):
    /// with the task list on, an accepted plan seeds tasks instead of
    /// persisting `PlanCreated` prose, and the summary rides the
    /// executor's first request.
    #[tokio::test]
    async fn an_accepted_plan_seeds_the_task_list_instead_of_prose() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));

        let planner = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("1. leer notas.txt\n2. responder".to_string()),
            CompletionEvent::Done,
        ]]);
        let executor = RequestCapturingModel {
            inner: ScriptedModel::new(vec![vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ]]),
            requests: Arc::clone(&requests),
        };

        let engine = Engine::new(
            Box::new(executor),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_planner(Box::new(planner))
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "haz dos cosas", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanCreated { .. })),
            "planner→tasks: no prose plan in the history"
        );

        let requests = requests.lock().unwrap().clone();
        let round1_text: String = requests[0]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            round1_text.contains("1 [pending] leer notas.txt"),
            "the seeded list rides the first executor request: {round1_text}"
        );
        assert!(round1_text.contains("2 [pending] responder"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Marking a task `done` via `task_update` must persist
    /// `AgentEvent::TaskCompleted` with that task's description — the
    /// durable signal `braze-memory`'s `ProjectMemoryHook` depends on,
    /// since the task list itself is in-memory only (J-4).
    #[tokio::test]
    async fn marking_a_task_done_persists_task_completed() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "task_add".to_string(),
                    arguments: serde_json::json!({"description": "leer notas.txt"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "task_update".to_string(),
                    arguments: serde_json::json!({"id": 1, "status": "done"}),
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
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "leé el archivo", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::TaskCompleted { description } if description == "leer notas.txt"
            )),
            "task_update to done must persist TaskCompleted with the task's description"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The mirror case: `task_add` alone, or a transition to
    /// `in_progress`, must NOT persist `TaskCompleted` — only an actual
    /// completion should.
    #[tokio::test]
    async fn adding_or_progressing_a_task_does_not_persist_task_completed() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "task_add".to_string(),
                    arguments: serde_json::json!({"description": "leer notas.txt"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "task_update".to_string(),
                    arguments: serde_json::json!({"id": 1, "status": "in_progress"}),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("trabajando".to_string()),
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
        )
        .with_task_list_enabled(true);

        engine
            .run_turn(&session, "leé el archivo", &mut NoopObserver)
            .await
            .expect("turn must converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TaskCompleted { .. })),
            "task_add and in_progress transitions must never persist TaskCompleted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- C′.1: search_tools — herramientas diferidas en dos niveles
    // (docs/harness-engineering-hooks-skills-2026-07-10.md § I.3) ---

    /// A provider with many stubs — the "gateway grande" fixture. Every
    /// tool resolves a permissive schema and invokes successfully.
    struct NoisyToolsProvider {
        count: usize,
        invocations: Arc<AtomicU32>,
    }

    #[async_trait]
    impl ToolProvider for NoisyToolsProvider {
        fn provider_id(&self) -> &str {
            "test:noisy"
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            let mut stubs: Vec<ToolStub> = (0..self.count)
                .map(|i| ToolStub {
                    name: format!("noise_tool_{i}"),
                    summary: "an unrelated operation".to_string(),
                    source: "test:noisy".to_string(),
                    input_schema: None,
                })
                .collect();
            stubs.push(ToolStub {
                name: "frobnicate_target".to_string(),
                summary: "frobnicates the target dataset".to_string(),
                source: "test:noisy".to_string(),
                input_schema: None,
            });
            Ok(stubs)
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if name.starts_with("noise_tool_") || name == "frobnicate_target" {
                Ok(Some(ToolSchema {
                    name: name.to_string(),
                    description: "test tool".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: "frobnicated".to_string(),
                is_error: false,
            })
        }
    }

    /// The full two-level loop: a big provider hides behind
    /// `search_tools`; the model searches, the hit activates, the next
    /// round's inventory lists it, and the call dispatches to the real
    /// provider. The small provider stays visible throughout.
    #[tokio::test]
    async fn search_tools_hides_a_big_provider_and_activation_makes_the_hit_invocable() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invocations = Arc::new(AtomicU32::new(0));
        let echo_invocations = Arc::new(AtomicU32::new(0));

        let model = RequestCapturingModel {
            inner: ScriptedModel::new(vec![
                // Round 1: the model searches the hidden catalog.
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-1".to_string(),
                        name: "search_tools".to_string(),
                        arguments: serde_json::json!({ "query": "frobnicate dataset" }),
                    },
                    CompletionEvent::Done,
                ],
                // Round 2: calls the tool the search surfaced.
                vec![
                    CompletionEvent::ToolCallRequested {
                        id: "call-2".to_string(),
                        name: "frobnicate_target".to_string(),
                        arguments: serde_json::json!({}),
                    },
                    CompletionEvent::Done,
                ],
                // Round 3: converges.
                vec![
                    CompletionEvent::TextDelta("listo".to_string()),
                    CompletionEvent::Done,
                ],
            ]),
            requests: Arc::clone(&requests),
        };

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![
                Box::new(EchoToolProvider::new(Arc::clone(&echo_invocations))),
                Box::new(NoisyToolsProvider {
                    count: 50,
                    invocations: Arc::clone(&invocations),
                }),
            ]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_search_threshold(40);

        engine
            .run_turn(&session, "frobnica el dataset", &mut NoopObserver)
            .await
            .expect("turn must converge");

        // Cloned out so no MutexGuard lives across the await below.
        let requests = requests.lock().unwrap().clone();
        // Round 1's inventory: echo (small provider, visible), NO noise
        // tools, and the search meta-tool.
        let round1_names: Vec<&str> = requests[0]
            .tool_stubs
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(round1_names.contains(&"echo"));
        assert!(round1_names.contains(&"search_tools"));
        assert!(
            !round1_names.iter().any(|n| n.starts_with("noise_tool_")),
            "the big provider's tools must be hidden: {round1_names:?}"
        );
        assert!(
            !round1_names.contains(&"frobnicate_target"),
            "the target starts hidden too"
        );

        // Round 2's inventory: the search hit is now listed.
        let round2_names: Vec<&str> = requests[1]
            .tool_stubs
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            round2_names.contains(&"frobnicate_target"),
            "the activated hit must resurface: {round2_names:?}"
        );

        // And the real provider was actually invoked.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        // The search result itself reached the model as a tool result.
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallCompleted { result, .. }
                    if result.content.contains("frobnicate_target") && !result.is_error
            )),
            "the search must answer with the matching tools"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// J-9 (docs/AUDITORIA-2026-07-v7.md): naming a hidden tool directly
    /// — without activating it via `search_tools` — must NOT dispatch it.
    /// The model gets a recoverable, actionable error instead; after a
    /// real search activates the tool, the same direct call works. This
    /// is what makes "the model can only use what's listed or searched
    /// for" literally true for the search_tools A/B.
    #[tokio::test]
    async fn a_deferred_tool_called_without_activation_is_rejected_not_dispatched() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let invocations = Arc::new(AtomicU32::new(0));
        let echo_invocations = Arc::new(AtomicU32::new(0));

        let model = ScriptedModel::new(vec![
            // Round 1: guesses the hidden tool's name directly.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-1".to_string(),
                    name: "frobnicate_target".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            // Round 2: does what the error told it to do.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-2".to_string(),
                    name: "search_tools".to_string(),
                    arguments: serde_json::json!({ "query": "frobnicate" }),
                },
                CompletionEvent::Done,
            ],
            // Round 3: the activated tool now dispatches for real.
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-3".to_string(),
                    name: "frobnicate_target".to_string(),
                    arguments: serde_json::json!({}),
                },
                CompletionEvent::Done,
            ],
            // Round 4: converges.
            vec![
                CompletionEvent::TextDelta("listo".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![
                Box::new(EchoToolProvider::new(Arc::clone(&echo_invocations))),
                Box::new(NoisyToolsProvider {
                    count: 50,
                    invocations: Arc::clone(&invocations),
                }),
            ]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_tool_search_threshold(40);

        engine
            .run_turn(&session, "frobnica el dataset", &mut NoopObserver)
            .await
            .expect("turn must converge");

        // The provider ran exactly once — for round 3, never for the
        // unactivated round-1 call.
        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let round1_result = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { id, result } if id == "call-1" => Some(result),
                _ => None,
            })
            .expect("the blocked call must still complete its event pair");
        assert!(round1_result.is_error);
        assert!(
            round1_result.content.contains("search_tools"),
            "the error must tell the model the way in: {}",
            round1_result.content
        );
        let round3_result = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { id, result } if id == "call-3" => Some(result),
                _ => None,
            })
            .expect("the post-activation call must complete");
        assert!(!round3_result.is_error);
        assert_eq!(round3_result.content, "frobnicated");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

}

// SWE-Edit #17: tests del subagente editor. Provider de juguete
// configurable: edit_file tiene éxito o falla según un umbral, write_file
// cuenta sus despachos reales (el assert central del interlock), read_file
// devuelve un stub. edit_file exitoso anexa un bloque [post-edit check]
// para ejercitar la derivación de CompileStatus.
#[cfg(test)]
mod editor_tests {
    use super::*;
    use crate::editor::EDITOR_TOOL;
    use crate::engine::Engine;
    use crate::engine::test_support::*;
    use braze_events::{AgentEvent, NoopObserver};
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_types::SessionId;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct EditorToyProvider {
        /// Los primeros `edit_fails` despachos de edit_file fallan; el
        /// resto tiene éxito. `u32::MAX` = siempre falla.
        edit_fails: u32,
        edit_calls: AtomicU32,
        write_invocations: Arc<AtomicU32>,
        /// Bloque que edit_file exitoso anexa (para CompileStatus).
        check_block: &'static str,
    }

    #[async_trait::async_trait]
    impl braze_tools_core::ToolProvider for EditorToyProvider {
        fn provider_id(&self) -> &str {
            "test:editor-toy"
        }

        async fn list_stubs(
            &self,
        ) -> Result<Vec<braze_types::ToolStub>, braze_tools_core::ToolError> {
            Ok(["read_file", "edit_file", "write_file"]
                .iter()
                .map(|name| braze_types::ToolStub {
                    name: name.to_string(),
                    summary: format!("{name} de juguete"),
                    source: "test:editor-toy".to_string(),
                    input_schema: None,
                })
                .collect())
        }

        async fn resolve_schema(
            &self,
            name: &str,
        ) -> Result<Option<braze_tools_core::ToolSchema>, braze_tools_core::ToolError> {
            Ok(Some(braze_tools_core::ToolSchema {
                name: name.to_string(),
                description: format!("{name} de juguete"),
                input_schema: serde_json::json!({"type": "object"}),
            }))
        }

        async fn invoke(
            &self,
            call: &braze_types::ToolCall,
        ) -> Result<braze_types::ToolResult, braze_tools_core::ToolError> {
            match call.name.as_str() {
                "read_file" => Ok(braze_types::ToolResult {
                    tool_call_id: call.id.clone(),
                    content: "fn foo() {}".to_string(),
                    is_error: false,
                }),
                "edit_file" => {
                    let n = self.edit_calls.fetch_add(1, Ordering::SeqCst);
                    if n < self.edit_fails {
                        Ok(braze_types::ToolResult {
                            tool_call_id: call.id.clone(),
                            content: "edit_file failed: old_string not found".to_string(),
                            is_error: true,
                        })
                    } else {
                        Ok(braze_types::ToolResult {
                            tool_call_id: call.id.clone(),
                            content: format!("edited{}", self.check_block),
                            is_error: false,
                        })
                    }
                }
                _ => {
                    self.write_invocations.fetch_add(1, Ordering::SeqCst);
                    Ok(braze_types::ToolResult {
                        tool_call_id: call.id.clone(),
                        content: format!("written{}", self.check_block),
                        is_error: false,
                    })
                }
            }
        }
    }

    const CHECK_OK: &str = "\n\n[post-edit check] `cargo check` passed in 1s — the code COMPILES.";

    fn editor_call(id: &str, path: &str, instruction: &str) -> CompletionEvent {
        CompletionEvent::ToolCallRequested {
            id: id.to_string(),
            name: EDITOR_TOOL.to_string(),
            arguments: serde_json::json!({ "path": path, "instruction": instruction }),
        }
    }

    fn child_edit(id: &str, path: &str) -> CompletionEvent {
        CompletionEvent::ToolCallRequested {
            id: id.to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({ "path": path, "old_string": "foo", "new_string": "bar" }),
        }
    }

    fn child_write(id: &str, path: &str) -> CompletionEvent {
        CompletionEvent::ToolCallRequested {
            id: id.to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": path, "content": "fn bar() {}" }),
        }
    }

    fn engine_with(
        model: ScriptedModel,
        provider: EditorToyProvider,
        store: FileSessionStore,
    ) -> Engine {
        Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(provider)]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_editor_enabled(true)
    }

    /// Happy path: el padre delega, el hijo edita y converge, el archivo
    /// aterriza y compila. El transcript del hijo (su edit_file) NO llega
    /// al log del padre — solo el editor call, su resultado, el
    /// EditorDelegated (landed=true, compiles=pass) y un Usage editor_child.
    #[tokio::test]
    async fn a_delegated_edit_lands_and_keeps_the_child_transcript_out_of_the_parent_log() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                editor_call("d1", "src/lib.rs", "rename foo to bar"),
                CompletionEvent::Done,
            ],
            // rondas del hijo (mismo backend) — con Usage, como un
            // backend real, para que el agregado editor_child se emita:
            vec![
                child_edit("c1", "src/lib.rs"),
                CompletionEvent::Usage {
                    input_tokens: 40,
                    output_tokens: 12,
                    stop_reason: Some("tool_use".to_string()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta(
                    "State: fully edited. Compiles: yes. Change: renamed foo to bar.".to_string(),
                ),
                CompletionEvent::Done,
            ],
            // ronda final del padre:
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let writes = Arc::new(AtomicU32::new(0));
        let engine = engine_with(
            model,
            EditorToyProvider {
                edit_fails: 0,
                edit_calls: AtomicU32::new(0),
                write_invocations: Arc::clone(&writes),
                check_block: CHECK_OK,
            },
            store,
        );

        engine
            .run_turn(&session, "edita src/lib.rs", &mut NoopObserver)
            .await
            .expect("el turno converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");

        let delegated = events.iter().find_map(|e| match e {
            AgentEvent::EditorDelegated {
                landed,
                compiles,
                path,
                ..
            } => Some((*landed, compiles.clone(), path.clone())),
            _ => None,
        });
        assert_eq!(
            delegated,
            Some((true, "pass".to_string(), "src/lib.rs".to_string()))
        );

        // El editor call del padre SÍ está; el edit_file del hijo NO.
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::AssistantToolCall { name, .. } if name == EDITOR_TOOL)
            ),
            "el editor call del padre debe estar persistido"
        );
        assert!(
            !events.iter().any(
                |e| matches!(e, AgentEvent::AssistantToolCall { name, .. } if name == "edit_file")
            ),
            "el edit_file del HIJO no debe llegar al log del padre (aislamiento)"
        );
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Usage { stop_reason: Some(r), .. } if r == "editor_child")),
            "el Usage agregado del hijo debe estar"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El interlock L-10 propio del hijo: tras 2 edit_file fallidos, un
    /// write_file del hijo se bloquea sin despachar (el provider nunca lo
    /// ve) y la delegación reporta landed=false.
    #[tokio::test]
    async fn the_child_interlock_blocks_write_file_after_two_failed_edits() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                editor_call("d1", "src/lib.rs", "cambio imposible"),
                CompletionEvent::Done,
            ],
            vec![child_edit("c1", "src/lib.rs"), CompletionEvent::Done], // falla 1
            vec![child_edit("c2", "src/lib.rs"), CompletionEvent::Done], // falla 2
            vec![child_write("c3", "src/lib.rs"), CompletionEvent::Done], // bloqueado
            vec![
                CompletionEvent::TextDelta(
                    "State: unchanged. Compiles: n/a. Change: none.".to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("no pude".to_string()),
                CompletionEvent::Done,
            ],
        ]);
        let writes = Arc::new(AtomicU32::new(0));
        let engine = engine_with(
            model,
            EditorToyProvider {
                edit_fails: u32::MAX, // edit_file siempre falla
                edit_calls: AtomicU32::new(0),
                write_invocations: Arc::clone(&writes),
                check_block: "",
            },
            store,
        );

        engine
            .run_turn(&session, "edita src/lib.rs", &mut NoopObserver)
            .await
            .expect("el turno converge: el interlock devuelve error de tool, no mata el turno");

        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "el write_file del hijo tras 2 edits fallidos NO debe llegar al provider"
        );
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let landed = events.iter().find_map(|e| match e {
            AgentEvent::EditorDelegated { landed, .. } => Some(*landed),
            _ => None,
        });
        assert_eq!(landed, Some(false), "ninguna edición aterrizó");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// No-convergencia CON una edición previa exitosa: el outcome es error
    /// y le dice al padre que el archivo quedó a medias — la línea de
    /// seguridad del diseño (releer antes de asumir estado limpio).
    #[tokio::test]
    async fn a_non_converging_child_that_already_edited_warns_the_parent_to_reread() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // El hijo edita con éxito cada ronda y NUNCA converge (siempre
        // llama tool) → agota MAX_EDITOR_CHILD_ROUNDS con landed=true.
        let mut rounds = vec![vec![
            editor_call("d1", "src/lib.rs", "editar en loop"),
            CompletionEvent::Done,
        ]];
        for i in 0..crate::editor::MAX_EDITOR_CHILD_ROUNDS {
            rounds.push(vec![
                child_edit(&format!("c{i}"), "src/lib.rs"),
                CompletionEvent::Done,
            ]);
        }
        rounds.push(vec![
            CompletionEvent::TextDelta("ok".to_string()),
            CompletionEvent::Done,
        ]);
        let model = ScriptedModel::new(rounds);
        let writes = Arc::new(AtomicU32::new(0));
        let engine = engine_with(
            model,
            EditorToyProvider {
                edit_fails: 0,
                edit_calls: AtomicU32::new(0),
                write_invocations: Arc::clone(&writes),
                check_block: CHECK_OK,
            },
            store,
        );

        engine
            .run_turn(&session, "edita", &mut NoopObserver)
            .await
            .expect("el turno converge");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        let result = events.iter().find_map(|e| match e {
            AgentEvent::ToolCallCompleted { id, result } if id == "d1" => Some(result.clone()),
            _ => None,
        });
        let result = result.expect("el editor call persiste su resultado");
        assert!(result.is_error, "no-convergencia es error");
        assert!(
            result.content.contains("partially modified") && result.content.contains("read it"),
            "debe avisar que el archivo quedó a medias, got: {}",
            result.content
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
