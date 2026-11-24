//! The chat column: the selected draft's conversation with its agent, drawn
//! on gpui's `list()` with tool calls as compact rows, and the composer.
//!
//! Each draft gets its own [`Agent`], for the backend its descriptor names
//! (pi or Claude Code), started when the draft is selected and stopped once
//! it is idle or left. The chat reports to its owner
//! through [`ChatEvent`]: the owner saves the draft before a prompt, reloads
//! it after the agent writes it, and locks the editor while the agent runs.
//!
//! A non-empty selection in the draft editor is attached to the next
//! message: a stack of quotes above the message field shows it, and sending
//! puts it in the prompt ([`quote`]). It stays attached while the composer
//! has focus, since the editor keeps its selection when it loses focus.
//! Sending or the quote's × detaches it, and selecting something else
//! attaches that.
//!
//! With several selections in the editor (see
//! [`Editor::extra_selections`]), they all attach, one quote each in the
//! same stack. Each quote's × drops that selection and Clear drops them
//! all; sending sends them together and drops them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsString;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use draftmux_agent::quote::{self, Quote};
use draftmux_agent::transcript::{self, Item, Retry, Target, Tool, Transcript};
use draftmux_agent::{Agent, Event, Events, History, Prompted, ToolKind};
use draftmux_claude_rpc::{ClaudeAgent, DraftContext, DraftSession, Record};
use draftmux_pi_rpc::{LaunchSpec, PiAgent};
use draftmux_storage::{Backend, ClaudeSession, Draft, PiSession, SourceKind, Store};
use editor::{Editor, EditorEvent, EditorStyle};
use futures::{StreamExt, stream};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FollowMode, FontWeight, HighlightStyle, Hsla, KeyBinding, ListAlignment, ListState,
    MouseButton, SharedString, StyledText, Task, Transformation, Window, actions, anchored,
    deferred, div, ease_out_quint, font, list, percentage, point, prelude::*, px, relative, svg,
};
use shell_env::ShellEnv;

use crate::assets::{
    CHEVRON_ICON, CLAUDE_CODE_ICON, CLOSE_ICON, PI_ICON, QUOTE_ICON, SEND_ICON, SPINNER_ARC_ICON,
    SPINNER_TRACK_ICON, STOP_ICON, TOOL_BASH_ICON, TOOL_EDIT_ICON, TOOL_OTHER_ICON, TOOL_READ_ICON,
    TOOL_WRITE_ICON,
};
use crate::bounce::{Bounce, ListBounce};
use crate::chat_model::{self, ChatModel};
use crate::chrome::shadow;
use crate::edit_changes::{self, EditChange, EditHunk};
use crate::markup_view::{self, MarkupStyle, SelectBody};
use crate::profiles::ProfileCatalog;
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::selectable::{self, Scroller, TextSelection};
use crate::theme::{self, MONO, Palette, Theme};

actions!(chat, [Send, Stop, CompactSession]);

/// What compacting the conversation keeps, beyond each agent's own summary.
/// The draft itself is sent again after compaction, so the summary needn't
/// hold it.
const COMPACT_INSTRUCTIONS: &str = "This conversation is now about writing one message in \
Draftmux. Keep what that message needs: where it goes and who reads it, what it is for, the \
facts and decisions its content relies on, the user's stated preferences on tone and wording, \
and wordings the user rejected. Leave out tool output, file contents and the draft's text, \
which is sent again.";

/// How long an idle agent process is kept.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long pi gets to answer a request. pi answers a prompt only after
/// compacting and running input hooks or an extension command.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// What a Claude draft's agent asks the app to keep in the draft's
/// descriptor.
type Records = futures::channel::mpsc::UnboundedReceiver<Record>;

/// How long `claude --version` gets to answer.
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
/// A Claude draft's process: how long claude gets to answer a control
/// request, and the longest line it may print.
const CLAUDE_CONFIG: draftmux_claude_rpc::Config = draftmux_claude_rpc::Config {
    idle_timeout: IDLE_TIMEOUT,
    request_timeout: REQUEST_TIMEOUT,
    max_record: 16 << 20,
};
/// How long the draft's selection must stay put before the composer's
/// quotes show it, so dragging a selection doesn't flicker them.
const QUOTES_SETTLE: Duration = Duration::from_millis(250);
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
        KeyBinding::new(editor::secondary!("alt-k"), CompactSession, None),
    ]);
}

pub enum ChatEvent {
    /// The user sent `text`, the prompt with any attached selection. Save
    /// the draft, then pass both to [`Chat::prompt`].
    Send(String),
    /// The agent wrote draft `id`, selected or not; `text` is its new text.
    DraftWritten { id: String, text: String },
    /// Draft `id`'s agent is `running` a turn or not: reported whenever its
    /// transcript changes, and when `id` is selected.
    Running { id: String, running: bool },
    /// Draft `id` is selected, its first draft is pending, and its agent is
    /// ready: send `text` like [`ChatEvent::Send`] unless the draft already
    /// has text. Reported each time the draft is shown until the first draft
    /// is no longer pending.
    FirstDraft { id: String, text: String },
    /// Draft `id`'s agent accepted a prompt; a rejected or failed one isn't
    /// reported.
    Accepted { id: String },
    /// Draft `id`'s Claude agent reported something its descriptor must
    /// keep. Record it in the draft's `meta.json`.
    Recorded { id: String, record: Record },
    /// The user asked to move draft `id`, which continues no session, to a
    /// new session on the other agent. Record it, then pass the draft to
    /// [`Chat::restart`].
    SwitchAgent { id: String },
}

impl EventEmitter<ChatEvent> for Chat {}

/// The user's own message in a prompt sent with [`ChatEvent::Send`],
/// without the selection attached to it.
pub fn message(prompt: &str) -> &str {
    quote::parse(prompt).1
}

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
    /// The draft selections the composer's quote stack shows, which follow
    /// the editor's once they settle.
    shown: Vec<Range<usize>>,
    settle_shown: Task<()>,
    /// The attached selections' quotes are listed above their chip.
    quotes_listed: Cell<bool>,
    /// The send button's state and the one it is changing from.
    send_state: Cell<(SendState, SendState)>,
    style: MarkupStyle,
    /// The text zoom `style` was made for.
    style_zoom: f32,
    /// The changes of each finished edit, by tool call id.
    edit_hunks: RefCell<HashMap<String, Rc<Vec<EditHunk>>>>,
    /// The context chip's popover is open.
    context_open: bool,
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
    agent: Option<Result<Arc<dyn Agent>, String>>,
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
    fn claude(&self) -> bool {
        matches!(self.draft.meta.backend, Backend::Claude(_))
    }

    /// The agent's name in the chat's own text.
    fn agent_name(&self) -> &'static str {
        if self.claude() { "Claude" } else { "pi" }
    }

    /// Whether the agent is still starting or the earlier conversation is
    /// still loading. An agent that can't start isn't starting.
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
                    let editor = editor.read(cx);
                    if this.detached.is_some() {
                        this.detached = Some(editor.selected_range());
                    }
                    this.shown = editor.selected_ranges();
                    this.settle_shown = Task::ready(());
                }
                EditorEvent::SelectionChanged(_) | EditorEvent::ExtraSelectionsChanged => {
                    if let EditorEvent::SelectionChanged(range) = event
                        && this.detached.as_ref() != Some(range)
                    {
                        this.detached = None;
                    }
                    this.settle_shown = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(QUOTES_SETTLE).await;
                        this.update(cx, |this, cx| {
                            this.shown = this.draft_editor.read(cx).selected_ranges();
                            cx.notify();
                        })
                        .ok();
                    });
                }
                EditorEvent::GhostAccepted(_)
                | EditorEvent::GhostDismissed
                | EditorEvent::TrackedClicked(_) => return,
            }
            cx.notify();
        })
        .detach();
        cx.observe_global::<Theme>(|this: &mut Self, cx| {
            let zoom = this.style_zoom;
            this.style = message_style(theme::get(cx));
            this.style_zoom = theme::get(cx).zoom;
            // Item heights measured at another text size no longer hold.
            if this.style_zoom != zoom {
                for session in this.sessions.values() {
                    session.list.remeasure();
                }
            }
        })
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
            shown: Vec::new(),
            settle_shown: Task::ready(()),
            quotes_listed: Cell::new(false),
            send_state: Cell::new((SendState::Disabled, SendState::Disabled)),
            style: message_style(theme::get(cx)),
            style_zoom: theme::get(cx).zoom,
            edit_hunks: RefCell::default(),
            context_open: false,
        }
    }

    /// The changes a finished edit made, worked out once per edit.
    fn edit_hunks(&self, tool: &Tool) -> Option<Rc<Vec<EditHunk>>> {
        if !matches!(tool.kind, ToolKind::Edit { .. }) || tool.running() || tool.is_error() {
            return None;
        }
        let patch = tool.patch.as_deref()?;
        let mut cache = self.edit_hunks.borrow_mut();
        let hunks = cache
            .entry(tool.id.clone())
            .or_insert_with(|| Rc::new(edit_changes::edit_hunks(patch)));
        Some(hunks.clone())
    }

    fn session(&self) -> Option<&Session> {
        self.sessions.get(self.current.as_deref()?)
    }

    /// A run or a compaction is under way: nothing else can be sent.
    fn busy(&self) -> bool {
        self.session()
            .is_some_and(|session| session.transcript.busy || session.transcript.compacting)
    }

    /// Replaces the selected draft's earlier messages with a summary, unless
    /// its agent is busy or isn't running.
    pub fn compact(&mut self, cx: &mut Context<Self>) {
        self.context_open = false;
        if self.busy() {
            cx.notify();
            return;
        }
        let Some(id) = self.current.clone() else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let Some(Ok(agent)) = &session.agent else {
            return;
        };
        let compact = agent.compact(COMPACT_INSTRUCTIONS.to_owned());
        session.transcript.compact_requested();
        let request = id.clone();
        cx.spawn(async move |this, cx| {
            let id = request;
            let result = compact.await;
            this.update(cx, |this, cx| {
                if let Err(err) = result
                    && let Some(session) = this.sessions.get_mut(&id)
                {
                    session.transcript.compact_failed(err.to_string());
                }
                this.after_change(&id, cx);
            })
            .ok();
        })
        .detach();
        self.after_change(&id, cx);
    }

    /// Drops `draft`'s conversation and agent, and starts its agent again
    /// from its descriptor, such as after it moved to another agent. The
    /// composer keeps its text.
    pub fn restart(&mut self, draft: &Draft, cx: &mut Context<Self>) {
        self.sessions.remove(&draft.id);
        if self.current.as_deref() != Some(draft.id.as_str()) {
            return;
        }
        let unsent = self.composer.read(cx).text().to_owned();
        self.current = None;
        self.set_draft(Some(draft), cx);
        self.composer.update(cx, |composer, cx| {
            composer.set_text(&unsent, cx);
            composer.select_range(unsent.len()..unsent.len(), cx);
        });
    }

    /// Drops draft `id`'s conversation and stops its agent, as the draft is
    /// deleted. The shown draft is left alone.
    pub fn forget(&mut self, id: &str) {
        if self.current.as_deref() != Some(id) {
            self.sessions.remove(id);
        }
    }

    /// Asks the owner to move the selected draft to the other agent,
    /// confirming first when that leaves a conversation behind.
    fn switch_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session() else {
            return;
        };
        if session.transcript.busy {
            return;
        }
        let id = session.draft.id.clone();
        if session.transcript.items.is_empty() {
            cx.emit(ChatEvent::SwitchAgent { id });
            return;
        }
        let (from, to) = if session.claude() {
            ("Claude Code", "pi")
        } else {
            ("pi", "Claude Code")
        };
        let answer = window.prompt(
            gpui::PromptLevel::Info,
            &format!("Switch this draft to {to}?"),
            Some(&format!(
                "{to} starts a new conversation; this one with {from} isn't carried over. \
                 The draft's text stays as it is."
            )),
            &["Cancel", "Switch"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(1) {
                this.update(cx, |_, cx| cx.emit(ChatEvent::SwitchAgent { id }))
                    .ok();
            }
        })
        .detach();
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
        if let Some(id) = self.current.clone() {
            cx.emit(ChatEvent::Running {
                id,
                running: self.busy(),
            });
        }
        cx.notify();
    }

    /// Creates the draft's agent once the shell environment is known, then
    /// focuses it if the draft is still selected.
    fn start_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let ready = ShellEnv::ready(cx);
        let claude = session.claude();
        let id = id.to_owned();
        session._events = cx.spawn(async move |this, cx| {
            let env = ready.await;
            // Runs claude, so off the UI thread.
            let program = match claude {
                true => {
                    let env = env.clone();
                    let check = async move { claude_program(&env) };
                    Some(cx.background_executor().spawn(check).await)
                }
                false => None,
            };
            let Some((events, records)) = this
                .update(cx, |this, cx| this.create_agent(&id, &env, program, cx))
                .ok()
                .flatten()
            else {
                return;
            };
            let records = match records {
                Some(records) => records.map(Incoming::Record).boxed(),
                None => stream::empty().boxed(),
            };
            let mut incoming = stream::select(events.map(Incoming::Event), records);
            while let Some(incoming) = incoming.next().await {
                let updated = this.update(cx, |this, cx| match incoming {
                    Incoming::Event(event) => this.on_event(&id, event, cx),
                    Incoming::Record(record) => cx.emit(ChatEvent::Recorded {
                        id: id.clone(),
                        record,
                    }),
                });
                if updated.is_err() {
                    break;
                }
            }
        });
    }

    /// `program` is the checked `claude` for a Claude draft.
    fn create_agent(
        &mut self,
        id: &str,
        env: &Arc<ShellEnv>,
        program: Option<Result<PathBuf, String>>,
        cx: &mut Context<Self>,
    ) -> Option<(Events, Option<Records>)> {
        let composer = self.composer.focus_handle(cx);
        let session = self.sessions.get_mut(id)?;
        let draft = &session.draft;
        let started = match &draft.meta.backend {
            Backend::Pi(pi) => {
                pi_agent(draft, pi, env).map(|(agent, events)| (agent, events, None))
            }
            Backend::Claude(claude) => program
                .unwrap_or_else(|| Err("Claude Code wasn't checked.".into()))
                .and_then(|program| claude_agent(draft, claude, env, program))
                .map(|(agent, events, records)| (agent, events, Some(records))),
        };
        let (agent, streams) = match started {
            Ok((agent, events, records)) => (Ok(agent), Some((events, records))),
            Err(err) => (Err(err), None),
        };
        let backend = match &session.draft.meta.backend {
            Backend::Pi(pi) => {
                let dir = &pi.agent_dir;
                let profile_name = ProfileCatalog::global(cx)
                    .read(cx)
                    .entry(dir)
                    .map(|entry| entry.profile.name.clone())
                    .unwrap_or_else(|| dir.display().to_string());
                chat_model::Backend::Pi {
                    profile_name: profile_name.into(),
                }
            }
            Backend::Claude(_) => chat_model::Backend::Claude,
        };
        session.model = agent.as_ref().ok().map(|agent| {
            let agent = agent.clone();
            cx.new(|_| ChatModel::new(agent, backend, composer))
        });
        session.agent = Some(agent);
        if self.current.as_deref() == Some(id) {
            self.focus_agent(id, cx);
        }
        self.sync_list(id, cx);
        streams
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
            Self::load_history(id, &*agent, cx);
        }
        self.request_first_draft(id, cx);
    }

    fn load_history(id: &str, agent: &dyn Agent, cx: &mut Context<Self>) {
        let history = agent.history();
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let history = history.await;
            this.update(cx, |this, cx| this.history_loaded(&id, history, cx))
                .ok();
        })
        .detach();
    }

    /// Requests the history of the selected draft again after it failed to
    /// load. The transcript offers this only before anything else is shown,
    /// so the history still can't include a prompt.
    fn retry_history(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.current.clone() else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let Some(Ok(agent)) = &session.agent else {
            return;
        };
        let agent = agent.clone();
        session.transcript.retrying_history();
        session.history_ready = false;
        Self::load_history(&id, &*agent, cx);
        self.sync_list(&id, cx);
    }

    /// Asks the owner to send the first-draft request for a handed-off
    /// draft whose agent is ready. The owner decides whether it is still due.
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
        cx.emit(ChatEvent::FirstDraft {
            id: id.to_owned(),
            text: first_draft_prompt(session.draft.meta.note.as_deref()),
        });
    }

    fn history_loaded(
        &mut self,
        id: &str,
        history: Result<History, draftmux_agent::Error>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        session.history_ready = true;
        let History {
            earlier,
            messages,
            context,
        } = match history {
            Ok(history) => history,
            Err(err) => {
                session.transcript.history_failed(&err.to_string());
                self.sync_list(id, cx);
                return;
            }
        };
        let mut items = transcript::history(&messages);
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
        if let Some(summary) = &session.draft.meta.summary {
            items.insert(
                0,
                Item::Summary {
                    text: summary.clone(),
                    expanded: false,
                },
            );
        }
        session.transcript.prepend(items);
        // A model request reported since is newer.
        if session.transcript.context.is_none() {
            session.transcript.context = context;
        }
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
            (None, Ok(_)) => Err("The agent is still starting.".to_owned()),
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
                        if reply.is_ok() {
                            cx.emit(ChatEvent::Accepted { id: id.clone() });
                        }
                        match reply {
                            Ok(Prompted::Handled) => session.transcript.handled(),
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

    /// The draft editor's selections, unless the user detached the only one.
    fn attached(&self, cx: &App) -> Vec<Quote> {
        self.quotes(&self.draft_editor.read(cx).selected_ranges(), cx)
    }

    /// `ranges` of the draft, unless the user detached the only one.
    fn quotes(&self, ranges: &[Range<usize>], cx: &App) -> Vec<Quote> {
        if self.session().is_none() {
            return Vec::new();
        }
        if let [range] = ranges
            && self.detached.as_ref() == Some(range)
        {
            return Vec::new();
        }
        let text = self.draft_editor.read(cx).text();
        ranges
            .iter()
            .filter_map(|range| Quote::new(text, range.clone()))
            .collect()
    }

    fn detach(&mut self, cx: &mut Context<Self>) {
        let ranges = self.draft_editor.read(cx).selected_ranges();
        self.detached = ranges.first().cloned();
        self.shown = ranges;
        self.settle_shown = Task::ready(());
        cx.notify();
    }

    /// Drops the selections in `ranges` from the draft editor, which drops
    /// their quotes.
    fn drop_selections(&mut self, ranges: &[Range<usize>], cx: &mut Context<Self>) {
        self.draft_editor.update(cx, |editor, cx| {
            let extra = editor
                .extra_selections()
                .iter()
                .filter(|range| !ranges.contains(range))
                .cloned()
                .collect();
            editor.set_extra_selections(extra, cx);
            let selection = editor.selected_range();
            if ranges.contains(&selection) {
                editor.select_range(selection.end..selection.end, cx);
            }
        });
        self.shown = self.draft_editor.read(cx).selected_ranges();
        self.settle_shown = Task::ready(());
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_owned();
        if text.is_empty() || self.busy() || self.session().is_none() {
            return;
        }
        self.composer
            .update(cx, |editor, cx| editor.set_text("", cx));
        let quotes = self.attached(cx);
        self.send_with(&text, quotes, cx);
    }

    /// Sends `text` about the draft editor's selections, all of them even
    /// when the composer has the only one detached: the selection popover's
    /// comment field. Returns whether it was sent.
    pub fn comment(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let text = text.trim();
        if text.is_empty() || self.busy() || self.session().is_none() {
            return false;
        }
        let draft = self.draft_editor.read(cx);
        let quotes = draft
            .selected_ranges()
            .into_iter()
            .filter_map(|range| Quote::new(draft.text(), range))
            .collect();
        self.send_with(text, quotes, cx);
        true
    }

    fn send_with(&mut self, text: &str, quotes: Vec<Quote>, cx: &mut Context<Self>) {
        if quotes.len() > 1 {
            // The message took the selections; pi is about to change them.
            let ranges: Vec<_> = quotes.iter().map(|quote| quote.range.clone()).collect();
            self.drop_selections(&ranges, cx);
        } else if !quotes.is_empty() {
            // The message took the selection; the editor keeps it selected.
            self.detach(cx);
        }
        cx.emit(ChatEvent::Send(quote::prompt(text, &quotes)));
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

    fn on_event(&mut self, id: &str, event: Event, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let written = match &event {
            Event::ToolFinished {
                draft_write: true, ..
            } => match std::fs::read_to_string(session.draft.draft_path()) {
                Ok(text) => Some(text),
                Err(err) => {
                    eprintln!("draftmux: reading the agent's edit: {err}");
                    None
                }
            },
            _ => None,
        };
        if let (Event::Ready(state), Some(model)) = (&event, &session.model) {
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
        if let Some(session) = self.sessions.get(id) {
            cx.emit(ChatEvent::Running {
                id: id.to_owned(),
                running: session.transcript.busy,
            });
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
            .cloned();
        match retry {
            Some(Retry::Prompt(text)) => cx.emit(ChatEvent::Send(text)),
            Some(Retry::History) => self.retry_history(cx),
            None => {}
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
        let zoom = theme::get(cx).zoom;
        if let Some(session) = self.session()
            && index == session.transcript.items.len()
        {
            return activity_row(session, &palette, zoom);
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
            Item::User { text, quotes } => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.86))
                        .min_w_0()
                        .px(px(11. * zoom))
                        .py(px(7. * zoom))
                        .rounded_tl(px(12. * zoom))
                        .rounded_tr(px(12. * zoom))
                        .rounded_bl(px(12. * zoom))
                        .rounded_br(px(3. * zoom))
                        .bg(palette.accent)
                        .text_color(palette.on_accent)
                        .text_size(px(13. * zoom))
                        .line_height(px(19.5 * zoom))
                        .when(!quotes.is_empty(), |bubble_div| {
                            bubble_div.child(div().mb(px(3. * zoom)).children(quotes.iter().map(
                                |quote| {
                                    quote_label(
                                        quote,
                                        palette.on_accent_muted,
                                        palette.on_accent,
                                        zoom,
                                    )
                                },
                            )))
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
                .px(px(2. * zoom))
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
                    .px(px(2. * zoom))
                    .text_size(px(12. * zoom))
                    .child(div().flex_1().min_w_0().text_color(color).child(message))
                    .when(retry, |row| {
                        row.child(
                            div()
                                .id(("retry", index))
                                .flex_none()
                                .px(px(9. * zoom))
                                .py(px(2. * zoom))
                                .rounded(px(999. * zoom))
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
                .text_size(px(12. * zoom))
                .text_color(palette.faint)
                .text_center()
                .child(text.clone())
                .into_any_element(),
            Item::Summary { text, expanded } => {
                summary_row(text, *expanded, index, &palette, zoom, window, cx)
            }
        };
        let entrance = session
            .entering
            .get(&index)
            .map(|start| start.elapsed().as_secs_f32() / ENTRANCE.as_secs_f32())
            .filter(|progress| *progress < 1.);
        div()
            .px(px(14.))
            .pt(if stacked {
                px(0. * zoom)
            } else {
                px(10. * zoom)
            })
            .when(index + 1 == items.len() && !session.activity_row, |item| {
                item.pb(px(10. * zoom))
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
        let zoom = theme::get(cx).zoom;
        let finish = settling(session.finished.get(&index));
        let turn = settling(session.turned.get(&index));
        if finish.is_some() || turn.is_some() {
            window.request_animation_frame();
        }
        let meta = &session.draft.meta;
        let home = std::env::home_dir();
        // The row shows a short form; hovering it shows the whole target.
        let (target, full) = match tool.target() {
            Target::Path(path) => (
                display_path(
                    path,
                    &meta.cwd,
                    &session.draft.draft_path(),
                    home.as_deref(),
                ),
                crate::drafts::tilde(
                    &absolute_path(path, &meta.cwd, home.as_deref()),
                    home.as_deref(),
                ),
            ),
            Target::Text(text) => (
                text.to_owned(),
                match &tool.kind {
                    ToolKind::Bash { command } => command.clone(),
                    _ => text.to_owned(),
                },
            ),
        };
        let running = tool.running();
        let icon = if running {
            spinner(palette).size(px(11. * zoom)).into_any_element()
        } else {
            svg()
                .path(tool_icon(&tool.kind))
                .size(px(14. * zoom))
                .text_color(if tool.is_error() {
                    palette.tool_error
                } else {
                    palette.faint
                })
                .when_some(finish, |icon, delta| icon.opacity(delta))
                .into_any_element()
        };
        let hunks = self.edit_hunks(tool);
        let stats = tool.stats.filter(|_| !running && hunks.is_none());
        let change_count = hunks
            .as_ref()
            .map(|hunks| hunks.iter().map(|hunk| hunk.changes.len()).sum::<usize>());
        let row = div()
            .id(("tool", index))
            .group("tool-row")
            .flex()
            .items_center()
            .gap(px(7. * zoom))
            .py(px(3. * zoom))
            .pl(px(2. * zoom))
            .pr(px(6. * zoom))
            .mx(px(-2. * zoom))
            .rounded(px(6. * zoom))
            .hover(|style| style.bg(palette.hover))
            .active(|style| style.opacity(0.8))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
            .child(
                div()
                    .size(px(16. * zoom))
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
                    .gap(px(7. * zoom))
                    .line_height(px(18. * zoom))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.5 * zoom))
                            .text_color(if running { palette.text } else { palette.muted })
                            .child(tool.verb()),
                    )
                    .child(
                        div()
                            .id(("tool-target", index))
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(12. * zoom))
                            .text_color(palette.text)
                            .tooltip(crate::tooltip::text(full))
                            .child(target),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6. * zoom))
                    .pl(px(8. * zoom))
                    .when_some(finish, |meta, delta| meta.opacity(delta))
                    .when_some(stats, |meta, (added, removed)| {
                        meta.child(stat(
                            format!("+{added}"),
                            palette.added,
                            palette.added_bg,
                            zoom,
                        ))
                        .when_some(removed, |meta, removed| {
                            meta.child(stat(
                                format!("−{removed}"),
                                palette.removed,
                                palette.removed_bg,
                                zoom,
                            ))
                        })
                    })
                    .when_some(change_count, |meta, count| {
                        meta.child(
                            div()
                                .text_size(px(11. * zoom))
                                .text_color(palette.faint)
                                .child(match count {
                                    0 => "whitespace".to_owned(),
                                    1 => "1 change".to_owned(),
                                    n => format!("{n} changes"),
                                }),
                        )
                    })
                    .when_some(tool.duration(), |meta, duration| {
                        meta.child(
                            div()
                                .text_size(px(11. * zoom))
                                .text_color(palette.faint)
                                .child(format_duration(duration)),
                        )
                    })
                    .child({
                        let chevron = svg()
                            .path(CHEVRON_ICON)
                            .size(px(10. * zoom))
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
        // Where each hunk's edited text is in the draft, if it still is.
        let draft = self.draft_editor.clone();
        let found: Vec<Option<usize>> = match &hunks {
            Some(hunks) if tool.expanded => {
                let text = draft.read(cx).text();
                hunks
                    .iter()
                    .map(|hunk| text.find(hunk.text.trim_end_matches('\n')))
                    .collect()
            }
            _ => Vec::new(),
        };
        div()
            .flex()
            .flex_col()
            .child(row)
            .child(crate::fold::fold(
                ("tool-fold", index),
                tool.expanded,
                false,
                || match hunks {
                    Some(hunks) => changes_body(&hunks, &found, draft, index, palette, zoom),
                    None => tool_body(tool, index, palette, zoom).into_any_element(),
                },
                window,
                cx,
            ))
            .into_any_element()
    }

    fn header(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let claude = self.session().is_some_and(Session::claude);
        // A draft that continues no session can move to the other agent.
        let switchable = self
            .session()
            .is_some_and(|session| session.draft.meta.source.is_none());
        let busy = self.busy();
        let detail = if claude {
            Some((
                "No project settings".to_owned(),
                "Claude Code runs this draft without project settings, CLAUDE.md, hooks or MCP \
                 servers, and may edit only the draft."
                    .to_owned(),
            ))
        } else {
            self.session().and_then(|session| {
                let dir = &session.draft.meta.pi()?.agent_dir;
                profile_label(dir, std::env::home_dir().as_deref())
                    .map(|label| (label, dir.display().to_string()))
            })
        };
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
                div()
                    .id("agent")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        svg()
                            .path(if claude { CLAUDE_CODE_ICON } else { PI_ICON })
                            // The starburst's thin rays need a little more
                            // room than the pi mark's solid blocks to look
                            // the same size.
                            .when(claude, |icon| icon.size(px(14.)))
                            .when(!claude, |icon| icon.size(px(12.)))
                            .flex_none()
                            .text_color(palette.muted),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.muted)
                            .child(if claude { "Claude Code" } else { "Pi" }),
                    )
                    .when(switchable, |agent| {
                        agent
                            .ml(px(-4.))
                            .mr(px(-4.))
                            .px(px(4.))
                            .h(px(22.))
                            .rounded(px(5.))
                            .tooltip(crate::tooltip::text(if busy {
                                "Can't switch agents while the agent is working"
                            } else if claude {
                                "Switch to Pi"
                            } else {
                                "Switch to Claude Code"
                            }))
                            .when(!busy, |agent| {
                                agent
                                    .hover(|style| style.bg(palette.hover))
                                    .active(|style| style.opacity(0.8))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.switch_agent(window, cx)
                                    }))
                            })
                    }),
            )
            .child(div().flex_1())
            .when_some(detail, |header, (label, path)| {
                header.child(
                    div()
                        .id("agent-detail")
                        .min_w_0()
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(palette.muted)
                        .text_ellipsis()
                        .tooltip(crate::tooltip::text(path))
                        .child(label),
                )
            })
            .children(self.context_chip(palette, cx))
    }

    /// The conversation's size, which opens the popover that compacts it;
    /// none until the agent can be asked.
    fn context_chip(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session()?;
        let Some(Ok(_)) = &session.agent else {
            return None;
        };
        let transcript = &session.transcript;
        let label: SharedString = match transcript.context {
            _ if transcript.compacting => "Compacting…".into(),
            Some(tokens) => short_tokens(tokens).into(),
            None => "Compact".into(),
        };
        let open = self.context_open;
        let chip = div()
            .id("context")
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .h(px(20.))
            .px(px(6.))
            .rounded(px(5.))
            .text_size(px(11.))
            .text_color(palette.muted)
            .cursor_pointer()
            .hover(|style| style.bg(palette.hover))
            .when(open, |chip| chip.bg(palette.hover))
            .active(|style| style.opacity(0.8))
            .when(!open, |chip| {
                chip.tooltip(crate::tooltip::text(
                    "Conversation size. Click to compact it.",
                ))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.context_open = !this.context_open;
                    cx.notify();
                }),
            )
            .child(label)
            .when(open, |chip| {
                chip.child(
                    div().absolute().top_full().right_0().mt_1().child(deferred(
                        anchored()
                            .anchor(Anchor::TopRight)
                            .snap_to_window_with_margin(px(8.))
                            .child(self.context_popover(palette, cx)),
                    )),
                )
            });
        Some(chip.into_any_element())
    }

    fn context_popover(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let transcript = self.session().map(|session| &session.transcript);
        let context = transcript.and_then(|transcript| transcript.context);
        let compacting = transcript.is_some_and(|transcript| transcript.compacting);
        let busy = self.busy();
        let size: AnyElement = match context {
            Some(tokens) => div()
                .child(
                    StyledText::new(format!("{} tokens in context", grouped(tokens)))
                        .with_highlights([(
                            0..grouped(tokens).len(),
                            HighlightStyle {
                                font_weight: Some(FontWeight::SEMIBOLD),
                                color: Some(palette.text),
                                ..HighlightStyle::default()
                            },
                        )]),
                )
                .into_any_element(),
            None => div()
                .child("The size shows after the agent's next reply.")
                .into_any_element(),
        };
        let button_label = if compacting {
            "Compacting…"
        } else {
            "Compact"
        };
        div()
            .id("context-popover")
            .occlude()
            .w(px(260.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .bg(palette.menu)
            .rounded(px(8.))
            .border_1()
            .border_color(palette.hairline)
            .shadow_lg()
            .text_size(px(12.))
            .line_height(px(17.))
            .text_color(palette.muted)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.context_open = false;
                cx.notify();
            }))
            .child(size)
            .child(div().text_color(palette.faint).child(
                "Replaces the earlier conversation with a summary of what the message \
                     needs and how you want it written.",
            ))
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("compact")
                        .h(px(24.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .text_color(palette.on_accent)
                        .bg(palette.accent)
                        .child(button_label)
                        .when(busy, |button| {
                            button.opacity(0.5).when(!compacting, |button| {
                                button.tooltip(crate::tooltip::text("Waiting for the reply"))
                            })
                        })
                        .when(!busy, |button| {
                            button
                                .cursor_pointer()
                                .hover(|style| style.opacity(0.9))
                                .active(|style| style.opacity(0.8))
                                .on_click(cx.listener(|this, _, _, cx| this.compact(cx)))
                        }),
                ),
            )
    }

    /// The footer's count of attached selections, which lists their quotes
    /// above it when clicked.
    fn quotes_chip(
        &self,
        quotes: &[Quote],
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let listed = self.quotes_listed.get();
        let list = listed.then(|| {
            div().absolute().top_0().right_0().child(deferred(
                anchored()
                    .anchor(Anchor::BottomRight)
                    .offset(point(px(6.), px(-8.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("quote-list")
                            .occlude()
                            .w(px(360.))
                            .pb(px(4.))
                            .rounded(COMPOSER_RADIUS)
                            .border_1()
                            .border_color(palette.line_strong)
                            .bg(palette.control)
                            .shadow(vec![
                                crate::chrome::shadow(theme::hex(0x00000033), 0., 0., 0.5),
                                crate::chrome::shadow(theme::hex(0x00000029), 6., 18., 0.),
                            ])
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.quotes_listed.set(false);
                                cx.notify();
                            }))
                            .child(self.quote_stack(quotes, palette, cx)),
                    ),
            ))
        });
        div()
            .id("quotes-chip")
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(20.))
            .px(px(6.))
            .rounded(px(5.))
            .border_1()
            .border_color(palette.hairline)
            .bg(if listed {
                palette.control_hover
            } else {
                palette.hover
            })
            .text_color(palette.text)
            .hover(|style| style.bg(palette.control_hover))
            .active(|style| style.opacity(0.8))
            .tooltip(crate::tooltip::text("Show the attached selections"))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.quotes_listed.set(!this.quotes_listed.get());
                cx.notify();
            }))
            .child(
                svg()
                    .path(QUOTE_ICON)
                    .size(px(11.))
                    .flex_none()
                    .text_color(palette.faint),
            )
            .child(match quotes.len() {
                1 => "1 selection".to_string(),
                n => format!("{n} selections"),
            })
            .children(list)
    }

    /// The attached selections' quotes, one row each, so every one stays
    /// visible and removable while writing.
    fn quote_stack(
        &self,
        quotes: &[Quote],
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ranges: Vec<_> = quotes.iter().map(|quote| quote.range.clone()).collect();
        // The only selection is the editor's own, which detaching keeps
        // selected; extra ones are dropped from the editor.
        let single = quotes.len() == 1;
        let head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(18.))
            .pl(px(5.))
            .pr(px(2.))
            .pb(px(2.))
            .text_size(px(11.))
            .text_color(palette.muted)
            .child(match quotes.len() {
                1 => "1 selection".to_string(),
                n => format!("{n} selections"),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(palette.faint)
                    .when(!single, |hint| hint.child("pi edits each one")),
            )
            .child(
                div()
                    .id("clear-selections")
                    .flex_none()
                    .px(px(5.))
                    .rounded(px(4.))
                    .hover(|style| style.bg(palette.control_hover).text_color(palette.text))
                    .active(|style| style.opacity(0.8))
                    .tooltip(crate::tooltip::text(if single {
                        "Don't attach the selection"
                    } else {
                        "Don't attach these selections"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if single {
                            this.detach(cx)
                        } else {
                            this.drop_selections(&ranges, cx)
                        }
                    }))
                    .child("Clear"),
            );
        let rows = quotes.iter().enumerate().map(|(index, quote)| {
            let range = quote.range.clone();
            div()
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
                .child(
                    quote_label(quote, palette.faint, palette.text, 1.)
                        .flex_1()
                        .justify_between(),
                )
                .child(
                    close_button(
                        ("detach-quote", index),
                        "Don't attach this selection",
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if single {
                            this.detach(cx)
                        } else {
                            this.drop_selections(std::slice::from_ref(&range), cx)
                        }
                    })),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(6.))
            .pt(px(6.))
            .pb(px(2.))
            .child(head)
            .children(rows)
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
        let quotes = self.quotes(&self.shown, cx);
        let agent = session.map_or("pi", Session::agent_name);
        let placeholder: SharedString = match quotes.len() {
            0 => format!("Ask {agent} to change the draft, or ask about the session…").into(),
            1 => format!("Ask {agent} to change the selection, or ask about it…").into(),
            n => format!("Tell {agent} what to do with these {n} selections…").into(),
        };
        // The quotes sit in a chip in the footer, listed above it on
        // demand, so attaching a selection neither resizes the transcript nor
        // covers it.
        if quotes.is_empty() {
            self.quotes_listed.set(false);
        }
        let chip = (!quotes.is_empty()).then(|| self.quotes_chip(&quotes, &palette, cx));
        // The placeholder sits where the composer's first line would, at its
        // size.
        let zoom = theme::get(cx).zoom;
        let style = theme::zoom_editor(composer_style(&palette), zoom);
        div().flex_none().px(px(12.)).pt(px(8.)).pb(px(12.)).child(
            div()
                .key_context(KEY_CONTEXT)
                .on_action(cx.listener(|this, _: &Send, _, cx| this.send(cx)))
                .on_action(cx.listener(|this, _: &Stop, _, cx| this.stop(cx)))
                .rounded(COMPOSER_RADIUS)
                .border_1()
                .border_color(palette.line_strong)
                .bg(palette.control)
                .child(
                    div()
                        .relative()
                        .child(self.composer.clone())
                        .when(empty, |input| {
                            input.child(
                                div()
                                    .absolute()
                                    .top(style.padding_top)
                                    .left(style.padding_x)
                                    .text_size(style.font_size)
                                    .line_height(style.line_height)
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
                        .child(div().flex_1().min_w_0().overflow_hidden().children(model))
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
                                    let hovered = state
                                        .background(&palette)
                                        .blend(palette.on_accent.opacity(0.15));
                                    button
                                        .hover(move |style| style.bg(hovered))
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
            .child(self.header(&palette, cx))
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
fn message_style(theme: &Theme) -> MarkupStyle {
    let mut style = MarkupStyle::github(theme.dark);
    style.text_size = px(13.);
    style.block_gap = px(8.);
    style.heading_gap = px(12.);
    style.code_padding = px(10.);
    style.zoomed(theme.zoom)
}

/// The corner radius of the composer and the quote stack above it.
const COMPOSER_RADIUS: gpui::Pixels = px(10.);

/// The composer's input: the UI font at chat size.
fn composer_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: font(editor::platform::ui_font()),
        font_size: px(13.),
        line_height: px(13. * 1.45),
        padding_x: px(11.),
        padding_top: px(9.),
        padding_bottom: px(11.),
        text: palette.text,
        heading: palette.text,
        grow_rows: Some((2, 8)),
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
        read_only_text: Some(palette.on_accent),
        heading: palette.on_accent,
        selection: theme::hex(0xffffff59),
        inactive_selection: theme::hex(0xffffff33),
        fit_content: true,
        ..palette.editor_style()
    }
}

/// The × that detaches a quote.
fn close_button(
    id: impl Into<gpui::ElementId>,
    tooltip: &'static str,
    palette: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(18.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .hover(|style| style.bg(palette.control_hover))
        .active(|style| style.opacity(0.8))
        .tooltip(crate::tooltip::text(tooltip))
        .child(
            svg()
                .path(CLOSE_ICON)
                .size(px(10.))
                .text_color(palette.muted),
        )
}

/// The quote icon, the start of the selected text and its lines, on one
/// line: `faint` for the icon and lines, `text` for the excerpt.
fn quote_label(quote: &Quote, faint: Hsla, text: Hsla, zoom: f32) -> gpui::Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(5. * zoom))
        .text_size(px(11.5 * zoom))
        .line_height(px(16. * zoom))
        .child(
            svg()
                .path(QUOTE_ICON)
                .size(px(12. * zoom))
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

fn tool_icon(kind: &ToolKind) -> &'static str {
    match kind {
        ToolKind::Bash { .. } => TOOL_BASH_ICON,
        ToolKind::Read { .. } => TOOL_READ_ICON,
        ToolKind::Edit { .. } => TOOL_EDIT_ICON,
        ToolKind::Write { .. } => TOOL_WRITE_ICON,
        _ => TOOL_OTHER_ICON,
    }
}

/// `path` as the model wrote it, shown relative to `cwd`; the draft itself
/// is `draft.md`.
fn display_path(path: &str, cwd: &Path, draft_path: &Path, home: Option<&Path>) -> String {
    let absolute = absolute_path(path, cwd, home);
    if same_path(&absolute, draft_path) {
        return "draft.md".into();
    }
    match absolute.strip_prefix(cwd) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => crate::drafts::tilde(&absolute, home),
    }
}

/// `path` as the model wrote it, made absolute against `cwd` or `~`.
fn absolute_path(path: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => cwd.join(path),
    }
}

/// Whether `a` and `b` name the same file. macOS volumes ignore case by
/// default, so an agent can reach the draft as `…/draftmux/…` as well as
/// `…/Draftmux/…`.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(target_os = "macos") {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// How long the brightness wave takes to cross the activity label.
pub(crate) const WAVE_PERIOD: f32 = 1.8;
/// How far behind its predecessor each letter of the label brightens.
const WAVE_STAGGER: f32 = 0.045;

/// A streamed reply occupies the waiting slot itself. Tools have their own
/// spinner; after a tool finishes, show that pi is thinking again.
fn needs_activity_row(starting: bool, transcript: &Transcript) -> bool {
    starting
        || transcript.compacting
        || (transcript.busy
            && match transcript.items.last() {
                Some(Item::Assistant { .. }) => false,
                Some(Item::Tool(tool)) => !tool.running(),
                _ => true,
            })
}

/// The waiting slot before a visible reply replaces it.
fn activity_row(session: &Session, palette: &Palette, zoom: f32) -> AnyElement {
    let text: SharedString = if session.starting() {
        format!("Starting {}…", session.agent_name()).into()
    } else if session.transcript.compacting {
        "Compacting the conversation…".into()
    } else {
        "Thinking…".into()
    };
    // Inset like the messages: the item padding plus their own 2px.
    let row = div()
        .pt(px(10. * zoom))
        .pb(px(10. * zoom))
        .px(px(16.))
        .text_size(px(13. * zoom))
        .line_height(px(19.5 * zoom))
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
            row.child(StyledText::new(text.clone()).with_highlights(highlights))
        },
    )
    .into_any_element()
}

/// The wave's brightness at `time` seconds: rises over the first 15% of
/// each period, settles by 40%, then rests.
pub(crate) fn wave(time: f32) -> f32 {
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

/// A grey ring with a turning accent arc, 11px unless resized with `.size`.
pub(crate) fn spinner(palette: &Palette) -> gpui::Div {
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

fn stat(text: String, color: Hsla, background: Hsla, zoom: f32) -> impl IntoElement {
    div()
        .px(px(4. * zoom))
        .rounded(px(4. * zoom))
        .font_family(MONO)
        .text_size(px(11. * zoom))
        .line_height(px(16. * zoom))
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

/// A token count as the context chip shows it, such as `84k`.
fn short_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..999_500 => format!("{}k", (tokens + 500) / 1_000),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// A token count with thousands separated, such as `84,212`.
fn grouped(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn format_duration(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f64())
    }
}

/// The summary a draft started from: a notice that expands to show it.
fn summary_row(
    text: &str,
    expanded: bool,
    index: usize,
    palette: &Palette,
    zoom: f32,
    window: &mut Window,
    cx: &mut Context<Chat>,
) -> AnyElement {
    let chevron = svg()
        .path(CHEVRON_ICON)
        .size(px(10. * zoom))
        .text_color(palette.faint);
    let label = div()
        .id(("summary", index))
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .text_size(px(12. * zoom))
        .text_color(palette.faint)
        .cursor_pointer()
        .hover(|style| style.text_color(palette.muted))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
        .child("Started from a summary")
        .child(if expanded {
            chevron.with_transformation(Transformation::rotate(percentage(0.25)))
        } else {
            chevron
        });
    let text = text.to_owned();
    let line = palette.hairline;
    let muted = palette.muted;
    div()
        .flex()
        .flex_col()
        .child(label)
        .child(crate::fold::fold(
            ("summary-fold", index),
            expanded,
            false,
            move || {
                div()
                    .mt_2()
                    .px(px(10. * zoom))
                    .py(px(8. * zoom))
                    .rounded(px(6. * zoom))
                    .border_1()
                    .border_color(line)
                    .text_size(px(12. * zoom))
                    .line_height(px(18. * zoom))
                    .text_color(muted)
                    .whitespace_normal()
                    .child(text)
                    .into_any_element()
            },
            window,
            cx,
        ))
        .into_any_element()
}

/// The expanded row: output keeps its whitespace and scrolls sideways.
fn tool_body(tool: &Tool, index: usize, palette: &Palette, zoom: f32) -> impl IntoElement {
    let lines: Vec<AnyElement> = tool
        .body()
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let command = matches!(tool.kind, ToolKind::Bash { .. }) && i == 0;
            div()
                .px(px(10. * zoom))
                .whitespace_nowrap()
                .when(command, |line| {
                    line.text_color(palette.faint).mb(px(4. * zoom))
                })
                .child(blank_as_space(line))
                .into_any_element()
        })
        .collect();
    div()
        .id(("tool-body", index))
        .ml(px(24. * zoom))
        .my(px(4. * zoom))
        .py(px(8. * zoom))
        .max_h(px(200. * zoom))
        .overflow_scroll()
        .rounded(px(7. * zoom))
        .bg(palette.tool_body)
        .font_family(MONO)
        .text_size(px(11.5 * zoom))
        .line_height(px(17. * zoom))
        .text_color(palette.tool_text)
        .child(div().flex().flex_col().min_w_full().children(lines))
}

/// An expanded edit row: each change with a few words around it. Clicking
/// one shows it in the draft, when the draft still has the edited text.
fn changes_body(
    hunks: &[EditHunk],
    found: &[Option<usize>],
    draft: Entity<Editor>,
    index: usize,
    palette: &Palette,
    zoom: f32,
) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    for (h, hunk) in hunks.iter().enumerate() {
        let at = found.get(h).copied().flatten();
        for change in &hunk.changes {
            let number = rows.len() + 1;
            let row = div()
                .id(("tool-change", rows.len()))
                .flex()
                .gap(px(8. * zoom))
                .py(px(6. * zoom))
                .pl(px(10. * zoom))
                .pr(px(12. * zoom))
                .when(number > 1, |row| {
                    row.border_t_1().border_color(palette.hairline)
                })
                .child(
                    div()
                        .w(px(14. * zoom))
                        .flex_none()
                        .text_size(px(11. * zoom))
                        .text_color(palette.faint)
                        .child(number.to_string()),
                )
                .child(div().min_w_0().child(change_text(change, palette)));
            let row = match at {
                Some(at) => {
                    let target = at + change.target.start..at + change.target.end;
                    let draft = draft.clone();
                    row.hover(|style| style.bg(palette.hover))
                        .active(|style| style.opacity(0.8))
                        .tooltip(crate::tooltip::text("Show in draft"))
                        .on_click(move |_, _, cx| {
                            draft.update(cx, |editor, cx| {
                                editor.reveal(target.clone(), cx);
                                editor.flash(vec![target.clone()], cx);
                            });
                        })
                        .into_any_element()
                }
                None => row.into_any_element(),
            };
            rows.push(row);
        }
    }
    if rows.is_empty() {
        rows.push(
            div()
                .px(px(10. * zoom))
                .py(px(6. * zoom))
                .text_color(palette.faint)
                .child("Only whitespace changed")
                .into_any_element(),
        );
    }
    div()
        .id(("tool-body", index))
        .ml(px(24. * zoom))
        .my(px(4. * zoom))
        .py(px(2. * zoom))
        .max_h(px(200. * zoom))
        .overflow_y_scroll()
        .rounded(px(7. * zoom))
        .bg(palette.tool_body)
        .text_size(px(12.5 * zoom))
        .line_height(px(19. * zoom))
        .text_color(palette.muted)
        .children(rows)
        .into_any_element()
}

/// A change with its context: removed text struck through, added text
/// tinted, and an arrow between them when there are both.
fn change_text(change: &EditChange, palette: &Palette) -> StyledText {
    let mut text = change.before.clone();
    let mut highlights = Vec::new();
    let mut push = |text: &mut String, part: &str, style: HighlightStyle| {
        let start = text.len();
        text.push_str(part);
        highlights.push((start..text.len(), style));
    };
    if !change.removed.is_empty() {
        push(
            &mut text,
            &change.removed,
            HighlightStyle {
                color: Some(palette.diff_removed),
                strikethrough: Some(gpui::StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(palette.removed.opacity(0.55)),
                }),
                ..Default::default()
            },
        );
    }
    if !change.removed.is_empty() && !change.added.is_empty() {
        push(
            &mut text,
            " → ",
            HighlightStyle {
                color: Some(palette.faint),
                ..Default::default()
            },
        );
    }
    if !change.added.is_empty() {
        push(
            &mut text,
            &change.added,
            HighlightStyle {
                color: Some(palette.diff_added),
                background_color: Some(palette.added_bg),
                ..Default::default()
            },
        );
    }
    text.push_str(&change.after);
    StyledText::new(text).with_highlights(highlights)
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

/// Keeps `record` in a Claude draft's `backend`.
pub fn record(backend: &mut Backend, record: &Record) {
    let Backend::Claude(claude) = backend else {
        return;
    };
    match record {
        Record::Transcript(path) => claude.transcript = Some(path.clone()),
        Record::Model(model) => claude.model = Some(model.clone()),
        Record::Effort(effort) => claude.effort = effort.clone(),
    }
}

/// What a draft's agent loop receives.
enum Incoming {
    Event(Event),
    Record(Record),
}

fn pi_agent(
    draft: &Draft,
    pi_session: &PiSession,
    env: &Arc<ShellEnv>,
) -> Result<(Arc<dyn Agent>, Events), String> {
    let pi = env.pi().map_err(|err| err.to_string())?.to_path_buf();
    let session_file = pi_session
        .session_file
        .clone()
        .ok_or("This draft has no pi session yet.")?;
    let extension = crate::resources::pi_extension().map_err(|err| format!("{err:#}"))?;
    let env = env.clone();
    let spec = LaunchSpec {
        command: Arc::new(move || env.command(&pi)),
        session_file,
        extension,
        cwd: draft.meta.cwd.clone(),
        agent_dir: pi_session.agent_dir.clone(),
        draft_path: draft.draft_path(),
        instructions: draft.meta.settings.instructions.clone(),
        summary: summary_message(draft),
        context_files: draft.meta.settings.context_files,
    };
    let (agent, events) = PiAgent::new(spec, IDLE_TIMEOUT, REQUEST_TIMEOUT);
    Ok((Arc::new(agent), events))
}

/// The `claude` on the login shell's PATH, once its version is supported.
/// Blocks while it runs claude.
pub(crate) fn claude_program(env: &ShellEnv) -> Result<PathBuf, String> {
    let program = env.claude().map_err(|err| err.to_string())?.to_path_buf();
    check_claude(&program, env.vars())?;
    Ok(program)
}

/// Fails unless `program`'s Claude Code version is supported. Blocks while
/// it runs claude with `env`'s allowlisted variables. Only a supported
/// version is remembered, so a failed check is tried again next time.
pub(crate) fn check_claude(program: &Path, env: &[(OsString, OsString)]) -> Result<(), String> {
    static SUPPORTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    if !SUPPORTED
        .lock()
        .unwrap()
        .iter()
        .any(|known| known == program)
    {
        let version = claude_version(program, env)?;
        eprintln!("draftmux: Claude Code {version} at {}", program.display());
        SUPPORTED.lock().unwrap().push(program.to_owned());
    }
    Ok(())
}

/// `program`'s Claude Code version when it is supported, else a message for
/// the user. Blocks while it runs claude with `env`'s allowlisted variables.
pub(crate) fn claude_version(
    program: &Path,
    env: &[(OsString, OsString)],
) -> Result<String, String> {
    let command = draftmux_claude_rpc::version_command(program, env);
    let output = crate::process::run("Claude Code", command, None, VERSION_TIMEOUT, None)?;
    draftmux_claude_rpc::check_version(program, &output)
}

/// `draft`'s Claude Code session, from its descriptor.
pub(crate) fn draft_session(draft: &Draft, claude: &ClaudeSession) -> DraftSession {
    DraftSession {
        id: claude.session_id.to_string(),
        dir: draft.session_dir(),
        config_dir: claude.config_dir.clone(),
        fork_boundary: claude.fork.as_ref().map(|fork| fork.boundary.to_string()),
        transcript: claude.transcript.clone(),
    }
}

fn claude_agent(
    draft: &Draft,
    claude: &ClaudeSession,
    env: &ShellEnv,
    program: PathBuf,
) -> Result<(Arc<dyn Agent>, Events, Records), String> {
    let spec = draftmux_claude_rpc::LaunchSpec {
        program,
        cwd: draft.meta.cwd.clone(),
        env: env.vars().to_vec(),
        extra_env: Vec::new(),
        model: claude.model.clone(),
        effort: claude.effort.clone(),
        tools: Vec::new(),
        setting_sources: Vec::new(),
        session: Some(draftmux_claude_rpc::Session::Draft(draft_session(
            draft, claude,
        ))),
        hooks: Vec::new(),
        permission_prompt: false,
        system_prompt: None,
    };
    let context = DraftContext {
        draft_path: draft.draft_path(),
        instructions: draft.meta.settings.instructions.clone().unwrap_or_default(),
        target: Box::new(target_syntax(draft)),
        summary: summary_message(draft),
    };
    let (agent, events, records) = ClaudeAgent::new(spec, CLAUDE_CONFIG, context)?;
    Ok((Arc::new(agent), events, records))
}

/// For a draft started from a summary, the message that tells its agent so.
fn summary_message(draft: &Draft) -> Option<String> {
    let summary = draft.meta.summary.as_deref()?;
    let source = draft.meta.source.as_ref()?;
    Some(draftmux_agent::summary_message(summary, &source.path))
}

/// Reads `draft`'s target from its `meta.json`, which the app keeps
/// current, as the pi extension does; the last one read if that fails.
fn target_syntax(draft: &Draft) -> impl Fn() -> Option<String> + std::marker::Send + Sync + use<> {
    let store = draft.dir.parent().map(Store::new);
    let id = draft.id.clone();
    let last = Mutex::new(draft.meta.target);
    move || {
        let mut last = last.lock().unwrap();
        if let Some(Ok(loaded)) = store.as_ref().map(|store| store.load(&id)) {
            *last = loaded.meta.target;
        }
        Some(crate::selection::syntax(*last).to_owned())
    }
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
    fn token_counts_are_short_in_the_chip_and_grouped_in_the_popover() {
        assert_eq!(short_tokens(812), "812");
        assert_eq!(short_tokens(84_212), "84k");
        assert_eq!(short_tokens(999_499), "999k");
        assert_eq!(short_tokens(1_260_000), "1.3M");
        assert_eq!(grouped(84_212), "84,212");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }

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
        let mut transcript = Transcript::default();
        let apply = |transcript: &mut Transcript, event| {
            transcript.apply(&event, None, Instant::now());
        };
        let rows = |transcript: &Transcript| {
            transcript.items.len() + usize::from(needs_activity_row(false, transcript))
        };
        assert!(needs_activity_row(true, &transcript));
        assert!(!needs_activity_row(false, &transcript));
        transcript.sent("hello".into(), String::new());
        assert!(needs_activity_row(false, &transcript));
        assert_eq!(rows(&transcript), 2);
        apply(&mut transcript, Event::AssistantStarted);
        for delta in ["Hello", " there", "."] {
            apply(&mut transcript, Event::AssistantText(delta.into()));
            assert!(!needs_activity_row(false, &transcript));
            assert_eq!(rows(&transcript), 2);
        }
        apply(
            &mut transcript,
            Event::AssistantEnded {
                text: "Hello there.".into(),
                stop: draftmux_agent::Stop::Completed,
            },
        );
        assert_eq!(rows(&transcript), 2);
        apply(&mut transcript, Event::RunSettled { aborted: false });
        assert_eq!(rows(&transcript), 2);
        assert!(!needs_activity_row(false, &transcript));
        transcript.sent("again".into(), String::new());
        assert!(needs_activity_row(false, &transcript));
        assert_eq!(rows(&transcript), 4);
    }

    #[test]
    fn running_tools_own_the_activity_indicator() {
        let mut transcript = Transcript::default();
        transcript.sent("hello".into(), String::new());
        transcript.apply(
            &Event::ToolStarted(draftmux_agent::ToolCall {
                id: "read-1".into(),
                kind: ToolKind::Read {
                    path: "draft.md".into(),
                },
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
    fn paths_are_shown_relative_to_the_cwd() {
        let cwd = Path::new("/repo");
        let draft = Path::new("/data/d1/draft.md");
        let home = Some(Path::new("/Users/me"));
        assert_eq!(display_path("src/a.rs", cwd, draft, home), "src/a.rs");
        assert_eq!(display_path("/repo/src/a.rs", cwd, draft, home), "src/a.rs");
        assert_eq!(
            display_path("/data/d1/draft.md", cwd, draft, home),
            "draft.md"
        );
        assert_eq!(display_path("~/notes.md", cwd, draft, home), "~/notes.md");
        assert_eq!(display_path("/etc/hosts", cwd, draft, home), "/etc/hosts");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_draft_is_recognized_in_any_case_on_macos() {
        let draft = Path::new("/Users/me/Library/Application Support/Draftmux/drafts/d1/draft.md");
        assert_eq!(
            display_path(
                "/Users/me/Library/Application Support/draftmux/drafts/d1/draft.md",
                Path::new("/repo"),
                draft,
                Some(Path::new("/Users/me")),
            ),
            "draft.md"
        );
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
