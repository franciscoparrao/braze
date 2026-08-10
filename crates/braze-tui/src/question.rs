//! [`ChannelQuestionPrompt`]: the TUI implementation of
//! [`braze_permissions::QuestionPrompt`] — the multiple-choice sibling
//! of [`crate::approval::ChannelConfirmationPrompt`], and what finally
//! wires `ask_user` (E′ I.5) into `--tui` (until now the tool was
//! plain-chat-only: `braze-cli` passed `ask_user_prompt = None` for
//! `--tui` because there was no overlay to answer from).
//!
//! Unlike `ChannelConfirmationPrompt`, this persists nothing itself: an
//! `ask_user` exchange is already durable as its
//! `AssistantToolCall`/`ToolCallCompleted` pair in the rollout log (the
//! engine persists both), so writing a parallel event here would just
//! duplicate it. Permissions need their own events because a decision
//! must be replayable into a fresh `PermissionGuard` on `--resume`;
//! a question's answer needs no replay — it's conversation content.

use async_trait::async_trait;
use braze_permissions::QuestionPrompt;
use tokio::sync::{mpsc, oneshot};

/// One pending question, as the app loop sees it: what to ask, the
/// choices, and a one-shot channel to send the chosen index back
/// through. `respond` is consumed exactly once — by the user answering
/// (`app.rs`'s `answer_pending_question`), or by being dropped if the
/// app quits with this still unanswered (the awaiting `ask()` then sees
/// a closed channel and returns `None` — "no answer", never a guess).
pub struct QuestionRequest {
    pub question: String,
    pub options: Vec<String>,
    pub respond: oneshot::Sender<Option<usize>>,
}

/// Sends each `ask()` over a channel to the TUI event loop and blocks on
/// the reply — same shape as `ChannelConfirmationPrompt`, for the same
/// reason: stdin is unusable once the terminal is in raw mode.
pub struct ChannelQuestionPrompt {
    tx: mpsc::UnboundedSender<QuestionRequest>,
}

impl ChannelQuestionPrompt {
    pub fn new(tx: mpsc::UnboundedSender<QuestionRequest>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl QuestionPrompt for ChannelQuestionPrompt {
    /// NO-ANSWER DEFAULT (see `QuestionPrompt`'s doc comment): if the
    /// request can't be delivered (the app loop's receiver is gone) or
    /// the answer channel closes without a reply, this returns `None` —
    /// the `AskUserProvider` then tells the model "the user did not
    /// answer, proceed with your best judgment", which is exactly the
    /// honest outcome. Never invent a choice on the user's behalf.
    async fn ask(&self, question: &str, options: &[String]) -> Option<usize> {
        let (respond_tx, respond_rx) = oneshot::channel();
        let request = QuestionRequest {
            question: question.to_string(),
            options: options.to_vec(),
            respond: respond_tx,
        };
        if self.tx.send(request).is_err() {
            return None;
        }
        respond_rx.await.unwrap_or(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_chosen_index_travels_back_over_the_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prompt = ChannelQuestionPrompt::new(tx);

        let ask = tokio::spawn(async move {
            prompt
                .ask("¿cuál?", &["a".to_string(), "b".to_string()])
                .await
        });

        let request = rx.recv().await.expect("expected a QuestionRequest");
        assert_eq!(request.question, "¿cuál?");
        assert_eq!(request.options, vec!["a", "b"]);
        request.respond.send(Some(1)).expect("respond channel open");

        assert_eq!(ask.await.expect("task join"), Some(1));
    }

    #[tokio::test]
    async fn an_explicit_no_answer_travels_back_as_none() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prompt = ChannelQuestionPrompt::new(tx);

        let ask = tokio::spawn(async move {
            prompt
                .ask("¿a o b?", &["a".to_string(), "b".to_string()])
                .await
        });

        let request = rx.recv().await.expect("expected a QuestionRequest");
        request.respond.send(None).expect("respond channel open");

        assert_eq!(ask.await.expect("task join"), None);
    }

    #[tokio::test]
    async fn a_dropped_receiver_returns_none_instead_of_hanging() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx); // app loop already gone
        let prompt = ChannelQuestionPrompt::new(tx);
        assert_eq!(
            prompt.ask("¿?", &["a".to_string(), "b".to_string()]).await,
            None
        );
    }

    #[tokio::test]
    async fn dropping_the_respond_sender_without_answering_returns_none() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prompt = ChannelQuestionPrompt::new(tx);

        let ask =
            tokio::spawn(
                async move { prompt.ask("¿?", &["a".to_string(), "b".to_string()]).await },
            );

        let request = rx.recv().await.expect("expected a QuestionRequest");
        drop(request); // simulates the app quitting with this still pending

        assert_eq!(ask.await.expect("task join"), None);
    }
}
