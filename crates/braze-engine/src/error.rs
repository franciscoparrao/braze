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

    /// A round with no tool calls stopped because of
    /// `stop_reason: "max_tokens"`/`"length"` — N-24
    /// (docs/AUDITORIA-2026-07-v2.md). The text gathered so far may be cut
    /// off mid-sentence (or mid-tool-call-JSON that then failed to
    /// parse); persisting it as a normal, converged final answer would
    /// look identical downstream to a response the model actually
    /// finished on its own.
    #[error("model's final response was truncated by the token budget before it could finish")]
    TruncatedFinalResponse,

    /// A round produced no text and no tool calls at all — not a
    /// legitimate final answer, just a wasted round. Surfaced as an error
    /// (docs/AUDITORIA-2026-07-v2.md, "una completion vacía termina el
    /// turno como éxito silencioso") rather than treated as silent
    /// convergence, since under best-of-n several empty candidates can
    /// share the same signature and win the vote outright.
    #[error("model's response had no text and requested no tool calls")]
    EmptyModelResponse,

    /// A second `run_turn` call was attempted on this `Engine` while a
    /// first one was still in flight — N-17
    /// (docs/AUDITORIA-2026-07-v2.md). Not reachable via any current
    /// caller (every caller serializes turns), but two concurrent turns
    /// would share one `TaskNotifier`'s single completion channel and
    /// silently steal each other's tool-call results. Reject the misuse
    /// explicitly instead of leaving it undocumented and unguarded.
    #[error("a run_turn call is already in progress on this Engine")]
    ConcurrentTurn,
}
