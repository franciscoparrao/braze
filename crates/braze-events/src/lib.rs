//! `AgentEvent` stream and background-task dispatch with push notification.
//!
//! The engine's main loop is *notified* when a background task completes
//! (via [`TaskNotifier::next_completed`]) rather than polling task status —
//! see PLAN.md, principle B.3.

mod error;
mod event;
mod notify;

pub use error::EventsError;
pub use event::AgentEvent;
pub use notify::{BackgroundTask, TaskHandle, TaskNotifier};
