//! The chat column: the selected draft's conversation with its pi agent,
//! drawn on gpui's `list()` with tool calls as compact rows (docs/design.md,
//! Preview and chat rendering), and the composer.
//!
//! Each draft gets its own [`Agent`], started when the draft is selected
//! and stopped once it is idle or left. The chat reports to its owner
//! through [`ChatEvent`]: the owner saves the draft before a prompt, reloads
//! it after the agent writes it, and locks the editor while the agent runs.

mod transcript;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use draftmux_pi_rpc::{Agent, AgentEvent, Disposition, LaunchSpec};
use draftmux_storage::{Draft, SourceKind, Target};
use editor::{Editor, EditorStyle};
use futures::StreamExt;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FollowMode, FontWeight, Hsla, KeyBinding, ListAlignment, ListState, Task,
    Transformation, Window, actions, div, font, list, percentage, prelude::*, px, relative, svg,
};
use shell_env::ShellEnv;

use crate::assets::{
    CHEVRON_ICON, SEND_ICON, SPINNER_ARC_ICON, SPINNER_TRACK_ICON, STOP_ICON, TOOL_BASH_ICON,
    TOOL_EDIT_ICON, TOOL_OTHER_ICON, TOOL_READ_ICON, TOOL_WRITE_ICON,
};
use crate::chat_model::ChatModel;
use crate::markup_view::{self, MarkupStyle};
use crate::profiles::ProfileCatalog;
use crate::theme::{self, Palette, Theme};
use transcript::{DiffLine, Item, Tool, Transcript};

actions!(chat, [Send, Stop]);

/// How long an idle pi process is kept for the selected draft.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const KEY_CONTEXT: &str = "Composer";

const MONO: &str = "Menlo";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // Later bindings win at the same depth, so this replaces the
        // editor's newline; shift-enter still inserts one.
        KeyBinding::new("enter", Send, Some("Composer > Editor")),
        KeyBinding::new("escape", Stop, Some(KEY_CONTEXT)),
    ]);
}

pub enum ChatEvent {
    /// The user sent `text`. Save the draft, then pass both to
    /// [`Chat::prompt`].
    Send(String),
    /// The agent wrote the selected draft; this is its new text.
    DraftWritten(String),
    /// The selected draft's agent started or finished a run.
    Running(bool),
}

impl EventEmitter<ChatEvent> for Chat {}

pub struct Chat {
    sessions: HashMap<String, Session>,
    current: Option<String>,
    composer: Entity<Editor>,
    style: MarkupStyle,
    /// Wakes up when the selected draft's idle pi should stop.
    idle: Task<()>,
}

struct Session {
    draft: Draft,
    /// `Err` says why the draft can't talk to pi.
    agent: Option<Result<Arc<Agent>, String>>,
    transcript: Transcript,
    list: ListState,
    /// Shows and switches the model of the running agent.
    model: Option<Entity<ChatModel>>,
    history_loaded: bool,
    /// The target changed while a run was going; start a new process once
    /// it settles, since the extension reads the target at startup.
    respawn: bool,
    _events: Task<()>,
}

impl Focusable for Chat {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Chat {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let composer = theme::editor(composer_style, cx);
        cx.observe(&composer, |_, _, cx| cx.notify()).detach();
        cx.observe_global::<Theme>(|this, cx| this.style = message_style(theme::get(cx).dark))
            .detach();
        Self {
            sessions: HashMap::new(),
            current: None,
            composer,
            style: message_style(theme::get(cx).dark),
            idle: Task::ready(()),
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
        self.current = id.clone();
        self.idle = Task::ready(());
        if let (Some(id), Some(draft)) = (id, draft) {
            match self.sessions.get_mut(&id) {
                Some(session) => {
                    session.draft = draft.clone();
                    self.focus_agent(&id, cx);
                }
                None => {
                    let list = ListState::new(0, ListAlignment::Bottom, px(600.));
                    list.set_follow_mode(FollowMode::Tail);
                    self.sessions.insert(
                        id.clone(),
                        Session {
                            draft: draft.clone(),
                            agent: None,
                            transcript: Transcript::new(),
                            list,
                            model: None,
                            history_loaded: false,
                            respawn: false,
                            _events: Task::ready(()),
                        },
                    );
                    self.start_agent(&id, cx);
                }
            }
        }
        cx.emit(ChatEvent::Running(self.busy()));
        cx.notify();
    }

    /// The draft's target changed. pi reads it at startup, so the process
    /// is replaced, after the current run if there is one.
    pub fn set_target(&mut self, id: &str, target: Target, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        session.draft.meta.target = target;
        if session.transcript.busy {
            session.respawn = true;
        } else {
            self.start_agent(id, cx);
        }
    }

    /// Creates the draft's agent once the shell environment is known, then
    /// focuses it if the draft is still selected.
    fn start_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        // Dropping the old agent stops its pi.
        session.agent = None;
        session.respawn = false;
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
                target: draft.meta.target,
                instructions: draft.meta.settings.instructions.clone(),
                context_files: draft.meta.settings.context_files,
            })
        })();
        let (agent, events) = match spec {
            Ok(spec) => {
                let (agent, events) = Agent::new(spec, IDLE_TIMEOUT);
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
            cx.new(|_| ChatModel::new(agent, profile_name))
        });
        session.agent = Some(agent);
        if self.current.as_deref() == Some(id) {
            self.focus_agent(id, cx);
        }
        cx.notify();
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
        self.schedule_idle(cx);
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
        let messages = match messages {
            Ok(messages) => messages,
            Err(err) => {
                eprintln!("draftmux: loading the chat history: {err}");
                return;
            }
        };
        let (mut items, earlier) = transcript::history(&messages);
        if earlier > 0 {
            let source = match session.draft.meta.source.kind {
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

    fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_owned();
        if text.is_empty() || self.busy() || self.session().is_none() {
            return;
        }
        self.composer
            .update(cx, |editor, cx| editor.set_text("", cx));
        cx.emit(ChatEvent::Send(text));
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
        session
            .transcript
            .apply(&event, written.as_deref(), Instant::now());
        if session.respawn && !session.transcript.busy {
            self.start_agent(id, cx);
        }
        if self.current.as_deref() == Some(id)
            && let Some(text) = written
        {
            cx.emit(ChatEvent::DraftWritten(text));
        }
        self.after_change(id, cx);
    }

    /// Updates the list, the owner and the idle timer after `id`'s
    /// transcript changed.
    fn after_change(&mut self, id: &str, cx: &mut Context<Self>) {
        self.sync_list(id, cx);
        if self.current.as_deref() == Some(id) {
            cx.emit(ChatEvent::Running(self.busy()));
            self.schedule_idle(cx);
        }
    }

    fn sync_list(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let old_len = session.list.item_count();
        if let Some((from, old_len)) = session.transcript.take_change(old_len) {
            session
                .list
                .splice(from..old_len, session.transcript.items.len() - from);
        }
        cx.notify();
    }

    fn schedule_idle(&mut self, cx: &mut Context<Self>) {
        let Some(Some(Ok(agent))) = self.session().map(|session| &session.agent) else {
            return;
        };
        let Some(deadline) = agent.deadline() else {
            return;
        };
        let agent = agent.clone();
        self.idle = cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(deadline.saturating_duration_since(Instant::now()))
                .await;
            agent.poll(Instant::now());
        });
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(id) = self.current.clone() else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(&id) {
            session.transcript.toggle(index);
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

    fn render_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = *theme::palette(cx);
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
            Item::User { text } => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.86))
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
                        .child(text.clone()),
                )
                .into_any_element(),
            Item::Assistant { doc, .. } => div()
                .px(px(2.))
                .child(markup_view::render(doc, &self.style, ("message", index)))
                .into_any_element(),
            Item::Tool(tool) => self.tool_row(tool, index, session, &palette, cx),
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
        div()
            .px(px(14.))
            .pt(if stacked { px(0.) } else { px(10.) })
            .when(index + 1 == items.len(), |item| item.pb(px(10.)))
            .child(body)
            .into_any_element()
    }

    fn tool_row(
        &self,
        tool: &Tool,
        index: usize,
        session: &Session,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                        if tool.expanded {
                            chevron.with_transformation(Transformation::rotate(percentage(0.25)))
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
                column.child(tool_body(tool, index, palette))
            })
            .into_any_element()
    }

    fn header(&self, palette: &Palette) -> impl IntoElement {
        div()
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(14.))
            .border_b_1()
            .border_color(palette.hairline)
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.faint)
                    .child("PI"),
            )
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let busy = self.busy();
        let session = self.session();
        let empty = self.composer.read(cx).text().trim().is_empty();
        let can_send = !busy && !empty && session.is_some();
        let model = session.and_then(|session| session.model.clone());
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
                                    .child("Ask pi to change the draft, or ask about the session…"),
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
                        .child(if busy {
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(spinner(&palette))
                                .child("Working…")
                        } else {
                            div().child("↵ send · ⇧↵ newline")
                        })
                        .child(
                            div()
                                .id("send")
                                .size(px(26.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .bg(if busy {
                                    palette.send_busy
                                } else if can_send {
                                    palette.accent
                                } else {
                                    palette.send_disabled
                                })
                                .text_color(palette.on_accent)
                                .child(
                                    svg()
                                        .path(if busy { STOP_ICON } else { SEND_ICON })
                                        .size(px(if busy { 10. } else { 14. }))
                                        .text_color(palette.on_accent),
                                )
                                .when(busy || can_send, |button| {
                                    button.on_click(cx.listener(move |this, _, _, cx| {
                                        if busy {
                                            this.stop(cx);
                                        } else {
                                            this.send(cx);
                                        }
                                    }))
                                }),
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
                    .child(
                        list(
                            session.list.clone(),
                            cx.processor(|this, index, _, cx| this.render_item(index, cx)),
                        )
                        .flex_1()
                        .min_h_0(),
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
        font: font(".SystemUIFont"),
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

fn tool_icon(name: &str) -> &'static str {
    match name {
        "bash" => TOOL_BASH_ICON,
        "read" => TOOL_READ_ICON,
        "edit" => TOOL_EDIT_ICON,
        "write" => TOOL_WRITE_ICON,
        _ => TOOL_OTHER_ICON,
    }
}

/// A grey ring with a turning accent arc.
fn spinner(palette: &Palette) -> impl IntoElement {
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
