//! The chat column: the selected draft's conversation with its pi agent,
//! drawn on gpui's `list()` with tool calls as compact rows, and the composer.
//!
//! Each draft gets its own [`Agent`], started when the draft is selected
//! and stopped once it is idle or left. The chat reports to its owner
//! through [`ChatEvent`]: the owner saves the draft before a prompt, reloads
//! it after the agent writes it, and locks the editor while the agent runs.
//!
//! A non-empty selection in the draft editor is attached to the next
//! message: a chip in the composer's footer shows it, and sending puts it in
//! the prompt ([`quote`]). It stays attached while the composer has focus, since
//! the editor keeps its selection when it loses focus. Sending or the chip's
//! × detaches it, and selecting something else attaches that.

mod quote;
mod transcript;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use draftmux_pi_rpc::{Agent, AgentEvent, Disposition, LaunchSpec};
use draftmux_storage::{Draft, SourceKind};
use editor::{Editor, EditorEvent, EditorStyle};
use futures::StreamExt;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FollowMode, FontWeight, HighlightStyle, Hsla, KeyBinding, ListAlignment, ListState,
    StyledText, Task, Transformation, Window, actions, div, ease_out_quint, font, list, percentage,
    prelude::*, px, relative, svg,
};
use shell_env::ShellEnv;

use crate::assets::{
    CHEVRON_ICON, CLOSE_ICON, PI_ICON, QUOTE_ICON, SEND_ICON, SPINNER_ARC_ICON, SPINNER_TRACK_ICON,
    STOP_ICON, TOOL_BASH_ICON, TOOL_EDIT_ICON, TOOL_OTHER_ICON, TOOL_READ_ICON, TOOL_WRITE_ICON,
};
use crate::bounce::{Bounce, ListBounce};
use crate::chat_model::ChatModel;
use crate::chrome::shadow;
use crate::markup_view::{self, MarkupStyle, SelectBody};
use crate::profiles::ProfileCatalog;
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::selectable::{self, Scroller, TextSelection};
use crate::theme::{self, MONO, Palette, Theme};
use quote::Quote;
use transcript::{DiffLine, Item, Tool, Transcript};

actions!(chat, [Send, Stop]);

/// How long an idle pi process is kept.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long pi gets to answer a request. pi answers a prompt only after
/// compacting and running input hooks or an extension command.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// How long the draft's selection must stay put before the composer's chip
/// shows it, so dragging a selection doesn't flicker the chip.
const CHIP_SETTLE: Duration = Duration::from_millis(250);
const KEY_CONTEXT: &str = "Composer";
/// How long a newly arrived item takes to fade in and rise into place.
const ENTRANCE: Duration = Duration::from_millis(200);
/// How far below its place a newly arrived item starts.
const ENTRANCE_RISE: f32 = 6.;
/// How long a tool row takes to settle when its tool finishes or it is
/// expanded or collapsed.
const TOOL_SETTLE: Duration = Duration::from_millis(150);
/// How long the send button takes to change colour.
const SEND_FADE: Duration = Duration::from_millis(150);
/// How long "Jump to latest" takes to fade in and rise into place.
const JUMP_FADE: Duration = Duration::from_millis(150);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // Later bindings win at the same depth, so this replaces the
        // editor's newline; shift-enter still inserts one.
        KeyBinding::new("enter", Send, Some("Composer > Editor")),
        KeyBinding::new("escape", Stop, Some(KEY_CONTEXT)),
    ]);
}

pub enum ChatEvent {
    /// The user sent `text`, the prompt with any attached selection. Save
    /// the draft, then pass both to [`Chat::prompt`].
    Send(String),
    /// The agent wrote draft `id`, selected or not; `text` is its new text.
    DraftWritten { id: String, text: String },
    /// The selected draft's agent started or finished a run.
    Running(bool),
    /// The selected draft was handed off and its agent is ready: record
    /// that the first draft was requested, then send `text` like
    /// [`ChatEvent::Send`] unless the draft already has text.
    FirstDraft(String),
}

impl EventEmitter<ChatEvent> for Chat {}

pub struct Chat {
    sessions: HashMap<String, Session>,
    current: Option<String>,
    /// Selection across the agent's messages.
    text_selection: Entity<TextSelection>,
    composer: Entity<Editor>,
    /// The draft editor, whose selection is attached to the next message.
    draft_editor: Entity<Editor>,
    /// The selection the user detached; the selection follows text edits,
    /// so this does too.
    detached: Option<Range<usize>>,
    /// The draft selection the composer's chip shows, which follows the
    /// editor's once it settles.
    chip: Range<usize>,
    settle_chip: Task<()>,
    /// The send button's state and the one it is changing from.
    send_state: Cell<(SendState, SendState)>,
    style: MarkupStyle,
}

#[derive(Clone, Copy, PartialEq)]
enum SendState {
    Disabled,
    Ready,
    Busy,
}

impl SendState {
    fn background(self, palette: &Palette) -> Hsla {
        match self {
            Self::Disabled => palette.send_disabled,
            Self::Ready => palette.accent,
            Self::Busy => palette.send_busy,
        }
    }
}

struct Session {
    draft: Draft,
    /// `Err` says why the draft can't talk to pi.
    agent: Option<Result<Arc<Agent>, String>>,
    transcript: Transcript,
    list: ListState,
    /// The transcript's rubber band at its ends.
    bounce: Rc<RefCell<Bounce>>,
    scrollbar: ScrollbarState,
    /// Shows and switches the model of the running agent.
    model: Option<Entity<ChatModel>>,
    history_loaded: bool,
    /// Whether the requested history arrived, or won't.
    history_ready: bool,
    /// Read-only editors that make the user's messages selectable, by
    /// transcript index, with the text each was made for.
    bubbles: HashMap<usize, (String, Entity<Editor>)>,
    /// When items that arrived while the session was shown started their
    /// entrance, by transcript index. Restored history and items that
    /// arrived unseen appear at once.
    entering: HashMap<usize, Instant>,
    /// When tools finished while the session was shown, by transcript index;
    /// their result icon and stats fade in.
    finished: HashMap<usize, Instant>,
    /// When tool rows were expanded or collapsed, by transcript index; their
    /// chevron turns.
    turned: HashMap<usize, Instant>,
    /// Whether the list ends with a placeholder awaiting a visible reply.
    activity_row: bool,
    /// The composer text left unsent when another draft was selected.
    unsent: String,
    _events: Task<()>,
}

impl Session {
    /// Whether pi is still starting or the earlier conversation is still
    /// loading. A pi that can't start isn't starting.
    fn starting(&self) -> bool {
        match &self.agent {
            None => true,
            Some(Ok(_)) => !self.history_ready,
            Some(Err(_)) => false,
        }
    }
}

impl Focusable for Chat {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Chat {
    pub fn new(draft_editor: Entity<Editor>, cx: &mut Context<Self>) -> Self {
        let composer = theme::editor(composer_style, cx);
        cx.observe(&composer, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&draft_editor, |this, editor, event, cx| {
            match event {
                EditorEvent::Edited => {
                    let range = editor.read(cx).selected_range();
                    if this.detached.is_some() {
                        this.detached = Some(range.clone());
                    }
                    this.chip = range;
                    this.settle_chip = Task::ready(());
                }
                EditorEvent::SelectionChanged(range) => {
                    if this.detached.as_ref() != Some(range) {
                        this.detached = None;
                    }
                    let range = range.clone();
                    this.settle_chip = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(CHIP_SETTLE).await;
                        this.update(cx, |this, cx| {
                            this.chip = range;
                            cx.notify();
                        })
                        .ok();
                    });
                }
            }
            cx.notify();
        })
        .detach();
        cx.observe_global::<Theme>(|this, cx| this.style = message_style(theme::get(cx).dark))
            .detach();
        let text_selection = cx.new(TextSelection::new);
        cx.observe(&text_selection, |_, _, cx| cx.notify()).detach();
        Self {
            sessions: HashMap::new(),
            current: None,
            text_selection,
            composer,
            draft_editor,
            detached: None,
            chip: 0..0,
            settle_chip: Task::ready(()),
            send_state: Cell::new((SendState::Disabled, SendState::Disabled)),
            style: message_style(theme::get(cx).dark),
        }
    }

    fn session(&self) -> Option<&Session> {
        self.sessions.get(self.current.as_deref()?)
    }

    fn busy(&self) -> bool {
        self.session()
            .is_some_and(|session| session.transcript.busy)
    }

    /// Shows `draft`'s conversation and starts its pi; `None` shows nothing.
    pub fn set_draft(&mut self, draft: Option<&Draft>, cx: &mut Context<Self>) {
        let id = draft.map(|draft| draft.id.clone());
        if id == self.current {
            return;
        }
        if let Some(Some(Ok(agent))) = self.session().map(|session| &session.agent) {
            agent.blur();
        }
        let unsent = self.composer.read(cx).text().to_owned();
        if let Some(session) = self
            .current
            .as_deref()
            .and_then(|current| self.sessions.get_mut(current))
        {
            session.unsent = unsent;
        }
        self.current = id.clone();
        self.text_selection
            .update(cx, |selection, cx| selection.reset(cx));
        if let (Some(id), Some(draft)) = (id, draft) {
            match self.sessions.get_mut(&id) {
                Some(session) => {
                    session.draft = draft.clone();
                    self.focus_agent(&id, cx);
                }
                None => {
                    // Measured up front so the rubber band stretches at the
                    // real ends, not where measuring has got to.
                    let list = ListState::new(0, ListAlignment::Bottom, px(600.)).measure_all();
                    list.set_follow_mode(FollowMode::Tail);
                    let chat = cx.entity().downgrade();
                    list.set_scroll_handler(move |_, _, cx| {
                        chat.update(cx, |_, cx| cx.notify()).ok();
                    });
                    self.sessions.insert(
                        id.clone(),
                        Session {
                            draft: draft.clone(),
                            agent: None,
                            transcript: Transcript::default(),
                            list,
                            bounce: Rc::default(),
                            scrollbar: ScrollbarState::default(),
                            model: None,
                            history_loaded: false,
                            history_ready: false,
                            bubbles: HashMap::new(),
                            entering: HashMap::new(),
                            finished: HashMap::new(),
                            turned: HashMap::new(),
                            activity_row: false,
                            unsent: String::new(),
                            _events: Task::ready(()),
                        },
                    );
                    self.start_agent(&id, cx);
                }
            }
        }
        let unsent = self
            .session()
            .map(|session| session.unsent.clone())
            .unwrap_or_default();
        self.composer.update(cx, |composer, cx| {
            composer.set_text(&unsent, cx);
            composer.select_range(unsent.len()..unsent.len(), cx);
        });
        let scroller = self
            .session()
            .map(|session| Scroller::List(session.list.clone()));
        self.text_selection
            .update(cx, |selection, _| selection.set_scroller(scroller));
        cx.emit(ChatEvent::Running(self.busy()));
        cx.notify();
    }

    /// Creates the draft's agent once the shell environment is known, then
    /// focuses it if the draft is still selected.
    fn start_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let ready = ShellEnv::ready(cx);
        let id = id.to_owned();
        session._events = cx.spawn(async move |this, cx| {
            let env = ready.await;
            let Some(mut events) = this
                .update(cx, |this, cx| this.create_agent(&id, &env, cx))
                .ok()
                .flatten()
            else {
                return;
            };
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |this, cx| this.on_event(&id, event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    fn create_agent(
        &mut self,
        id: &str,
        env: &Arc<ShellEnv>,
        cx: &mut Context<Self>,
    ) -> Option<futures::channel::mpsc::UnboundedReceiver<AgentEvent>> {
        let composer = self.composer.focus_handle(cx);
        let session = self.sessions.get_mut(id)?;
        let draft = &session.draft;
        let spec = (|| {
            let pi = env.pi().map_err(|err| err.to_string())?.to_path_buf();
            let session_file = draft
                .meta
                .pi_session_file
                .clone()
                .ok_or("This draft has no pi session yet.")?;
            let extension = crate::resources::pi_extension().map_err(|err| format!("{err:#}"))?;
            let env = env.clone();
            Ok::<_, String>(LaunchSpec {
                command: Arc::new(move || env.command(&pi)),
                session_file,
                extension,
                cwd: draft.meta.cwd.clone(),
                agent_dir: draft.meta.pi_agent_dir.clone(),
                draft_path: draft.draft_path(),
                instructions: draft.meta.settings.instructions.clone(),
                context_files: draft.meta.settings.context_files,
            })
        })();
        let (agent, events) = match spec {
            Ok(spec) => {
                let (agent, events) = Agent::new(spec, IDLE_TIMEOUT, REQUEST_TIMEOUT);
                (Ok(Arc::new(agent)), Some(events))
            }
            Err(err) => (Err(err), None),
        };
        session.model = agent.as_ref().ok().map(|agent| {
            let dir = &session.draft.meta.pi_agent_dir;
            let profile_name = ProfileCatalog::global(cx)
                .read(cx)
                .entry(dir)
                .map(|entry| entry.profile.name.clone())
                .unwrap_or_else(|| dir.display().to_string());
            let agent = agent.clone();
            cx.new(|_| ChatModel::new(agent, profile_name, composer))
        });
        session.agent = Some(agent);
        if self.current.as_deref() == Some(id) {
            self.focus_agent(id, cx);
        }
        self.sync_list(id, cx);
        events
    }

    /// Starts the selected draft's pi and, the first time, loads its
    /// history.
    fn focus_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let Some(Ok(agent)) = &session.agent else {
            return;
        };
        let agent = agent.clone();
        if let Err(err) = agent.focus() {
            session.history_ready = true;
            session.transcript.send_failed(err.to_string());
            self.sync_list(id, cx);
            return;
        }
        if !session.history_loaded {
            session.history_loaded = true;
            // Requested before any prompt can be sent, so the history pi
            // returns doesn't include it.
            let messages = agent.get_messages();
            let id = id.to_owned();
            cx.spawn(async move |this, cx| {
                let messages = messages.await;
                this.update(cx, |this, cx| this.history_loaded(&id, messages, cx))
                    .ok();
            })
            .detach();
        }
        self.request_first_draft(id, cx);
    }

    /// Asks the owner to send the first-draft request for a handed-off
    /// draft whose agent is ready.
    fn request_first_draft(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.current.as_deref() != Some(id) {
            return;
        }
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        if !session.draft.meta.first_draft_pending || session.transcript.busy {
            return;
        }
        session.draft.meta.first_draft_pending = false;
        cx.emit(ChatEvent::FirstDraft(first_draft_prompt(
            session.draft.meta.note.as_deref(),
        )));
    }

    fn history_loaded(
        &mut self,
        id: &str,
        messages: Result<Vec<serde_json::Value>, draftmux_pi_rpc::Error>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        session.history_ready = true;
        let messages = match messages {
            Ok(messages) => messages,
            Err(err) => {
                eprintln!("draftmux: loading the chat history: {err}");
                self.sync_list(id, cx);
                return;
            }
        };
        let (mut items, earlier) = transcript::history(&messages);
        if earlier > 0
            && let Some(source) = &session.draft.meta.source
        {
            let source = match source.kind {
                SourceKind::Pi => "pi",
                SourceKind::Claude => "Claude Code",
            };
            let plural = if earlier == 1 { "" } else { "s" };
            items.insert(
                0,
                Item::Notice(format!(
                    "Continues the {source} session ({earlier} earlier message{plural})"
                )),
            );
        }
        session.transcript.prepend(items);
        // Prepending moves every item to a new index.
        session.bubbles.clear();
        session.entering.clear();
        session.finished.clear();
        session.turned.clear();
        self.sync_list(id, cx);
    }

    /// Sends `text` to the selected draft's agent. `saved` is the draft
    /// text as saved for pi, or why saving failed.
    pub fn prompt(&mut self, text: String, saved: Result<String, String>, cx: &mut Context<Self>) {
        let Some(id) = self.current.clone() else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let agent = match (&session.agent, saved) {
            (_, Err(err)) => Err(format!("Couldn't save the draft before sending: {err}")),
            (Some(Ok(agent)), Ok(saved)) => {
                session.transcript.sent(text.clone(), saved);
                Ok(agent.clone())
            }
            (Some(Err(err)), Ok(_)) => Err(err.clone()),
            (None, Ok(_)) => Err("pi is still starting.".to_owned()),
        };
        match agent {
            Ok(agent) => {
                let reply = agent.prompt(text);
                let id = id.clone();
                cx.spawn(async move |this, cx| {
                    let reply = reply.await;
                    this.update(cx, |this, cx| {
                        let Some(session) = this.sessions.get_mut(&id) else {
                            return;
                        };
                        match reply {
                            Ok(Disposition::Handled) => session.transcript.handled(),
                            Ok(_) => return,
                            Err(err) => session.transcript.send_failed(err.to_string()),
                        }
                        this.after_change(&id, cx);
                    })
                    .ok();
                })
                .detach();
            }
            Err(err) => {
                session.transcript.sent(text, String::new());
                session.transcript.send_failed(err);
            }
        }
        self.after_change(&id, cx);
    }

    /// Puts `text` in the composer for the user to send, unless they have
    /// started a message there.
    pub fn offer(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.composer.read(cx).text().trim().is_empty() {
            return;
        }
        self.composer.update(cx, |composer, cx| {
            composer.set_text(text, cx);
            composer.select_range(text.len()..text.len(), cx);
        });
    }

    /// The draft editor's selection, unless the user detached it.
    fn attached(&self, cx: &App) -> Option<Quote> {
        self.quote(self.draft_editor.read(cx).selected_range(), cx)
    }

    /// `range` of the draft, unless the user detached it.
    fn quote(&self, range: Range<usize>, cx: &App) -> Option<Quote> {
        self.session()?;
        if self.detached.as_ref() == Some(&range) {
            return None;
        }
        Quote::new(self.draft_editor.read(cx).text(), range)
    }

    fn detach(&mut self, cx: &mut Context<Self>) {
        let range = self.draft_editor.read(cx).selected_range();
        self.detached = Some(range.clone());
        self.chip = range;
        self.settle_chip = Task::ready(());
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_owned();
        if text.is_empty() || self.busy() || self.session().is_none() {
            return;
        }
        self.composer
            .update(cx, |editor, cx| editor.set_text("", cx));
        // The message took the selection; the editor keeps it selected.
        let quote = self.attached(cx);
        if quote.is_some() {
            self.detach(cx);
        }
        cx.emit(ChatEvent::Send(quote::prompt(&text, quote.as_ref())));
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(Some(Ok(agent))) = self.session().map(|session| &session.agent) else {
            return;
        };
        if !self.busy() {
            return;
        }
        let abort = agent.abort();
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = abort.await {
                    eprintln!("draftmux: stopping the agent: {err}");
                }
            })
            .detach();
    }

    fn on_event(&mut self, id: &str, event: AgentEvent, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let written = match &event {
            AgentEvent::Pi(pi) if pi.is_draft_write() => {
                match std::fs::read_to_string(session.draft.draft_path()) {
                    Ok(text) => Some(text),
                    Err(err) => {
                        eprintln!("draftmux: reading the agent's edit: {err}");
                        None
                    }
                }
            }
            _ => None,
        };
        if let (AgentEvent::Ready(state), Some(model)) = (&event, &session.model) {
            model.update(cx, |model, cx| model.set_state(state.clone(), cx));
        }
        let shown = self.current.as_deref() == Some(id);
        let running: Vec<usize> = if shown {
            running_tools(&session.transcript).collect()
        } else {
            Vec::new()
        };
        let now = Instant::now();
        session.transcript.apply(&event, written.as_deref(), now);
        if !running.is_empty() {
            let still: Vec<usize> = running_tools(&session.transcript).collect();
            session
                .finished
                .retain(|_, start| now.saturating_duration_since(*start) < TOOL_SETTLE);
            for index in running.into_iter().filter(|index| !still.contains(index)) {
                session.finished.insert(index, now);
            }
        }
        if let Some(text) = written {
            cx.emit(ChatEvent::DraftWritten {
                id: id.to_owned(),
                text,
            });
        }
        self.after_change(id, cx);
    }

    /// Updates the list and the owner after `id`'s transcript changed.
    fn after_change(&mut self, id: &str, cx: &mut Context<Self>) {
        self.enter_new_items(id);
        self.sync_list(id, cx);
        if self.current.as_deref() == Some(id) {
            cx.emit(ChatEvent::Running(self.busy()));
        }
    }

    /// Starts the entrance of the items appended since the list last
    /// synced, if `id` is the shown session.
    fn enter_new_items(&mut self, id: &str) {
        if self.current.as_deref() != Some(id) {
            return;
        }
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let shown = session.list.item_count() - usize::from(session.activity_row);
        let now = Instant::now();
        session
            .entering
            .retain(|_, start| now.saturating_duration_since(*start) < ENTRANCE);
        for index in shown..session.transcript.items.len() {
            // Streamed replies replace the waiting slot without sliding or
            // fading; their text already supplies the visible activity.
            if !matches!(session.transcript.items[index], Item::Assistant { .. }) {
                session.entering.insert(index, now);
            }
        }
    }

    fn sync_list(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let old_len = session.list.item_count();
        let old_items = old_len - usize::from(session.activity_row);
        let activity_row = needs_activity_row(session.starting(), &session.transcript);
        let items = session.transcript.items.len();
        let from = match session.transcript.take_change(old_items) {
            Some(from) => Some(from),
            None if activity_row != session.activity_row => Some(old_items),
            None => None,
        };
        if let Some(from) = from {
            // The former last item loses its bottom padding when another
            // row follows it. Remeasure it along with the changed tail.
            let from = if items != old_items || activity_row != session.activity_row {
                from.min(old_items.saturating_sub(1))
            } else {
                from
            };
            session
                .list
                .splice(from..old_len, items - from + usize::from(activity_row));
            // Measures the new items; `measure_all` otherwise only runs once.
            session.list.remeasure_items(from..from);
        }
        session.activity_row = activity_row;
        cx.notify();
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(id) = self.current.clone() else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(&id) {
            session.transcript.toggle(index);
            let now = Instant::now();
            session
                .turned
                .retain(|_, start| now.saturating_duration_since(*start) < TOOL_SETTLE);
            session.turned.insert(index, now);
        }
        self.sync_list(&id, cx);
    }

    fn retry(&mut self, index: usize, cx: &mut Context<Self>) {
        let retry = self
            .session()
            .and_then(|session| session.transcript.retry_at(index))
            .map(str::to_owned);
        if let Some(text) = retry {
            cx.emit(ChatEvent::Send(text));
        }
    }

    /// The selectable text of the user message at `index`.
    fn bubble(
        &mut self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Editor>> {
        let session = self.sessions.get_mut(self.current.as_deref()?)?;
        if let Some((made_for, editor)) = session.bubbles.get(&index)
            && made_for == text
        {
            return Some(editor.clone());
        }
        let editor = theme::editor(bubble_style, cx);
        editor.update(cx, |editor, cx| {
            editor.set_markdown(false, cx);
            editor.set_text(text, cx);
            editor.set_read_only(true, cx);
        });
        session
            .bubbles
            .insert(index, (text.to_owned(), editor.clone()));
        Some(editor)
    }

    fn render_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = *theme::palette(cx);
        if let Some(session) = self.session()
            && index == session.transcript.items.len()
        {
            return activity_row(session, &palette);
        }
        let bubble = match self.session().and_then(|s| s.transcript.items.get(index)) {
            Some(Item::User { text, .. }) => {
                let text = text.clone();
                self.bubble(index, &text, cx)
            }
            _ => None,
        };
        let Some(session) = self.session() else {
            return div().into_any_element();
        };
        let items = &session.transcript.items;
        let Some(item) = items.get(index) else {
            return div().into_any_element();
        };
        // Consecutive tool rows stack without a gap.
        let stacked =
            matches!(item, Item::Tool(_)) && index > 0 && matches!(items[index - 1], Item::Tool(_));
        let body = match item {
            Item::User { text, quote } => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.86))
                        .min_w_0()
                        .px(px(11.))
                        .py(px(7.))
                        .rounded_tl(px(12.))
                        .rounded_tr(px(12.))
                        .rounded_bl(px(12.))
                        .rounded_br(px(3.))
                        .bg(palette.accent)
                        .text_color(palette.on_accent)
                        .text_size(px(13.))
                        .line_height(px(19.5))
                        .when_some(quote.as_ref(), |bubble_div, quote| {
                            bubble_div.child(
                                quote_label(quote, palette.on_accent_muted, palette.on_accent)
                                    .mb(px(3.)),
                            )
                        })
                        // The bubble selects its own text.
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .map(|bubble_div| match bubble {
                            Some(editor) => bubble_div.child(editor),
                            None => bubble_div.child(text.clone()),
                        }),
                )
                .into_any_element(),
            Item::Assistant { doc, .. } => div()
                .px(px(2.))
                .child(markup_view::render(
                    doc,
                    &self.style,
                    ("message", index),
                    Some(SelectBody {
                        state: self.text_selection.clone(),
                        body: index,
                        color: palette.selection,
                        registry: None,
                    }),
                ))
                .into_any_element(),
            Item::Tool(tool) => self.tool_row(tool, index, session, &palette, window, cx),
            Item::Failure { error, .. } => {
                let retry = session.transcript.retry_at(index).is_some();
                let (message, color) = match error {
                    Some(error) => (error.clone(), palette.danger),
                    None => ("Stopped".to_owned(), palette.muted),
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(2.))
                    .text_size(px(12.))
                    .child(div().flex_1().min_w_0().text_color(color).child(message))
                    .when(retry, |row| {
                        row.child(
                            div()
                                .id(("retry", index))
                                .flex_none()
                                .px(px(9.))
                                .py(px(2.))
                                .rounded(px(999.))
                                .border_1()
                                .border_color(palette.line_strong)
                                .bg(palette.control)
                                .text_color(palette.text)
                                .hover(|style| style.bg(palette.control_hover))
                                .active(|style| style.opacity(0.8))
                                .on_click(cx.listener(move |this, _, _, cx| this.retry(index, cx)))
                                .child("Retry"),
                        )
                    })
                    .into_any_element()
            }
            Item::Notice(text) => div()
                .text_size(px(12.))
                .text_color(palette.faint)
                .text_center()
                .child(text.clone())
                .into_any_element(),
        };
        let entrance = session
            .entering
            .get(&index)
            .map(|start| start.elapsed().as_secs_f32() / ENTRANCE.as_secs_f32())
            .filter(|progress| *progress < 1.);
        div()
            .px(px(14.))
            .pt(if stacked { px(0.) } else { px(10.) })
            .when(index + 1 == items.len() && !session.activity_row, |item| {
                item.pb(px(10.))
            })
            .child(body)
            // Offset, not resized, so the list's measured height holds.
            .when_some(entrance, |item, progress| {
                window.request_animation_frame();
                let delta = ease_out_quint()(progress);
                let rise = if crate::platform::reduce_motion() {
                    0.
                } else {
                    ENTRANCE_RISE
                };
                item.relative().top(px(rise * (1. - delta))).opacity(delta)
            })
            .into_any_element()
    }

    fn tool_row(
        &self,
        tool: &Tool,
        index: usize,
        session: &Session,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let finish = settling(session.finished.get(&index));
        let turn = settling(session.turned.get(&index));
        if finish.is_some() || turn.is_some() {
            window.request_animation_frame();
        }
        let meta = &session.draft.meta;
        let target = tool.target(
            &meta.cwd,
            &session.draft.draft_path(),
            std::env::home_dir().as_deref(),
        );
        let running = tool.running();
        let icon = if running {
            spinner(palette).into_any_element()
        } else {
            svg()
                .path(tool_icon(&tool.name))
                .size(px(14.))
                .text_color(if tool.is_error() {
                    palette.tool_error
                } else {
                    palette.faint
                })
                .when_some(finish, |icon, delta| icon.opacity(delta))
                .into_any_element()
        };
        let stats = tool.stats.filter(|_| !running);
        let row = div()
            .id(("tool", index))
            .group("tool-row")
            .flex()
            .items_center()
            .gap(px(7.))
            .py(px(3.))
            .pl(px(2.))
            .pr(px(6.))
            .mx(px(-2.))
            .rounded(px(6.))
            .hover(|style| style.bg(palette.hover))
            .active(|style| style.opacity(0.8))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
            .child(
                div()
                    .size(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon),
            )
            .child(
                // Verb and target share a baseline.
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(7.))
                    .line_height(px(18.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.5))
                            .text_color(if running { palette.text } else { palette.muted })
                            .child(tool.verb()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(palette.text)
                            .child(target),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(8.))
                    .when_some(finish, |meta, delta| meta.opacity(delta))
                    .when_some(stats, |meta, (added, removed)| {
                        meta.child(stat(format!("+{added}"), palette.added, palette.added_bg))
                            .when_some(removed, |meta, removed| {
                                meta.child(stat(
                                    format!("−{removed}"),
                                    palette.removed,
                                    palette.removed_bg,
                                ))
                            })
                    })
                    .when_some(tool.duration(), |meta, duration| {
                        meta.child(
                            div()
                                .text_size(px(11.))
                                .text_color(palette.faint)
                                .child(format_duration(duration)),
                        )
                    })
                    .child({
                        let chevron = svg()
                            .path(CHEVRON_ICON)
                            .size(px(10.))
                            .text_color(palette.faint);
                        // Collapsing turns it back while the row is hovered.
                        let turned = match turn.filter(|_| !crate::platform::reduce_motion()) {
                            Some(delta) if tool.expanded => delta,
                            Some(delta) => 1. - delta,
                            None if tool.expanded => 1.,
                            None => 0.,
                        };
                        if tool.expanded || turned > 0. {
                            chevron.with_transformation(Transformation::rotate(percentage(
                                0.25 * turned,
                            )))
                        } else {
                            chevron
                                .invisible()
                                .group_hover("tool-row", |style| style.visible())
                        }
                    }),
            );
        div()
            .flex()
            .flex_col()
            .child(row)
            .when(tool.expanded, |column| {
                column.child(
                    div()
                        .when_some(turn, |body, delta| body.opacity(delta))
                        .child(tool_body(tool, index, palette)),
                )
            })
            .into_any_element()
    }

    fn header(&self, palette: &Palette) -> impl IntoElement {
        let profile = self.session().and_then(|session| {
            let dir = &session.draft.meta.pi_agent_dir;
            profile_label(dir, std::env::home_dir().as_deref())
                .map(|label| (label, dir.display().to_string()))
        });
        div()
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(14.))
            .gap(px(7.))
            .overflow_hidden()
            .border_b_1()
            .border_color(palette.hairline)
            .child(
                svg()
                    .path(PI_ICON)
                    .size(px(12.))
                    .flex_none()
                    .text_color(palette.muted),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.muted)
                    .child("Pi"),
            )
            .when_some(profile, |header, (label, path)| {
                header.child(
                    div()
                        .id("pi-profile")
                        .min_w_0()
                        .ml_auto()
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(palette.muted)
                        .text_ellipsis()
                        .tooltip(crate::tooltip::text(path))
                        .child(label),
                )
            })
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let busy = self.busy();
        let session = self.session();
        let empty = self.composer.read(cx).text().trim().is_empty();
        let can_send = !busy && !empty && session.is_some();
        let state = if busy {
            SendState::Busy
        } else if can_send {
            SendState::Ready
        } else {
            SendState::Disabled
        };
        let (shown, mut from) = self.send_state.get();
        if state != shown {
            from = shown;
            self.send_state.set((state, from));
        }
        let model = session.and_then(|session| session.model.clone());
        let quote = self.quote(self.chip.clone(), cx);
        let placeholder = if quote.is_some() {
            "Ask pi to change the selection, or ask about it…"
        } else {
            "Ask pi to change the draft, or ask about the session…"
        };
        // The chip sits in the footer, so attaching a selection doesn't
        // change the composer's height.
        let chip = quote.map(|quote| {
            div()
                .min_w_0()
                .max_w(relative(0.6))
                .flex()
                .items_center()
                .gap(px(2.))
                .h(px(22.))
                .pl(px(7.))
                .pr(px(2.))
                .rounded(px(6.))
                .border_1()
                .border_color(palette.hairline)
                .bg(palette.hover)
                .child(quote_label(&quote, palette.faint, palette.text))
                .child(
                    div()
                        .id("detach-selection")
                        .size(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .hover(|style| style.bg(palette.control_hover))
                        .active(|style| style.opacity(0.8))
                        .tooltip(crate::tooltip::text("Don't attach the selection"))
                        .on_click(cx.listener(|this, _, _, cx| this.detach(cx)))
                        .child(
                            svg()
                                .path(CLOSE_ICON)
                                .size(px(10.))
                                .text_color(palette.muted),
                        ),
                )
                .with_animation(
                    "selection-chip",
                    Animation::new(ENTRANCE).with_easing(ease_out_quint()),
                    move |chip, delta| {
                        let rise = if crate::platform::reduce_motion() {
                            0.
                        } else {
                            ENTRANCE_RISE
                        };
                        chip.relative().top(px(rise * (1. - delta))).opacity(delta)
                    },
                )
        });
        div().flex_none().px(px(12.)).pt(px(8.)).pb(px(12.)).child(
            div()
                .key_context(KEY_CONTEXT)
                .on_action(cx.listener(|this, _: &Send, _, cx| this.send(cx)))
                .on_action(cx.listener(|this, _: &Stop, _, cx| this.stop(cx)))
                .rounded(px(10.))
                .border_1()
                .border_color(palette.line_strong)
                .bg(palette.control)
                .child(
                    div()
                        .h(px(58.))
                        .relative()
                        .child(self.composer.clone())
                        .when(empty, |input| {
                            input.child(
                                div()
                                    .absolute()
                                    .top(px(9.))
                                    .left(px(11.))
                                    .text_size(px(13.))
                                    .text_color(palette.faint)
                                    .child(placeholder),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl(px(11.))
                        .pr(px(6.))
                        .pb(px(6.))
                        .text_size(px(11.))
                        .text_color(palette.faint)
                        .child(div().flex_1().min_w_0().children(model))
                        // While the agent runs, the transcript shows it.
                        .when(!busy && chip.is_none(), |footer| {
                            footer.child("↵ send · ⇧↵ newline")
                        })
                        .children(chip)
                        .child(
                            div()
                                .id("send")
                                .size(px(26.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .text_color(palette.on_accent)
                                .when(busy || can_send, |button| {
                                    button
                                        .active(|style| style.opacity(0.8))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if busy {
                                                this.stop(cx);
                                            } else {
                                                this.send(cx);
                                            }
                                        }))
                                })
                                .with_animation(
                                    ("send-state", state as usize),
                                    Animation::new(SEND_FADE).with_easing(ease_out_quint()),
                                    move |button, delta| {
                                        let icon = if state == SendState::Busy {
                                            svg().path(STOP_ICON).size(px(10.))
                                        } else {
                                            svg().path(SEND_ICON).size(px(14.))
                                        };
                                        // The icon fades in only when it changes.
                                        let swapped =
                                            (state == SendState::Busy) != (from == SendState::Busy);
                                        button
                                            .bg(from
                                                .background(&palette)
                                                .blend(state.background(&palette).opacity(delta)))
                                            .child(
                                                icon.text_color(palette.on_accent)
                                                    .opacity(if swapped { delta } else { 1. }),
                                            )
                                    },
                                ),
                        ),
                ),
        )
    }
}

impl Render for Chat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let log = match self.session() {
            Some(session) => {
                let unavailable = match &session.agent {
                    Some(Err(err)) => Some(err.clone()),
                    _ => None,
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(session.draft.meta.is_sample(), |log| {
                        log.child(sample_card(&palette))
                    })
                    .child(
                        selectable::area(&self.text_selection, cx)
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(ListBounce::new(
                                "transcript-bounce",
                                session.bounce.clone(),
                                session.list.clone(),
                                list(
                                    session.list.clone(),
                                    cx.processor(|this, index, window, cx| {
                                        this.render_item(index, window, cx)
                                    }),
                                )
                                .size_full(),
                            ))
                            .child(scrollbar(
                                Source::List(session.list.clone()),
                                &session.scrollbar,
                            ))
                            .when(
                                !session.list.is_following_tail()
                                    && session.list.max_offset_for_scrollbar().y
                                        + session.list.scroll_px_offset_for_scrollbar().y
                                        > px(1.),
                                |log| {
                                    let list = session.list.clone();
                                    log.child(
                                        div()
                                            .id("jump-to-latest")
                                            .absolute()
                                            .right(px(20.))
                                            .size(px(30.))
                                            .rounded_full()
                                            .bg(palette.card)
                                            .text_color(palette.text)
                                            .border_1()
                                            .border_color(palette.card_ring)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .hover(|style| style.bg(palette.control_hover))
                                            .active(|style| style.opacity(0.8))
                                            .tooltip(crate::tooltip::text("Jump to latest"))
                                            .on_click(move |_, window, _| {
                                                list.set_follow_mode(FollowMode::Tail);
                                                window.refresh();
                                            })
                                            .child(
                                                svg()
                                                    .path(SEND_ICON)
                                                    .size(px(16.))
                                                    .text_color(palette.text)
                                                    .with_transformation(Transformation::rotate(
                                                        percentage(0.5),
                                                    )),
                                            )
                                            .with_animation(
                                                "jump-to-latest-in",
                                                Animation::new(JUMP_FADE)
                                                    .with_easing(ease_out_quint()),
                                                |button, delta| {
                                                    let rise = if crate::platform::reduce_motion() {
                                                        0.
                                                    } else {
                                                        4.
                                                    };
                                                    button
                                                        .bottom(px(10. - rise * (1. - delta)))
                                                        .opacity(delta)
                                                },
                                            ),
                                    )
                                },
                            ),
                    )
                    .children(unavailable.map(|err| {
                        div()
                            .px(px(14.))
                            .pb(px(4.))
                            .text_size(px(12.))
                            .text_color(palette.danger)
                            .child(err)
                    }))
            }
            None => div().flex_1(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.header(&palette))
            .child(log)
            .when(self.current.is_some(), |chat| chat.child(self.composer(cx)))
    }
}

/// Stands in for the session recap of a handed-off draft.
fn sample_card(palette: &Palette) -> impl IntoElement {
    const LEAD: &str = "Sample draft";
    let text = format!(
        "{LEAD} · no session attached. Handed-off drafts show the conversation they came from here."
    );
    let lead = HighlightStyle {
        color: Some(palette.text),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    div().px(px(12.)).pt(px(12.)).child(
        div()
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .bg(palette.card)
            .shadow(vec![
                shadow(palette.card_ring, 0., 0., 0.5),
                shadow(palette.card_drop, 1., 2., 0.),
            ])
            .text_size(px(12.))
            .line_height(px(18.))
            .text_color(palette.muted)
            .child(StyledText::new(text).with_highlights([(0..LEAD.len(), lead)])),
    )
}

/// Assistant messages: GitHub's markdown at chat size.
fn message_style(dark: bool) -> MarkupStyle {
    let mut style = MarkupStyle::github(dark);
    style.text_size = px(13.);
    style.block_gap = px(8.);
    style.heading_gap = px(12.);
    style.code_padding = px(10.);
    style
}

/// The composer's input: the UI font at chat size.
fn composer_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: font(editor::platform::ui_font()),
        font_size: px(13.),
        line_height: px(13. * 1.45),
        padding_x: px(11.),
        padding_top: px(9.),
        padding_bottom: px(4.),
        text: palette.text,
        heading: palette.text,
        ..palette.editor_style()
    }
}

/// A user message: the UI font in the bubble's colours, selectable but not
/// editable.
fn bubble_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: font(editor::platform::ui_font()),
        font_size: px(13.),
        line_height: px(19.5),
        padding_x: px(0.),
        padding_top: px(0.),
        padding_bottom: px(0.),
        text: palette.on_accent,
        read_only_text: palette.on_accent,
        heading: palette.on_accent,
        selection: theme::hex(0xffffff59),
        inactive_selection: theme::hex(0xffffff33),
        fit_content: true,
        ..palette.editor_style()
    }
}

/// The quote icon, the start of the selected text and its lines, on one
/// line: `faint` for the icon and lines, `text` for the excerpt.
fn quote_label(quote: &Quote, faint: Hsla, text: Hsla) -> gpui::Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .text_size(px(11.5))
        .line_height(px(16.))
        .child(
            svg()
                .path(QUOTE_ICON)
                .size(px(12.))
                .flex_none()
                .text_color(faint),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(text)
                .child(quote.excerpt()),
        )
        .child(
            div()
                .flex_none()
                .text_color(faint)
                .child(quote.lines_label()),
        )
}

fn tool_icon(name: &str) -> &'static str {
    match name {
        "bash" => TOOL_BASH_ICON,
        "read" => TOOL_READ_ICON,
        "edit" => TOOL_EDIT_ICON,
        "write" => TOOL_WRITE_ICON,
        _ => TOOL_OTHER_ICON,
    }
}

/// How long the brightness wave takes to cross the activity label.
const WAVE_PERIOD: f32 = 1.8;
/// How far behind its predecessor each letter of the label brightens.
const WAVE_STAGGER: f32 = 0.045;

/// A streamed reply occupies the waiting slot itself. Tools have their own
/// spinner; after a tool finishes, show that pi is thinking again.
fn needs_activity_row(starting: bool, transcript: &Transcript) -> bool {
    starting
        || (transcript.busy
            && match transcript.items.last() {
                Some(Item::Assistant { .. }) => false,
                Some(Item::Tool(tool)) => !tool.running(),
                _ => true,
            })
}

/// The waiting slot before a visible reply replaces it.
fn activity_row(session: &Session, palette: &Palette) -> AnyElement {
    let text = if session.starting() {
        "Starting pi…"
    } else {
        "Thinking…"
    };
    // Inset like the messages: the item padding plus their own 2px.
    let row = div()
        .pt(px(10.))
        .pb(px(10.))
        .px(px(16.))
        .text_size(px(13.))
        .line_height(px(19.5))
        // Highlight blending preserves the base alpha. Resolve the muted
        // colour onto the chat background so the wave can reach full contrast.
        .text_color(palette.chat.blend(palette.muted));
    // GPUI freezes repeating animations at phase zero for Reduce Motion.
    let palette = *palette;
    // Animate inside the element: list rows may be reused without calling
    // their renderer. No chat-model or preview invalidation is needed.
    row.with_animation(
        "activity-shimmer",
        Animation::new(Duration::from_secs_f32(WAVE_PERIOD))
            .repeat_synced()
            .with_max_fps(30.),
        move |row, phase| {
            // One shaped run preserves kerning and a fixed label width.
            let highlights = text.char_indices().enumerate().map(|(ix, (at, c))| {
                let lit = wave(phase * WAVE_PERIOD - ix as f32 * WAVE_STAGGER);
                let style = HighlightStyle {
                    color: Some(palette.text.opacity(lit)),
                    ..HighlightStyle::default()
                };
                (at..at + c.len_utf8(), style)
            });
            row.child(StyledText::new(text).with_highlights(highlights))
        },
    )
    .into_any_element()
}

/// The wave's brightness at `time` seconds: rises over the first 15% of
/// each period, settles by 40%, then rests.
fn wave(time: f32) -> f32 {
    let p = time.rem_euclid(WAVE_PERIOD) / WAVE_PERIOD;
    let rise = if p < 0.15 {
        p / 0.15
    } else if p < 0.4 {
        1. - (p - 0.15) / 0.25
    } else {
        0.
    };
    rise * rise * (3. - 2. * rise)
}

/// A grey ring with a turning accent arc.
pub(crate) fn spinner(palette: &Palette) -> impl IntoElement {
    div()
        .size(px(11.))
        .relative()
        .child(
            svg()
                .absolute()
                .size_full()
                .path(SPINNER_TRACK_ICON)
                .text_color(palette.spinner_track),
        )
        .child(
            svg()
                .absolute()
                .size_full()
                .path(SPINNER_ARC_ICON)
                .text_color(palette.accent)
                .with_animation(
                    "spin",
                    Animation::new(Duration::from_millis(700)).repeat(),
                    |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
                ),
        )
}

fn stat(text: String, color: Hsla, background: Hsla) -> impl IntoElement {
    div()
        .px(px(4.))
        .rounded(px(4.))
        .font_family(MONO)
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(color)
        .bg(background)
        .child(text)
}

/// How far along a tool row's settle is, eased, if it started at `start`
/// and hasn't finished.
fn settling(start: Option<&Instant>) -> Option<f32> {
    let progress = start?.elapsed().as_secs_f32() / TOOL_SETTLE.as_secs_f32();
    (progress < 1.).then(|| ease_out_quint()(progress))
}

/// The transcript indices of the tools still running.
fn running_tools(transcript: &Transcript) -> impl Iterator<Item = usize> + '_ {
    transcript
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            Item::Tool(tool) if tool.running() => Some(index),
            _ => None,
        })
}

fn format_duration(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}

/// The expanded row: output keeps its whitespace and scrolls sideways.
fn tool_body(tool: &Tool, index: usize, palette: &Palette) -> impl IntoElement {
    let lines: Vec<AnyElement> = match tool.diff() {
        Some(diff) => diff
            .into_iter()
            .map(|line| {
                let (sign, text, color, background) = match line {
                    DiffLine::Context(text) => (" ", text, palette.tool_text, None),
                    DiffLine::Removed(text) => {
                        ("-", text, palette.diff_removed, Some(palette.removed_bg))
                    }
                    DiffLine::Added(text) => {
                        ("+", text, palette.diff_added, Some(palette.added_bg))
                    }
                };
                div()
                    .flex()
                    .min_w_full()
                    .pr(px(10.))
                    .text_color(color)
                    .when_some(background, |line, bg| line.bg(bg))
                    .child(
                        div()
                            .w(px(18.))
                            .flex_none()
                            .text_center()
                            .text_color(palette.faint)
                            .child(sign),
                    )
                    .child(div().whitespace_nowrap().child(blank_as_space(text)))
                    .into_any_element()
            })
            .collect(),
        None => tool
            .body()
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let command = tool.name == "bash" && i == 0;
                div()
                    .px(px(10.))
                    .whitespace_nowrap()
                    .when(command, |line| line.text_color(palette.faint).mb(px(4.)))
                    .child(blank_as_space(line))
                    .into_any_element()
            })
            .collect(),
    };
    div()
        .id(("tool-body", index))
        .ml(px(24.))
        .my(px(4.))
        .py(px(8.))
        .max_h(px(200.))
        .overflow_scroll()
        .rounded(px(7.))
        .bg(palette.tool_body)
        .font_family(MONO)
        .text_size(px(11.5))
        .line_height(px(17.))
        .text_color(palette.tool_text)
        .child(div().flex().flex_col().min_w_full().children(lines))
}

/// Empty lines would collapse to no height.
fn blank_as_space(text: String) -> String {
    if text.is_empty() { " ".into() } else { text }
}
/// The request that starts a handed-off draft, with the note the handing-off
/// agent left about what to write.
fn first_draft_prompt(note: Option<&str>) -> String {
    let mut text = "Write the first draft of the message from our conversation.".to_owned();
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        text.push_str("\n\nWhat to write: ");
        text.push_str(note);
    }
    text
}

/// Show custom agent directories; the standard Pi directory needs no label.
fn profile_label(dir: &std::path::Path, home: Option<&std::path::Path>) -> Option<String> {
    if dir.as_os_str().is_empty() || home.is_some_and(|home| dir == home.join(".pi/agent")) {
        return None;
    }
    Some(crate::drafts::tilde(dir, home))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_profile_uses_only_the_drafts_directory() {
        use std::path::Path;
        let home = Some(Path::new("/home/user"));
        assert_eq!(
            profile_label(Path::new("/home/user/.pi-work/agent"), home),
            Some("~/.pi-work/agent".into())
        );
        assert_eq!(profile_label(Path::new("/home/user/.pi/agent"), home), None);
        assert_eq!(
            profile_label(Path::new("/other/.pi/agent"), home),
            Some("/other/.pi/agent".into())
        );
        assert_eq!(
            profile_label(Path::new("/custom/work/agent"), home),
            Some("/custom/work/agent".into())
        );
        assert_eq!(
            profile_label(Path::new("/home/user/.pi/agent"), None),
            Some("/home/user/.pi/agent".into())
        );
        assert_eq!(profile_label(Path::new(""), home), None);
    }

    #[test]
    fn streamed_reply_replaces_waiting_slot_and_completion_keeps_it() {
        use draftmux_pi_rpc::{AssistantDelta, Event};
        let mut transcript = Transcript::default();
        let apply = |transcript: &mut Transcript, event| {
            transcript.apply(&AgentEvent::Pi(event), None, Instant::now());
        };
        let rows = |transcript: &Transcript| {
            transcript.items.len() + usize::from(needs_activity_row(false, transcript))
        };
        assert!(needs_activity_row(true, &transcript));
        assert!(!needs_activity_row(false, &transcript));
        transcript.sent("hello".into(), String::new());
        assert!(needs_activity_row(false, &transcript));
        assert_eq!(rows(&transcript), 2);
        apply(
            &mut transcript,
            Event::MessageStart {
                message: serde_json::json!({"role": "assistant", "content": []}),
            },
        );
        for delta in ["Hello", " there", "."] {
            apply(
                &mut transcript,
                Event::MessageUpdate(AssistantDelta::TextDelta {
                    content_index: 0,
                    delta: delta.into(),
                }),
            );
            assert!(!needs_activity_row(false, &transcript));
            assert_eq!(rows(&transcript), 2);
        }
        apply(
            &mut transcript,
            Event::MessageEnd {
                message: serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "Hello there."}]}),
            },
        );
        assert_eq!(rows(&transcript), 2);
        apply(&mut transcript, Event::AgentSettled { aborted: false });
        assert_eq!(rows(&transcript), 2);
        assert!(!needs_activity_row(false, &transcript));
        transcript.sent("again".into(), String::new());
        assert!(needs_activity_row(false, &transcript));
        assert_eq!(rows(&transcript), 4);
    }

    #[test]
    fn running_tools_own_the_activity_indicator() {
        use draftmux_pi_rpc::Event;
        let mut transcript = Transcript::default();
        transcript.sent("hello".into(), String::new());
        transcript.apply(
            &AgentEvent::Pi(Event::ToolExecutionStart {
                tool_call_id: "read-1".into(),
                tool_name: "read".into(),
                args: serde_json::json!({"path": "draft.md"}),
            }),
            None,
            Instant::now(),
        );
        assert!(!needs_activity_row(false, &transcript));
        let Some(Item::Tool(tool)) = transcript.items.last_mut() else {
            panic!("expected a tool row");
        };
        tool.status = transcript::ToolStatus::Done {
            is_error: false,
            duration: None,
        };
        assert!(needs_activity_row(false, &transcript));
        transcript.busy = false;
        assert!(!needs_activity_row(false, &transcript));
    }

    #[test]
    fn shimmer_can_reach_full_contrast_in_both_themes() {
        for dark in [false, true] {
            let palette = Palette::resolve(dark, false);
            let base = palette.chat.blend(palette.muted);
            assert_eq!(base.a, 1.);
            assert_eq!(base.blend(palette.text.opacity(wave(0.))), base);
            assert_eq!(base.blend(palette.text.opacity(1.)), palette.text);
            assert_ne!(base, palette.text);
        }
    }

    #[test]
    fn activity_labels_rest_at_the_reduced_motion_phase() {
        for text in ["Starting pi…", "Thinking…"] {
            for (ix, _) in text.chars().enumerate() {
                assert_eq!(wave(-(ix as f32) * WAVE_STAGGER), 0.);
            }
        }
    }

    #[test]
    fn wave_is_bounded_and_continuous_at_the_loop() {
        for step in -1800..3600 {
            assert!((0.0..=1.0).contains(&wave(step as f32 / 1000.)));
        }
        assert!((wave(WAVE_PERIOD - 0.0001) - wave(0.0001)).abs() < 0.001);
    }

    #[test]
    fn wave_rises_settles_and_rests_each_period() {
        assert_eq!(wave(0.), 0.);
        assert!((wave(0.15 * WAVE_PERIOD) - 1.).abs() < 1e-4);
        assert_eq!(wave(0.5 * WAVE_PERIOD), 0.);
        // Letters behind the first wrap into the previous period.
        assert!((wave(-0.85 * WAVE_PERIOD) - 1.).abs() < 1e-4);
    }
}
