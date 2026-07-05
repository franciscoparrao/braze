//! [`TuiError`]: everything that can go wrong driving the terminal UI
//! itself. Engine failures surface as an `AgentEvent`-adjacent error cell
//! in the transcript instead (see `app.rs`'s `TuiUpdate::TurnFinished`) —
//! a single failed turn must not crash the whole session, matching the
//! plain-chat loop's existing behavior in `braze-cli`.

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),
}
