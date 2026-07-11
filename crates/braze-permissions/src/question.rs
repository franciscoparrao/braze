//! [`QuestionPrompt`]: the harness asking the human to *choose*, the
//! multiple-choice sibling of [`crate::ConfirmationPrompt`]'s yes/no
//! (E′ I.5, docs/harness-engineering-hooks-skills-2026-07-10.md).
//!
//! Lives here, next to `ConfirmationPrompt`, for the same reason: both
//! are "the harness blocks on a human answer", and both need to be
//! reachable by the two front-ends that implement them (`braze-cli` over
//! stdin, `braze-tui` over its overlay) plus the tool that calls them
//! (`braze-tools-local::AskUserProvider`) — this is the lowest crate all
//! three already depend on.
//!
//! Why a tool at all: for a small model, guessing wrong on a genuine
//! branch point costs a whole turn of tool calls plus the cleanup;
//! asking costs ~100 tokens. Turning "edited the wrong file" into "asked
//! which file" downgrades a destructive failure to light friction — the
//! same posture as `write_file`'s P0.3 preflight. Only wired into
//! interactive sessions (there's no one to ask in `run`/the bench).

use async_trait::async_trait;

/// Asks the user to pick one of `options` for `question`.
#[async_trait]
pub trait QuestionPrompt: Send + Sync {
    /// Returns the chosen 0-based index into `options`, or `None` when
    /// the user gave no usable answer (EOF, out-of-range input, an I/O
    /// error) — the caller surfaces that as "no answer" rather than
    /// guessing on the user's behalf. Implementations may assume
    /// `options` has 2..=4 entries (the `AskUserProvider` validates the
    /// count before calling).
    async fn ask(&self, question: &str, options: &[String]) -> Option<usize>;
}
