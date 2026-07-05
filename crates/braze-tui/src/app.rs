//! The event loop: `tokio::select!` between keyboard input and the
//! current turn's live updates (PLAN.md § "Fase TUI — diseño"). One
//! [`Engine::run_turn`] runs at a time, spawned as a background task so
//! the composer stays responsive while the model streams — a second
//! submission is ignored while one is in flight (two concurrent
//! `run_turn` calls on the same session would race on the session
//! store's loads).

use std::collections::HashMap;
use std::sync::Arc;

use braze_engine::Engine;
use braze_events::AgentEvent;
use braze_types::SessionId;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;

use crate::error::TuiError;
use crate::history_cell::{AssistantMarkdownCell, ErrorCell, HistoryCell, ToolCallCell, UserCell};
use crate::markdown_stream::MarkdownStreamCollector;
use crate::observer::{ChannelObserver, TuiUpdate};
use crate::terminal::{ACTIVE_ROWS, Backend};

/// Drives the interactive TUI chat loop against an already-configured
/// `Engine` for `session` until the user quits (Ctrl+C, or Ctrl+D on an
/// empty composer). `braze-cli` builds `engine` exactly as it does for
/// the plain-text `chat`/`run` path — this is just another frontend
/// driving the same composition root.
pub async fn run(
    terminal: &mut Terminal<Backend>,
    engine: Engine,
    session: SessionId,
) -> Result<(), TuiError> {
    App::new(Arc::new(engine), session).run(terminal).await
}

struct App {
    engine: Arc<Engine>,
    session: SessionId,
    /// Accumulates the assistant's streaming text for the round in
    /// progress and gates what's safe to commit to the scrollback vs.
    /// still-live preview — see its own doc comment.
    markdown: MarkdownStreamCollector,
    /// `tool_call_id` -> tool name, recorded at `AssistantToolCall` time
    /// (the only event carrying both) so the later `ToolCallCompleted`
    /// (which only carries the id) can still render a `ToolCallCell`
    /// with a name. Entries are removed once consumed.
    pending_tool_names: HashMap<String, String>,
    composer: TextArea<'static>,
    turn_running: bool,
    should_quit: bool,
    update_tx: mpsc::UnboundedSender<TuiUpdate>,
    update_rx: mpsc::UnboundedReceiver<TuiUpdate>,
}

impl App {
    fn new(engine: Arc<Engine>, session: SessionId) -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        Self {
            engine,
            session,
            markdown: MarkdownStreamCollector::default(),
            pending_tool_names: HashMap::new(),
            composer: TextArea::default(),
            turn_running: false,
            should_quit: false,
            update_tx,
            update_rx,
        }
    }

    async fn run(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let mut events = EventStream::new();

        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if self.should_quit {
                return Ok(());
            }

            tokio::select! {
                maybe_event = events.next() => {
                    match maybe_event {
                        Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                            self.on_key(key, terminal)?;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => return Err(err.into()),
                        // The input stream itself ended (stdin closed) —
                        // treat like an explicit quit rather than
                        // spinning on a stream that will never yield
                        // anything again.
                        None => return Ok(()),
                    }
                }
                Some(update) = self.update_rx.recv() => {
                    self.apply_update(update, terminal)?;
                }
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let composer_is_empty = self.composer.lines().len() == 1 && self.composer.lines()[0].is_empty();
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) && composer_is_empty => {
                self.should_quit = true;
            }
            // Ctrl+J: literal newline, bypassing `TextArea::input`'s own
            // `Key::Enter` handling (which we deliberately never reach —
            // plain Enter is intercepted below as submit, before it ever
            // gets forwarded to the composer).
            (KeyCode::Char('j'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.composer.insert_newline();
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !self.turn_running {
                    self.submit(terminal)?;
                }
                // Else: a turn is already running — ignore the
                // submission rather than racing a second `run_turn`
                // against the same session (see this module's doc
                // comment). The composer keeps whatever was typed.
            }
            _ => {
                self.composer.input(Event::Key(key));
            }
        }
        Ok(())
    }

    fn submit(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let text = self.composer.lines().join("\n");
        let user_text = text.trim().to_string();
        if user_text.is_empty() {
            return Ok(());
        }
        self.composer = TextArea::default();

        self.commit_cell(
            &UserCell {
                text: user_text.clone(),
            },
            terminal,
        )?;

        self.turn_running = true;
        self.markdown = MarkdownStreamCollector::default();
        self.pending_tool_names.clear();

        let engine = Arc::clone(&self.engine);
        let session = self.session;
        let tx = self.update_tx.clone();
        tokio::spawn(async move {
            let mut observer = ChannelObserver::new(tx.clone());
            let result = engine.run_turn(&session, &user_text, &mut observer).await;
            let _ = tx.send(TuiUpdate::TurnFinished(result.map_err(|err| err.to_string())));
        });

        Ok(())
    }

    fn apply_update(
        &mut self,
        update: TuiUpdate,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        match update {
            TuiUpdate::TextDelta(delta) => {
                self.markdown.push(&delta);
                if let Some(ready) = self.markdown.commit_ready() {
                    self.commit_cell(&AssistantMarkdownCell { markdown: ready }, terminal)?;
                }
            }
            TuiUpdate::Event(AgentEvent::AssistantText { .. }) => {
                // The round's text is now persisted — flush whatever's
                // left in the collector (the trailing partial line, or
                // an unclosed fence; either way, nothing more is coming
                // for it this round).
                if let Some(tail) = self.markdown.finish() {
                    self.commit_cell(&AssistantMarkdownCell { markdown: tail }, terminal)?;
                }
            }
            TuiUpdate::Event(AgentEvent::AssistantToolCall { id, name, .. }) => {
                self.pending_tool_names.insert(id, name);
            }
            TuiUpdate::Event(AgentEvent::ToolCallStarted { name, .. }) => {
                self.commit_cell(&ToolCallCell::running(name), terminal)?;
            }
            TuiUpdate::Event(AgentEvent::ToolCallCompleted { id, result }) => {
                let name = self
                    .pending_tool_names
                    .remove(&id)
                    .unwrap_or_else(|| "tool".to_string());
                self.commit_cell(
                    &ToolCallCell::done(name, result.is_error, &result.content),
                    terminal,
                )?;
            }
            TuiUpdate::Event(_) => {
                // Permission/compaction/usage/unknown events are oleada 4
                // (PLAN.md § "Fase TUI — diseño"). The engine still sees
                // and acts on these events normally — they're mirrored
                // here too, just not drawn yet in this skeleton.
            }
            TuiUpdate::TurnFinished(Ok(())) => {
                self.turn_running = false;
            }
            TuiUpdate::TurnFinished(Err(message)) => {
                self.turn_running = false;
                self.commit_cell(&ErrorCell { message }, terminal)?;
            }
        }
        Ok(())
    }

    /// Writes `cell` once into the terminal's native scrollback via
    /// `insert_before`, wrapped to the terminal's current width. Never
    /// re-rendered afterwards — see `docs/TUI-INVESTIGACION-2026-07.md`'s
    /// convergence #1 and this module's doc comment.
    fn commit_cell(&self, cell: &dyn HistoryCell, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let width = terminal.size()?.width;
        let paragraph = Paragraph::new(cell.as_text()).wrap(Wrap { trim: false });
        let height = paragraph.line_count(width).max(1) as u16;
        terminal.insert_before(height, |buf| {
            paragraph.render(buf.area, buf);
        })?;
        Ok(())
    }

    fn draw(&self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let [active_area, hint_area, composer_area] = Layout::vertical([
            Constraint::Length(ACTIVE_ROWS),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(area);

        let pending = self.markdown.pending();
        if !pending.is_empty() {
            // Deliberately plain text, not markdown-rendered: this is
            // still-unstable, partial content (see
            // `MarkdownStreamCollector::pending`'s doc comment) — only
            // committed, final chunks get `tui-markdown` treatment.
            let paragraph = Paragraph::new(pending).wrap(Wrap { trim: false });
            let total_lines = paragraph.line_count(active_area.width) as u16;
            let scroll_y = total_lines.saturating_sub(ACTIVE_ROWS);
            frame.render_widget(paragraph.scroll((scroll_y, 0)), active_area);
        }

        let hint = if self.turn_running {
            "esperando respuesta del modelo... (Ctrl+C para salir)"
        } else {
            "Enter enviar · Ctrl+J salto de linea · Ctrl+C salir"
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
            hint_area,
        );

        frame.render_widget(&self.composer, composer_area);
    }
}
