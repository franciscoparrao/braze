//! [`TerminalQuestionPrompt`]: the stdin implementation of
//! [`braze_permissions::QuestionPrompt`] for the plain (non-TUI)
//! interactive chat loop — the `ask_user` tool (E′ I.5) blocks on this
//! to bring back the user's choice.
//!
//! Reads from the SAME line reader the chat loop owns
//! ([`SharedStdin`]), not a fresh `BufReader` over stdin. That matters:
//! a `BufReader` reads ahead greedily, so under piped input a second,
//! independent reader would find the bytes for its answer already
//! swallowed into the chat loop's buffer (observed live — the answer got
//! consumed as the next chat message). Sharing one reader behind a mutex
//! keeps a single buffer over stdin; no lock is held across the turn
//! (the loop only locks to read a line), so the tool locking it
//! mid-turn can't deadlock.

use std::sync::Arc;

use async_trait::async_trait;
use braze_permissions::QuestionPrompt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, Stdin};
use tokio::sync::Mutex;

/// The one line reader over stdin, shared by the chat loop and every
/// stdin-backed prompt so they never race for the same buffered bytes.
pub type SharedStdin = Arc<Mutex<Lines<BufReader<Stdin>>>>;

/// Builds the shared reader the plain chat loop and `TerminalQuestionPrompt`
/// both read through.
pub fn shared_stdin() -> SharedStdin {
    Arc::new(Mutex::new(BufReader::new(tokio::io::stdin()).lines()))
}

pub struct TerminalQuestionPrompt {
    stdin: SharedStdin,
}

impl TerminalQuestionPrompt {
    pub fn new(stdin: SharedStdin) -> Self {
        Self { stdin }
    }
}

#[async_trait]
impl QuestionPrompt for TerminalQuestionPrompt {
    /// Prints the question and a numbered menu, then reads one line.
    /// Returns `Some(index)` for a valid `1..=N` choice; `None` for EOF,
    /// an out-of-range/non-numeric answer, or any I/O error — never
    /// guesses a choice on the user's behalf (same safety default as the
    /// y/n prompt: anything but an unambiguous answer means "no answer").
    async fn ask(&self, question: &str, options: &[String]) -> Option<usize> {
        // v8 K-4 (docs/AUDITORIA-2026-07-v8.md): `question`/`options`
        // los escribe el MODELO — la misma seam que J-19 cerró para los
        // approval prompts. Sin sanitizar, ANSI embebido puede repintar
        // el terminal o forjar visualmente un prompt de aprobación.
        let question = braze_permissions::sanitize_control_chars(question);
        let mut menu = format!("\n{question}\n");
        for (i, option) in options.iter().enumerate() {
            let option = braze_permissions::sanitize_control_chars(option);
            menu.push_str(&format!("  {}) {option}\n", i + 1));
        }
        menu.push_str(&format!("Elige [1-{}]: ", options.len()));

        let mut stdout = tokio::io::stdout();
        if stdout.write_all(menu.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
            return None;
        }

        let mut reader = self.stdin.lock().await;
        match reader.next_line().await {
            Ok(Some(line)) => match line.trim().parse::<usize>() {
                Ok(n) if (1..=options.len()).contains(&n) => Some(n - 1),
                _ => None,
            },
            Ok(None) | Err(_) => None, // EOF or I/O error.
        }
    }
}
