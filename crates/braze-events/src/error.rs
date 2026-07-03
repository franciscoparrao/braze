use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EventsError {
    #[error("background task channel closed unexpectedly")]
    ChannelClosed,
}
