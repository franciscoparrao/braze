//! [`ChannelObserver`]: the [`TurnObserver`] implementation the app loop
//! (`app.rs`) drives from — the concrete use of the seam
//! `braze-events::TurnObserver` added in oleada 1 (PLAN.md § "Fase TUI —
//! diseño"). Each spawned turn gets a fresh instance; it forwards every
//! callback into an unbounded channel back to the app loop.

use braze_events::{AgentEvent, TurnObserver};
use tokio::sync::mpsc::UnboundedSender;

/// One unit of live turn activity, forwarded from the spawned turn task
/// to the app's event loop. Kept deliberately thin — the app decides how
/// to render each variant; this is just the wire format between the two
/// tasks. `TurnFinished` is sent separately by the spawn site in
/// `app.rs` (it isn't part of `TurnObserver` — the turn's overall
/// `Result` is only known after `run_turn` returns), not by this
/// observer.
#[derive(Debug, Clone)]
pub enum TuiUpdate {
    TextDelta(String),
    Event(AgentEvent),
    /// The turn's overall `Result`, stringified (`EngineError` isn't
    /// `Clone`, and the app loop only needs to display it). Sent
    /// directly by the spawn site in `app.rs` once `run_turn` returns —
    /// not by this observer, since the outcome isn't known until after
    /// the turn completes.
    TurnFinished(Result<(), String>),
}

/// Forwards every callback into an unbounded channel — never blocks the
/// engine, and lets the app loop consume updates at its own pace via
/// `tokio::select!`.
pub struct ChannelObserver {
    tx: UnboundedSender<TuiUpdate>,
}

impl ChannelObserver {
    pub fn new(tx: UnboundedSender<TuiUpdate>) -> Self {
        Self { tx }
    }
}

impl TurnObserver for ChannelObserver {
    fn on_text_delta(&mut self, delta: &str) {
        // A send error means the app loop's receiver was dropped (the
        // TUI is shutting down) — the turn keeps running to completion
        // regardless (it still persists to the session store via the
        // engine's normal append path), it just has no one left to
        // notify of its progress.
        let _ = self.tx.send(TuiUpdate::TextDelta(delta.to_string()));
    }

    fn on_event(&mut self, event: &AgentEvent) {
        let _ = self.tx.send(TuiUpdate::Event(event.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_deltas_and_events_to_the_channel_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut observer = ChannelObserver::new(tx);

        observer.on_text_delta("hola");
        observer.on_event(&AgentEvent::UserMessage {
            text: "x".to_string(),
        });
        observer.on_text_delta("mundo");

        assert!(matches!(rx.try_recv(), Ok(TuiUpdate::TextDelta(d)) if d == "hola"));
        assert!(matches!(rx.try_recv(), Ok(TuiUpdate::Event(AgentEvent::UserMessage { .. }))));
        assert!(matches!(rx.try_recv(), Ok(TuiUpdate::TextDelta(d)) if d == "mundo"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_dropped_receiver_does_not_panic_the_observer() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let mut observer = ChannelObserver::new(tx);
        observer.on_text_delta("ignored, no one is listening");
    }
}
