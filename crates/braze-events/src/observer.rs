//! [`TurnObserver`]: the live view of a turn's lifecycle, designed for
//! frontends (CLI today, `braze-tui` next — see PLAN.md § "Fase TUI —
//! diseño"). The engine mirrors every [`AgentEvent`] it persists to the
//! session store into [`TurnObserver::on_event`] *as it happens*, and
//! forwards model text deltas (which are not `AgentEvent`s — only the
//! final consolidated `AssistantText` is) into
//! [`TurnObserver::on_text_delta`].
//!
//! This is a mirror, not a replacement: persistence to the
//! [`SessionStore`](https://docs.rs) rollout log is unchanged and remains
//! the source of truth. An observer that does nothing (the defaults)
//! costs nothing.

use crate::event::AgentEvent;

/// Live consumer of a single turn's activity. All methods default to
/// no-ops so implementors only override what they render.
///
/// `Send` bound: the engine holds `&mut dyn TurnObserver` across `.await`
/// points inside `run_turn`, so the observer must be safe to move with
/// the future.
pub trait TurnObserver: Send {
    /// A model text fragment, in stream order. Called zero or more times
    /// per round, before the round's consolidated `AssistantText` event
    /// (if any) reaches [`TurnObserver::on_event`].
    fn on_text_delta(&mut self, _delta: &str) {}

    /// Mirror of an [`AgentEvent`] the engine just persisted to the
    /// session store, in persistence order.
    fn on_event(&mut self, _event: &AgentEvent) {}
}

/// Observer that ignores everything — for headless callers
/// (`braze-bench`, tests) that only care about the turn's outcome.
pub struct NoopObserver;

impl TurnObserver for NoopObserver {}

/// Adapter for callers that only want the text deltas as a closure —
/// the exact shape `Engine::run_turn`'s old `on_text: &mut dyn
/// FnMut(&str)` parameter had, preserved so a plain-text frontend
/// (today's `braze chat`/`braze run`) stays a one-liner.
pub struct TextDeltaObserver<F: FnMut(&str) + Send>(pub F);

impl<F: FnMut(&str) + Send> TurnObserver for TextDeltaObserver<F> {
    fn on_text_delta(&mut self, delta: &str) {
        (self.0)(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_observer_forwards_deltas_to_the_closure() {
        let mut collected = String::new();
        {
            let mut observer = TextDeltaObserver(|delta: &str| collected.push_str(delta));
            observer.on_text_delta("hola ");
            observer.on_text_delta("mundo");
            // Default `on_event` is a no-op and must not disturb anything.
            observer.on_event(&AgentEvent::UserMessage {
                text: "x".to_string(),
            });
        }
        assert_eq!(collected, "hola mundo");
    }

    #[test]
    fn noop_observer_accepts_everything_silently() {
        let mut observer = NoopObserver;
        observer.on_text_delta("ignored");
        observer.on_event(&AgentEvent::AssistantText {
            text: "ignored".to_string(),
        });
    }
}
