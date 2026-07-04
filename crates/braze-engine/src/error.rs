//! Errors surfaced by [`crate::Engine`].
//!
//! Kept small deliberately: `#[from]` covers propagation from each
//! composed crate's own error type, so there is no need for a bespoke
//! variant per possible failure inside the loop itself.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    #[error("session store error: {0}")]
    Session(#[from] braze_session::SessionError),

    #[error("model backend error: {0}")]
    Model(#[from] braze_model::ModelError),

    #[error("tool registry error: {0}")]
    Tool(#[from] braze_tools_core::ToolError),

    /// The main loop hit its safety iteration cap (see
    /// `Engine::MAX_TURN_ITERATIONS`) without the model converging on a
    /// final text-only response. Not necessarily a bug in the model or the
    /// engine — a legitimate long tool-use chain could also hit this —
    /// but the MVP has no better answer than surfacing it and stopping
    /// rather than looping forever.
    #[error("turn exceeded the maximum of {0} model/tool-call round trips without converging")]
    TurnDidNotConverge(usize),

    /// A `ModelBackend`'s completion stream ended without ever yielding
    /// `CompletionEvent::Done` and without reporting an `Err` first — an
    /// invariant every implementation must uphold (see
    /// `braze_model::ModelBackend::complete`'s doc comment). Surfacing
    /// this instead of silently treating whatever partial text/tool-calls
    /// arrived as a complete, converged response.
    #[error("model backend's completion stream ended without a terminal event")]
    IncompleteStream,
}
