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
/// 3. Any prior `CompactionOccurred` summaries found among the older
///    events are concatenated into `DurableState::summary`, so callers
///    get a running plain-text digest of everything compacted so far
///    without having to re-derive it from `durable_events`.
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
/// implementation keeps the invariant simple and absolute: **every event
/// in the input ends up in exactly one of `durable_events` or the
/// returned tactical vector, never neither**. Concretely, such
/// "orphaned" older non-durable-typed events are kept raw in the tactical
/// vector (prepended, in original order, ahead of the true last-N raw
/// window) instead of being dropped or force-fit into `durable_events`
/// under a type they don't have. In steady state, once the engine
/// regularly calls `compact_tactical` and appends the resulting
/// `CompactionOccurred` event, this case does not arise — this is purely
/// a defensive fallback for out-of-band or first-time splits.
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

/// Rough char length of an event's textual payload, used only as an input
/// to the token-count heuristic in `compact_tactical` — not persisted.
fn approx_char_len(event: &AgentEvent) -> usize {
    match event {
        AgentEvent::UserMessage { text } | AgentEvent::AssistantText { text } => text.len(),
        AgentEvent::ToolCallStarted { id, name, .. } => id.len() + name.len(),
        // Added in Fase 5 (braze-engine history reconstruction, see
        // AgentEvent::AssistantToolCall's doc comment). This heuristic
        // only feeds `compact_tactical`'s size estimate over the raw
        // tactical window — it is unrelated to whether the event also
        // counts as `is_settled_durable` (which it now does, see above).
        AgentEvent::AssistantToolCall {
            id,
            name,
            arguments,
        } => id.len() + name.len() + arguments.to_string().len(),
        AgentEvent::ToolCallCompleted { id, result } => id.len() + result.content.len(),
        AgentEvent::CompactionOccurred { summary, .. } => summary.len(),
        AgentEvent::PermissionRequested { action, .. }
        | AgentEvent::PermissionDecided { action, .. } => action.len(),
    }
}

impl ContextCompactor for SimpleContextCompactor {
    fn split(&self, events: &[AgentEvent]) -> (DurableState, Vec<AgentEvent>) {
        let window_len = self.tactical_window.min(events.len());
        let window_start = events.len() - window_len;

        let mut durable_events = Vec::new();
        let mut summary_parts = Vec::new();
        let mut tactical = Vec::new();

        for (i, event) in events.iter().enumerate() {
            if i >= window_start {
                // Inside the raw tactical window: always kept verbatim,
                // regardless of type.
                tactical.push(event.clone());
                continue;
            }

            if is_settled_durable(event) {
                if let AgentEvent::CompactionOccurred { summary, .. } = event {
                    summary_parts.push(summary.clone());
                }
                durable_events.push(event.clone());
            } else {
                // Orphaned older non-durable-typed event — see the
                // "no-silent-loss invariant" doc comment above.
                tactical.push(event.clone());
            }
        }

        let durable = DurableState {
            summary: summary_parts.join(" "),
            durable_events,
        };
        (durable, tactical)
    }

    fn compact_tactical(&self, tactical: &[AgentEvent]) -> Result<String, SessionError> {
        if tactical.is_empty() {
            return Ok("No tactical events to compact.".to_string());
        }

        let mut user_messages = 0u32;
        let mut assistant_texts = 0u32;
        let mut assistant_tool_calls = 0u32;
        let mut tool_calls_started = 0u32;
        let mut tool_calls_completed = 0u32;
        let mut tool_errors = 0u32;
        let mut permission_events = 0u32;
        let mut prior_compactions = 0u32;
        let mut char_count = 0usize;

        for event in tactical {
            char_count += approx_char_len(event);
            match event {
                AgentEvent::UserMessage { .. } => user_messages += 1,
                AgentEvent::AssistantText { .. } => assistant_texts += 1,
                AgentEvent::AssistantToolCall { .. } => assistant_tool_calls += 1,
                AgentEvent::ToolCallStarted { .. } => tool_calls_started += 1,
                AgentEvent::ToolCallCompleted { result, .. } => {
                    tool_calls_completed += 1;
                    if result.is_error {
                        tool_errors += 1;
                    }
                }
                AgentEvent::PermissionRequested { .. } | AgentEvent::PermissionDecided { .. } => {
                    permission_events += 1;
                }
                AgentEvent::CompactionOccurred { .. } => prior_compactions += 1,
            }
        }

        // Rough token estimate (~4 chars/token), consistent in spirit
        // with `AgentEvent::CompactionOccurred::dropped_tokens_estimate` —
        // this is what that field is meant to approximate, not an exact
        // count from the model's own tokenizer.
        let dropped_tokens_estimate = (char_count / 4) as u32;

        Ok(format!(
            "Compacted {total} tactical event(s): {um} user message(s), {at} assistant \
             message(s), {atc} assistant tool call(s) requested, {tcs} tool call(s) started, \
             {tcc} tool call(s) completed ({err} error(s)), {pe} permission event(s), {co} \
             prior compaction(s) folded in. Estimated dropped tokens: ~{tok}.",
            total = tactical.len(),
            um = user_messages,
            at = assistant_texts,
            atc = assistant_tool_calls,
            tcs = tool_calls_started,
            tcc = tool_calls_completed,
            err = tool_errors,
            pe = permission_events,
            co = prior_compactions,
            tok = dropped_tokens_estimate,
        ))
    }
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
    fn compact_tactical_reports_counts_and_is_deterministic() {
        let compactor = SimpleContextCompactor::default();
        let events = vec![user("hi"), tool_completed("1"), user("bye")];

        let summary_a = compactor.compact_tactical(&events).unwrap();
        let summary_b = compactor.compact_tactical(&events).unwrap();
        assert_eq!(summary_a, summary_b);
        assert!(summary_a.contains("3 tactical event"));
        assert!(summary_a.contains("2 user message"));
        assert!(summary_a.contains("1 tool call(s) completed"));
    }

    #[test]
    fn compact_tactical_handles_empty_input() {
        let compactor = SimpleContextCompactor::default();
        let summary = compactor.compact_tactical(&[]).unwrap();
        assert_eq!(summary, "No tactical events to compact.");
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
