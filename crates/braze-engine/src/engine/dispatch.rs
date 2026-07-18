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
        } = state;

        let mut handle_to_id: HashMap<TaskHandle, String> = HashMap::new();
        let mut pending: HashSet<TaskHandle> = HashSet::new();
        // F6: resolves a completed call's id back to its tool name, so a
        // successful completion can be checked against
        // `MUTATING_TOOL_NAMES` without threading the name through the
        // background task machinery.
        let mut id_to_name: HashMap<String, String> = HashMap::new();

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
            if !seen_calls.insert(call_key) {
                tracing::warn!(
                    tool = %call.name,
                    "model repeated an identical tool call this turn; nudging instead of re-dispatching"
                );
                self.append_and_notify(
                    session,
                    &AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        result: ToolResult {
                            tool_call_id: call.id.clone(),
                            content: format!(
                                "You already called '{}' with these exact arguments \
                                 earlier in this turn — the result has not changed. Do \
                                 not repeat this call; either use the result you already \
                                 have, or respond to the user with text instead of \
                                 calling a tool.",
                                call.name
                            ),
                            is_error: true,
                        },
                    },
                    observer,
                )
                .await?;
                continue;
            }

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
                    let available = available_tools
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
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
                                    "Unknown tool '{}'. Available tools are: {available}. \
                                     Retry using one of these exact names.",
                                    call.name
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
        // failed so the turn proceeds rather than hanging forever.
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
                    self.append_and_notify(
                        session,
                        &AgentEvent::ToolCallCompleted { id, result },
                        observer,
                    )
                    .await?;
                }
                None => {
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
}
