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
    /// How many SEARCH/REPLACE blocks the edit-fence parser synthesized
    /// into `edit_file` calls this round (`crate::edit_fence`, A/B del
    /// impuesto JSON) — 0 always when the lever is off. Threaded so
    /// `run_turn` can persist `AgentEvent::EditFenceApplied`, the
    /// treatment arm's mechanism check.
    pub(super) fence_edits: usize,
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
            self.edit_fence_enabled,
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
    ///
    /// `edit_fence_enabled` idem (planner round: always `false`) — a plan
    /// that *quotes* a SEARCH/REPLACE block as a step to take later must
    /// stay plan text, not execute the edit during planning.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn complete_once_with(
        &self,
        model: &dyn ModelBackend,
        req: CompletionRequest,
        observer: &mut dyn TurnObserver,
        emit_deltas: bool,
        rescue_enabled: bool,
        envelope_enabled: bool,
        edit_fence_enabled: bool,
    ) -> Result<RoundOutcome, EngineError> {
        // Deadline de streaming por ronda (round-economics § 4.4 del
        // piloto): el reloj arranca ANTES del request — en un backend
        // HTTP los headers pueden tardar todo el prefill, y en el
        // LocalBackend el prefill corre dentro del stream; los dos
        // pertenecen a la ronda. Cada espera de abajo corre contra lo
        // que queda del deadline, así que la ronda entera —request,
        // prefill, generación, y también un stream que se quedó mudo—
        // queda acotada. `Instant` monotónico, misma razón que el
        // presupuesto del turno.
        let round_started = std::time::Instant::now();
        let deadline_error = |deadline: Duration| {
            let elapsed = round_started.elapsed();
            tracing::warn!(
                deadline_ms = deadline.as_millis(),
                elapsed_ms = elapsed.as_millis(),
                "round blew its per-round wall-clock deadline; abandoning the stream mid-generation"
            );
            EngineError::RoundWallClockExhausted {
                deadline_ms: deadline.as_millis(),
                elapsed_ms: elapsed.as_millis(),
            }
        };

        let mut stream = match self.max_round_wall_clock {
            Some(deadline) => match tokio::time::timeout(deadline, model.complete(req)).await {
                Ok(started) => started?,
                Err(_) => return Err(deadline_error(deadline)),
            },
            None => model.complete(req).await?,
        };

        let mut text_buffer = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<RoundUsage> = None;
        let mut saw_done = false;
        let mut truncated = false;

        loop {
            let next = match self.max_round_wall_clock {
                Some(deadline) => {
                    let Some(remaining) = deadline.checked_sub(round_started.elapsed()) else {
                        return Err(deadline_error(deadline));
                    };
                    match tokio::time::timeout(remaining, stream.next()).await {
                        Ok(next) => next,
                        // Dropear `stream` es la señal de cancelación
                        // río arriba: el LocalBackend detecta al
                        // consumidor caído y corta la generación; un
                        // backend HTTP cierra la conexión.
                        Err(_) => return Err(deadline_error(deadline)),
                    }
                }
                None => stream.next().await,
            };
            let Some(event) = next else { break };
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
        // Edit-fence (A/B del impuesto JSON, `crate::edit_fence`): the
        // instructed edit channel of the `+ablate:edit-fence` arm. Runs
        // BEFORE the envelope and the rescue ladder, outside their
        // accounting (same reasoning as the envelope below), and WITHOUT
        // the `tool_calls.is_empty()` guard — a response may legitimately
        // call `read_file` natively AND emit a fence edit in its text.
        let mut fence_edits: usize = 0;
        if edit_fence_enabled {
            let (calls, remaining_text) = crate::edit_fence::extract_edit_fence_calls(&text_buffer);
            if !calls.is_empty() {
                fence_edits = calls.len();
                tracing::info!(
                    count = fence_edits,
                    "parsed SEARCH/REPLACE edit-fence block(s) into edit_file call(s)"
                );
                tool_calls.extend(calls);
                text_buffer = remaining_text;
            }
        }

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
            fence_edits,
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
    use braze_events::{NoopObserver, TextDeltaObserver};
    use braze_model::CompletionEvent;
    use braze_session::{FileSessionStore, SimpleContextCompactor};
    use braze_types::SessionId;
    use std::sync::atomic::{AtomicU32, Ordering};

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

    /// Deadline de streaming por ronda (round-economics § 4.4 del
    /// piloto): una ronda cuyo stream se queda mudo a mitad de
    /// generación — la ronda desbocada que el corte en borde de ronda no
    /// puede acotar — tiene que fallar con `RoundWallClockExhausted` al
    /// vencer el deadline, no colgarse hasta un backstop externo.
    #[tokio::test]
    async fn a_round_that_stalls_mid_stream_fails_at_the_per_round_deadline() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = StallingModel::new(vec![(
            vec![CompletionEvent::TextDelta("pensando…".to_string())],
            true, // emite un delta y después silencio para siempre
        )]);

        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_round_wall_clock(Some(Duration::from_millis(50)));

        let started = std::time::Instant::now();
        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        match result {
            Err(EngineError::RoundWallClockExhausted {
                deadline_ms,
                elapsed_ms,
            }) => {
                assert_eq!(deadline_ms, 50);
                assert!(
                    elapsed_ms >= 50,
                    "el corte no puede llegar antes del deadline, got {elapsed_ms} ms"
                );
            }
            other => panic!("expected RoundWallClockExhausted, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "el corte tiene que ser por el deadline de la ronda, no por un backstop lejano"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El deadline por ronda acota cada ronda POR SEPARADO, no al turno:
    /// un turno de dos rondas donde cada una cabe en el deadline pero la
    /// suma no, converge normal. Sin esto, el deadline sería un
    /// presupuesto de turno redundante con `with_max_turn_wall_clock`.
    #[tokio::test]
    async fn the_per_round_deadline_bounds_each_round_not_the_turn() {
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
            ToolRegistry::new(vec![Box::new(
                crate::engine::test_support::SlowEchoToolProvider::new(
                    Arc::clone(&invocations),
                    // El tool duerme más que el deadline entero: el tiempo
                    // de tools NO corre contra el deadline de la ronda —
                    // eso ya lo acota `tool_completion_timeout`.
                    Duration::from_millis(120),
                ),
            )]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_max_round_wall_clock(Some(Duration::from_millis(80)));

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("dos rondas que caben cada una en el deadline convergen normal");
        assert_eq!(invocations.load(std::sync::atomic::Ordering::SeqCst), 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Lo que el deadline preserva y el backstop de infraestructura no:
    /// las rondas ya completadas del turno. La ronda 0 (tool call +
    /// Usage) persiste entera; la ronda 1 se desboca y el error sale por
    /// el camino normal de `run_turn`, así que el rollout log conserva la
    /// tool call y su resultado — la contabilidad que un
    /// `tokio::time::timeout` de afuera censura (J-21/J-10).
    #[tokio::test]
    async fn a_mid_turn_stall_preserves_the_completed_rounds_accounting() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = StallingModel::new(vec![
            (
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
                false,
            ),
            (vec![], true), // la ronda 1 nunca emite nada: desbocada
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
        .with_max_round_wall_clock(Some(Duration::from_millis(50)));

        let result = engine.run_turn(&session, "hola", &mut NoopObserver).await;
        assert!(
            matches!(result, Err(EngineError::RoundWallClockExhausted { .. })),
            "expected RoundWallClockExhausted, got {result:?}"
        );
        assert_eq!(
            invocations.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "el tool de la ronda 0 corrió completo antes del corte"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallCompleted { id, .. }
                    if id == "call-1")),
            "la ronda 0 completada tiene que estar persistida — esa es la contabilidad \
             que este corte preserva y el backstop censura"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // P1.1 resto (v9 L-5, 2026-08-18): clusters edit-fence, envelope
    // parsing y best-of-n (G10) movidos VERBATIM del `mod tests` de
    // engine/mod.rs; `envelope_kind` viaja con el de envelope.

    // --- Edit-fence (A/B del impuesto JSON,
    //     docs/hypothesis-2026-08-10-json-tax-edit-fence.md) ---

    /// El camino completo del brazo fence: el modelo emite prosa + un
    /// bloque SEARCH/REPLACE, el parser lo sintetiza como `edit_file`,
    /// dispatch lo ejecuta contra el provider real (schema-válido), y
    /// queda el rastro contable (`EditFenceApplied`, NUNCA
    /// `TextualRescueApplied` — la separación es el mecanismo del A/B).
    #[tokio::test]
    async fn an_edit_fence_block_is_parsed_dispatched_and_counted() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("Fixing the constant.\n\nsrc/lib.rs\n".to_string()),
                CompletionEvent::TextDelta(
                    "<<<<<<< SEARCH\nlet x = 1;\n=======\nlet x = 2;\n>>>>>>> REPLACE\n".to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EditRecordingToolProvider::new(Arc::clone(
                &calls,
            )))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        )
        .with_edit_fence_enabled(true);

        engine
            .run_turn(&session, "fix the constant", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "the fence edit must reach the tool");
        assert_eq!(
            recorded[0],
            serde_json::json!({
                "path": "src/lib.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;",
            }),
            "the block's sections must arrive verbatim as edit_file args"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::EditFenceApplied { blocks: 1 })),
            "the fence channel must persist its own bench-countable event"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextualRescueApplied { .. })),
            "the instructed fence channel must NOT count as a rescue"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("Fixing the constant.")
            )),
            "the surrounding prose must survive as the round's text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("SEARCH")
            )),
            "the consumed block must not be persisted as conversational text"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// La otra mitad del brazo: con el lever ON, `edit_file` no aparece
    /// en el inventario del request y el system prompt lleva la
    /// gramática del fence; con el lever OFF (default), ni lo uno ni lo
    /// otro — no-op estricto.
    #[tokio::test]
    async fn edit_fence_lever_hides_the_stub_and_injects_the_addendum() {
        for lever_on in [true, false] {
            let (store, dir) = temp_store();
            let session = SessionId::new();

            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let model = RequestCapturingModel {
                inner: ScriptedModel::new(vec![vec![
                    CompletionEvent::TextDelta("ok".to_string()),
                    CompletionEvent::Done,
                ]]),
                requests: Arc::clone(&requests),
            };

            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let engine = Engine::new(
                Box::new(model),
                ToolRegistry::new(vec![Box::new(EditRecordingToolProvider::new(calls))]),
                Arc::new(store),
                Box::new(SimpleContextCompactor::default()),
                Box::new(TestNotifier::new()),
                "system prompt".to_string(),
                1024,
            )
            .with_edit_fence_enabled(lever_on);

            engine
                .run_turn(&session, "hola", &mut NoopObserver)
                .await
                .expect("turn should succeed");

            let captured = requests.lock().unwrap().clone();
            assert!(!captured.is_empty());
            let req = &captured[0];
            let has_edit_stub = req.tool_stubs.iter().any(|s| s.name == "edit_file");
            let has_addendum = req.system_prompt.contains("<<<<<<< SEARCH");
            if lever_on {
                assert!(!has_edit_stub, "lever ON must hide the edit_file stub");
                assert!(has_addendum, "lever ON must inject the fence grammar");
            } else {
                assert!(has_edit_stub, "lever OFF must keep the edit_file stub");
                assert!(!has_addendum, "lever OFF must not touch the system prompt");
            }

            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    // --- Envelope parsing (A/B constrained decoding,
    //     docs/constrained-decoding-ab-design.md) ---

    #[test]
    fn parse_envelope_response_extracts_a_tool_call_with_its_reasoning() {
        let text = r#"{"action": "tool_call", "reasoning": "need the file",
                       "name": "read_file", "arguments": {"path": "x.txt"}}"#;
        match parse_envelope_response(text) {
            Some(EnvelopeResponse::ToolCall { call, reasoning }) => {
                assert_eq!(call.name, "read_file");
                assert_eq!(call.arguments, serde_json::json!({"path": "x.txt"}));
                assert!(call.id.starts_with("envelope-"));
                assert_eq!(reasoning.as_deref(), Some("need the file"));
            }
            other => panic!("expected a tool call, got {}", envelope_kind(&other)),
        }
    }

    #[test]
    fn parse_envelope_response_defaults_missing_arguments_to_an_empty_object() {
        let text = r#"{"action": "tool_call", "name": "list_dir"}"#;
        match parse_envelope_response(text) {
            Some(EnvelopeResponse::ToolCall { call, reasoning }) => {
                assert_eq!(call.arguments, serde_json::json!({}));
                assert_eq!(reasoning, None);
            }
            other => panic!("expected a tool call, got {}", envelope_kind(&other)),
        }
    }

    #[test]
    fn parse_envelope_response_rejects_non_object_arguments() {
        let text = r#"{"action": "tool_call", "name": "read_file", "arguments": "x.txt"}"#;
        assert!(parse_envelope_response(text).is_none());
    }

    #[test]
    fn parse_envelope_response_extracts_a_final_answer_and_drops_reasoning() {
        let text = r#"{"action": "final_answer", "reasoning": "done thinking", "text": "42"}"#;
        match parse_envelope_response(text) {
            Some(EnvelopeResponse::FinalAnswer { text }) => assert_eq!(text, "42"),
            other => panic!("expected a final answer, got {}", envelope_kind(&other)),
        }
    }

    #[test]
    fn parse_envelope_response_accepts_a_json_fenced_envelope() {
        let text = "```json\n{\"action\": \"final_answer\", \"text\": \"42\"}\n```";
        assert!(matches!(
            parse_envelope_response(text),
            Some(EnvelopeResponse::FinalAnswer { .. })
        ));
    }

    /// Non-envelope shapes must fall through untouched so the rescue
    /// ladder stays the owner of every other textual format: bare
    /// rescue-shape JSON (no `action`), an unknown action, and prose.
    #[test]
    fn parse_envelope_response_ignores_non_envelope_shapes() {
        assert!(
            parse_envelope_response(r#"{"name": "read_file", "arguments": {"path": "x"}}"#)
                .is_none()
        );
        assert!(
            parse_envelope_response(r#"{"action": "run", "name": "x", "arguments": {}}"#).is_none()
        );
        assert!(parse_envelope_response("I read the file and it says 42.").is_none());
        assert!(parse_envelope_response(r#"{"action": "final_answer"}"#).is_none());
    }

    fn envelope_kind(envelope: &Option<EnvelopeResponse>) -> &'static str {
        match envelope {
            Some(EnvelopeResponse::ToolCall { .. }) => "a tool call",
            Some(EnvelopeResponse::FinalAnswer { .. }) => "a final answer",
            None => "none",
        }
    }

    /// The envelope is the *primary* parse channel of prompt-tools mode,
    /// not a rescue: the call must dispatch, the `reasoning` must survive
    /// as the round's text, and — the A/B's mechanism check depends on
    /// this — NO `TextualRescueApplied` may be persisted for it.
    #[tokio::test]
    async fn an_envelope_tool_call_dispatches_without_counting_as_a_rescue() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    r#"{"action": "tool_call", "reasoning": "I will echo hi",
                       "name": "echo", "arguments": {"text": "hi"}}"#
                        .to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta(
                    r#"{"action": "final_answer", "text": "done"}"#.to_string(),
                ),
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
        .with_envelope_parsing_enabled(true);

        engine
            .run_turn(&session, "please echo hi", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        assert_eq!(invocations.load(Ordering::SeqCst), 1);

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::ToolCallCompleted { result, .. } if result.content == "echoed: hi")),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "I will echo hi"
            )),
            "the envelope's reasoning must survive as the round's text"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text == "done"
            )),
            "the final_answer's inner text must be the turn's final text"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"action\"")
            )),
            "the raw envelope JSON must never be persisted as conversational text"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextualRescueApplied { .. })),
            "an envelope parse must NOT count as a textual rescue — the \
             A/B's mechanism check is `rescues ≈ 0` on the constrained arm"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A `final_answer` envelope whose inner text happens to look like a
    /// bare-JSON tool call must stay text: the model explicitly declared
    /// it final, so the rescue ladder is suppressed for that round.
    #[tokio::test]
    async fn an_envelope_final_answer_is_never_reinterpreted_by_the_rescue_ladder() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let inner = r#"{\"name\": \"echo\", \"arguments\": {\"text\": \"hi\"}}"#;
        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(format!(
                r#"{{"action": "final_answer", "text": "{inner}"}}"#
            )),
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
        .with_envelope_parsing_enabled(true);

        engine
            .run_turn(
                &session,
                "show me the JSON for an echo call",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "a declared-final answer must not be dispatched as a tool call"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the inner text must be persisted verbatim as the answer"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Default-off is a strict no-op: without
    /// `with_envelope_parsing_enabled(true)` an envelope-shaped response
    /// takes the pre-existing path — the bare-JSON rescue fires on its
    /// `name`/`arguments` fields and counts as a rescue, exactly as it
    /// did before this lever existed.
    #[tokio::test]
    async fn envelope_parsing_disabled_leaves_the_pre_existing_rescue_path_intact() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    r#"{"action": "tool_call", "name": "echo", "arguments": {"text": "hi"}}"#
                        .to_string(),
                ),
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

        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextualRescueApplied { .. })),
            "with the lever off, the bare-JSON rescue owns this shape and must count as a rescue"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// F2 (docs/AUDITORIA-2026-07-v3.md): qwen3-coder's bare `<function=>`
    /// XML grammar has no native number type, so a `limit: integer`
    /// parameter comes back from the rescue as the string `"5"` — without
    /// schema-guided coercion this fails validation deterministically
    /// (every call to a tool with a numeric param, rescued via this
    /// format, would burn a repair round it can't even fix, since the
    /// XML grammar has no way to emit a JSON number). With the fix, the
    /// call dispatches on the first attempt and the tool receives a real
    /// JSON number.
    #[tokio::test]
    async fn qwen3_coder_xml_with_a_stringified_integer_param_gets_coerced_before_dispatch() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta(
                    "<function=echo_limit>\n<parameter=text>\nhi\n</parameter>\n\
                     <parameter=limit>\n5\n</parameter>\n</function>"
                        .to_string(),
                ),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("done".to_string()),
                CompletionEvent::Done,
            ],
        ]);

        let invocations = Arc::new(AtomicU32::new(0));
        let received_limit = Arc::new(std::sync::Mutex::new(None));
        let engine = Engine::new(
            Box::new(model),
            ToolRegistry::new(vec![Box::new(EchoWithLimitToolProvider::new(
                Arc::clone(&invocations),
                Arc::clone(&received_limit),
            ))]),
            Arc::new(store),
            Box::new(SimpleContextCompactor::default()),
            Box::new(TestNotifier::new()),
            "system prompt".to_string(),
            1024,
        );

        engine
            .run_turn(&session, "please echo hi with limit 5", &mut NoopObserver)
            .await
            .expect("turn should succeed — coercion must let validation pass on the first try");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "must dispatch exactly once — no schema-repair retry round needed"
        );
        assert_eq!(
            received_limit.lock().unwrap().clone(),
            Some(serde_json::json!(5)),
            "the tool must receive a real JSON number, not the string \"5\""
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression test for N-15 (docs/AUDITORIA-2026-07-v2.md):
    /// `with_textual_rescue_enabled(false)` must stop the rescue from
    /// dispatching a real tool a user only asked to see the JSON for —
    /// the raw text is persisted as ordinary conversational text instead.
    #[tokio::test]
    async fn textual_rescue_can_be_disabled() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta(
                r#"{"name": "echo", "arguments": {"text": "hi"}}"#.to_string(),
            ),
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
        .with_textual_rescue_enabled(false);

        engine
            .run_turn(
                &session,
                "muéstrame el JSON para invocar echo",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "the tool must never actually be invoked when the rescue is disabled"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::AssistantText { text } if text.contains("\"name\"")
            )),
            "the raw JSON must be persisted as ordinary text instead of dispatched"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- técnica G10: best-of-n / test-time scaling (docs/AUDITORIA-2026-07.md) ---

    /// Regression test for G10's core value proposition: a 2-vote
    /// majority ("hi") beats a 1-vote dissenter ("wrong") among 3
    /// candidates, and only the winning call is ever dispatched.
    #[tokio::test]
    async fn best_of_n_dispatches_the_majority_tool_call_signature() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-a".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "hi" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-b".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "wrong" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-c".to_string(),
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
        )
        .with_best_of_n(3);

        engine
            .run_turn(
                &session,
                "please echo hi (with a dissenting distractor)",
                &mut NoopObserver,
            )
            .await
            .expect("turn should succeed");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "only the winning candidate's call should ever reach the real tool"
        );

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        {
            Some(AgentEvent::ToolCallCompleted { result, .. }) => {
                assert_eq!(
                    result.content, "echoed: hi",
                    "the 2-vote majority ('hi') must win over the 1-vote dissenter ('wrong')"
                );
            }
            other => panic!("expected a ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A 1-vs-1 tie must resolve deterministically to the
    /// earliest-generated candidate — never `Iterator::max_by_key`'s
    /// "last wins" default, which would make the outcome depend on
    /// implementation details of the vote-counting loop.
    #[tokio::test]
    async fn best_of_n_breaks_ties_by_keeping_the_earliest_candidate() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-a".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "first" }),
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::ToolCallRequested {
                    id: "call-b".to_string(),
                    name: "echo".to_string(),
                    arguments: serde_json::json!({ "text": "second" }),
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
        )
        .with_best_of_n(2);

        engine
            .run_turn(&session, "please echo something", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::ToolCallCompleted { .. }))
        {
            Some(AgentEvent::ToolCallCompleted { result, .. }) => {
                assert_eq!(
                    result.content, "echoed: first",
                    "a 1-vs-1 tie must keep the earliest-generated candidate"
                );
            }
            other => panic!("expected a ToolCallCompleted, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `self.best_of_n` real model calls happen per round — the
    /// persisted `Usage` must reflect the *summed* cost across every
    /// candidate, not just the winner's, or token/cost accounting
    /// silently under-reports by every discarded candidate's share.
    /// `stop_reason` is taken from the winning candidate specifically.
    #[tokio::test]
    async fn best_of_n_sums_usage_across_candidates_and_keeps_the_winners_stop_reason() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        // Both candidates answer with the same plain text (no tool call
        // — same "no tool call" signature, so it's a 1-vs-1 tie and
        // candidate 0 wins per the tie-break rule tested above).
        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    stop_reason: Some("end_turn".to_string()),
                    // Deliberately mismatched Some/None across candidates
                    // — exercises `sum_optional_u32`'s "at least one
                    // candidate reported it" rule, not just the trivial
                    // both-Some or both-None cases.
                    cache_read_tokens: Some(6),
                    cache_write_tokens: None,
                    escalation_trigger: None,
                },
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("hola".to_string()),
                CompletionEvent::Usage {
                    input_tokens: 20,
                    output_tokens: 8,
                    stop_reason: Some("stop_sequence".to_string()),
                    cache_read_tokens: Some(4),
                    cache_write_tokens: Some(2),
                    escalation_trigger: None,
                },
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
        .with_best_of_n(2);

        engine
            .run_turn(&session, "hola", &mut NoopObserver)
            .await
            .expect("turn should succeed");

        let verify_store = FileSessionStore::new(dir.clone());
        let events = verify_store.load(&session).await.expect("load events");
        match events
            .iter()
            .find(|e| matches!(e, AgentEvent::Usage { .. }))
        {
            Some(AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            }) => {
                assert_eq!(
                    *input_tokens, 30,
                    "usage must sum every candidate's cost, not just the winner's"
                );
                assert_eq!(*output_tokens, 13);
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("end_turn"),
                    "stop_reason must reflect the winning candidate specifically"
                );
                assert_eq!(
                    *cache_read_tokens,
                    Some(10),
                    "cache_read_tokens must sum across candidates like input/output do"
                );
                assert_eq!(
                    *cache_write_tokens,
                    Some(2),
                    "one candidate's None must not zero out the other's reported value"
                );
            }
            other => panic!("expected a Usage event, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// `best_of_n: 0` (e.g. from a misconfigured env var) must degrade
    /// gracefully to the same single-call path as the default (`1`),
    /// not panic on an empty candidate vec.
    #[tokio::test]
    async fn best_of_n_set_to_zero_behaves_like_disabled_not_a_panic() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![vec![
            CompletionEvent::TextDelta("hola".to_string()),
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
        .with_best_of_n(0);

        let mut streamed = String::new();
        engine
            .run_turn(
                &session,
                "hola",
                &mut TextDeltaObserver(|chunk: &str| streamed.push_str(chunk)),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(streamed, "hola");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Deltas from individual best-of-n candidates never reach the
    /// observer live (there's no single "the" answer to show until the
    /// vote resolves one) — only the winner's full text arrives, as one
    /// delta, right after voting.
    #[tokio::test]
    async fn best_of_n_suppresses_live_deltas_but_delivers_the_winners_full_text_once() {
        let (store, dir) = temp_store();
        let session = SessionId::new();

        let model = ScriptedModel::new(vec![
            vec![
                CompletionEvent::TextDelta("respuesta ".to_string()),
                CompletionEvent::TextDelta("candidata".to_string()),
                CompletionEvent::Done,
            ],
            vec![
                CompletionEvent::TextDelta("otra ".to_string()),
                CompletionEvent::TextDelta("respuesta".to_string()),
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
        .with_best_of_n(2);

        let mut observer = RecordingObserver {
            deltas: Vec::new(),
            events: Vec::new(),
        };
        engine
            .run_turn(&session, "hola", &mut observer)
            .await
            .expect("turn should succeed");

        // Neither candidate's individual deltas streamed live — exactly
        // one delta arrives, carrying the (tied, so earliest-kept)
        // winner's whole text in one shot.
        assert_eq!(observer.deltas, vec!["respuesta candidata".to_string()]);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
