//! El summary fallback tools-free — P1.1 paso 3 (v8 § 3). Extraído
//! VERBATIM de `engine/mod.rs` (2026-07-18): la última llamada sin
//! tools que rescata un turno no-convergido resumiendo el progreso ya
//! hecho (`attempt_tools_free_summary_round`), su outcome tipado y el
//! stripping de tool calls filtradas al texto del resumen (U-16).

use super::*;

impl Engine {
    /// Makes one last tools-free request asking the model to summarize
    /// whatever it learned and answer with that — persisted as a normal
    /// `AssistantText` ([`SummaryFallbackOutcome::Summarized`]) on
    /// success. Callers share the shape "the turn didn't converge
    /// normally but there may already be real progress worth summarizing
    /// instead of just failing outright": [`Engine::run_turn`] exhausting
    /// [`MAX_TURN_ITERATIONS`] or its token budget, and (U-1, found live
    /// 2026-07-07 against qwen3.5-coder/Nitro) a round mid-turn coming
    /// back with neither text nor a tool call *after* this turn already
    /// dispatched at least one successful tool call — each caller logs
    /// its own context and picks its own reaction to a non-`Summarized`
    /// outcome, since "exhausted the iteration cap", "went empty right
    /// after real work" and "the fallback call itself died" are different
    /// situations worth telling apart.
    ///
    /// [`SummaryFallbackOutcome::Empty`] when the call completed but
    /// produced nothing usable; [`SummaryFallbackOutcome::CallFailed`]
    /// when the attempt itself failed — a legitimate hard failure (e.g.
    /// the backend is unreachable) is surfaced by the caller as an error
    /// rather than silently swallowed here.
    pub(super) async fn attempt_tools_free_summary_round(
        &self,
        session: &SessionId,
        messages: &[Message],
        observer: &mut dyn TurnObserver,
    ) -> Result<SummaryFallbackOutcome, EngineError> {
        // H-3 (docs/AUDITORIA-2026-07-v5.md): records that this fallback was
        // *reached for*, regardless of whether it goes on to succeed —
        // success is separately visible as the `AssistantText` this
        // function may or may not append below.
        self.append_and_notify(session, &AgentEvent::SummaryFallbackAttempted, observer)
            .await?;

        let req = CompletionRequest {
            messages: messages.to_vec(),
            tool_stubs: Vec::new(),
            system_prompt: format!(
                "{}\n\nDo not call any tool — none are available in this request. Summarize \
                 what you found so far and answer the user with the best answer you can give \
                 from the information already gathered.",
                self.system_prompt
            ),
            max_tokens: self.max_tokens,
        };

        // Bajo (docs/AUDITORIA-2026-07-v2.md, "attempt_final_summary_round
        // traga el error real"): both error paths below used to discard
        // the actual cause (a real backend failure — auth, network,
        // rate limit — looked identical to "the model just didn't
        // produce anything usable"). Logging it here makes the real cause
        // visible instead of silently lost, regardless of which
        // `EngineError` the caller ultimately raises on `Ok(false)`.
        let mut stream = match self.model.complete(req).await {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "tools-free summary round's model call itself failed"
                );
                return Ok(SummaryFallbackOutcome::CallFailed);
            }
        };

        // Deltas are *not* streamed live here (unlike the main round in
        // `complete_once_with`) — same trade-off `complete_with_best_of_n`
        // already makes and documents: there is no single "the" answer to
        // show token-by-token until the full response is in hand and
        // known-clean. Here that's because a leaked tool-call block
        // (hallazgo U-16 below) can only be detected and stripped once the
        // whole buffer has arrived; streaming it live first would show the
        // user the raw garbage regardless of what happens to the
        // *persisted* text afterward.
        let mut text_buffer = String::new();
        let mut saw_done = false;
        let mut usage: Option<RoundUsage> = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(CompletionEvent::TextDelta(delta)) => {
                    text_buffer.push_str(&delta);
                }
                Ok(CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                    cache_read_tokens,
                    cache_write_tokens,
                    escalation_trigger,
                }) => {
                    // H-4 (docs/AUDITORIA-2026-07-v5.md): this used to be
                    // skipped as "not worth the same bookkeeping as a
                    // normal round" — but the fallback re-sends the ENTIRE
                    // conversation history as its prompt, making it one of
                    // the most expensive single calls a turn can make, and
                    // dropping its Usage made that cost invisible to the
                    // bench (`TaskResult::input_tokens` under-reported
                    // exactly on the degraded turns a cost analysis most
                    // needs to see). H-3 counts that the fallback was
                    // *attempted*; this records what it *cost*.
                    usage = Some(RoundUsage {
                        input_tokens,
                        output_tokens,
                        stop_reason,
                        cache_read_tokens,
                        cache_write_tokens,
                        escalation_trigger,
                    });
                }
                Ok(CompletionEvent::Done) => {
                    saw_done = true;
                    break;
                }
                // No tools were offered in this request, so a tool call
                // here would itself be a violation of the request — ignore
                // rather than act on it.
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "tools-free summary round's stream failed mid-response"
                    );
                    break;
                }
            }
        }

        // Persisted regardless of whether the fallback produced usable
        // text below — the model call happened and was paid for either
        // way, same reasoning as `SummaryFallbackAttempted` counting
        // attempts rather than successes. One `Usage` per model round is
        // the invariant `TaskResult::rounds` counts on; this round is a
        // real model round.
        if let Some(round_usage) = usage {
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
            // Same pairing `run_turn` maintains: if THIS call happened to
            // be the one that triggered a reactive escalation (the
            // fallback goes through the same `ModelBackend`, decorator
            // included), the episode is persisted here too instead of
            // silently dropped.
            if let Some(trigger) = escalation_trigger {
                self.append_and_notify(
                    session,
                    &AgentEvent::EscalationToLead { trigger },
                    observer,
                )
                .await?;
            }
        }

        if saw_done && !text_buffer.is_empty() {
            // U-16 (docs/usability-log-2026-07-07-si2.md): this request
            // explicitly carries no tool stubs and tells the model not to
            // call any tool, but a model habituated to its own native
            // tool-call template (`z-ai/glm-5.2`, observed live) can still
            // emit one as plain text anyway. Unlike the main round, this
            // one has no rescue ladder to dispatch it to — there is
            // nothing to dispatch it *to* — so the only choice is between
            // showing the leaked block to the user as if it were the real
            // answer, or stripping it. Showing it is strictly worse: it
            // turns a recoverable "the model tried to call a tool it
            // couldn't" into "the turn succeeded" with garbage as the
            // punchline.
            let cleaned = strip_leaked_tool_call_shapes(&text_buffer);
            if cleaned.trim().is_empty() {
                return Ok(SummaryFallbackOutcome::Empty);
            }
            observer.on_text_delta(&cleaned);
            self.append_and_notify(
                session,
                &AgentEvent::AssistantText { text: cleaned },
                observer,
            )
            .await?;
            return Ok(SummaryFallbackOutcome::Summarized);
        }

        // `saw_done` with no text: the call ran to completion and just
        // produced nothing. Without `Done`, the stream died (or ended)
        // mid-response — indistinguishable here from a real backend
        // failure, so it must keep reading as one.
        if saw_done {
            Ok(SummaryFallbackOutcome::Empty)
        } else {
            Ok(SummaryFallbackOutcome::CallFailed)
        }
    }
}

/// What [`Engine::attempt_tools_free_summary_round`] actually got out of
/// its one extra model call. A plain `bool` used to conflate the two
/// non-success shapes, and they deserve different handling at the U-1
/// call site: `Empty` (the call completed but produced nothing usable) is
/// the reasoning-model quirk observed live with gpt-oss:20b on the
/// memory-distillation smoke (2026-07-16) — thinking-channel models can
/// finish a turn whose real work already landed on disk with a final
/// content of "" — while `CallFailed` (the fallback's own request or
/// stream died) may hide a real backend failure (auth, network, rate
/// limit) that must keep surfacing as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SummaryFallbackOutcome {
    /// Usable summary text was produced and persisted as `AssistantText`.
    Summarized,
    /// The model call completed normally but yielded no usable text.
    Empty,
    /// The fallback's own model call or stream failed before completing.
    CallFailed,
}

/// Strips any tool-call-shaped block the rescue ladder recognizes,
/// keeping only the surrounding prose — used by
/// [`Engine::attempt_tools_free_summary_round`] only, whose request
/// explicitly carries `tool_stubs: Vec::new()` and tells the model no
/// tool is available: a recognized block there can never be legitimately
/// dispatched (there is nothing to dispatch it *to*), so the choice is
/// between showing it to the user as if it were the real answer or
/// discarding it. Showing it is strictly worse — it replaces one
/// confusing failure (`EmptyModelResponse`) with a worse one (garbage
/// presented as a successful summary). docs/usability-log-2026-07-07-si2.md,
/// hallazgo U-16: `z-ai/glm-5.2` emitted its native (malformed)
/// `<tool_call>` syntax even after being told not to call any tool, and
/// this round — unlike the main one in [`Engine::complete_once_with`] —
/// had no rescue logic at all to catch it before persisting it verbatim.
pub(super) fn strip_leaked_tool_call_shapes(text: &str) -> String {
    for extract in [
        extract_tagged_tool_calls as fn(&str) -> (Vec<ToolCall>, String),
        extract_function_xml_tool_calls,
        extract_pythonic_tool_calls,
    ] {
        let (calls, remaining) = extract(text);
        if !calls.is_empty() {
            return remaining;
        }
    }
    text.to_string()
}


#[cfg(test)]
mod tests {
    use super::*;
    // P1.1 resto (v9 L-5, 2026-08-18): tests de strip_leaked_tool_call_
    // shapes movidos VERBATIM del `mod tests` de engine/mod.rs —
    // fixtures compartidas en engine/test_support.rs.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use braze_events::NoopObserver;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_tools_core::ToolRegistry;
    use braze_types::SessionId;

    use crate::engine::Engine;
    use crate::engine::test_support::*;

    // --- strip_leaked_tool_call_shapes (hallazgo U-16,
    // docs/usability-log-2026-07-07-si2.md: attempt_tools_free_summary_round
    // had no rescue logic at all, so a leaked tool-call block there used
    // to get persisted verbatim as if it were the model's real answer) ---

    #[test]
    fn a_leaked_tagged_call_with_no_other_text_strips_to_empty() {
        let text =
            "<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>";
        assert_eq!(strip_leaked_tool_call_shapes(text), "");
    }

    #[test]
    fn a_leaked_call_alongside_real_prose_keeps_only_the_prose() {
        let text = "Basado en lo que leí hasta ahora, el fix consiste en...\n<tool_call>read_file<arg_key>path</arg_key><arg_value>x.txt</arg_value></tool_call>";
        assert_eq!(
            strip_leaked_tool_call_shapes(text),
            "Basado en lo que leí hasta ahora, el fix consiste en..."
        );
    }

    #[test]
    fn plain_prose_with_no_leaked_call_is_returned_unchanged() {
        let text = "No hay nada raro acá, solo texto normal.";
        assert_eq!(strip_leaked_tool_call_shapes(text), text);
    }

    /// Regression test for the rescue escalera's ordering: a `<tool_call>`
    /// tagged block must win even if the response also happens to contain
    /// bracketed text that looks pythonic-shaped elsewhere.
    #[tokio::test]
    async fn a_llama_pythonic_call_is_rescued_end_to_end_when_no_structured_call_arrives() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Voy a revisar.[echo(text=\"hi\")]".to_string()),
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

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the pythonic call must actually reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Voy a revisar.")
            )),
            "the surrounding prose must be persisted as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("echo(")
            )),
            "the bracketed call must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
