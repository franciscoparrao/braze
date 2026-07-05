//! `AgentEvent` stream and background-task dispatch with push notification.
//!
//! The engine's main loop is *notified* when a background task completes
//! (via [`TaskNotifier::next_completed`]) rather than polling task status —
//! see PLAN.md, principle B.3.

mod channel_notifier;
mod error;
mod event;
mod notify;
mod observer;

pub use channel_notifier::ChannelTaskNotifier;
pub use error::EventsError;
pub use event::AgentEvent;
pub use notify::{BackgroundTask, TaskHandle, TaskNotifier};
pub use observer::{NoopObserver, TextDeltaObserver, TurnObserver};
