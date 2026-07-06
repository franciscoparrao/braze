use braze_events::AgentEvent;

use crate::compactor::{ContextCompactor, DurableState};
use crate::error::SessionError;

/// Default size of the raw tactical window kept by [`SimpleContextCompactor`]
/// when constructed via [`SimpleContextCompactor::default`].
pub const DEFAULT_TACTICAL_WINDOW: usize = 20;

/// MVP [`ContextCompactor`]: a real but simple split, not a tuned
/// summarizer (per PLAN.md).
///
/// ## Split semantics
///
/// Given the full raw event log:
/// 1. The last `tactical_window` events are always kept raw in the
///    returned tactical vector — this is the live conversational window
///    the engine still shows the model verbatim.
/// 2. Of the events *older* than that window, the ones that are already
///    "settled" (`ToolCallCompleted`, `CompactionOccurred`,
///    `PermissionDecided`, `AssistantToolCall` — completed tool results,
///    resolved permission decisions, and the `tool_use` requests that
///    precede them) are moved into `DurableState::durable_events` and
///    never re-summarized again.
/// 3. Every `CompactionOccurred` summary found anywhere in the log —
///    *including* one still inside the tactical window (N-3,
///    docs/AUDITORIA-2026-07-v2.md; a `CompactionOccurred` never renders
///    into a message on its own either way, so harvesting it early is
///    free) — is concatenated into `DurableState::summary`, so callers
///    get a running plain-text digest of everything compacted so far
///    without having to re-derive it from `durable_events`, and without a
///    blackout window right after the compaction that produced it.
///
/// ## The no-silent-loss invariant
///
/// PLAN.md does not specify what should happen to events older than the
/// tactical window that are *not* one of the three canonical durable
/// types (e.g. a `UserMessage` or `AssistantText` that aged out of the
/// window without ever being folded into a `CompactionOccurred` event —
/// this happens if the engine hasn't run `compact_tactical` recently
/// enough, or is calling `split` on a log for the first time). Rather
/// than invent a bespoke fallback summarizer for that edge case, this
/// implementation keeps the invariant simple: **every event in the input
/// ends up in exactly one of `durable_events`, the returned tactical
/// vector, or is already represented in `durable.summary` by a
/// compaction it chronologically precedes** — never silently
/// unaccounted for. Concretely, an "orphaned" older non-durable-typed
/// event is kept raw in the tactical vector (in original order, ahead of
/// the true last-N raw window) *unless* a later `CompactionOccurred`
/// exists in the log, in which case its content is already folded into
/// that summary and it is dropped rather than re-surfaced (see
/// `last_compaction_index` in [`SimpleContextCompactor::split`]) — this
/// is what keeps repeated compaction differential instead of
/// re-summarizing an ever-growing, mostly-redundant backlog every round.
#[derive(Debug, Clone, Copy)]
pub struct SimpleContextCompactor {
    tactical_window: usize,
}

impl SimpleContextCompactor {
    /// Creates a compactor that keeps the last `tactical_window` raw
    /// events in the tactical vector on every `split`.
    pub fn new(tactical_window: usize) -> Self {
        Self { tactical_window }
    }
}

impl Default for SimpleContextCompactor {
    fn default() -> Self {
        Self::new(DEFAULT_TACTICAL_WINDOW)
    }
}

fn is_settled_durable(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::ToolCallCompleted { .. }
            | AgentEvent::CompactionOccurred { .. }
            | AgentEvent::PermissionDecided { .. }
            // An old `tool_use` must migrate to `durable_events` together
            // with its matching `ToolCallCompleted`, in the same relative
            // order — otherwise the `tool_use` would be left orphaned in
            // `tactical` while its `tool_result` sits in
            // `durable_events`, which would be inconsistent given that
            // `braze-engine::history` now renders both sides
            // (`event_to_message_cleared`).
            | AgentEvent::AssistantToolCall { .. }
    )
}

/// Caps on how many items of each kind [`compact_tactical`]'s digest keeps
/// (always the most recent ones) — bounds the digest's own size
/// regardless of how large the folded backlog is, since a session that ran
/// a long time before its first compaction can hand `compact_tactical` an
/// arbitrarily large `tactical` slice.
const DIGEST_MAX_USER_REQUESTS: usize = 8;
const DIGEST_MAX_TOOL_CALLS: usize = 15;
const DIGEST_MAX_TOOL_ERRORS: usize = 8;

/// Truncates `text` to its first `max_words` whitespace-separated words,
/// appending an ellipsis if anything was cut. Operates on
/// [`str::split_whitespace`], which is UTF-8-safe (never splits inside a
/// multi-byte character) unlike a raw byte-length truncation.
fn truncate_words(text: &str, max_words: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max_words {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        format!("{}...", words[..max_words].join(" "))
    }
}

/// Best-effort one-line description of a tool call's arguments for the
/// digest: prefers a handful of common parameter names that tend to be the
/// single most identifying value (a path, a pattern, a shell command),
/// falling back to a truncated raw JSON dump so no argument shape ever
/// panics or produces an empty description.
fn summarize_tool_arguments(arguments: &serde_json::Value) -> String {
    for key in ["path", "pattern", "command", "text"] {
        if let Some(value) = arguments.get(key) {
            match value {
                serde_json::Value::String(s) => return truncate_words(s, 8),
                serde_json::Value::Array(items) => {
                    let joined = items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !joined.is_empty() {
                        return truncate_words(&joined, 8);
                    }
                }
                _ => {}
            }
        }
    }
    truncate_words(&arguments.to_string(), 8)
}

impl ContextCompactor for SimpleContextCompactor {
    fn split(&self, events: &[AgentEvent]) -> (DurableState, Vec<AgentEvent>) {
        let window_len = self.tactical_window.min(events.len());
        let window_start = events.len() - window_len;

        // Position of the most recent `CompactionOccurred` in the log, if
        // any. `Engine::load_messages` always folds the *entire* current
        // orphan backlog when it compacts (see its doc comment), so any
        // non-durable-typed ("orphan": `UserMessage`, `AssistantText`,
        // `ToolCallStarted`, `Usage`, `PermissionRequested`) event at an
        // earlier log position is already represented in that (or an even
        // earlier) compaction's summary text. Without this check, orphan
        // events never satisfy `is_settled_durable` and would keep
        // reappearing in `tactical` forever once they age out of the
        // window — pushing `tactical.len()` past the compaction threshold
        // on every subsequent call and re-triggering compaction every
        // round on an ever-growing, mostly-redundant backlog (see
        // docs/AUDITORIA-2026-07.md, hallazgos A2/C2).
        let last_compaction_index = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, AgentEvent::CompactionOccurred { .. }))
            .map(|(i, _)| i)
            .next_back();

        let mut durable_events = Vec::new();
        let mut summary_parts = Vec::new();
        let mut tactical = Vec::new();

        for (i, event) in events.iter().enumerate() {
            if i >= window_start {
                // Inside the raw tactical window: always kept verbatim,
                // regardless of type. A `CompactionOccurred` in here still
                // needs its summary harvested now, not just once it ages
                // out of the window (N-3, docs/AUDITORIA-2026-07-v2.md):
                // `history::event_to_block`/`event_to_block_cleared` never
                // render a `CompactionOccurred` into a message body either
                // way (it's audit-only, matched by the same arm as
                // `ToolCallStarted`/`Usage`), so without this, the summary
                // text is invisible to the model for the entire window
                // (~`tactical_window` events / several rounds) right after
                // the compaction that produced it — including the very
                // context (e.g. the user's original request) that
                // compaction was supposed to preserve.
                if let AgentEvent::CompactionOccurred { summary, .. } = event {
                    summary_parts.push(summary.clone());
                }
                tactical.push(event.clone());
                continue;
            }

            if is_settled_durable(event) {
                if let AgentEvent::CompactionOccurred { summary, .. } = event {
                    summary_parts.push(summary.clone());
                }
                durable_events.push(event.clone());
            } else if last_compaction_index.is_some_and(|lc| i < lc) {
                // Already folded into an earlier compaction's summary —
                // its content lives in `durable.summary` now, so it is
                // deliberately dropped here rather than re-surfaced as
                // tactical again (see the comment on
                // `last_compaction_index` above).
                continue;
            } else {
                // Orphaned older non-durable-typed event that no
                // compaction has covered yet — see the "no-silent-loss
                // invariant" doc comment above.
                tactical.push(event.clone());
            }
        }

        let durable = DurableState {
            summary: summary_parts.join(" "),
            durable_events,
        };
        (durable, tactical)
    }

    /// Extractive, deterministic digest (no LLM call — see PLAN.md /
    /// docs/SOTA-2026-07.md for why the compactor stays LLM-free) of what
    /// `tactical` is about to lose in raw form. Earlier versions of this
    /// method reported *only* event-type counts ("3 user message(s), 2
    /// tool call(s)...") with zero actual content — after compaction the
    /// model had no way to recover what the user asked for, which files
    /// were touched, or what failed, only how many of each. This instead
    /// pulls out short, concrete fragments (see `docs/AUDITORIA-2026-07.md`
    /// hallazgo C6): truncated user requests, `tool(args)` call sites,
    /// tool errors, and the most recent assistant reply — capped (see
    /// `DIGEST_MAX_*`) so the digest itself stays small regardless of how
    /// much backlog is being folded.
    fn compact_tactical(&self, tactical: &[AgentEvent]) -> Result<String, SessionError> {
        if tactical.is_empty() {
            return Ok("No tactical events to compact.".to_string());
        }

        let mut user_requests = Vec::new();
        let mut tool_calls = Vec::new();
        let mut tool_errors = Vec::new();
        let mut last_assistant_reply = None;
        let mut tool_names_by_id: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();

        for event in tactical {
            match event {
                AgentEvent::UserMessage { text } => {
                    user_requests.push(truncate_words(text, 15));
                }
                AgentEvent::AssistantText { text } => {
                    last_assistant_reply = Some(truncate_words(text, 30));
                }
                AgentEvent::AssistantToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    tool_names_by_id.insert(id.as_str(), name.as_str());
                    tool_calls.push(format!("{name}({})", summarize_tool_arguments(arguments)));
                }
                AgentEvent::ToolCallCompleted { id, result } if result.is_error => {
                    let name = tool_names_by_id
                        .get(id.as_str())
                        .copied()
                        .unwrap_or("unknown_tool");
                    tool_errors.push(format!("{name} -> {}", truncate_words(&result.content, 12)));
                }
                AgentEvent::ToolCallCompleted { .. }
                | AgentEvent::ToolCallStarted { .. }
                | AgentEvent::CompactionOccurred { .. }
                | AgentEvent::PermissionRequested { .. }
                | AgentEvent::PermissionDecided { .. }
                | AgentEvent::Usage { .. }
                | AgentEvent::Unknown => {}
            }
        }

        let mut out = String::from("Previous context (compacted):\n");

        if !user_requests.is_empty() {
            let tail = tail_capped(&user_requests, DIGEST_MAX_USER_REQUESTS);
            out.push_str("- User requests: ");
            out.push_str(
                &tail
                    .iter()
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            out.push('\n');
        }

        if !tool_calls.is_empty() {
            let tail = tail_capped(&tool_calls, DIGEST_MAX_TOOL_CALLS);
            out.push_str("- Tools used: ");
            out.push_str(&tail.join(", "));
            out.push('\n');
        }

        if !tool_errors.is_empty() {
            let tail = tail_capped(&tool_errors, DIGEST_MAX_TOOL_ERRORS);
            out.push_str("- Tool errors: ");
            out.push_str(&tail.join("; "));
            out.push('\n');
        }

        if let Some(reply) = last_assistant_reply {
            out.push_str(&format!("- Last assistant reply: \"{reply}\"\n"));
        }

        out.push_str(
            "Continue the task using this context. Do not repeat a tool call \
             you already made with the same arguments unless its result changed.",
        );

        Ok(out)
    }
}

/// Returns the last (most recent) `max` items of `items`, preserving
/// order — used to cap each section of `compact_tactical`'s digest.
fn tail_capped<T>(items: &[T], max: usize) -> &[T] {
    let start = items.len().saturating_sub(max);
    &items[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::ToolResult;

    fn user(text: &str) -> AgentEvent {
        AgentEvent::UserMessage {
            text: text.to_string(),
        }
    }

    fn tool_completed(id: &str) -> AgentEvent {
        AgentEvent::ToolCallCompleted {
            id: id.to_string(),
            result: ToolResult {
                tool_call_id: id.to_string(),
                content: "ok".to_string(),
                is_error: false,
            },
        }
    }

    fn tool_call(id: &str, name: &str) -> AgentEvent {
        AgentEvent::AssistantToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        }
    }

    fn compaction(summary: &str) -> AgentEvent {
        AgentEvent::CompactionOccurred {
            summary: summary.to_string(),
            dropped_tokens_estimate: 0,
        }
    }

    #[test]
    fn split_never_loses_events_when_log_shorter_than_window() {
        let compactor = SimpleContextCompactor::new(20);
        let events = vec![user("hola"), tool_completed("1"), user("chau")];

        let (durable, tactical) = compactor.split(&events);
        assert_eq!(durable.durable_events.len() + tactical.len(), events.len());
        // Whole log fits inside the window, so everything is tactical.
        assert_eq!(tactical.len(), events.len());
        assert!(durable.durable_events.is_empty());
    }

    #[test]
    fn split_moves_settled_durable_types_out_of_the_window() {
        let compactor = SimpleContextCompactor::new(1);
        let events = vec![
            user("older message"),
            tool_completed("1"),    // older, settled -> durable
            user("newest message"), // inside window (last 1) -> tactical
        ];

        let (durable, tactical) = compactor.split(&events);

        assert_eq!(durable.durable_events.len(), 1);
        assert!(matches!(
            durable.durable_events[0],
            AgentEvent::ToolCallCompleted { .. }
        ));
        assert_eq!(tactical.len(), 2);
        // No-silent-loss invariant: total in == total out.
        assert_eq!(durable.durable_events.len() + tactical.len(), events.len());
    }

    #[test]
    fn is_settled_durable_now_includes_assistant_tool_call() {
        assert!(is_settled_durable(&tool_call("1", "read_file")));
    }

    #[test]
    fn split_moves_a_tool_use_and_its_result_together_in_order() {
        let compactor = SimpleContextCompactor::new(1);
        let events = vec![
            user("older message"),
            tool_call("1", "read_file"), // older, now settled -> durable
            tool_completed("1"),         // older, settled -> durable
            user("newest message"),      // inside window (last 1) -> tactical
        ];

        let (durable, tactical) = compactor.split(&events);

        // Both halves of the pair migrate together, in original order —
        // otherwise the `tool_use` and its `tool_result` would end up
        // split across durable/tactical, which `history.rs` relies on
        // never happening.
        assert_eq!(durable.durable_events.len(), 2);
        assert!(matches!(
            durable.durable_events[0],
            AgentEvent::AssistantToolCall { .. }
        ));
        assert!(matches!(
            durable.durable_events[1],
            AgentEvent::ToolCallCompleted { .. }
        ));
        assert_eq!(tactical.len(), 2);
        assert_eq!(durable.durable_events.len() + tactical.len(), events.len());
    }

    #[test]
    fn split_keeps_orphaned_old_non_durable_events_in_tactical() {
        // window of 0 forces every event to be classified as "older".
        let compactor = SimpleContextCompactor::new(0);
        let events = vec![user("a"), tool_completed("1"), user("b")];

        let (durable, tactical) = compactor.split(&events);

        // tool_completed("1") -> durable; both UserMessage events, having
        // no durable type, must still show up somewhere (tactical, per
        // the documented fallback) rather than vanish.
        assert_eq!(durable.durable_events.len(), 1);
        assert_eq!(tactical.len(), 2);
        assert_eq!(durable.durable_events.len() + tactical.len(), events.len());
    }

    /// Regression test for A2/C2: an orphan (non-durable-typed) event that
    /// precedes a `CompactionOccurred` in the log must NOT keep
    /// reappearing in `tactical` on every subsequent `split` — it is
    /// already represented in that compaction's summary. Without this,
    /// `tactical.len()` never shrinks back down after a compaction runs,
    /// so the engine's threshold check stays permanently tripped.
    #[test]
    fn orphan_events_covered_by_a_later_compaction_are_dropped_not_resurfaced() {
        // window of 0 forces every event to be classified as "older", so
        // only the covering logic (not window membership) is under test.
        let compactor = SimpleContextCompactor::new(0);
        let events = vec![
            user("old message, already summarized"),
            compaction("summary of the above"),
        ];

        let (durable, tactical) = compactor.split(&events);

        // The orphan preceding the compaction is gone from tactical (its
        // content lives in `durable.summary` now) — NOT re-added, unlike
        // the no-compaction-yet case above.
        assert!(tactical.is_empty());
        assert_eq!(durable.durable_events.len(), 1);
        assert!(matches!(
            durable.durable_events[0],
            AgentEvent::CompactionOccurred { .. }
        ));
        assert_eq!(durable.summary, "summary of the above");
    }

    /// Regression test for N-3 (docs/AUDITORIA-2026-07-v2.md): a
    /// `CompactionOccurred` still *inside* the tactical window (i.e. the
    /// compaction that produced it just ran, or ran recently) must already
    /// contribute its text to `durable.summary` — not just once it ages
    /// out of the window. Before this fix, `summary_parts` was only
    /// harvested for events older than the window, so for the next
    /// `tactical_window` events (several rounds) after every compaction,
    /// `durable.summary` stayed empty even though the compaction's digest
    /// — including the user's original request — was sitting right there,
    /// unrendered (`CompactionOccurred` never becomes a message either
    /// way).
    #[test]
    fn a_compaction_still_inside_the_window_still_contributes_its_summary() {
        // window of 3 keeps every one of these 3 events inside the window.
        let compactor = SimpleContextCompactor::new(3);
        let events = vec![
            user("original request"),
            compaction("resumen reciente"),
            user("siguiente pregunta"),
        ];

        let (durable, tactical) = compactor.split(&events);

        assert_eq!(durable.summary, "resumen reciente");
        assert!(durable.durable_events.is_empty());
        // The CompactionOccurred event itself still stays in tactical too
        // (it's harmless there — it never renders into a message) — the
        // no-silent-loss invariant is unaffected.
        assert_eq!(tactical.len(), 3);
    }

    /// End-to-end idempotency: calling `split` repeatedly on a log that
    /// never grows must produce a stable, non-growing `tactical.len()`
    /// once a compaction has run — the exact scenario that used to
    /// re-trigger compaction forever (see A2/C2 in
    /// docs/AUDITORIA-2026-07.md).
    #[test]
    fn repeated_split_after_a_compaction_does_not_regrow_tactical() {
        let compactor = SimpleContextCompactor::new(0);
        let mut events = vec![user("a"), user("b"), user("c")];

        let (_, tactical_before) = compactor.split(&events);
        assert_eq!(tactical_before.len(), 3, "sanity: all three are orphans");

        // Simulate the engine folding the current backlog and appending
        // the resulting summary — exactly what `Engine::load_messages`
        // does when the threshold is crossed.
        events.push(compaction("folded a, b, c"));

        // Calling split() again and again (as load_messages does once per
        // round) must NOT keep re-surfacing a, b, c as tactical.
        for _ in 0..5 {
            let (durable, tactical) = compactor.split(&events);
            assert!(
                tactical.is_empty(),
                "covered orphans must not resurrect across repeated splits"
            );
            assert_eq!(durable.summary, "folded a, b, c");
        }
    }

    #[test]
    fn split_never_drops_events_across_a_range_of_window_sizes() {
        let events: Vec<AgentEvent> = (0..50)
            .map(|i| {
                if i % 3 == 0 {
                    tool_completed(&i.to_string())
                } else {
                    user(&format!("msg {i}"))
                }
            })
            .collect();

        for window in [0, 1, 5, 20, 49, 50, 100] {
            let compactor = SimpleContextCompactor::new(window);
            let (durable, tactical) = compactor.split(&events);
            assert_eq!(
                durable.durable_events.len() + tactical.len(),
                events.len(),
                "invariant violated for window={window}"
            );
        }
    }

    #[test]
    fn compact_tactical_is_deterministic() {
        let compactor = SimpleContextCompactor::default();
        let events = vec![user("hi"), tool_completed("1"), user("bye")];

        let summary_a = compactor.compact_tactical(&events).unwrap();
        let summary_b = compactor.compact_tactical(&events).unwrap();
        assert_eq!(summary_a, summary_b);
    }

    #[test]
    fn compact_tactical_handles_empty_input() {
        let compactor = SimpleContextCompactor::default();
        let summary = compactor.compact_tactical(&[]).unwrap();
        assert_eq!(summary, "No tactical events to compact.");
    }

    #[test]
    fn compact_tactical_extracts_user_requests_verbatim_not_just_a_count() {
        let compactor = SimpleContextCompactor::default();
        let events = vec![
            user("por favor lee el archivo de configuracion y dime que dice"),
            user("ahora borra ese archivo"),
        ];

        let summary = compactor.compact_tactical(&events).unwrap();

        assert!(summary.contains("User requests:"));
        assert!(summary.contains("por favor lee el archivo"));
        assert!(summary.contains("ahora borra ese archivo"));
    }

    #[test]
    fn compact_tactical_extracts_tool_calls_with_their_key_argument() {
        let compactor = SimpleContextCompactor::default();
        let events = vec![AgentEvent::AssistantToolCall {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "Cargo.toml"}),
        }];

        let summary = compactor.compact_tactical(&events).unwrap();

        assert!(summary.contains("Tools used:"));
        assert!(summary.contains("read_file(Cargo.toml)"));
    }

    #[test]
    fn compact_tactical_extracts_tool_errors_with_the_tool_name() {
        let compactor = SimpleContextCompactor::default();
        let events = vec![
            AgentEvent::AssistantToolCall {
                id: "call-1".to_string(),
                name: "shell_exec".to_string(),
                arguments: serde_json::json!({"command": ["rm", "x"]}),
            },
            AgentEvent::ToolCallCompleted {
                id: "call-1".to_string(),
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    content: "permission denied".to_string(),
                    is_error: true,
                },
            },
        ];

        let summary = compactor.compact_tactical(&events).unwrap();

        assert!(summary.contains("Tool errors:"));
        assert!(summary.contains("shell_exec -> permission denied"));
    }

    #[test]
    fn compact_tactical_keeps_only_the_most_recent_assistant_reply() {
        let compactor = SimpleContextCompactor::default();
        let events = vec![
            AgentEvent::AssistantText {
                text: "primera respuesta".to_string(),
            },
            AgentEvent::AssistantText {
                text: "segunda y ultima respuesta".to_string(),
            },
        ];

        let summary = compactor.compact_tactical(&events).unwrap();

        assert!(summary.contains("segunda y ultima respuesta"));
        assert!(!summary.contains("primera respuesta"));
    }

    #[test]
    fn compact_tactical_caps_each_section_to_the_most_recent_items() {
        let compactor = SimpleContextCompactor::default();
        let events: Vec<AgentEvent> = (0..(DIGEST_MAX_USER_REQUESTS + 5))
            .map(|i| user(&format!("pedido numero {i}")))
            .collect();

        let summary = compactor.compact_tactical(&events).unwrap();

        // The oldest requests are dropped in favor of the most recent
        // DIGEST_MAX_USER_REQUESTS ones — the digest stays bounded no
        // matter how large the folded backlog is.
        assert!(!summary.contains("pedido numero 0"));
        assert!(summary.contains(&format!("pedido numero {}", DIGEST_MAX_USER_REQUESTS + 4)));
    }

    #[test]
    fn default_uses_documented_default_window() {
        // Indirect check: a log of exactly DEFAULT_TACTICAL_WINDOW events
        // should be entirely tactical, none durable, under `default()`.
        let compactor = SimpleContextCompactor::default();
        let events: Vec<AgentEvent> = (0..DEFAULT_TACTICAL_WINDOW)
            .map(|i| user(&format!("msg {i}")))
            .collect();

        let (durable, tactical) = compactor.split(&events);
        assert!(durable.durable_events.is_empty());
        assert_eq!(tactical.len(), DEFAULT_TACTICAL_WINDOW);
    }
}
