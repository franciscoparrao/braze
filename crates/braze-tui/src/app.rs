//! The event loop: `tokio::select!` between keyboard input, the current
//! turn's live updates, and pending permission approvals (PLAN.md §
//! "Fase TUI — diseño"). One [`Engine::run_turn`] runs at a time, spawned
//! as a background task so the composer stays responsive while the
//! model streams — a second submission is ignored while one is in
//! flight (two concurrent `run_turn` calls on the same session would
//! race on the session store's loads).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use braze_engine::Engine;
use braze_events::AgentEvent;
use braze_types::SessionId;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use ratatui_textarea::{CursorMove, TextArea};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::EngineFactory;
use crate::approval::ApprovalRequest;
use crate::composer_trigger::{ComposerTrigger, detect_trigger, token_suffix_len};
use crate::error::TuiError;
use crate::history_cell::{
    AssistantMarkdownCell, ErrorCell, ExpandedToolOutputCell, HarnessNoteCell, HelpCell,
    HistoryCell, NoticeCell, PermissionCell, PlanCell, QuestionCell, ToolCallCell, UserCell,
};
use crate::markdown_stream::MarkdownStreamCollector;
use crate::mentions::{list_files, matching_files};
use crate::observer::{ChannelObserver, TuiUpdate};
use crate::question::QuestionRequest;
use crate::slash_commands::{SlashCommand, matching_commands};
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

/// Longest gap between two Esc presses that still counts as one
/// "Esc-Esc" (backtrack) rather than two unrelated single presses —
/// matches typical double-tap conventions (double-click, etc.).
const DOUBLE_ESC_WINDOW: Duration = Duration::from_millis(600);

/// Braille-dot spinner frames — the same de-facto standard cadence used
/// by most terminal coding agents (Claude Code included). Purely
/// cosmetic, so no theming: unlike `Theme`'s 4 semantic colors, these
/// glyphs render the same regardless of dark/light/high-contrast.
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// One full cycle at this cadence takes `SPINNER_FRAMES.len() *
/// SPINNER_FRAME_DURATION` ≈ 800ms — lively enough to read as "alive"
/// without being distracting.
const SPINNER_FRAME_DURATION: Duration = Duration::from_millis(80);

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
#[allow(clippy::too_many_arguments)] // mirrors `lib.rs::run`, the public seam
pub async fn run(
    terminal: &mut Terminal<Backend>,
    engine: Engine,
    live_session: Arc<std::sync::Mutex<SessionId>>,
    store: Arc<dyn braze_session::SessionStore>,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: mpsc::UnboundedReceiver<QuestionRequest>,
    status_line: String,
    theme: Theme,
    engine_factory: EngineFactory,
    model_candidates: Vec<String>,
) -> Result<(), TuiError> {
    App::new(
        Arc::new(engine),
        live_session,
        store,
        approvals,
        questions,
        status_line,
        theme,
        engine_factory,
        model_candidates,
    )
    .run(terminal)
    .await
}

/// What a spawned `/model` switch task reports back through
/// `App::switch_rx`: the spec it tried (for the error message — the
/// user may have typed it minutes ago) and either the rebuilt engine +
/// status-bar label or a display error.
type ModelSwitchOutcome = (String, Result<(Engine, String), String>);

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
        /// Characters still typed *after* the cursor within the same
        /// token (e.g. cursor mid-word) — deleted forward too, so
        /// accepting a completion never strands a leftover suffix right
        /// after the inserted replacement (bajo,
        /// docs/AUDITORIA-2026-07-v2.md, "replace_trigger_token deja
        /// residuo con el cursor a mitad de token"). See
        /// `composer_trigger::token_suffix_len`.
        suffix_len: usize,
        matches: Vec<&'static SlashCommand>,
        selected: usize,
    },
    Mention {
        query_len: usize,
        suffix_len: usize,
        matches: Vec<String>,
        selected: usize,
    },
    /// Esc-Esc (`handle_idle_escape`): prior user messages to jump back
    /// to, most recent first. `(event index in the loaded log, message
    /// text)` — the index is what `backtrack_to` slices the replayed
    /// prefix at. Capped to `POPUP_MAX_VISIBLE`, same simplification as
    /// `Slash`/`Mention` (no scrolling within the popup) — there's no
    /// query to type here to narrow it further, so this caps to "the
    /// last few messages", not an arbitrary point in a long
    /// conversation.
    Backtrack {
        messages: Vec<(usize, String)>,
        selected: usize,
    },
    /// `/model` with no argument: candidate `backend[:modelo]` specs to
    /// switch to (see `App::model_candidates`). Unlike `Backtrack`, the
    /// full list is kept and `draw_popup` *windows* over it around
    /// `selected` — there's no query to narrow a long list, and (unlike
    /// old messages) the tail of the list is no less relevant than the
    /// head, so capping at open time would hide arbitrary models.
    Model { specs: Vec<String>, selected: usize },
}

struct App {
    engine: Arc<Engine>,
    session: SessionId,
    /// N-12 (docs/AUDITORIA-2026-07-v2.md): the same shared handle every
    /// `ChannelConfirmationPrompt` this session's `PermissionGuard`s were
    /// built with reads from — `backtrack_to` writes the fresh session id
    /// into it alongside `self.session`, so a permission decision made
    /// *after* a backtrack persists against the session the user is
    /// actually now talking to, not the one this `App` started with.
    live_session: Arc<std::sync::Mutex<SessionId>>,
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
    /// When the last Esc was pressed while idle (`!turn_running`, no
    /// popup already handling it) — `handle_idle_escape` compares this
    /// against `Instant::now()` to detect a double-tap within
    /// `DOUBLE_ESC_WINDOW` and open the backtrack popup. `None` once
    /// consumed by a detected double-tap, or if the last Esc was long
    /// enough ago (or this is the first one this session).
    last_esc_at: Option<Instant>,
    turn_running: bool,
    /// Advanced by `run()`'s spinner tick (only while `turn_running` or
    /// `switching_model` — see the `tokio::select!` branch's `if` guard)
    /// and read by `spinner_glyph` to animate the wait hints. Before this
    /// field existed, the only feedback during a long tool-calling turn
    /// was static text — no visual confirmation braze was still alive
    /// (docs/usability-log-2026-07-07-si2.md, comparación contra el
    /// cookbook de OpenRouter). Wraps via modulo in `spinner_glyph`, so
    /// the exact integer value here never matters past that.
    spinner_frame: usize,
    /// The spawned turn's handle, so Esc can `abort()` it — see
    /// `interrupt_turn`. `None` whenever no turn is in flight.
    current_turn: Option<JoinHandle<()>>,
    /// Rebuilds the engine for `/model` — see `crate::EngineFactory`.
    engine_factory: EngineFactory,
    /// Candidate `backend[:modelo]` specs the `/model` picker offers,
    /// computed once at startup by `braze-cli` (config's backends +
    /// the Ollama server's installed models). Not refreshed mid-session
    /// — same accepted staleness as `mentionable_files`; `/model <spec>`
    /// reaches anything not (or no longer) on this list.
    model_candidates: Vec<String>,
    /// A `/model` switch is in flight (the factory is rebuilding the
    /// engine in a background task) — gates submissions and further
    /// switches the same way `turn_running` gates submissions, since a
    /// turn started against the old engine mid-swap would race the
    /// replacement.
    switching_model: bool,
    /// Sending half handed to each spawned switch task; `switch_rx` is
    /// the `select!` arm that applies the outcome. A long-lived channel
    /// (unlike the per-turn `update_tx`/`update_rx` pair): at most one
    /// switch is ever in flight (`switching_model`), so there's no stale
    /// predecessor to race against.
    switch_tx: mpsc::UnboundedSender<ModelSwitchOutcome>,
    switch_rx: mpsc::UnboundedReceiver<ModelSwitchOutcome>,
    /// Confirmation requests waiting on an answer, in arrival order —
    /// a `VecDeque` rather than a single `Option` because two tool
    /// calls dispatched concurrently in the same round can each need
    /// confirmation at once; only the front one is shown, answering it
    /// reveals the next.
    pending_approvals: VecDeque<ApprovalRequest>,
    /// `ask_user` questions waiting on an answer, in arrival order —
    /// same `VecDeque` rationale as `pending_approvals` (two tool calls
    /// dispatched concurrently in the same round can each ask at once).
    /// Only the front one is shown; a pending *approval* takes
    /// precedence over a pending question in both key routing and
    /// drawing (`on_key`/`draw` check approvals first) — a permission
    /// decision guards an action about to run, a question just blocks
    /// one tool's result.
    pending_questions: VecDeque<QuestionRequest>,
    /// Selection index into the *front* pending question's options —
    /// reset to 0 every time a question is answered (the next question's
    /// options are unrelated to where the cursor was on this one's).
    question_selected: usize,
    should_quit: bool,
    update_tx: mpsc::UnboundedSender<TuiUpdate>,
    update_rx: mpsc::UnboundedReceiver<TuiUpdate>,
    approval_rx: mpsc::UnboundedReceiver<ApprovalRequest>,
    question_rx: mpsc::UnboundedReceiver<QuestionRequest>,
}

impl App {
    #[allow(clippy::too_many_arguments)] // mirrors `lib.rs::run`, the public seam
    fn new(
        engine: Arc<Engine>,
        live_session: Arc<std::sync::Mutex<SessionId>>,
        store: Arc<dyn braze_session::SessionStore>,
        approval_rx: mpsc::UnboundedReceiver<ApprovalRequest>,
        question_rx: mpsc::UnboundedReceiver<QuestionRequest>,
        status_line: String,
        theme: Theme,
        engine_factory: EngineFactory,
        model_candidates: Vec<String>,
    ) -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (switch_tx, switch_rx) = mpsc::unbounded_channel();
        // The initial value is read out of the shared handle rather than
        // taken as a separate parameter — `live_session` is always
        // seeded with the session this run started on (see
        // `braze-cli::run`), so there is only ever one source of truth.
        let session = *live_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // "bordered" input style: plain `─` lines above/below, no
        // OSC 11 background-color detection needed — works on any
        // terminal, same portability reasoning `theme.rs` already
        // documents for colors. Set once here (not re-set every `draw`,
        // which only borrows `&self`) since the style never changes
        // mid-session.
        let mut composer = TextArea::default();
        composer.set_block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
        Self {
            engine,
            session,
            live_session,
            store,
            status_line,
            theme,
            total_input_tokens: 0,
            total_output_tokens: 0,
            markdown: MarkdownStreamCollector::default(),
            pending_tool_names: HashMap::new(),
            composer,
            popup: None,
            mentionable_files: None,
            last_esc_at: None,
            turn_running: false,
            spinner_frame: 0,
            current_turn: None,
            engine_factory,
            model_candidates,
            switching_model: false,
            switch_tx,
            switch_rx,
            pending_approvals: VecDeque::new(),
            pending_questions: VecDeque::new(),
            question_selected: 0,
            should_quit: false,
            update_tx,
            update_rx,
            approval_rx,
            question_rx,
        }
    }

    /// Current animation frame for the wait hints — wraps via modulo, so
    /// `spinner_frame` can grow unbounded for the life of the process
    /// without ever needing to reset. Delegates to a free function (same
    /// pattern as `truncate_for_display`/`backtrack_preview` below) so
    /// the cycling logic is testable without constructing a full `App`.
    fn spinner_glyph(&self) -> char {
        spinner_glyph_at(self.spinner_frame)
    }

    async fn run(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        let mut events = EventStream::new();
        // Ticks every `SPINNER_FRAME_DURATION` — but the `select!` branch
        // below only polls it (`, if self.turn_running ||
        // self.switching_model`) while there's actually something to
        // animate, so it costs nothing while idle: the branch is simply
        // never evaluated, no ticks accumulate to "catch up" on later.
        let mut spinner_interval = tokio::time::interval(SPINNER_FRAME_DURATION);

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
                        // N-10 (docs/AUDITORIA-2026-07-v2.md): with
                        // bracketed paste enabled (`terminal::setup`),
                        // crossterm delivers a paste as one `Event::Paste`
                        // instead of a flood of key events — handle it as
                        // a single literal insert rather than falling
                        // through to `Some(Ok(_)) => {}` (which used to
                        // silently drop it, or — without bracketed paste
                        // enabled at all — let each embedded `\r`/`\n`
                        // masquerade as the user pressing Enter).
                        Some(Ok(Event::Paste(text))) => {
                            self.on_paste(text);
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
                _ = spinner_interval.tick(), if self.turn_running || self.switching_model => {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                }
                Some((spec, result)) = self.switch_rx.recv() => {
                    self.finish_model_switch(spec, result, terminal)?;
                }
                Some(request) = self.approval_rx.recv() => {
                    // N-29 (docs/AUDITORIA-2026-07-v2.md): aborting the
                    // top-level turn task (`interrupt_turn`) does not
                    // cancel the tool-dispatch background tasks it
                    // spawned — one of those can still call `confirm()`
                    // after the turn was abandoned, sending a request
                    // here well after `turn_running` went back to
                    // `false`. A legitimate request only ever arrives
                    // while its own turn is still running, so anything
                    // arriving while idle is stale by construction — deny
                    // it immediately instead of queuing an approval
                    // overlay (which would otherwise lock the composer)
                    // for a turn that's already gone.
                    if self.turn_running {
                        self.pending_approvals.push_back(request);
                    } else {
                        let _ = request.respond.send(false);
                    }
                }
                Some(request) = self.question_rx.recv() => {
                    // Same staleness reasoning as the approval arm above
                    // (N-29): a legitimate `ask_user` only ever arrives
                    // while its own turn is still running — anything
                    // arriving while idle belongs to an abandoned turn.
                    // Answer "no answer" (never a guessed choice) instead
                    // of queuing an overlay for a turn that's gone.
                    if self.turn_running {
                        self.pending_questions.push_back(request);
                    } else {
                        let _ = request.respond.send(None);
                    }
                }
            }
        }
    }

    async fn on_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
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
                    // Bajo (docs/AUDITORIA-2026-07-v2.md, "last_esc_at no
                    // se limpia cuando otro handler consume el Esc"): this
                    // Esc is answering an approval, not arming/completing
                    // the idle Esc-Esc backtrack double-tap — a stale
                    // timestamp here could make a later, unrelated single
                    // idle Esc misread as the second tap.
                    self.last_esc_at = None;
                    self.answer_pending_approval(false, terminal)?
                }
                // Ignore everything else while a decision is pending —
                // no typing into the composer, no accidental submit.
                _ => {}
            }
            return Ok(());
        }

        if !self.pending_questions.is_empty() {
            let options_len = self
                .pending_questions
                .front()
                .map(|q| q.options.len())
                .unwrap_or(0);
            match key.code {
                // Direct pick: '1'..='4' (AskUserProvider caps options at
                // 2..=4, so single digits always suffice). Out-of-range
                // digits are ignored rather than answering anything.
                KeyCode::Char(c @ '1'..='4') => {
                    let index = c as usize - '1' as usize;
                    if index < options_len {
                        self.answer_pending_question(Some(index), terminal)?;
                    }
                }
                KeyCode::Up => {
                    self.question_selected = self
                        .question_selected
                        .checked_sub(1)
                        .unwrap_or(options_len.saturating_sub(1));
                }
                KeyCode::Down => {
                    if options_len > 0 {
                        self.question_selected = (self.question_selected + 1) % options_len;
                    }
                }
                KeyCode::Enter => {
                    let index = self.question_selected.min(options_len.saturating_sub(1));
                    self.answer_pending_question(Some(index), terminal)?;
                }
                KeyCode::Esc => {
                    // Same `last_esc_at` hygiene as the approval Esc arm
                    // above: this Esc is declining a question, not part
                    // of an idle Esc-Esc backtrack double-tap.
                    self.last_esc_at = None;
                    self.answer_pending_question(None, terminal)?;
                }
                // Ignore everything else while a question is pending —
                // same "no typing into the composer" rule as approvals.
                _ => {}
            }
            return Ok(());
        }

        if let Some(popup) = &self.popup {
            // N-28 (docs/AUDITORIA-2026-07-v2.md): `Slash`/`Mention` have
            // a query that further typing narrows — falling through to
            // the composer and re-evaluating via `refresh_popup` is
            // correct for those. `Backtrack`/`Model` have no query at
            // all: any key besides the ones handled below means the user
            // is just continuing to compose, not choosing an entry.
            // Closing it here (instead of silently leaving it open
            // underneath whatever they type) is what makes a later Enter
            // submit their draft normally instead of being hijacked as
            // "accept this selection" and discarding it.
            let is_queryless = matches!(
                popup,
                ComposerPopup::Backtrack { .. } | ComposerPopup::Model { .. }
            );
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
                    self.accept_popup_selection(terminal).await?;
                    return Ok(());
                }
                KeyCode::Esc => {
                    // Bajo (docs/AUDITORIA-2026-07-v2.md, "last_esc_at no
                    // se limpia cuando otro handler consume el Esc"): see
                    // the identical note on the pending-approval Esc arm
                    // above.
                    self.last_esc_at = None;
                    self.popup = None;
                    return Ok(());
                }
                _ if is_queryless => {
                    self.popup = None;
                }
                // Anything else (typing more of the query, Backspace,
                // ...) falls through to the normal composer handling
                // below, which then re-evaluates the popup from the new
                // cursor state via `refresh_popup`.
                _ => {}
            }
        }

        let composer_is_empty =
            self.composer.lines().len() == 1 && self.composer.lines()[0].is_empty();
        match (key.code, key.modifiers) {
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) && composer_is_empty => {
                self.should_quit = true;
            }
            (KeyCode::Esc, KeyModifiers::NONE) if self.turn_running => {
                // Bajo (docs/AUDITORIA-2026-07-v2.md, "last_esc_at no se
                // limpia cuando otro handler consume el Esc"): see the
                // identical note on the pending-approval Esc arm above.
                self.last_esc_at = None;
                self.interrupt_turn(terminal)?;
            }
            // Idle Esc: not consumed by anything else above (no popup,
            // no pending approval, no turn to interrupt) — arms or
            // fires the Esc-Esc backtrack double-tap. A single idle Esc
            // does nothing else, matching the "no-op" behavior this key
            // already had here before backtrack existed.
            //
            // N-28 (docs/AUDITORIA-2026-07-v2.md): only a genuine new
            // `Press` may arm/complete the double-tap — a terminal's
            // auto-repeat for a *held* Esc (e.g. holding it down to
            // interrupt a turn) delivers `KeyEventKind::Repeat` events,
            // and by the time `turn_running` flips to `false` those
            // repeats would otherwise land here and could open the
            // backtrack popup — which then hijacks Enter and can discard
            // whatever the user is typing — without them ever having
            // released the key, let alone pressed it twice.
            (KeyCode::Esc, KeyModifiers::NONE) if key.kind == KeyEventKind::Press => {
                self.handle_idle_escape(terminal).await?;
            }
            (KeyCode::Esc, KeyModifiers::NONE) => {}
            // Ctrl+J: literal newline, bypassing `TextArea::input`'s own
            // `Key::Enter` handling (which we deliberately never reach —
            // plain Enter is intercepted below as submit, before it ever
            // gets forwarded to the composer).
            (KeyCode::Char('j'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.composer.insert_newline();
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !self.turn_running && !self.switching_model {
                    self.submit(terminal)?;
                }
                // Else: a turn is already running (or a `/model` switch
                // is rebuilding the engine) — ignore the submission
                // rather than racing a second `run_turn` against the
                // same session, or starting a turn on an engine about to
                // be replaced (see this module's doc comment). The
                // composer keeps whatever was typed.
            }
            _ => {
                self.composer.input(Event::Key(key));
            }
        }

        self.refresh_popup();
        Ok(())
    }

    /// Handles a bracketed paste (N-10, docs/AUDITORIA-2026-07-v2.md):
    /// inserts the pasted text literally into the composer in one atomic
    /// edit via `insert_str` (which itself splits embedded `\n`/`\r\n`
    /// into real composer lines — never a submit), instead of letting the
    /// terminal replay it key-by-key. Gated the same as normal typing:
    /// ignored while a permission decision is pending (matches
    /// `on_key`'s "no typing into the composer" rule for that state);
    /// allowed while a turn is running, same as typing already is.
    fn on_paste(&mut self, text: String) {
        if !self.pending_approvals.is_empty() {
            return;
        }
        self.composer.insert_str(&text);
        self.refresh_popup();
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
        // A `Backtrack`/`Model` popup isn't driven by a `/`/`@` cursor
        // trigger at all (they open from `handle_idle_escape` /
        // `run_slash_command`, with an empty composer) — re-deriving
        // from cursor state on every key would immediately close it
        // again the moment `on_key` calls this at the end of the very
        // same keystroke that just opened it. Only
        // `accept_popup_selection`/an explicit Esc close them.
        if matches!(
            self.popup,
            Some(ComposerPopup::Backtrack { .. }) | Some(ComposerPopup::Model { .. })
        ) {
            return;
        }

        let cursor = self.composer.cursor();
        let is_first_line = cursor.0 == 0;
        let Some(line) = self.composer.lines().get(cursor.0) else {
            self.popup = None;
            return;
        };

        let suffix_len = token_suffix_len(line, cursor.1);
        self.popup = match detect_trigger(line, cursor.1, is_first_line) {
            Some(ComposerTrigger::Slash(query)) => {
                let matches: Vec<&'static SlashCommand> = matching_commands(&query)
                    .into_iter()
                    .take(POPUP_MAX_VISIBLE)
                    .collect();
                (!matches.is_empty()).then(|| ComposerPopup::Slash {
                    query_len: query.chars().count(),
                    suffix_len,
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
                    suffix_len,
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
            ComposerPopup::Backtrack { messages, .. } => messages.len(),
            ComposerPopup::Model { specs, .. } => specs.len(),
        };
        if len == 0 {
            return;
        }
        let selected = match popup {
            ComposerPopup::Slash { selected, .. }
            | ComposerPopup::Mention { selected, .. }
            | ComposerPopup::Backtrack { selected, .. }
            | ComposerPopup::Model { selected, .. } => selected,
        };
        *selected = (*selected as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Dispatches on the popup kind: `Slash`/`Mention` just autocomplete
    /// the composer text (see `replace_trigger_token`), `Backtrack`
    /// swaps `self.session` and loads the target message for editing
    /// (see `backtrack_to`) — genuinely different actions, unlike
    /// `move_popup_selection` which is the same index arithmetic for
    /// every kind.
    async fn accept_popup_selection(
        &mut self,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        let Some(popup) = self.popup.take() else {
            return Ok(());
        };
        match popup {
            ComposerPopup::Slash {
                query_len,
                suffix_len,
                matches,
                selected,
            } => {
                if let Some(cmd) = matches.get(selected) {
                    self.replace_trigger_token(query_len, suffix_len, cmd.name);
                }
                Ok(())
            }
            ComposerPopup::Mention {
                query_len,
                suffix_len,
                matches,
                selected,
            } => {
                if let Some(path) = matches.get(selected) {
                    self.replace_trigger_token(query_len, suffix_len, path);
                }
                Ok(())
            }
            ComposerPopup::Backtrack { messages, selected } => {
                let Some((event_index, text)) = messages.into_iter().nth(selected) else {
                    return Ok(());
                };
                self.backtrack_to(event_index, text, terminal).await
            }
            ComposerPopup::Model { specs, selected } => {
                let Some(spec) = specs.into_iter().nth(selected) else {
                    return Ok(());
                };
                self.start_model_switch(spec, terminal)
            }
        }
    }

    /// Replaces the whole `/query` or `@query` token around the cursor
    /// with `replacement` (plus a trailing space): deletes exactly
    /// `query_len` characters backward (the query, not the `/`/`@` marker
    /// itself) and `suffix_len` characters forward (whatever was still
    /// typed after the cursor within the same token — bajo,
    /// docs/AUDITORIA-2026-07-v2.md, "replace_trigger_token deja residuo
    /// con el cursor a mitad de token"; see
    /// `composer_trigger::token_suffix_len`), then inserts the full
    /// replacement. Does not submit or execute anything by itself:
    /// accepting a `/help` suggestion only autocompletes the composer to
    /// `"/help "`, same as accepting any other word — a separate Enter
    /// (now with the popup closed) actually submits/executes it, via
    /// `submit`'s own slash-command interception.
    fn replace_trigger_token(&mut self, query_len: usize, suffix_len: usize, replacement: &str) {
        for _ in 0..query_len {
            self.composer.delete_char();
        }
        if suffix_len > 0 {
            self.composer.delete_str(suffix_len);
        }
        self.composer.insert_str(replacement);
        self.composer.insert_str(" ");
    }

    /// Esc-Esc detection (`on_key`'s idle-Esc arm): the first Esc while
    /// idle just arms the timer; a second one within
    /// `DOUBLE_ESC_WINDOW` opens the backtrack popup. A single idle Esc
    /// otherwise does nothing, same as before backtrack existed.
    async fn handle_idle_escape(
        &mut self,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        let now = Instant::now();
        let is_double_tap = self
            .last_esc_at
            .is_some_and(|previous| now.duration_since(previous) < DOUBLE_ESC_WINDOW);
        if !is_double_tap {
            self.last_esc_at = Some(now);
            return Ok(());
        }
        self.last_esc_at = None;
        self.open_backtrack_popup(terminal).await
    }

    /// Reads the session's rollout log fresh (same seam as
    /// `expand_last_tool_call`) and opens a `ComposerPopup::Backtrack`
    /// listing the most recent `AgentEvent::UserMessage`s, most recent
    /// first. A `NoticeCell` instead if the store can't be read, or if
    /// there's no user message yet to backtrack to (a fresh session).
    async fn open_backtrack_popup(
        &mut self,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
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

        let messages: Vec<(usize, String)> = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                AgentEvent::UserMessage { text } => Some((index, text.clone())),
                _ => None,
            })
            .rev()
            .take(POPUP_MAX_VISIBLE)
            .collect();

        if messages.is_empty() {
            return self.commit_cell(
                &NoticeCell {
                    message: "no hay ningún mensaje anterior al cual retroceder".to_string(),
                    theme: self.theme,
                },
                terminal,
            );
        }

        self.popup = Some(ComposerPopup::Backtrack {
            messages,
            selected: 0,
        });
        Ok(())
    }

    /// Rewinds to before the user message at `event_index`: replays
    /// every event strictly before it (never `event_index` itself, nor
    /// anything after) into a **brand-new session id**, switches
    /// `self.session` to it, and loads `text` into the composer for
    /// editing before resubmitting. A new session rather than mutating
    /// the current one in place — `SessionStore` is append-only by
    /// design (`braze-session`'s frozen contract has no
    /// truncate/rewind, and adding one would mean reconciling
    /// `FileSessionStore`'s in-memory cache too); replaying the prefix
    /// into a fresh id needs nothing beyond `append`/`load`, which
    /// already exist, and leaves the original session's full history
    /// intact and still `--resume`-able. The scrollback keeps showing
    /// everything that already happened — nothing is hidden — only
    /// which session id future turns append to changes.
    ///
    /// Known accepted limitation (bajo, docs/AUDITORIA-2026-07-v2.md,
    /// "replay de backtrack fallido deja archivo de sesión huérfano"): if
    /// replaying the prefix fails partway (disk full, permission error),
    /// the new session's partially-written file is left on disk,
    /// unreferenced by any live state. `SessionStore` (frozen contract)
    /// has no delete method, and adding one for this rare, harmless
    /// (nothing points to the orphan; it's never read back) cleanup case
    /// isn't proportionate to the fix.
    async fn backtrack_to(
        &mut self,
        event_index: usize,
        text: String,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        let events = match self.store.load(&self.session).await {
            Ok(events) => events,
            Err(err) => {
                return self.commit_cell(
                    &NoticeCell {
                        message: format!("no se pudo retroceder: {err}"),
                        theme: self.theme,
                    },
                    terminal,
                );
            }
        };

        let new_session = SessionId::new();
        let prefix = &events[..event_index.min(events.len())];
        for event in prefix {
            if let Err(err) = self.store.append(&new_session, event).await {
                return self.commit_cell(
                    &NoticeCell {
                        message: format!("no se pudo retroceder: {err}"),
                        theme: self.theme,
                    },
                    terminal,
                );
            }
        }
        // N-26 (docs/AUDITORIA-2026-07-v2.md): the replicated prefix can
        // end with an orphaned tool_use whose repair (if any existed at
        // all) lives *after* the cut point in the original log — repair
        // it here too, exactly as `Engine::run_turn` does for a
        // crash-orphaned tool_use, so the new session doesn't inherit a
        // permanently-unresumable one.
        for repair in braze_engine::synthesize_orphan_repairs(prefix) {
            if let Err(err) = self.store.append(&new_session, &repair).await {
                return self.commit_cell(
                    &NoticeCell {
                        message: format!("no se pudo retroceder: {err}"),
                        theme: self.theme,
                    },
                    terminal,
                );
            }
        }
        self.session = new_session;
        // N-12 (docs/AUDITORIA-2026-07-v2.md): keep the shared handle
        // every `ChannelConfirmationPrompt` reads from in sync with
        // `self.session` — otherwise a permission decision made after
        // this backtrack would keep landing in the pre-backtrack
        // session's rollout log.
        *self
            .live_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = new_session;

        self.composer = TextArea::from(text.lines());
        self.composer.move_cursor(CursorMove::Bottom);
        self.composer.move_cursor(CursorMove::End);

        self.commit_cell(
            &NoticeCell {
                message: "↩ retrocediste a un mensaje anterior — edita y reenvia (nueva sesión, ver la barra de estado)".to_string(),
                theme: self.theme,
            },
            terminal,
        )
    }

    /// Executes a built-in `/command` — only ever called from `submit`
    /// after `parse_slash_command` confirms `command` is a registered
    /// name, so the wildcard arm here is unreachable in practice, not a
    /// silent fallback for a typo.
    fn run_slash_command(
        &mut self,
        command: &str,
        args: Option<&str>,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        match command {
            "help" => self.commit_cell(&HelpCell, terminal),
            // `/model <backend>[:<modelo>]` switches directly; a bare
            // `/model` opens the candidates picker instead.
            "model" => match args {
                Some(spec) => self.start_model_switch(spec.to_string(), terminal),
                None => self.open_model_picker(terminal),
            },
            "quit" | "exit" => {
                self.should_quit = true;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Opens the `/model` picker over `model_candidates` — a
    /// `NoticeCell` pointing at the argument form instead if startup
    /// couldn't compute any candidate (e.g. Ollama down and no other
    /// backend configured).
    fn open_model_picker(&mut self, terminal: &mut Terminal<Backend>) -> Result<(), TuiError> {
        if self.model_candidates.is_empty() {
            return self.commit_cell(
                &NoticeCell {
                    message:
                        "no hay modelos candidatos conocidos — usa /model <backend>[:<modelo>]"
                            .to_string(),
                    theme: self.theme,
                },
                terminal,
            );
        }
        self.popup = Some(ComposerPopup::Model {
            specs: self.model_candidates.clone(),
            selected: 0,
        });
        Ok(())
    }

    /// Kicks off a `/model` switch: spawns the `EngineFactory` rebuild
    /// as a background task (reconnecting MCP servers can take a moment
    /// — the UI keeps drawing) and reports the outcome back through
    /// `switch_rx`, where `finish_model_switch` applies it. Gated on
    /// both `turn_running` and `switching_model`: the engine must not be
    /// replaced under a live turn, and two rebuilds racing each other
    /// would make "which engine won?" order-dependent.
    fn start_model_switch(
        &mut self,
        spec: String,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        if self.turn_running || self.switching_model {
            return self.commit_cell(
                &NoticeCell {
                    message: "hay un turno o cambio de modelo en curso — inténtalo cuando termine"
                        .to_string(),
                    theme: self.theme,
                },
                terminal,
            );
        }
        self.switching_model = true;
        self.commit_cell(
            &NoticeCell {
                message: format!("⏳ cambiando modelo a {spec}…"),
                theme: self.theme,
            },
            terminal,
        )?;

        let future = (self.engine_factory)(spec.clone());
        let tx = self.switch_tx.clone();
        tokio::spawn(async move {
            // A send error means the app loop is gone (quit mid-switch)
            // — nothing left to apply the new engine to.
            let _ = tx.send((spec, future.await));
        });
        Ok(())
    }

    /// Applies a finished `/model` switch (the `switch_rx` arm of the
    /// event loop): on success swaps the engine and status-bar label —
    /// the session id is untouched, so the conversation continues
    /// exactly where it was, now against the new backend/model (the
    /// engine reloads history from the session store each turn) — and on
    /// failure keeps the current engine running as if nothing happened.
    fn finish_model_switch(
        &mut self,
        spec: String,
        result: Result<(Engine, String), String>,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        self.switching_model = false;
        match result {
            Ok((engine, status_line)) => {
                self.engine = Arc::new(engine);
                self.status_line = status_line;
                self.commit_cell(
                    &NoticeCell {
                        message: format!("✓ modelo cambiado a {}", self.status_line),
                        theme: self.theme,
                    },
                    terminal,
                )
            }
            Err(message) => self.commit_cell(
                &ErrorCell {
                    message: format!("no se pudo cambiar a {spec}: {message}"),
                    theme: self.theme,
                },
                terminal,
            ),
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
        // them at all (see `slash_commands`'s doc comment). Only the
        // first whitespace-delimited token needs to match a registered
        // command name (not the whole string) so a recognized command
        // followed by trailing text (e.g. "/quit ahora") is still
        // recognized as that command instead of falling through to be
        // sent to the model as ordinary text.
        if let Some(rest) = trimmed.strip_prefix('/')
            && let Some((command, args)) = crate::slash_commands::parse_slash_command(rest)
        {
            self.composer = TextArea::default();
            self.popup = None;
            return self.run_slash_command(command, args, terminal);
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
            let _ = tx.send(TuiUpdate::TurnFinished(
                result.map_err(|err| err.to_string()),
            ));
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
        // Same for pending `ask_user` questions: anything still queued
        // belongs to the turn just abandoned — answer "no answer" (the
        // provider's honest no-guess outcome), and reset the selection
        // cursor for whatever the next turn may ask.
        for request in self.pending_questions.drain(..) {
            let _ = request.respond.send(None);
        }
        self.question_selected = 0;

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

    /// Answers the front pending `ask_user` question and commits a
    /// `QuestionCell` recording the exchange — the question sibling of
    /// `answer_pending_approval`. `choice` is the 0-based option index,
    /// or `None` when the user declined (Esc). The selection cursor
    /// resets for whatever question is revealed next.
    fn answer_pending_question(
        &mut self,
        choice: Option<usize>,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        let Some(request) = self.pending_questions.pop_front() else {
            return Ok(());
        };
        self.question_selected = 0;
        let answer_text = choice.and_then(|i| request.options.get(i).cloned());
        let question = request.question.clone();
        // Best-effort send, same as approvals: the awaiting `ask()` may
        // already be gone if its turn was interrupted.
        let _ = request.respond.send(choice);
        self.commit_cell(
            &QuestionCell {
                question,
                answer: answer_text,
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
    async fn expand_last_tool_call(
        &mut self,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
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
            TuiUpdate::Event(AgentEvent::PlanCreated { plan }) => {
                let theme = self.theme;
                self.commit_cell(&PlanCell { plan, theme }, terminal)?;
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
            TuiUpdate::Event(AgentEvent::HarnessNote { kind, text }) => {
                // J-26 (docs/AUDITORIA-2026-07-v7.md): this event IS
                // rendered into the model's history — the user should
                // see what the harness told the model, not just the
                // model's reaction to it.
                let theme = self.theme;
                self.commit_cell(&HarnessNoteCell { kind, text, theme }, terminal)?;
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
                // N-30 (docs/AUDITORIA-2026-07-v2.md): a round that fails
                // mid-stream never gets an `AgentEvent::AssistantText`
                // (A3/B4 — the engine never persists partial text as a
                // final answer), so the collector's tail is never
                // flushed via that path the way a successful round's is.
                // Without this, whatever was streamed before the error
                // stays stuck in the 2-row live preview forever — not in
                // the transcript, and not visibly connected to the error
                // cell about to be committed below.
                if let Some(tail) = self.markdown.finish() {
                    self.commit_cell(&AssistantMarkdownCell { markdown: tail }, terminal)?;
                }
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
    fn commit_cell(
        &self,
        cell: &dyn HistoryCell,
        terminal: &mut Terminal<Backend>,
    ) -> Result<(), TuiError> {
        let width = terminal.size()?.width;
        let paragraph = Paragraph::new(cell.as_text()).wrap(Wrap { trim: false });
        // N-32 (docs/AUDITORIA-2026-07-v2.md): see `clamp_height`'s doc
        // comment — reachable in practice via the markdown fence-gating
        // in `markdown_stream.rs` committing an unclosed trailing fence
        // as one atomic chunk on `finish()`: a model dumping a multi-MB
        // fenced block gets here.
        let height = clamp_height(paragraph.line_count(width));
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

        // A pending approval takes precedence over a pending question
        // (see `pending_questions`'s doc comment) — the question's
        // overlay only shows once no approval is in front.
        let front_question = self
            .pending_approvals
            .is_empty()
            .then(|| self.pending_questions.front())
            .flatten();

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
        } else if let Some(request) = front_question {
            // `ask_user` options list — same rows the `/`/`@` popup
            // reuses (a question only arrives mid-turn, when no popup
            // can be open and the live preview has already flushed its
            // round's text). The question itself renders in the composer
            // slot below.
            let options_area = Rect {
                x: active_area.x,
                y: active_area.y,
                width: active_area.width,
                height: active_area.height + hint_area.height,
            };
            draw_question_options(frame, options_area, request, self.question_selected);
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
                "esperando tu decisión... (y permitir · n/Esc denegar)".to_string()
            } else if self.switching_model {
                format!("{} cambiando de modelo... (Ctrl+C salir)", self.spinner_glyph())
            } else if self.turn_running {
                format!(
                    "{} esperando respuesta del modelo... (Ctrl+C salir · Esc interrumpir)",
                    self.spinner_glyph()
                )
            } else {
                "Enter enviar · Ctrl+J salto de linea · / comandos · @ archivos · Ctrl+T output · Ctrl+C salir"
                    .to_string()
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
            let mut answer_hint =
                "esta acción puede ser irreversible — y permitir · n/Esc denegar".to_string();
            if self.pending_approvals.len() > 1 {
                answer_hint.push_str(&format!("  ({} pendientes)", self.pending_approvals.len()));
            }
            // Same bordered treatment as the composer itself (`App::new`'s
            // `TextArea::set_block`) — this area is the same slot, so it
            // should read as "the same input area, showing something
            // else right now" rather than switching to an unbordered
            // look just because a `TextArea` isn't what's rendering.
            let border = Block::default().borders(Borders::TOP | Borders::BOTTOM);
            let inner_area = border.inner(composer_area);
            frame.render_widget(border, composer_area);
            // N-31 (docs/AUDITORIA-2026-07-v2.md): `inner_area` is a fixed
            // few rows tall — a long or multi-line description (e.g. a
            // multi-line `shell_exec` command) can wrap past it, silently
            // pushing the y/n hint line off-screen with no indication the
            // user is approving something they can't fully see. Reserve
            // the last row for the hint always, and cap the description
            // to what provably fits in the rest (word-wrapping can only
            // use *fewer* rows than this char-budget estimate, never
            // more), with a visible "…" marker if anything had to be cut.
            let available_rows_for_description =
                usize::from(inner_area.height.saturating_sub(1).max(1));
            let max_chars = available_rows_for_description
                .saturating_mul(usize::from(inner_area.width.max(1)));
            let description = truncate_for_display(&request.description, max_chars);
            let lines = vec![
                Line::from(description),
                Line::from(answer_hint).style(Style::default().fg(self.theme.warning)),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner_area);
        } else if let Some(request) = front_question {
            // The question itself, in the same bordered composer slot the
            // approval overlay uses (same "the input area, showing
            // something else right now" reasoning) — its options render
            // above, in `draw_question_options`. Same N-31 protections as
            // the approval overlay: the hint row is always reserved, and
            // the question is capped to what provably fits above it.
            let border = Block::default().borders(Borders::TOP | Borders::BOTTOM);
            let inner_area = border.inner(composer_area);
            frame.render_widget(border, composer_area);
            let available_rows_for_question =
                usize::from(inner_area.height.saturating_sub(1).max(1));
            let max_chars = available_rows_for_question
                .saturating_mul(usize::from(inner_area.width.max(1)))
                .saturating_sub(2); // the "? " marker's own columns
            let question = truncate_for_display(&request.question, max_chars);
            let hint = format!(
                "↑↓ o 1-{} elegir · Enter responder · Esc no responder",
                request.options.len()
            );
            let lines = vec![
                Line::from(format!("? {question}")),
                Line::from(hint).style(Style::default().fg(self.theme.warning)),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner_area);
        } else {
            frame.render_widget(&self.composer, composer_area);
        }
    }
}

/// Renders the front pending `ask_user` question's options into `area` —
/// numbered (matching the '1'..='4' direct-pick keys), selection
/// reversed, windowed around the selection the same way the `/model`
/// picker windows (4 options don't fit the 3-row budget). A free
/// function for the same reason as `draw_popup`: it needs only the
/// request and selection, not the rest of `App`.
fn draw_question_options(
    frame: &mut ratatui::Frame,
    area: Rect,
    request: &QuestionRequest,
    selected: usize,
) {
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let total = request.options.len();
    let start = popup_window_start(selected, total, POPUP_MAX_VISIBLE);
    let lines: Vec<Line> = request
        .options
        .iter()
        .enumerate()
        .skip(start)
        .take(POPUP_MAX_VISIBLE)
        .map(|(i, option)| {
            let style = if i == selected {
                selected_style
            } else {
                Style::default()
            };
            // The `(i/total)` marker only appears when the list can't
            // fully fit — same signal as the `/model` picker's.
            let marker = if total > POPUP_MAX_VISIBLE {
                format!("  ({}/{})", i + 1, total)
            } else {
                String::new()
            };
            Line::from(Span::styled(format!("{}. {option}{marker}", i + 1), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Renders the `/`/`@` suggestion list into `area` — a free function
/// (not a method) since it only needs `popup`, not the rest of `App`.
fn draw_popup(frame: &mut ratatui::Frame, area: Rect, popup: &ComposerPopup) {
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);

    let lines: Vec<Line> = match popup {
        ComposerPopup::Slash {
            matches, selected, ..
        } => matches
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
        ComposerPopup::Mention {
            matches, selected, ..
        } => matches
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
        ComposerPopup::Backtrack { messages, selected } => messages
            .iter()
            .enumerate()
            .map(|(i, (_, text))| {
                let style = if i == *selected {
                    selected_style
                } else {
                    Style::default()
                };
                Line::from(Span::styled(backtrack_preview(text), style))
            })
            .collect(),
        ComposerPopup::Model { specs, selected } => {
            // Windows over the full candidate list (see
            // `ComposerPopup::Model`'s doc comment) — the selection can
            // walk past the 3-row budget, and the visible slice follows.
            let start = popup_window_start(*selected, specs.len(), POPUP_MAX_VISIBLE);
            specs
                .iter()
                .enumerate()
                .skip(start)
                .take(POPUP_MAX_VISIBLE)
                .map(|(i, spec)| {
                    let style = if i == *selected {
                        selected_style
                    } else {
                        Style::default()
                    };
                    // The `(i+1)/total` marker is what tells the user
                    // there's more above/below the 3 visible rows.
                    Line::from(Span::styled(
                        format!("⇄ {spec}  ({}/{})", i + 1, specs.len()),
                        style,
                    ))
                })
                .collect()
        }
    };

    frame.render_widget(Paragraph::new(lines), area);
}

/// First visible index for a `visible`-row window over a `len`-item list
/// keeping `selected` in view: the window sticks to the top until the
/// selection walks past it, then follows so the selection sits on the
/// last row, and never starts so late that fewer than `visible` items
/// remain when the list has at least that many.
fn popup_window_start(selected: usize, len: usize, visible: usize) -> usize {
    selected
        .saturating_sub(visible.saturating_sub(1))
        .min(len.saturating_sub(visible))
}

/// The pure cycling logic behind `App::spinner_glyph` — `frame` is
/// `App.spinner_frame`, an ever-incrementing counter; this is what
/// actually wraps it into a valid `SPINNER_FRAMES` index.
fn spinner_glyph_at(frame: usize) -> char {
    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
}

/// Longest single line shown per entry in the backtrack popup before
/// truncating — a message's own first line stands in for the whole
/// thing, same "keep it scannable" rationale as
/// `history_cell::summarize_tool_output`'s ~80-char cap.
const BACKTRACK_PREVIEW_MAX_CHARS: usize = 70;

fn backtrack_preview(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let truncated = first_line.chars().count() > BACKTRACK_PREVIEW_MAX_CHARS;
    let mut preview: String = first_line
        .chars()
        .take(BACKTRACK_PREVIEW_MAX_CHARS)
        .collect();
    if truncated {
        preview.push('…');
    }
    format!("↩ {preview}")
}

/// Clamps a wrapped line count into the `u16` range `Terminal::insert_before`
/// needs, saturating instead of truncating (N-32,
/// docs/AUDITORIA-2026-07-v2.md). `usize as u16` is a bare truncating
/// cast — applying it to a count of exactly 65536 (a multiple of
/// `u16::MAX + 1`) silently produces `0` (`insert_before(0, ...)` inserts
/// nothing at all, dropping the whole cell), and any higher count wraps
/// to some other small number instead of clamping. `clamp` runs entirely
/// in `usize` before the cast, so it's lossless in both directions.
fn clamp_height(line_count: usize) -> u16 {
    line_count.clamp(1, usize::from(u16::MAX)) as u16
}

/// Caps `text` to at most `max_chars` terminal display *columns*,
/// appending a visible "…" marker if anything had to be cut — N-31,
/// docs/AUDITORIA-2026-07-v2.md. Budgets by display width
/// (`unicode_width`), not `chars().count()` (bajo,
/// docs/AUDITORIA-2026-07-v2.md, "truncación por char-count vs. ancho de
/// display (CJK)"): a CJK/emoji character occupies ~2 columns, so a
/// char-count budget could let the result overflow the terminal width
/// this is meant to fit — exactly the y/n approval hint line N-31 exists
/// to keep visible.
fn truncate_for_display(text: &str, max_chars: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    if text.width() <= max_chars {
        return text.to_string();
    }
    let budget = max_chars.saturating_sub(1); // leave room for the marker itself
    let mut truncated = String::new();
    let mut width_so_far = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if width_so_far + w > budget {
            break;
        }
        width_so_far += w;
        truncated.push(c);
    }
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- spinner_glyph_at (comparación contra el cookbook de OpenRouter,
    // docs/usability-log-2026-07-07-si2.md) ---

    #[test]
    fn spinner_glyph_at_zero_is_the_first_frame() {
        assert_eq!(spinner_glyph_at(0), SPINNER_FRAMES[0]);
    }

    #[test]
    fn spinner_glyph_at_wraps_around_after_the_last_frame() {
        let len = SPINNER_FRAMES.len();
        assert_eq!(spinner_glyph_at(len), SPINNER_FRAMES[0]);
        assert_eq!(spinner_glyph_at(len + 1), SPINNER_FRAMES[1]);
    }

    /// `App.spinner_frame` only ever grows (`wrapping_add` on every
    /// tick, never reset) — this pins that a large, multi-cycle value
    /// still lands on a valid index instead of panicking.
    #[test]
    fn spinner_glyph_at_handles_a_large_frame_count() {
        let len = SPINNER_FRAMES.len();
        assert_eq!(spinner_glyph_at(len * 137 + 3), SPINNER_FRAMES[3]);
    }

    #[test]
    fn backtrack_preview_shows_only_the_first_line() {
        assert_eq!(backtrack_preview("hola\nmundo"), "↩ hola");
    }

    #[test]
    fn backtrack_preview_truncates_a_long_first_line() {
        let long_line = "a".repeat(BACKTRACK_PREVIEW_MAX_CHARS + 10);
        let preview = backtrack_preview(&long_line);
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= BACKTRACK_PREVIEW_MAX_CHARS + "↩ ".chars().count() + 1);
    }

    #[test]
    fn backtrack_preview_leaves_a_short_message_untouched() {
        assert_eq!(backtrack_preview("hola"), "↩ hola");
    }

    /// Regression test for N-31 (docs/AUDITORIA-2026-07-v2.md): a
    /// description within the budget is left untouched.
    #[test]
    fn truncate_for_display_leaves_a_short_string_untouched() {
        assert_eq!(truncate_for_display("hola", 10), "hola");
    }

    /// The truncated result must never exceed `max_chars` (the whole
    /// point — this is what guarantees the y/n hint line always fits in
    /// the remaining row), and must visibly mark that it was cut.
    #[test]
    fn truncate_for_display_caps_at_max_chars_with_a_visible_marker() {
        let long = "x".repeat(100);
        let truncated = truncate_for_display(&long, 10);
        assert_eq!(truncated.chars().count(), 10);
        assert!(truncated.ends_with('…'));
    }

    /// Character-count, not byte-length — must not panic or split a
    /// multi-byte codepoint even when the budget lands mid-character.
    #[test]
    fn truncate_for_display_is_utf8_safe() {
        let text = "é".repeat(20); // each 'é' is 2 bytes in UTF-8
        let truncated = truncate_for_display(&text, 5);
        assert_eq!(truncated.chars().count(), 5);
        assert!(truncated.ends_with('…'));
    }

    /// Regression test for the "truncación por char-count vs. ancho de
    /// display (CJK)" bajo (docs/AUDITORIA-2026-07-v2.md): each CJK
    /// character occupies 2 display columns, so a budget in columns must
    /// keep far fewer *characters* than the same budget in a Latin
    /// string would.
    #[test]
    fn truncate_for_display_budgets_by_display_width_not_char_count() {
        use unicode_width::UnicodeWidthStr;

        let cjk = "文".repeat(20); // each char is 2 columns wide
        let truncated = truncate_for_display(&cjk, 10);
        assert!(
            truncated.width() <= 10,
            "expected the result to fit within 10 display columns, got width {} ({truncated:?})",
            truncated.width()
        );
        // 9 columns of budget (10 minus the marker's 1 column) / 2
        // columns per char = 4 full characters, plus the marker.
        assert_eq!(truncated, "文文文文…");
    }

    /// Regression test for N-32 (docs/AUDITORIA-2026-07-v2.md): a bare
    /// `as u16` cast on exactly 65536 (a multiple of `u16::MAX + 1`)
    /// truncates to `0` — the exact silent-content-drop bug — instead of
    /// clamping to `u16::MAX`.
    #[test]
    fn clamp_height_saturates_instead_of_wrapping_to_zero() {
        assert_eq!(clamp_height(65_536), u16::MAX);
        assert_eq!(clamp_height(65_537), u16::MAX);
        assert_eq!(clamp_height(usize::from(u16::MAX)), u16::MAX);
    }

    #[test]
    fn clamp_height_never_returns_zero() {
        assert_eq!(clamp_height(0), 1);
    }

    #[test]
    fn clamp_height_leaves_a_normal_count_untouched() {
        assert_eq!(clamp_height(42), 42);
    }

    #[test]
    fn popup_window_sticks_to_the_top_until_the_selection_walks_past_it() {
        assert_eq!(popup_window_start(0, 10, 3), 0);
        assert_eq!(popup_window_start(1, 10, 3), 0);
        assert_eq!(popup_window_start(2, 10, 3), 0);
    }

    #[test]
    fn popup_window_follows_a_selection_below_the_visible_rows() {
        // Selection sits on the window's last row as it walks down.
        assert_eq!(popup_window_start(3, 10, 3), 1);
        assert_eq!(popup_window_start(9, 10, 3), 7);
    }

    #[test]
    fn popup_window_never_starts_past_len_minus_visible() {
        // A list shorter than the window always renders from the top.
        assert_eq!(popup_window_start(1, 2, 3), 0);
        assert_eq!(popup_window_start(0, 0, 3), 0);
    }
}
