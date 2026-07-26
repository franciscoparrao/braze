//! Una ronda de completion — P1.1 paso 4 (v8 § 3). Extraído VERBATIM
//! de `engine/mod.rs` (2026-07-18): `complete_once{,_with}` (consumir
//! el stream a un `RoundOutcome`, con la escalera de rescate en el
//! camino), `complete_with_best_of_n` (técnica G10: votación entre
//! candidatos) y los tipos/helpers de la ronda.

use super::*;

/// One round's `Usage` report — a named struct instead of a growing tuple
/// so the ~4 sites that build/sum/destructure it (`complete_once_with`,
/// `complete_with_best_of_n`, `run_turn`'s persist call) can't mix up
/// positional fields (docs/usability-log-2026-07-07-si2.md, prompt-caching
/// design — this grew from 3 fields to 5 when cache token counts were
/// added, past the point a tuple stays readable).
#[derive(Debug, Clone)]
pub(super) struct RoundUsage {
    pub(super) input_tokens: u32,
    pub(super) output_tokens: u32,
    pub(super) stop_reason: Option<String>,
    pub(super) cache_read_tokens: Option<u32>,
    pub(super) cache_write_tokens: Option<u32>,
    /// Set when `EscalatingBackend` stamped this round's `Usage` event as
    /// the one that triggered a reactive escalation (H-3,
    /// docs/AUDITORIA-2026-07-v5.md) — see
    /// `CompletionEvent::Usage::escalation_trigger`'s doc comment.
    pub(super) escalation_trigger: Option<String>,
}

/// The resolved outcome of one full model completion — everything the
/// round loop in [`Engine::run_turn`] needs to decide what happens next,
/// whether it came from a single attempt ([`Engine::complete_once`]) or
/// was chosen by vote among several ([`Engine::complete_with_best_of_n`]).
///
/// (Doc reunido con su struct en el paso 4 del split — en `engine.rs`
/// este bloque había quedado pegado sobre `RoundUsage`.)
pub(super) struct RoundOutcome {
    pub(super) text_buffer: String,
    pub(super) tool_calls: Vec<ToolCall>,
    pub(super) usage: Option<RoundUsage>,
    /// Set when the backend reported `stop_reason: "max_tokens"`/`"length"`
    /// for this round — N-24 (docs/AUDITORIA-2026-07-v2.md): `run_turn`
    /// checks this before treating an empty-`tool_calls` round as a
    /// legitimate final answer, since a truncated response may be cut off
    /// mid-sentence (or mid-tool-call-JSON, which then fails to parse
    /// downstream with no other indication why).
    pub(super) truncated: bool,
    /// Which rung of the textual-rescue ladder recovered a tool call this
    /// round, if any (H-3, docs/AUDITORIA-2026-07-v5.md) — see
    /// `AgentEvent::TextualRescueApplied`'s doc comment.
    pub(super) rescue_applied: Option<String>,
}

impl Engine {
    /// Makes one completion call and consumes its stream fully into a
    /// [`RoundOutcome`] — the single-attempt building block both the
    /// normal path and técnica G10's best-of-n voting
    /// (docs/AUDITORIA-2026-07.md) are built from; extracted verbatim
    /// from what used to be inline in `run_turn`'s round loop, no
    /// behavior change for the `best_of_n <= 1` (default) path.
    ///
    /// `emit_deltas` controls whether text deltas reach `observer` as
    /// they stream in: `false` when this is one of several best-of-n
    /// candidates being generated — there is no single "the" answer to
    /// show live until voting picks one (see
    /// `complete_with_best_of_n`'s doc comment for what happens to the
    /// winner's text instead).
    pub(super) async fn complete_once(
        &self,
        req: CompletionRequest,
        observer: &mut dyn TurnObserver,
        emit_deltas: bool,
    ) -> Result<RoundOutcome, EngineError> {
        self.complete_once_with(
            self.model.as_ref(),
            req,
            observer,
            emit_deltas,
            self.textual_rescue_enabled,
            self.envelope_parsing_enabled,
        )
        .await
    }

    /// [`Engine::complete_once`], parameterized on which backend answers
    /// — the executor (`self.model`) on the normal path, or the optional
    /// planner (`self.planner`) for the planning round (PLAN.md § "Split
    /// planificador/ejecutor"). Everything else (stream consumption,
    /// truncation flag) is identical by construction.
    ///
    /// `rescue_enabled` is threaded separately from `self.textual_rescue_enabled`
    /// — F7 (docs/AUDITORIA-2026-07-v3.md): the planning round always
    /// passes `false` regardless of the executor's setting. A local
    /// planner emitting its own native tool-template leak (e.g. Qwen's
    /// `<tool_call>{...}</tool_call>`) while listing the concrete tools it
    /// would use — exactly what `planning_system_prompt` asks for — would
    /// otherwise have those blocks *removed* from the plan text before
    /// `attempt_planning_round` even looks at `outcome.tool_calls`
    /// (already ignored there), silently deleting the very steps that
    /// named a tool from the persisted `PlanCreated` plan.
    ///
    /// `envelope_enabled` is threaded the same way (planner round: always
    /// `false`) — a plan that happens to be a whole-response JSON object
    /// must stay plan text, and the planner backend is never in
    /// prompt-tools mode anyway.
    pub(super) async fn complete_once_with(
        &self,
        model: &dyn ModelBackend,
        req: CompletionRequest,
        observer: &mut dyn TurnObserver,
        emit_deltas: bool,
        rescue_enabled: bool,
        envelope_enabled: bool,
    ) -> Result<RoundOutcome, EngineError> {
        let mut stream = model.complete(req).await?;

        let mut text_buffer = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<RoundUsage> = None;
        let mut saw_done = false;
        let mut truncated = false;

        while let Some(event) = stream.next().await {
            match event {
                Ok(CompletionEvent::TextDelta(delta)) => {
                    if emit_deltas {
                        observer.on_text_delta(&delta);
                    }
                    text_buffer.push_str(&delta);
                }
                Ok(CompletionEvent::ToolCallRequested {
                    id,
                    name,
                    arguments,
                }) => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                Ok(CompletionEvent::Usage {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                    cache_read_tokens,
                    cache_write_tokens,
                    escalation_trigger,
                }) => {
                    tracing::debug!(
                        input_tokens,
                        output_tokens,
                        stop_reason = stop_reason.as_deref(),
                        cache_read_tokens,
                        cache_write_tokens,
                        "model usage this round"
                    );
                    // "max_tokens"/"length" means the round's output
                    // (which may have been a tool call's JSON arguments,
                    // mid-construction) was cut off by the token budget
                    // rather than the model finishing on its own — the
                    // tool call then fails to parse and gets silently
                    // dropped downstream with no other indication of
                    // why. Surfacing it here at least makes that
                    // diagnosable from logs.
                    if matches!(stop_reason.as_deref(), Some("max_tokens") | Some("length")) {
                        tracing::warn!(
                            output_tokens,
                            max_tokens = self.max_tokens,
                            "model output was truncated by max_tokens this round; a tool call's \
                             arguments may have been cut off mid-construction"
                        );
                        truncated = true;
                    }
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
                Err(err) => {
                    // Whatever text/tool-calls arrived before the stream
                    // failed must NOT be treated as a complete, converged
                    // response — nothing from this attempt has been
                    // persisted yet, so propagating here silently
                    // discards the partial attempt instead of persisting
                    // it as if it were real. See `ModelError::StreamError`'s
                    // doc comment / docs/AUDITORIA-2026-07.md hallazgo A3/B4.
                    return Err(err.into());
                }
            }
        }

        // A `ModelBackend` must uphold the invariant that its stream
        // either yields an `Err` or ends with `Ok(Done)` as its last item
        // (see the trait's doc comment) — this only trips if a backend
        // implementation violates that, but the same reasoning as the
        // `Err` arm above applies: an unconverged, possibly-truncated
        // attempt must not be persisted as if it were complete.
        if !saw_done {
            return Err(EngineError::IncompleteStream);
        }

        // Fallback for models that don't emit a structured tool call —
        // small/local models, or a template without native tool-call
        // support — but instead write the call out in plain text.
        // Rescuing it here beats treating it as the model's final
        // answer, which would end the turn having silently ignored what
        // was clearly meant to be a tool call. See
        // docs/AUDITORIA-2026-07.md hallazgo B5. Applied per-attempt
        // (not after best-of-n voting) so a textually described tool
        // call counts as a real candidate signature for the vote too.
        //
        // Escalera, most-specific first: (1) the `<tool_call>…</tool_call>`
        // tagged format Qwen/Hermes models emit natively (explicit
        // markers — admits surrounding prose and several calls per
        // response; the inner grammar may be qwen2.5's JSON or
        // qwen3-coder's `<function=…>` XML), then (2) a bare
        // `<function=…>` XML block without the wrapper, then (3) Llama
        // 3.x's native "pythonic" format (C2, docs/AUDITORIA-2026-07-v3.md
        // — the escalera covered Qwen's two formats but nothing for
        // Llama, one of the most commonly installed local model
        // families), then (4) a bare JSON object that is the entire
        // response (optionally ```json-fenced).
        // H-3 (docs/AUDITORIA-2026-07-v5.md): which rung (if any) actually
        // rescued a tool call this round, threaded into `RoundOutcome` so
        // `run_turn` can persist `AgentEvent::TextualRescueApplied` — the
        // action already existed (the `tracing::info!` calls below), this
        // just gives it a counted, bench-readable trail too.
        // Envelope parsing (docs/constrained-decoding-ab-design.md) runs
        // BEFORE the rescue ladder and outside its accounting: in
        // prompt-tools mode the envelope is the instructed, primary
        // format — treating it as a rescue would make the A/B's own
        // mechanism check (`rescues ≈ 0` on the constrained arm)
        // unsatisfiable by construction. A parsed `final_answer` also
        // suppresses the ladder below: the model explicitly declared the
        // text final, and a JSON-looking final answer must not be
        // re-interpreted as a tool call.
        let mut envelope_handled = false;
        if envelope_enabled
            && tool_calls.is_empty()
            && let Some(envelope) = parse_envelope_response(&text_buffer)
        {
            envelope_handled = true;
            match envelope {
                EnvelopeResponse::ToolCall { call, reasoning } => {
                    tracing::info!(
                        tool = %call.name,
                        "parsed a prompt-tools envelope tool call"
                    );
                    tool_calls.push(call);
                    text_buffer = reasoning.unwrap_or_default();
                }
                EnvelopeResponse::FinalAnswer { text } => {
                    tracing::info!("parsed a prompt-tools envelope final answer");
                    text_buffer = text;
                }
            }
        }

        let mut rescue_applied: Option<String> = None;
        if rescue_enabled && !envelope_handled && tool_calls.is_empty() {
            type TextualExtractor = fn(&str) -> (Vec<ToolCall>, String);
            const RESCUE_LADDER: &[(TextualExtractor, &str)] = &[
                (
                    extract_tagged_tool_calls,
                    "<tool_call> tagged (Qwen/Hermes)",
                ),
                (
                    extract_function_xml_tool_calls,
                    "<function=> XML (qwen3-coder)",
                ),
                (extract_pythonic_tool_calls, "pythonic [func(...)] (Llama)"),
            ];

            let mut rescued_from_ladder = false;
            for (extract, format) in RESCUE_LADDER {
                let (calls, remaining_text) = extract(&text_buffer);
                if !calls.is_empty() {
                    tracing::info!(
                        count = calls.len(),
                        format,
                        "rescued tool call(s) emitted as text in a native tool-template format instead of structured tool_calls entries"
                    );
                    tool_calls.extend(calls);
                    text_buffer = remaining_text;
                    rescued_from_ladder = true;
                    rescue_applied = Some((*format).to_string());
                    break;
                }
            }

            if !rescued_from_ladder && let Some(rescued) = try_parse_textual_tool_call(&text_buffer)
            {
                tracing::info!(
                    tool = %rescued.name,
                    "rescued a tool call the model emitted as plain text instead of a structured tool_calls entry"
                );
                tool_calls.push(rescued);
                text_buffer.clear();
                rescue_applied = Some("plain-text fallback".to_string());
            }
        }

        Ok(RoundOutcome {
            text_buffer,
            tool_calls,
            usage,
            rescue_applied,
            truncated,
        })
    }

    /// Técnica G10 (docs/AUDITORIA-2026-07.md): generates `self.best_of_n`
    /// independent completions of the same request and picks the one
    /// whose outcome — canonicalized as the sorted set of (tool name,
    /// canonical arguments) pairs it requested, or an empty set for a
    /// "no tool call, final answer" candidate — is the most common among
    /// all attempts (plurality vote; ties keep the earliest-generated
    /// candidate, never `Iterator::max_by_key`'s "last wins" default).
    /// Cheap test-time scaling with no training required: the evidence
    /// behind this (docs/AUDITORIA-2026-07.md § 6, técnica 10 — Corradini
    /// et al. 2025, BDCC) is that letting a small model try several times
    /// and vote beats one greedy attempt, particularly on the ambiguous
    /// decisions this project's own sweep flagged as weak
    /// (`error_recovery`, `distractor_selection` — both 0/5 for
    /// `qwen2.5:3b`/`7b`, see `CLAUDE.md`).
    ///
    /// Deltas are not streamed live during the `best_of_n` attempts (see
    /// `complete_once`'s `emit_deltas` parameter) — there is no single
    /// "the" answer to show token-by-token until voting resolves one.
    /// The winner's full text is delivered to `observer` as a single
    /// delta right after the vote, so downstream consumers (the plain
    /// CLI, `braze-tui`) still receive it exactly the way they receive
    /// every other round's text, just without the live streaming feel
    /// for this specific round — an accepted, deliberate trade-off.
    ///
    /// The persisted usage for the round is the *sum* across every
    /// candidate (this makes `self.best_of_n` real model calls, and
    /// token/cost accounting must reflect that), with `stop_reason`
    /// taken specifically from the winning candidate.
    pub(super) async fn complete_with_best_of_n(
        &self,
        req: &CompletionRequest,
        observer: &mut dyn TurnObserver,
    ) -> Result<RoundOutcome, EngineError> {
        // N-13 (docs/AUDITORIA-2026-07-v2.md): a transient error on one
        // candidate (e.g. a rate-limit blip on attempt 3 of 5) must not
        // discard the other candidates already paid for and abort the
        // whole round — that would multiply the effective failure
        // probability by `best_of_n`, backwards from what this technique
        // is for. Vote among whichever candidates actually succeeded;
        // only propagate an error if every single one failed.
        //
        // P1.4 (docs/AUDITORIA-2026-07-v6.md): candidates run
        // concurrently — they are independent completions of the same
        // request, deltas are suppressed (each gets its own
        // `NoopObserver`; the caller's `observer` only ever sees the
        // winner's text, below), and the vote's tie-break needs attempt
        // ORDER, not attempt timing, which `join_all`'s input-order
        // result preserves. The wall-clock win is real only on cloud
        // backends — a local Ollama server serializes requests unless
        // `OLLAMA_NUM_PARALLEL > 1`, so there this is at best neutral.
        let attempts = futures::future::join_all((0..self.best_of_n).map(|attempt| {
            let req = req.clone();
            async move {
                let mut candidate_observer = braze_events::NoopObserver;
                (
                    attempt,
                    self.complete_once(req, &mut candidate_observer, false)
                        .await,
                )
            }
        }))
        .await;

        let mut candidates = Vec::with_capacity(self.best_of_n);
        let mut last_error = None;
        for (attempt, result) in attempts {
            match result {
                Ok(outcome) => {
                    tracing::debug!(
                        attempt,
                        n_tool_calls = outcome.tool_calls.len(),
                        "best-of-n candidate generated"
                    );
                    candidates.push(outcome);
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        error = %err,
                        "best-of-n candidate failed; continuing with the remaining attempts"
                    );
                    last_error = Some(err);
                }
            }
        }

        if candidates.is_empty() {
            return Err(
                last_error.unwrap_or(EngineError::TurnDidNotConverge(self.max_turn_iterations))
            );
        }

        let signatures: Vec<Vec<(String, String)>> =
            candidates.iter().map(candidate_signature).collect();
        let mut winner_index = 0;
        let mut winner_votes = 0;
        for (i, signature) in signatures.iter().enumerate() {
            let votes = signatures.iter().filter(|s| *s == signature).count();
            if votes > winner_votes {
                winner_votes = votes;
                winner_index = i;
            }
        }

        let total_input_tokens: u32 = candidates
            .iter()
            .filter_map(|c| c.usage.as_ref())
            .map(|u| u.input_tokens)
            .sum();
        let total_output_tokens: u32 = candidates
            .iter()
            .filter_map(|c| c.usage.as_ref())
            .map(|u| u.output_tokens)
            .sum();
        // Cache tokens sum the same way input/output do, for the same
        // reason: this is cost accounting, and every candidate's request
        // really was sent (and really did read/write cache), not just the
        // winner's — summing `None`s as 0 would understate real spend, so
        // stay `None` unless at least one candidate reported a cache
        // token count.
        let total_cache_read_tokens = sum_optional_u32(
            candidates
                .iter()
                .filter_map(|c| c.usage.as_ref().and_then(|u| u.cache_read_tokens)),
        );
        let total_cache_write_tokens = sum_optional_u32(
            candidates
                .iter()
                .filter_map(|c| c.usage.as_ref().and_then(|u| u.cache_write_tokens)),
        );
        let any_usage_reported = candidates.iter().any(|c| c.usage.is_some());
        let winner_stop_reason = candidates[winner_index]
            .usage
            .as_ref()
            .and_then(|u| u.stop_reason.clone());
        // H-3 (docs/AUDITORIA-2026-07-v5.md): every candidate this round
        // shares the same routing decision (D4's same-round dedup in
        // `EscalatingBackend::route` keys on `req.messages.len()`, which
        // every best-of-n attempt sends unchanged) — taking it from the
        // winner specifically is just the least surprising of several
        // equivalent choices, not a real distinction.
        let winner_escalation_trigger = candidates[winner_index]
            .usage
            .as_ref()
            .and_then(|u| u.escalation_trigger.clone());

        tracing::debug!(
            winner_index,
            winner_votes,
            n_candidates = candidates.len(),
            "best-of-n vote resolved"
        );

        let mut winner = candidates.swap_remove(winner_index);
        winner.usage = any_usage_reported.then_some(RoundUsage {
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            stop_reason: winner_stop_reason,
            cache_read_tokens: total_cache_read_tokens,
            cache_write_tokens: total_cache_write_tokens,
            escalation_trigger: winner_escalation_trigger,
        });

        if !winner.text_buffer.is_empty() {
            observer.on_text_delta(&winner.text_buffer);
        }

        Ok(winner)
    }
}

/// Sums an optional-per-item `u32` (a cache token count some backends
/// don't report at all) into a single optional total: `None` only if
/// *every* item was `None` (nothing reported anything — stay silent
/// rather than claim "0 tokens cached"), `Some(sum)` treating any
/// individual `None` as 0 once at least one item did report a value.
/// Shared by `Engine::complete_with_best_of_n`'s cache-token summing —
/// see that call site's comment for why this differs from just summing
/// `u32`s directly.
fn sum_optional_u32(values: impl Iterator<Item = u32>) -> Option<u32> {
    let mut sum = 0u32;
    let mut any = false;
    for v in values {
        sum += v;
        any = true;
    }
    any.then_some(sum)
}

/// Canonical signature of a completion outcome for técnica G10's vote
/// (`Engine::complete_with_best_of_n`): the sorted set of (tool name,
/// canonical arguments) pairs it requested — sorted so two candidates
/// that requested the same calls in a different order compare equal —
/// or an empty vec for a "no tool call, final answer" candidate. Reuses
/// the exact canonicalization `dispatch_tool_calls`'s repeated-call
/// detection already relies on (`arguments.to_string()`, stable because
/// `serde_json::Value` serializes object keys in sorted order without
/// the `preserve_order` feature).
fn candidate_signature(outcome: &RoundOutcome) -> Vec<(String, String)> {
    let mut signature: Vec<(String, String)> = outcome
        .tool_calls
        .iter()
        .map(|call| (call.name.clone(), call.arguments.to_string()))
        .collect();
    signature.sort();
    signature
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
    use std::sync::atomic::AtomicU32;

    /// Regression test for N-13 (docs/AUDITORIA-2026-07-v2.md): a
    /// transient error on one best-of-n candidate must not discard the
    /// other candidates already generated and fail the whole round —
    /// the turn must still converge by voting among the survivors.
    #[tokio::test]
    async fn best_of_n_votes_among_successful_candidates_when_one_attempt_errors() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = FlakyBestOfNModel {
            fail_on_attempt: 1,
            calls: AtomicU32::new(0),
            good_round: vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
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
        .with_best_of_n(3);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should still succeed despite one candidate erroring");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::AssistantText { text } if text == "done")),
            "expected the winning candidate's text to be persisted"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-13: if *every* best-of-n candidate fails,
    /// the round must still propagate an error — voting among survivors
    /// must not mask a genuine total failure as success.
    #[tokio::test]
    async fn best_of_n_fails_the_round_only_when_every_candidate_fails() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ErroringModel;
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_best_of_n(3);

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::Model(_))),
            "expected every candidate to fail and the error to propagate, got {result:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
