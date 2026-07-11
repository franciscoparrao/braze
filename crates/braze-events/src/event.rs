use braze_types::ToolResult;
use serde::{Deserialize, Serialize};

/// One entry in a session's event log. This is the vocabulary
/// `braze-session::SessionStore` persists and `ContextCompactor` splits
/// into durable state vs. tactical (live) conversation window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    UserMessage {
        text: String,
    },
    AssistantText {
        text: String,
    },
    /// The assistant requested a tool invocation. Captured separately from
    /// `ToolCallStarted` (which only records id/name/background) because
    /// reconstructing message history for the next model call requires the
    /// full `tool_use` block the assistant produced — the Anthropic API
    /// requires that block to appear in history before the matching
    /// `tool_result` (see `braze-engine::history`).
    AssistantToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolCallStarted {
        id: String,
        name: String,
        background: bool,
    },
    ToolCallCompleted {
        id: String,
        result: ToolResult,
    },
    CompactionOccurred {
        summary: String,
        dropped_tokens_estimate: u32,
    },
    PermissionRequested {
        action: String,
        reversible: bool,
        /// Coarse identity of the action being requested, if the caller
        /// could derive one (see `braze_permissions::derive_permission_key`).
        /// `#[serde(default)]` so rollout logs persisted before this field
        /// existed still deserialize, with `key: None`. `deserialize_with`
        /// (N-40, docs/AUDITORIA-2026-07-v2.md): a `PermissionKey` variant
        /// this binary doesn't recognize (written by a newer one) falls
        /// back to `None` for just this field instead of failing to
        /// deserialize this whole event — which would otherwise abort
        /// `load()` for the entire session at that line.
        #[serde(
            default,
            deserialize_with = "braze_types::deserialize_permission_key_lossy"
        )]
        key: Option<braze_types::PermissionKey>,
    },
    PermissionDecided {
        action: String,
        allowed: bool,
        /// Same coarse identity as `PermissionRequested::key`. When
        /// `allowed` is `true` and this is `Some`, a resumed session
        /// replays it back into a fresh `PermissionGuard` via
        /// `PermissionGuard::seed_remembered` so the same action isn't
        /// re-confirmed. `#[serde(default)]` for the same backward-compat
        /// reason as `PermissionRequested::key`; `deserialize_with` for
        /// the same N-40 lossy-fallback reason.
        #[serde(
            default,
            deserialize_with = "braze_types::deserialize_permission_key_lossy"
        )]
        key: Option<braze_types::PermissionKey>,
    },
    /// Token usage reported by the model backend for one completion round.
    /// Audit-only, like `ToolCallStarted`/`CompactionOccurred` — never
    /// rendered back into model history (see
    /// `braze-engine::history::event_to_message`). Added so tooling
    /// (e.g. `braze-bench`) can read per-round usage back out of the
    /// rollout log without `braze-engine` needing to expose it any other
    /// way.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        /// The provider's reason the round stopped (Anthropic's
        /// `stop_reason`, Ollama's `done_reason`), when the backend
        /// reports one — e.g. `"end_turn"`/`"stop"` for a normal
        /// completion vs. `"max_tokens"`/`"length"` for output truncated
        /// by the `max_tokens` budget. A tool call whose JSON arguments
        /// got cut off mid-stream by `max_tokens` fails to parse and is
        /// silently dropped with no other signal of *why* — this is what
        /// lets that be diagnosed instead of just observed as "the model
        /// gave up". `#[serde(default)]` for backward compat with rollout
        /// logs written before this field existed.
        #[serde(default)]
        stop_reason: Option<String>,
        /// Tokens of this round's prompt that hit an existing cache entry
        /// (OpenRouter's `usage.prompt_tokens_details.cached_tokens` —
        /// reported for any underlying provider that supports caching,
        /// whether it needed an explicit `cache_control` marker or cached
        /// automatically). `None` means "not reported", never a
        /// fabricated `Some(0)` — a caller needs to tell "this backend
        /// doesn't report caching" apart from "this round genuinely had
        /// no cache hit". `#[serde(default)]` for backward compat with
        /// rollout logs written before this field existed.
        #[serde(default)]
        cache_read_tokens: Option<u32>,
        /// Tokens newly written to cache by this round (billed at a
        /// premium over normal input price) — OpenRouter's
        /// `usage.prompt_tokens_details.cache_write_tokens`. Same
        /// `None`-means-"not reported" contract and backward-compat
        /// `#[serde(default)]` as `cache_read_tokens`.
        #[serde(default)]
        cache_write_tokens: Option<u32>,
    },
    /// A plan produced by the optional planner model before the turn's
    /// first executor round — PLAN.md § "Split planificador/ejecutor
    /// (`with_planner`)". Persisted as a first-class event (not a
    /// side-channel) so it survives `--resume`, gets digested by
    /// compaction, reaches the TUI via `TurnObserver`, and lets
    /// `braze-bench` tell planned turns apart from unplanned ones.
    /// Rendered into model history as an *assistant* text block
    /// (`braze-engine::history`) — the "model follows its own plan"
    /// framing is deliberate. Additive amendment to the frozen contract,
    /// same precedent as `AssistantToolCall`/`Usage`: an older binary
    /// reading a log with this event deserializes it as [`Self::Unknown`].
    PlanCreated {
        plan: String,
    },
    /// A tool call the model emitted as plain text in a native
    /// tool-template format (Qwen's `<tool_call>` tag, qwen3-coder's bare
    /// `<function=...>` XML, Llama's pythonic `func(...)`, or a bare JSON
    /// object) instead of a structured `tool_calls` entry, and
    /// `Engine::complete_once_with`'s rescue ladder recovered it (H-3,
    /// docs/AUDITORIA-2026-07-v5.md). Audit-only, same as
    /// `CompactionOccurred`/`Usage` — never rendered back into model
    /// history. Exists so `braze-bench` can count how often this
    /// SLM-first harness technique actually fires per task/skill, instead
    /// of the count only ever showing up as a `tracing::info!` line.
    TextualRescueApplied {
        /// Which rung of the rescue ladder succeeded — a short label like
        /// `"<tool_call> tagged (Qwen/Hermes)"` or `"plain-text fallback"`,
        /// matching the `format`/description already logged at the call
        /// site in `engine.rs`.
        parser: String,
    },
    /// `EscalatingBackend` (`braze-model::escalation`) routed this round to
    /// its lead model reactively, because the worker had a trailing streak
    /// of failed observations at or past its configured threshold (H-3,
    /// docs/AUDITORIA-2026-07-v5.md). Emitted only on the round that
    /// *triggers* the escalation window (`RouteDecision::LeadEscalating`),
    /// not on every subsequent round already inside that window
    /// (`RouteDecision::LeadEscalated`) — so counting these events counts
    /// escalation *episodes*, not raw lead-model calls.
    EscalationToLead {
        /// Human-readable reason, e.g. `"3 consecutive failed observations
        /// (threshold 2)"` — enough to eyeball in a rollout log without
        /// cross-referencing `EscalatingBackend`'s internal counters.
        trigger: String,
    },
    /// `Engine::attempt_tools_free_summary_round` was invoked — the turn's
    /// last round came back with neither text nor a tool call after the
    /// turn already made real progress, so the engine gave it one more
    /// shot with tools disabled, asking it to summarize what it found so
    /// far (H-3, docs/AUDITORIA-2026-07-v5.md). Emitted regardless of
    /// whether that attempt actually produced usable text — this event
    /// records that the fallback was *reached for*, not that it succeeded
    /// (success is separately visible as the `AssistantText` it may or may
    /// not append right after).
    SummaryFallbackAttempted,
    /// An operational note from the harness to the model, injected into
    /// the conversation at the moment it's actionable (A′.2,
    /// docs/harness-engineering-hooks-skills-2026-07-10.md § I.2): "80%
    /// of the turn's token budget is spent, finish now", "the next round
    /// is this turn's last". UNLIKE the audit-only H-3 lever events
    /// above, this one IS rendered back into model history (as a
    /// user-role text block — see `braze_engine::history::event_to_block`):
    /// its whole purpose is that the model sees it. A frontier model
    /// infers "I've used many rounds, I should wrap up"; a small model
    /// doesn't — it explores until the harness kills the turn, and the
    /// cut counts as a model failure when an announced deadline might
    /// have converged.
    HarnessNote {
        /// Machine-readable kind — `"turn_budget"` / `"iteration_cap"` —
        /// so `braze-bench` can count emissions per kind and the
        /// `no-harness-notes` ablation A/B can attribute effects.
        kind: String,
        /// The note the model sees, verbatim.
        text: String,
    },
    /// An audit-only engine hook (Paquete B′,
    /// docs/harness-engineering-hooks-skills-2026-07-10.md § Parte II)
    /// crossed its consecutive-failure threshold and was disabled for
    /// the rest of the session. Persisted exactly once per disable (not
    /// per failure — individual failures are `tracing::warn!` only) so
    /// the rollout log records that observability was lost from this
    /// point on, without a broken hook spamming the log. Audit-only,
    /// never rendered to the model; hook dispatch itself skips this
    /// variant so a failing hook can't feed back into hooks.
    HookErrored {
        /// The hook's stable id (`EngineHook::id`).
        id: String,
        /// Attach point that failed: `"on_event"` / `"before_model_request"`.
        point: String,
        /// The failure that crossed the threshold (an `Err` payload, or
        /// a timeout description).
        reason: String,
    },
    /// A skill's body was loaded into the turn's system prompt (D′,
    /// docs/harness-engineering-hooks-skills-2026-07-10.md § Parte III).
    /// Audit-only — the body itself is request-scoped (rebuilt from the
    /// registry, never persisted as conversation), so this event is the
    /// rollout log's only trace of WHAT guidance the model was given and
    /// what it cost; braze-bench counts these for the skills A/B.
    SkillLoaded {
        /// Normalized skill name.
        name: String,
        /// How it got selected — `"explicit_mention"` is the only v1
        /// trigger; the study's router (`"router_match"`) arrives with
        /// its own A/B or not at all.
        trigger: String,
        /// ~4 chars/token over the (possibly capped) injected body.
        estimated_tokens: u32,
        /// Whether the body hit `max_body_tokens` and was cut.
        truncated: bool,
    },
    /// A skill selection was recognized but NOT loaded — over the
    /// per-turn cap, or the file became unreadable after discovery.
    /// Same audit-only posture as [`AgentEvent::SkillLoaded`].
    SkillLoadSkipped {
        name: String,
        reason: String,
    },
    /// Catch-all for a `"type"` tag this binary's enum doesn't have a
    /// variant for (C9, docs/AUDITORIA-2026-07.md). `AgentEvent`'s serde
    /// shape is a frozen contract (PLAN.md) — a new variant is the only
    /// additive way to evolve it, and without this fallback, an older
    /// binary reading a rollout log written by a newer one (with a
    /// variant it doesn't know) fails `load` for the *entire* session at
    /// that line, not just the one it can't understand. `#[serde(other)]`
    /// on a fieldless variant is serde's own forward-compatibility escape
    /// hatch for internally-tagged enums: any unrecognized `type` value
    /// deserializes to this variant instead of erroring, discarding the
    /// rest of that line's fields (nothing useful to keep from a shape
    /// this binary has no definition for). Downstream code treats it like
    /// any other audit-only event — see
    /// `braze_session::SimpleContextCompactor::compact_tactical` and
    /// `braze_engine::history::event_to_message`.
    ///
    /// Known accepted limitation (bajo, docs/AUDITORIA-2026-07-v2.md,
    /// "AgentEvent::Unknown pierde el payload al replicarse en
    /// backtrack"): serde's `#[serde(other)]` for an internally-tagged
    /// enum only supports a unit variant — it cannot carry the original
    /// JSON's other fields alongside it, by construction of how serde
    /// resolves the tag before deserializing the rest of the object.
    /// Carrying the raw payload here would mean replacing this derive
    /// with a hand-written `Deserialize` for the whole enum — a much
    /// larger change to a frozen-contract type than this narrow case
    /// justifies. Practical effect: if `braze-tui`'s backtrack replicates
    /// a session containing an event type *this* binary doesn't
    /// recognize (written by a newer binary), the replicated copy loses
    /// that event's original fields — the untouched original session file
    /// still has them; only the new, backtracked-into session doesn't.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates loading a rollout log line written before this field
    /// existed: the JSON has no `key` at all. `#[serde(default)]` must
    /// still let it deserialize, defaulting to `None`.
    #[test]
    fn permission_decided_without_a_key_field_deserializes_with_none() {
        let json = r#"{"type":"permission_decided","action":"run `mv a b`","allowed":true}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::PermissionDecided {
                action,
                allowed,
                key,
            } => {
                assert_eq!(action, "run `mv a b`");
                assert!(allowed);
                assert_eq!(key, None);
            }
            other => panic!("expected PermissionDecided, got {other:?}"),
        }
    }

    /// Regression test for N-40 (docs/AUDITORIA-2026-07-v2.md): a
    /// `PermissionDecided` event carrying a `key` shape this binary
    /// doesn't recognize (simulating a `PermissionKey` variant a newer
    /// binary added) must still deserialize the whole event, with
    /// `key: None` — not fail the entire line (and, previously, abort
    /// `load()` for the whole session at that point).
    #[test]
    fn permission_decided_with_an_unrecognized_key_shape_still_deserializes() {
        let json = r#"{"type":"permission_decided","action":"run `mv a b`","allowed":true,"key":{"SomeFutureVariant":{"field":"value"}}}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::PermissionDecided { key, .. } => assert_eq!(key, None),
            other => panic!("expected PermissionDecided, got {other:?}"),
        }
    }

    #[test]
    fn plan_created_round_trips_through_json() {
        let event = AgentEvent::PlanCreated {
            plan: "1. leer el archivo\n2. responder".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("\"plan_created\""),
            "snake_case tag expected, got: {json}"
        );
        let decoded: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            AgentEvent::PlanCreated { plan } => {
                assert_eq!(plan, "1. leer el archivo\n2. responder");
            }
            other => panic!("expected PlanCreated, got {other:?}"),
        }
    }

    #[test]
    fn usage_round_trips_through_json() {
        let event = AgentEvent::Usage {
            input_tokens: 123,
            output_tokens: 45,
            stop_reason: Some("end_turn".to_string()),
            cache_read_tokens: Some(100),
            cache_write_tokens: Some(20),
        };
        let json = serde_json::to_string(&event).unwrap();
        let round_tripped: AgentEvent = serde_json::from_str(&json).unwrap();
        match round_tripped {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                stop_reason,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                assert_eq!(input_tokens, 123);
                assert_eq!(output_tokens, 45);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(cache_read_tokens, Some(100));
                assert_eq!(cache_write_tokens, Some(20));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    /// Simulates loading a rollout log line written before `stop_reason`
    /// existed: the JSON has no such field at all. `#[serde(default)]`
    /// must still let it deserialize, defaulting to `None`.
    #[test]
    fn usage_without_a_stop_reason_field_deserializes_with_none() {
        let json = r#"{"type":"usage","input_tokens":10,"output_tokens":5}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::Usage { stop_reason, .. } => assert_eq!(stop_reason, None),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    /// Same backward-compat contract as `usage_without_a_stop_reason_field_deserializes_with_none`,
    /// for the two cache-token fields added later (prompt-caching design,
    /// docs/usability-log-2026-07-07-si2.md) — a rollout log written
    /// before they existed must still deserialize, defaulting both to
    /// `None` rather than failing the whole session load.
    #[test]
    fn usage_without_cache_token_fields_deserializes_with_none() {
        let json =
            r#"{"type":"usage","input_tokens":10,"output_tokens":5,"stop_reason":"stop"}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        match event {
            AgentEvent::Usage {
                cache_read_tokens,
                cache_write_tokens,
                ..
            } => {
                assert_eq!(cache_read_tokens, None);
                assert_eq!(cache_write_tokens, None);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    /// Regression test for C9: a `"type"` value this enum has no variant
    /// for (simulating a rollout log written by a newer binary with an
    /// event kind this one predates) must deserialize to `Unknown`
    /// instead of failing — the whole point of the forward-compat escape
    /// hatch.
    #[test]
    fn unrecognized_type_tag_deserializes_as_unknown_instead_of_erroring() {
        let json = r#"{"type":"some_future_event_kind","whatever":"fields","it":1}"#;
        let event: AgentEvent = serde_json::from_str(json).expect("must deserialize");
        assert!(matches!(event, AgentEvent::Unknown));
    }

    // --- H-3 (docs/AUDITORIA-2026-07-v5.md): SLM-lever events ---

    #[test]
    fn textual_rescue_applied_round_trips_through_json() {
        let event = AgentEvent::TextualRescueApplied {
            parser: "<tool_call> tagged (Qwen/Hermes)".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("\"textual_rescue_applied\""),
            "snake_case tag expected, got: {json}"
        );
        let decoded: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            AgentEvent::TextualRescueApplied { parser } => {
                assert_eq!(parser, "<tool_call> tagged (Qwen/Hermes)");
            }
            other => panic!("expected TextualRescueApplied, got {other:?}"),
        }
    }

    #[test]
    fn escalation_to_lead_round_trips_through_json() {
        let event = AgentEvent::EscalationToLead {
            trigger: "3 consecutive failed observations (threshold 2)".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("\"escalation_to_lead\""),
            "snake_case tag expected, got: {json}"
        );
        let decoded: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            AgentEvent::EscalationToLead { trigger } => {
                assert_eq!(trigger, "3 consecutive failed observations (threshold 2)");
            }
            other => panic!("expected EscalationToLead, got {other:?}"),
        }
    }

    #[test]
    fn summary_fallback_attempted_round_trips_through_json() {
        let event = AgentEvent::SummaryFallbackAttempted;
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("\"summary_fallback_attempted\""),
            "snake_case tag expected, got: {json}"
        );
        let decoded: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, AgentEvent::SummaryFallbackAttempted));
    }

    /// A′.2: `HarnessNote` round-trips with its snake_case tag and both
    /// fields intact — it's the one operational event that gets rendered
    /// back to the model, so losing `text` on reload would silently
    /// change what a resumed session's model sees.
    #[test]
    fn harness_note_round_trips_through_json() {
        let event = AgentEvent::HarnessNote {
            kind: "turn_budget".to_string(),
            text: "over 80% of budget".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("\"harness_note\""),
            "snake_case tag expected, got: {json}"
        );
        let decoded: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            AgentEvent::HarnessNote { kind, text } => {
                assert_eq!(kind, "turn_budget");
                assert_eq!(text, "over 80% of budget");
            }
            other => panic!("expected HarnessNote, got {other:?}"),
        }
    }

    /// B′: `HookErrored` round-trips with its snake_case tag and all
    /// three fields — the rollout log's record of lost observability.
    #[test]
    fn hook_errored_round_trips_through_json() {
        let event = AgentEvent::HookErrored {
            id: "prompt-budget-audit".to_string(),
            point: "on_event".to_string(),
            reason: "timed out after 250ms".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("\"hook_errored\""),
            "snake_case tag expected, got: {json}"
        );
        let decoded: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            AgentEvent::HookErrored { id, point, reason } => {
                assert_eq!(id, "prompt-budget-audit");
                assert_eq!(point, "on_event");
                assert_eq!(reason, "timed out after 250ms");
            }
            other => panic!("expected HookErrored, got {other:?}"),
        }
    }

    /// D′: the two skill events round-trip with their snake_case tags.
    #[test]
    fn skill_events_round_trip_through_json() {
        let loaded = AgentEvent::SkillLoaded {
            name: "testing".to_string(),
            trigger: "explicit_mention".to_string(),
            estimated_tokens: 300,
            truncated: false,
        };
        let json = serde_json::to_string(&loaded).expect("serialize");
        assert!(json.contains("\"skill_loaded\""), "got: {json}");
        assert!(matches!(
            serde_json::from_str::<AgentEvent>(&json).expect("deserialize"),
            AgentEvent::SkillLoaded { ref name, .. } if name == "testing"
        ));

        let skipped = AgentEvent::SkillLoadSkipped {
            name: "review".to_string(),
            reason: "per-turn cap (2) reached".to_string(),
        };
        let json = serde_json::to_string(&skipped).expect("serialize");
        assert!(json.contains("\"skill_load_skipped\""), "got: {json}");
    }

    /// A rollout log written by an older binary (before H-3) obviously
    /// never contains these tags — but the inverse matters too: a binary
    /// that *predates* H-3 reading a log written by a newer one must fall
    /// back to `Unknown` for these tags rather than failing `load()` for
    /// the whole session, same C9 contract as any other future variant.
    /// Simulated here by feeding a tag this test build DOES know, through
    /// the same code path `Unknown`'s own test uses — a real predates-H-3
    /// binary can't be built in this test, so this just re-confirms the
    /// mechanism (`#[serde(other)]`) generalizes to these new tags too by
    /// construction, not by a tag-specific carve-out.
    #[test]
    fn h3_event_tags_are_ordinary_internally_tagged_variants_not_special_cased() {
        for tag in ["textual_rescue_applied", "escalation_to_lead", "summary_fallback_attempted"] {
            let json = format!(r#"{{"type":"{tag}_but_from_the_future","x":1}}"#);
            let event: AgentEvent = serde_json::from_str(&json).expect("must deserialize");
            assert!(matches!(event, AgentEvent::Unknown));
        }
    }
}
