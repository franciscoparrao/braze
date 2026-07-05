//! The event loop: `tokio::select!` between keyboard input, the current
//! turn's live updates, and pending permission approvals (PLAN.md §
//! "Fase TUI — diseño"). One [`Engine::run_turn`] runs at a time, spawned
//! as a background task so the composer stays responsive while the
//! model streams — a second submission is ignored while one is in
//! flight (two concurrent `run_turn` calls on the same session would
//! race on the session store's loads).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use braze_engine::Engine;
use braze_events::AgentEvent;
use braze_types::SessionId;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::approval::ApprovalRequest;
use crate::composer_trigger::{ComposerTrigger, detect_trigger};
use crate::error::TuiError;
use crate::history_cell::{
    AssistantMarkdownCell, ErrorCell, ExpandedToolOutputCell, HelpCell, HistoryCell, NoticeCell,
    PermissionCell, ToolCallCell, UserCell,
};
use crate::markdown_stream::MarkdownStreamCollector;
use crate::mentions::{list_files, matching_files};
use crate::observer::{ChannelObserver, TuiUpdate};
use crate::slash_commands::{SLASH_COMMANDS, SlashCommand, matching_commands};
use crate::status_bar;
use crate::terminal::{ACTIVE_ROWS, Backend};
use crate::theme::Theme;

/// Suggestions shown at once in the `/`/`@` popup — see `draw_popup`.
/// Kept small and fixed (no scrolling within the popup): it reuses the
/// same 3-row budget `ACTIVE_ROWS + hint` already occupies when not
/// popped up, rather than growing `VIEWPORT_HEIGHT` (ratatui has no
/// public API to resize an inline viewport's height at runtime outside
/// of responding to a real terminal `Resize` — see
/// `docs/TUI-INVESTIGACION-2026-07.md` on why growing/shrinking the
/// inline viewport is the fragile part of this whole approach). More
/// matches than fit just aren't shown — typing another character
/// narrows the query instead.
const POPUP_MAX_VISIBLE: usize = 3;

/// Drives the interactive TUI chat loop against an already-configured
/// `Engine` for `session` until the user quits (Ctrl+C, or Ctrl+D on an
/// empty composer). `braze-cli` builds `engine` exactly as it does for
/// the plain-text `chat`/`run` path — this is just another frontend
/// driving the same composition root. `approvals` is the receiving end
/// of the channel every `ChannelConfirmationPrompt` this session's
/// `PermissionGuard`s were built with sends into. `status_line` is a
/// short, static "backend:model" label shown in the status bar. `store`
/// is the same `SessionStore` handle `engine` was built with — passed
/// separately (not obtained through `engine`, which exposes no such
/// accessor) so Ctrl+T (`expand_last_tool_call`) can read the rollout
/// log fresh, the same seam `braze-cli`'s `ChannelConfirmationPrompt`
/// already uses this way.
pub async fn run(
    terminal: &mut Terminal<Backend>,
    engine: Engine,
    session: SessionId,
    store: Arc<dyn braze_session::SessionStore>,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    status_line: String,
    theme: Theme,
) -> Result<(), TuiError> {
    App::new(
        Arc::new(engine),
        session,
        store,
        approvals,
        status_line,
        theme,
    )
    .run(terminal)
    .await
}

/// `/command` and `@mention` suggestions currently shown above the
/// composer — "fase TUI 2" (PLAN.md). Not stored as trait objects or
/// references into `App` state (would need self-referential lifetimes);
/// `Mention`'s matches are cloned out of `App::mentionable_files` once,
/// cheap since they're already capped to `POPUP_MAX_VISIBLE`.
enum ComposerPopup {
    Slash {
        /// Character length of the query typed so far (everything after
        /// the `/`) — `accept_popup_selection` deletes exactly this many
        /// characters backward before inserting the full command name.
        query_len: usize,
        matches: Vec<&'static SlashCommand>,
        selected: usize,
    },
    Mention {
        query_len: usize,
        matches: Vec<String>,
        selected: usize,
    },
}

struct App {
    engine: Arc<Engine>,
    session: SessionId,
    /// Same handle `engine` was built with — used only to read the
    /// rollout log back (Ctrl+T, `expand_last_tool_call`); every write
    /// still goes exclusively through `engine`/`Engine::run_turn`.
    store: Arc<dyn braze_session::SessionStore>,
    status_line: String,
    /// Color preset every `HistoryCell` this session commits renders
    /// with — resolved once at startup (`braze-cli` from
    /// `Config::tui_theme`), never changes mid-session.
    theme: Theme,
    total_input_tokens: u64,
    total_output_tokens: u64,
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
    /// `/command` or `@mention` suggestions currently showing, if any —
    /// see `refresh_popup`. Always `None` while `turn_running` (no
    /// spare viewport rows to show it in without a live preview to hide
    /// — see `POPUP_MAX_VISIBLE`'s doc comment).
    popup: Option<ComposerPopup>,
    /// Every file under the cwd, relative paths, populated lazily on the
    /// first `@` trigger and cached for the rest of the session — see
    /// `mentionable_files`.
    mentionable_files: Option<Vec<String>>,
    turn_running: bool,
    /// The spawned turn's handle, so Esc can `abort()` it — see
    /// `interrupt_turn`. `None` whenever no turn is in flight.
    current_turn: Option<JoinHandle<()>>,
    /// Confirmation requests waiting on an answer, in arrival order —
    /// a `VecDeque` rather than a single `Option` because two tool
    /// calls dispatched concurrently in the same round can each need
    /// confirmation at once; only the front one is shown, answering it
    /// reveals the next.
    pending_approvals: VecDeque<ApprovalRequest>,
    should_quit: bool,
    update_tx: mpsc::UnboundedSender<TuiUpdate>,
    update_rx: mpsc::UnboundedReceiver<TuiUpdate>,
    approval_rx: mpsc::UnboundedReceiver<ApprovalRequest>,
}

impl App {
    fn new(
        engine: Arc<Engine>,
        session: SessionId,
        store: Arc<dyn braze_session::SessionStore>,
        approval_rx: mpsc::UnboundedReceiver<ApprovalRequest>,
        status_line: String,
        theme: Theme,
    ) -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        Self {
            engine,
            session,
            store,
            status_line,
            theme,
            total_input_tokens: 0,
            total_output_tokens: 0,
            markdown: MarkdownStreamCollector::default(),
            pending_tool_names: HashMap::new(),
            composer: TextArea::default(),
            popup: None,
            mentionable_files: None,
            turn_running: false,
            current_turn: None,
            pending_approvals: VecDeque::new(),
            should_quit: false,
            update_tx,
            update_rx,
            approval_rx,
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
                            self.on_key(key, terminal).await?;
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
                Some(request) = self.approval_rx.recv() => {
                    self.pending_approvals.push_back(request);
                }
            }
        }
    }

    async fn on_key(&mut self, key: KeyEvent, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        // Ctrl+C always quits, regardless of state — the universal
        // escape hatch, even mid-approval or mid-turn.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }

        // Ctrl+T is read-only (peeks at the rollout log, never mutates
        // anything) — harmless in any state, so it's checked globally
        // like Ctrl+C rather than gated behind "no popup/approval
        // active".
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.expand_last_tool_call(terminal).await?;
            return Ok(());
        }

        if !self.pending_approvals.is_empty() {
            match key.code {
                KeyCode::Char('y' | 'Y') => self.answer_pending_approval(true, terminal)?,
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.answer_pending_approval(false, terminal)?
                }
                // Ignore everything else while a decision is pending —
                // no typing into the composer, no accidental submit.
                _ => {}
            }
            return Ok(());
        }

        if self.popup.is_some() {
            match key.code {
                KeyCode::Up => {
                    self.move_popup_selection(-1);
                    return Ok(());
                }
                KeyCode::Down => {
                    self.move_popup_selection(1);
                    return Ok(());
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_popup_selection();
                    return Ok(());
                }
                KeyCode::Esc => {
                    self.popup = None;
                    return Ok(());
                }
                // Anything else (typing more of the query, Backspace,
                // ...) falls through to the normal composer handling
                // below, which then re-evaluates the popup from the new
                // cursor state via `refresh_popup`.
                _ => {}
            }
        }

        let composer_is_empty = self.composer.lines().len() == 1 && self.composer.lines()[0].is_empty();
        match (key.code, key.modifiers) {
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) && composer_is_empty => {
                self.should_quit = true;
            }
            (KeyCode::Esc, KeyModifiers::NONE) if self.turn_running => {
                self.interrupt_turn(terminal)?;
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

        self.refresh_popup();
        Ok(())
    }

    /// Re-evaluates whether the composer's cursor now sits inside an
    /// active `/`/`@` token, updating `self.popup` accordingly — called
    /// after every composer edit (`on_key`'s fallthrough arm), since
    /// typing, deleting, or moving the cursor can start, narrow, or end
    /// a trigger.
    fn refresh_popup(&mut self) {
        if self.turn_running {
            self.popup = None;
            return;
        }

        let cursor = self.composer.cursor();
        let is_first_line = cursor.0 == 0;
        let Some(line) = self.composer.lines().get(cursor.0) else {
            self.popup = None;
            return;
        };

        self.popup = match detect_trigger(line, cursor.1, is_first_line) {
            Some(ComposerTrigger::Slash(query)) => {
                let matches: Vec<&'static SlashCommand> = matching_commands(&query)
                    .into_iter()
                    .take(POPUP_MAX_VISIBLE)
                    .collect();
                (!matches.is_empty()).then(|| ComposerPopup::Slash {
                    query_len: query.chars().count(),
                    matches,
                    selected: 0,
                })
            }
            Some(ComposerTrigger::Mention(query)) => {
                let matches: Vec<String> = {
                    let files = self.mentionable_files();
                    matching_files(files, &query)
                        .into_iter()
                        .take(POPUP_MAX_VISIBLE)
                        .map(str::to_string)
                        .collect()
                };
                (!matches.is_empty()).then(|| ComposerPopup::Mention {
                    query_len: query.chars().count(),
                    matches,
                    selected: 0,
                })
            }
            None => None,
        };
    }

    /// Lazily walks the cwd on the first `@` trigger and caches the
    /// result for the rest of the session — see `mentions::list_files`'s
    /// doc comment for why a session-long-stale list is an accepted
    /// simplification, not silently wrong.
    fn mentionable_files(&mut self) -> &[String] {
        if self.mentionable_files.is_none() {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            self.mentionable_files = Some(list_files(&cwd));
        }
        self.mentionable_files.as_deref().unwrap_or(&[])
    }

    fn move_popup_selection(&mut self, delta: isize) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        let len = match popup {
            ComposerPopup::Slash { matches, .. } => matches.len(),
            ComposerPopup::Mention { matches, .. } => matches.len(),
        };
        if len == 0 {
            return;
        }
        let selected = match popup {
            ComposerPopup::Slash { selected, .. } | ComposerPopup::Mention { selected, .. } => {
                selected
            }
        };
        *selected = (*selected as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Replaces the `/query` or `@query` token behind the cursor with
    /// the selected suggestion (plus a trailing space) — deletes exactly
    /// `query_len` characters backward (the query, not the `/`/`@`
    /// marker itself) then inserts the full replacement. Does not submit
    /// or execute anything by itself: accepting a `/help` suggestion
    /// only autocompletes the composer to `"/help "`, same as accepting
    /// any other word — a separate Enter (now with the popup closed)
    /// actually submits/executes it, via `submit`'s own slash-command
    /// interception.
    fn accept_popup_selection(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let replacement = match popup {
            ComposerPopup::Slash {
                query_len,
                matches,
                selected,
            } => matches.get(selected).map(|cmd| (query_len, cmd.name.to_string())),
            ComposerPopup::Mention {
                query_len,
                matches,
                selected,
            } => matches.get(selected).map(|path| (query_len, path.clone())),
        };
        let Some((query_len, replacement)) = replacement else {
            return;
        };
        for _ in 0..query_len {
            self.composer.delete_char();
        }
        self.composer.insert_str(&replacement);
        self.composer.insert_str(" ");
    }

    /// Executes a built-in `/command` — only ever called from `submit`
    /// after confirming `command` exactly matches a `SLASH_COMMANDS`
    /// entry, so the wildcard arm here is unreachable in practice, not a
    /// silent fallback for a typo.
    fn run_slash_command(&mut self, command: &str, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        match command {
            "help" => self.commit_cell(&HelpCell, terminal),
            "quit" | "exit" => {
                self.should_quit = true;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn submit(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let text = self.composer.lines().join("\n");
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        // Built-in slash commands are handled entirely client-side and
        // never reach `Engine::run_turn` — the engine has no notion of
        // them at all (see `slash_commands`'s doc comment). Only an
        // exact match (post-autocomplete) counts; a message that merely
        // starts with `/` but isn't a recognized command name is sent
        // to the model as ordinary text instead.
        if let Some(command) = trimmed.strip_prefix('/')
            && SLASH_COMMANDS.iter().any(|c| c.name == command)
        {
            self.composer = TextArea::default();
            self.popup = None;
            return self.run_slash_command(command, terminal);
        }

        let user_text = trimmed.to_string();
        self.composer = TextArea::default();
        self.popup = None;

        self.commit_cell(
            &UserCell {
                text: user_text.clone(),
            },
            terminal,
        )?;

        self.turn_running = true;
        self.markdown = MarkdownStreamCollector::default();
        self.pending_tool_names.clear();

        // A fresh channel per turn, not a long-lived one reused across
        // turns: replacing `update_rx` drops the previous receiver, so
        // if a just-`interrupt_turn`-aborted task's send races a brand
        // new submit, it lands on an orphaned sender nobody reads from
        // anymore instead of corrupting the new turn's `markdown`
        // collector. `update_tx` keeps one live clone so `recv()` stays
        // pending (not immediately `None`) between turns.
        let (tx, rx) = mpsc::unbounded_channel();
        self.update_rx = rx;
        self.update_tx = tx.clone();

        let engine = Arc::clone(&self.engine);
        let session = self.session;
        let handle = tokio::spawn(async move {
            let mut observer = ChannelObserver::new(tx.clone());
            let result = engine.run_turn(&session, &user_text, &mut observer).await;
            let _ = tx.send(TuiUpdate::TurnFinished(result.map_err(|err| err.to_string())));
        });
        self.current_turn = Some(handle);

        Ok(())
    }

    /// Aborts the in-flight turn (Esc while `turn_running`). Safe:
    /// aborting a spawned task just drops its future at the next
    /// `.await` point — whatever was already persisted to the session
    /// store stays as-is, and any dangling `AssistantToolCall` with no
    /// matching `ToolCallCompleted` gets synthesized an error result by
    /// `Engine::repair_orphaned_tool_calls` the next time this session
    /// loads (the same mechanism that already handles a killed/crashed
    /// process — see PLAN.md § "Fase TUI — diseño"). Flushes whatever
    /// text had streamed in so far rather than discarding it: the user
    /// asked to stop generation, not to un-see what was already shown.
    fn interrupt_turn(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        if let Some(handle) = self.current_turn.take() {
            handle.abort();
        }
        self.turn_running = false;

        // Aborting the top-level `run_turn` task does not cancel any
        // tool-dispatch background task it spawned via `TaskNotifier`
        // (a separate `tokio::spawn`, per `ChannelTaskNotifier`) — one
        // of those could still be blocked in `confirm()` awaiting an
        // answer for a request already sitting in this queue. A new
        // turn can't have been submitted while `turn_running` was true,
        // so anything still queued here belongs to the turn just
        // abandoned — deny it (this codebase's safety default for any
        // ambiguous case) rather than leave a stale prompt that could
        // resurface confusingly once a new turn starts.
        for request in self.pending_approvals.drain(..) {
            let _ = request.respond.send(false);
        }

        if let Some(tail) = self.markdown.finish() {
            self.commit_cell(&AssistantMarkdownCell { markdown: tail }, terminal)?;
        }
        self.commit_cell(
            &NoticeCell {
                message: "⏸ interrupted by user".to_string(),
                theme: self.theme,
            },
            terminal,
        )?;
        Ok(())
    }

    /// Answers the front pending approval and commits a `PermissionCell`
    /// recording the decision. A queued approval that never got shown
    /// yet is untouched — only the front of the queue is ever answered
    /// (see `pending_approvals`'s doc comment).
    fn answer_pending_approval(
        &mut self,
        allowed: bool,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        let Some(request) = self.pending_approvals.pop_front() else {
            return Ok(());
        };
        let description = request.description.clone();
        // The other end (`ChannelConfirmationPrompt::confirm`) may have
        // stopped awaiting already only if it was itself dropped/aborted
        // (e.g. its turn got interrupted) — sending is best-effort, not
        // fatal if so.
        let _ = request.respond.send(allowed);
        self.commit_cell(
            &PermissionCell {
                description,
                allowed,
                theme: self.theme,
            },
            terminal,
        )?;
        Ok(())
    }

    /// Ctrl+T: commits the full, untruncated content of the most
    /// recently *completed* tool call to the scrollback — the simple
    /// alternative to a true fullscreen pager overlay (PLAN.md § "Fase
    /// TUI 2"): reads straight from the session store (the single
    /// source of truth for this content) rather than keeping any
    /// TUI-side cache of past cells. A no-op with a `NoticeCell` if no
    /// tool call has completed yet in this session, or if the store
    /// can't be read at all.
    async fn expand_last_tool_call(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let events = match self.store.load(&self.session).await {
            Ok(events) => events,
            Err(err) => {
                return self.commit_cell(
                    &NoticeCell {
                        message: format!("no se pudo leer el historial de la sesión: {err}"),
                        theme: self.theme,
                    },
                    terminal,
                );
            }
        };

        let Some((id, result)) = events.iter().rev().find_map(|event| match event {
            AgentEvent::ToolCallCompleted { id, result } => Some((id.clone(), result.clone())),
            _ => None,
        }) else {
            return self.commit_cell(
                &NoticeCell {
                    message: "todavía no se completó ninguna tool call en esta sesión".to_string(),
                    theme: self.theme,
                },
                terminal,
            );
        };

        // `ToolCallCompleted` doesn't carry the tool's name (only
        // `id`/`result`) — `AssistantToolCall` is the event that does,
        // same correlation `apply_update` already relies on for live
        // `ToolCallCell`s, just looked up from the full log here instead
        // of the in-memory `pending_tool_names` map.
        let name = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::AssistantToolCall {
                    id: call_id, name, ..
                } if *call_id == id => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "tool".to_string());

        self.commit_cell(
            &ExpandedToolOutputCell {
                name,
                is_error: result.is_error,
                content: result.content,
                theme: self.theme,
            },
            terminal,
        )
    }

    fn apply_update(
        &mut self,
        update: TuiUpdate,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        // Every `TuiUpdate` variant only ever originates from the
        // current turn's `ChannelObserver`/spawn block. If no turn is
        // currently running, this can only be a stale message racing a
        // turn that already finished or was interrupted (see
        // `submit`'s per-turn channel replacement and
        // `interrupt_turn`'s doc comment) — ignore it outright rather
        // than let it re-open `markdown`/`pending_tool_names` state a
        // fresh submit hasn't reset yet.
        if !self.turn_running && !matches!(update, TuiUpdate::TurnFinished(_)) {
            return Ok(());
        }

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
                self.commit_cell(&ToolCallCell::running(name, self.theme), terminal)?;
            }
            TuiUpdate::Event(AgentEvent::ToolCallCompleted { id, result }) => {
                let name = self
                    .pending_tool_names
                    .remove(&id)
                    .unwrap_or_else(|| "tool".to_string());
                self.commit_cell(
                    &ToolCallCell::done(name, result.is_error, &result.content, self.theme),
                    terminal,
                )?;
            }
            TuiUpdate::Event(AgentEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            }) => {
                self.total_input_tokens += u64::from(input_tokens);
                self.total_output_tokens += u64::from(output_tokens);
            }
            TuiUpdate::Event(_) => {
                // Compaction/permission-request-mirror/unknown events:
                // permission decisions are already rendered from
                // `answer_pending_approval`, not from their event
                // mirror, and compaction cells are "fase TUI 2" (PLAN.md
                // § "Fase TUI — diseño"). The engine still sees and acts
                // on all of these normally regardless.
            }
            // Only `TurnFinished` can still reach this point with
            // `turn_running` false (the early return above already
            // handled every other variant) — a stale completion from a
            // turn `interrupt_turn` already marked finished. Ignore it
            // instead of double-reporting (e.g. a second, confusing
            // error cell for a turn the user already saw get cut off).
            _ if !self.turn_running => {}
            TuiUpdate::TurnFinished(Ok(())) => {
                self.turn_running = false;
                self.current_turn = None;
            }
            TuiUpdate::TurnFinished(Err(message)) => {
                self.turn_running = false;
                self.current_turn = None;
                self.commit_cell(
                    &ErrorCell {
                        message,
                        theme: self.theme,
                    },
                    terminal,
                )?;
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

        if let Some(popup) = &self.popup {
            // Reuses the active-preview + hint rows for the popup
            // instead of growing the viewport — see `POPUP_MAX_VISIBLE`'s
            // doc comment. Safe: `refresh_popup` never sets a popup while
            // `turn_running`, so the preview area is otherwise idle.
            let popup_area = Rect {
                x: active_area.x,
                y: active_area.y,
                width: active_area.width,
                height: active_area.height + hint_area.height,
            };
            draw_popup(frame, popup_area, popup);
        } else {
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

            let [hint_left, hint_right] =
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(hint_area);

            let hint = if !self.pending_approvals.is_empty() {
                "esperando tu decisión... (y permitir · n/Esc denegar)"
            } else if self.turn_running {
                "esperando respuesta del modelo... (Ctrl+C salir · Esc interrumpir)"
            } else {
                "Enter enviar · Ctrl+J salto de linea · / comandos · @ archivos · Ctrl+T output · Ctrl+C salir"
            };
            frame.render_widget(
                Paragraph::new(hint).style(Style::default().fg(self.theme.muted)),
                hint_left,
            );

            let status = status_bar::render(
                &self.status_line,
                self.session,
                self.total_input_tokens,
                self.total_output_tokens,
            );
            frame.render_widget(
                Paragraph::new(status)
                    .style(Style::default().fg(self.theme.muted))
                    .alignment(Alignment::Right),
                hint_right,
            );
        }

        if let Some(request) = self.pending_approvals.front() {
            let mut lines = vec![Line::from(request.description.clone())];
            let mut answer_hint = "esta acción puede ser irreversible — y permitir · n/Esc denegar".to_string();
            if self.pending_approvals.len() > 1 {
                answer_hint.push_str(&format!("  ({} pendientes)", self.pending_approvals.len()));
            }
            lines.push(Line::from(answer_hint).style(Style::default().fg(self.theme.warning)));
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), composer_area);
        } else {
            frame.render_widget(&self.composer, composer_area);
        }
    }
}

/// Renders the `/`/`@` suggestion list into `area` — a free function
/// (not a method) since it only needs `popup`, not the rest of `App`.
fn draw_popup(frame: &mut ratatui::Frame, area: Rect, popup: &ComposerPopup) {
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);

    let lines: Vec<Line> = match popup {
        ComposerPopup::Slash { matches, selected, .. } => matches
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let style = if i == *selected {
                    selected_style
                } else {
                    Style::default()
                };
                Line::from(Span::styled(
                    format!("/{}  {}", cmd.name, cmd.description),
                    style,
                ))
            })
            .collect(),
        ComposerPopup::Mention { matches, selected, .. } => matches
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let style = if i == *selected {
                    selected_style
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!("@{path}"), style))
            })
            .collect(),
    };

    frame.render_widget(Paragraph::new(lines), area);
}
