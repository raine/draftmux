//! The main window: unified toolbar, translucent drafts sidebar, editor,
//! preview and chat columns.
//!
//! The window uses `WindowBackgroundAppearance::Blurred`, so anything painted
//! with alpha below 1 shows the blurred desktop. The root paints nothing; the
//! sidebar column paints a translucent tint, opaque with Reduce Transparency,
//! and the content panes are opaque.

mod preview;
mod preview_work;
mod sidebar;
mod skeleton;

use preview::Preview;

use std::ops::Range;
use std::time::{Duration, Instant};

use anyhow::Result;
use draftmux_storage::{Conflict, Store, Target};
use editor::{Editor, EditorEvent};
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Context, CursorStyle, DragMoveEvent,
    Empty, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton, Pixels,
    Point, PromptLevel, ScrollHandle, SharedString, StyleRefinement, Subscription, Task, Window,
    WindowControlArea, actions, div, ease_out_quint, point, prelude::*, px, svg,
};
use markup::Document;

use crate::app_settings::AppSettings;
use crate::assets::{EDITOR_PANE_ICON, PREVIEW_ICON};
use crate::chat::{Chat, ChatEvent};
use crate::copy::{self, Copy};
use crate::drafts::{Drafts, tilde};
use crate::file_watch;
use crate::format::{Format, Formatter};
use crate::markup_view;
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::selectable::{Scroller, SelectionChanged, TextSelection};
use crate::selection_popover::SelectionPopover;
use crate::source_map::SourceMap;
use crate::theme::{self, Palette};
use crate::update::{self, Updater};

actions!(chrome, [ToggleEditor, TogglePreview, ToggleSidebar]);

const TOOLBAR_HEIGHT: Pixels = px(52.);
const SIDEBAR_WIDTH: Pixels = px(228.);
const CHAT_WIDTH: Pixels = px(340.);
/// How long the target switch's newly selected segment takes to fade in.
const SEGMENT_FADE: Duration = Duration::from_millis(150);
const CHAT_MIN_WIDTH: Pixels = px(260.);
/// Room the chat never takes from the editor and preview.
const CONTENT_MIN_WIDTH: Pixels = px(320.);
/// How long the chat width settles before it is saved.
const CHAT_WIDTH_SAVE_DELAY: Duration = Duration::from_millis(300);
/// Origin of the close button; vertically centres the buttons in the toolbar.
pub const TRAFFIC_LIGHT_POSITION: Point<Pixels> = point(px(20.), px(19.));
/// How often relative times in the sidebar are redrawn.
const CLOCK_TICK: Duration = Duration::from_secs(30);

pub fn init(cx: &mut App) {
    sidebar::init(cx);
    cx.bind_keys([
        KeyBinding::new(editor::secondary!("\\"), ToggleSidebar, None),
        KeyBinding::new(editor::secondary!("alt-e"), ToggleEditor, None),
        KeyBinding::new(editor::secondary!("alt-p"), TogglePreview, None),
    ]);
}

/// The drag that resizes the chat column.
struct ChatResize;

pub struct Chrome {
    /// Set on toolbar mouse-down; the first mouse-move then hands the drag to
    /// the window server, so a plain click never starts a move.
    should_move: bool,
    drafts: Drafts,
    store: Store,
    chat_width: Pixels,
    save_chat_width: Task<()>,
    /// Shows the selected draft; reused across drafts.
    editor: Entity<Editor>,
    formatter: Entity<Formatter>,
    chat: Entity<Chat>,
    selection: Entity<SelectionPopover>,
    /// The selected draft's text parsed for its target.
    preview: Entity<Preview>,
    preview_work: preview_work::Work,
    preview_task: Task<()>,
    /// Source snapshot whose offsets the displayed preview uses.
    preview_source: String,
    /// Selection in the preview, mirrored to the editor as the markdown it
    /// was rendered from.
    preview_selection: Entity<TextSelection>,
    /// Lines the preview's text up with the markdown; built on demand and
    /// dropped when the preview changes.
    preview_map: Option<SourceMap>,
    /// The editor selection last set from the preview's.
    mirrored: Option<Range<usize>>,
    /// At least one of the markdown editor and the preview is shown.
    show_editor: bool,
    show_preview: bool,
    /// Why the selected draft's last save failed; cleared by the next save.
    save_error: Option<SharedString>,
    toast: Option<sidebar::Toast>,
    /// The Copy button shows a checkmark while this is set, with a serial
    /// so each copy replays it; the task clears it.
    copied: Option<(usize, Task<()>)>,
    /// The sidebar row context menu, while it is open.
    row_menu: Option<sidebar::RowMenu>,
    sidebar_scroll: ScrollHandle,
    sidebar_focus: FocusHandle,
    content_focus: FocusHandle,
    show_sidebar: bool,
    preview_scroll: ScrollHandle,
    scrollbars: [ScrollbarState; 3],
    /// Wakes up when the pending edit is due to be saved.
    autosave: Task<()>,
    /// The user is being asked whether to keep their edits over a change
    /// made to the draft's file outside the editor.
    asking_outside_edit: bool,
    _clock: Task<()>,
    _watch_draft: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Render for Chrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = editor::timing::span("chrome-render");
        let palette = *theme::palette(cx);
        let [sidebar_bar, editor_bar, preview_bar] = &self.scrollbars;
        let content = if self.drafts.current().is_some() {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .when(self.show_editor, |row| {
                    row.child(
                        pane(palette.pane, &palette)
                            .flex_1()
                            .relative()
                            .child(SelectionPopover::wrap(
                                &self.selection,
                                self.editor.clone().cached(
                                    StyleRefinement::default()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full(),
                                ),
                                cx,
                            ))
                            .child(scrollbar(Source::Editor(self.editor.clone()), editor_bar)),
                    )
                })
                .when(self.show_preview, |row| {
                    row.child(
                        pane(palette.pane, &palette)
                            .flex_1()
                            .relative()
                            .when(self.show_editor, |pane| pane.border_l_1())
                            .child({
                                let body = self.preview.clone().cached(
                                    StyleRefinement::default()
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full(),
                                );
                                SelectionPopover::wrap_preview(&self.selection, body, cx)
                            })
                            .child(scrollbar(
                                Source::Handle(self.preview_scroll.clone()),
                                preview_bar,
                            )),
                    )
                })
        } else {
            div()
                .flex_1()
                .min_w_0()
                .child(self.empty_state(&palette, cx))
        };

        let content = div()
            .size_full()
            .font_family(editor::platform::ui_font())
            .relative()
            .flex()
            .flex_col()
            .text_color(palette.text)
            .track_focus(&self.content_focus)
            .on_action(
                cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleEditor, window, cx| this.toggle_editor(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &TogglePreview, window, cx| this.toggle_preview(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Format, _, cx| {
                if this.drafts.current().is_some() {
                    this.formatter
                        .update(cx, |formatter, cx| formatter.format(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &crate::Quit, _, cx| {
                if this.flush_before_exit(cx) {
                    cx.quit();
                }
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ChatResize>, window, cx| {
                    let width = window.viewport_size().width - event.event.position.x;
                    this.resize_chat(
                        clamp_chat_width(width, window.viewport_size().width, this.show_sidebar),
                        cx,
                    );
                }),
            )
            .child(self.toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.show_sidebar, |row| {
                        row.child(self.sidebar(sidebar_bar, cx))
                    })
                    .child(content)
                    .child(
                        pane(palette.chat, &palette)
                            .w(clamp_chat_width(
                                self.chat_width,
                                window.viewport_size().width,
                                self.show_sidebar,
                            ))
                            .flex_none()
                            .relative()
                            .border_l_1()
                            .child(self.chat.clone())
                            .child(chat_resize_handle(cx)),
                    ),
            )
            .children(self.toast(cx));
        crate::platform::frame(
            content.into_any_element(),
            window,
            cx,
            Box::new(crate::Quit),
            "Draftmux",
        )
    }
}

impl Chrome {
    pub fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = theme::editor(Palette::editor_style, cx);
        let formatter = cx.new(|cx| Formatter::new(editor.clone(), cx));
        let chat = cx.new(|cx| Chat::new(editor.clone(), cx));
        let preview_scroll = ScrollHandle::new();
        let preview_selection = cx.new(|cx| {
            let mut selection = TextSelection::new(cx);
            selection.set_scroller(Some(Scroller::Handle(preview_scroll.clone())));
            // It stays selected while the user writes about it in the chat.
            selection.set_sticky(true);
            selection
        });
        let selection = cx
            .new(|cx| SelectionPopover::new(editor.clone(), preview_selection.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&preview_selection, |_, _, cx| cx.notify()),
            cx.subscribe(&preview_selection, |this, _, _: &SelectionChanged, cx| {
                this.preview_selected(cx)
            }),
            cx.subscribe(&chat, |this, _, event, cx| this.chat_event(event, cx)),
            cx.subscribe(&editor, |this, editor, event, cx| match event {
                EditorEvent::Edited => this.edited(editor.read(cx).text().to_owned(), cx),
                // Selecting something else in the editor replaces the
                // preview's selection.
                EditorEvent::SelectionChanged(range) => {
                    if this.mirrored.as_ref() != Some(range)
                        && this.preview_selection.read(cx).range().is_some()
                    {
                        this.mirrored = None;
                        this.preview_selection
                            .update(cx, |selection, cx| selection.clear(cx));
                    }
                }
            }),
            // Appearance is app-wide, and this window lives as long as the
            // app, so it reports appearance changes for every window.
            cx.observe_window_appearance(window, |_, window, cx| theme::sync_window(window, cx)),
            // Drafts created or archived by the CLI while the app was in the
            // background.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.drafts.refresh();
                    cx.notify();
                }
            }),
            cx.on_app_quit(|this, _| {
                this.flush().ok();
                async {}
            }),
            cx.observe(&Updater::global(cx), |_, _, cx| cx.notify()),
            cx.subscribe(
                &Updater::global(cx),
                |this, _, notice: &update::Notice, cx| {
                    if notice.error {
                        this.show_error(notice.text.clone(), cx);
                    } else {
                        this.show_toast(notice.text.clone(), cx);
                    }
                },
            ),
        ];
        // Closing the window releases this view before the app-quit hook
        // runs, so save while it still exists.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            this.update(cx, |this, cx| this.flush_before_exit(cx))
                .unwrap_or(true)
        });
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        // Picks up writes to the selected draft's file made outside the
        // editor: by an agent through `draftmux show --path`, or by pi's bash.
        let watch_draft = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(file_watch::POLL).await;
                if this
                    .update_in(cx, |this, window, cx| this.check_outside_edit(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let chat_width = store.chat_width().map_or(CHAT_WIDTH, px);
        let show_sidebar = !store.sidebar_hidden();
        let mut chrome = Self {
            should_move: false,
            drafts: Drafts::new(store.clone()),
            store,
            chat_width,
            save_chat_width: Task::ready(()),
            editor,
            formatter,
            chat,
            selection,
            preview: cx.new(|cx| Preview {
                document: None,
                fields: Default::default(),
                copied_field: None,
                copied_task: Task::ready(()),
                target: None,
                selection: preview_selection.clone(),
                scroll: preview_scroll.clone(),
                cache: Default::default(),
                appearance: None,
                writing: false,
                _selection: cx.observe(&preview_selection, |_, _, cx| cx.notify()),
            }),
            preview_work: preview_work::Work::default(),
            preview_task: Task::ready(()),
            preview_source: String::new(),
            preview_selection,
            preview_map: None,
            mirrored: None,
            show_editor: true,
            show_preview: true,
            save_error: None,
            toast: None,
            copied: None,
            row_menu: None,
            sidebar_scroll: ScrollHandle::new(),
            sidebar_focus: cx.focus_handle(),
            content_focus: cx.focus_handle(),
            show_sidebar,
            preview_scroll,
            scrollbars: Default::default(),
            autosave: Task::ready(()),
            asking_outside_edit: false,
            _clock: clock,
            _watch_draft: watch_draft,
            _subscriptions: subscriptions,
        };
        chrome._subscriptions.push(cx.subscribe(
            &chrome.preview,
            |this, _, event: &preview::FieldCopied, cx| {
                this.show_toast(format!("Copied {}", event.0), cx);
            },
        ));
        if let Some(home) = std::env::home_dir() {
            let settings = AppSettings::global(cx).read(cx).settings.clone();
            if let Err(err) = chrome.drafts.first_launch_sample(&settings, &home) {
                eprintln!("draftmux: creating the sample draft: {err:#}");
            }
        }
        window.focus(&chrome.content_focus, cx);
        if let Some(id) = chrome.drafts.rows().first().map(|row| row.draft.id.clone()) {
            chrome.select(&id, window, cx).ok();
        }
        chrome
    }

    /// Creates a fresh sample draft and selects it.
    fn open_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::global(cx).read(cx).settings.clone();
        let created = std::env::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home directory"))
            .and_then(|home| self.drafts.create_sample(&settings, &home));
        match created {
            Ok(id) => {
                self.select(&id, window, cx).ok();
            }
            Err(err) => self.show_error(format!("Couldn't create the sample draft: {err:#}"), cx),
        }
    }

    fn empty_state(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        pane(palette.pane, palette)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.muted)
                    .child("No drafts"),
            )
            .child(div().text_size(px(12.)).text_color(palette.faint).child(
                "Run /draftmux in Claude Code or /skill:draftmux in pi to hand a message off.",
            ))
            .child(
                div()
                    .id("open-sample")
                    .mt_3()
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(palette.line_strong)
                    .bg(palette.control)
                    .hover(|style| style.bg(palette.control_hover))
                    .active(|style| style.opacity(0.8))
                    .text_size(px(12.5))
                    .text_color(palette.text)
                    .on_click(cx.listener(|this, _, window, cx| this.open_sample(window, cx)))
                    .child("Open sample draft"),
            )
    }

    /// Selects `id`, as `draftmux open` asks once it has created the draft.
    pub fn focus_draft(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.drafts.refresh();
        self.select(id, window, cx)?;
        cx.notify();
        Ok(())
    }

    /// Loads `id` into the editor and preview, unless it's already selected.
    /// On failure, including failing to save the draft being left, shows the
    /// error and stays put.
    fn select(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        if self.drafts.current_id() == Some(id) {
            return Ok(());
        }
        let text = match self.drafts.select(id) {
            Ok(text) => text,
            Err(err) => {
                self.show_error(format!("Couldn't open the draft: {err:#}"), cx);
                return Err(err);
            }
        };
        self.save_error = None;
        self.autosave = Task::ready(());
        self.preview_work.invalidate();
        self.set_preview(None, cx);
        if let Some(draft) = self.drafts.current() {
            let (dir, target) = (draft.dir.clone(), draft.meta.target);
            self.formatter
                .update(cx, |formatter, cx| formatter.set_draft_dir(dir, cx));
            self.set_target_markup(target, cx);
        }
        self.sync_selection_draft(cx);
        self.editor
            .update(cx, |editor, cx| editor.set_text(&text, cx));
        self.drafts.loaded(self.editor.read(cx).text());
        let draft = self.drafts.current().cloned();
        self.chat
            .update(cx, |chat, cx| chat.set_draft(draft.as_ref(), cx));
        self.preview_scroll.set_offset(Point::default());
        // pi writes a handed-off draft's first draft, and only into an empty
        // editor. Keys typed elsewhere as the window comes to the front go to
        // the composer, so they can't keep it from being requested.
        if draft
            .as_ref()
            .is_some_and(|draft| draft.meta.first_draft_pending)
        {
            window.focus(&self.chat.focus_handle(cx), cx);
        } else if self.show_editor {
            window.focus(&self.editor.focus_handle(cx), cx);
        }
        cx.notify();
        Ok(())
    }

    /// Selection actions fork the selected draft's session and write for its
    /// target.
    fn sync_selection_draft(&self, cx: &mut Context<Self>) {
        let draft = self.drafts.current().cloned();
        self.selection
            .update(cx, |selection, cx| selection.set_draft(draft, cx));
    }

    /// Sets the chat column's width and saves it once dragging pauses.
    fn resize_chat(&mut self, width: Pixels, cx: &mut Context<Self>) {
        if width == self.chat_width {
            return;
        }
        self.chat_width = width;
        let store = self.store.clone();
        self.save_chat_width = cx.spawn(async move |_, cx| {
            cx.background_executor().timer(CHAT_WIDTH_SAVE_DELAY).await;
            if let Err(err) = store.set_chat_width(f32::from(width)) {
                eprintln!("draftmux: saving the chat width: {err:#}");
            }
        });
        cx.notify();
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_sidebar = !self.show_sidebar;
        self.row_menu = None;
        if !self.show_sidebar && self.sidebar_focus.contains_focused(window, cx) {
            if self.show_editor && self.drafts.current().is_some() {
                window.focus(&self.editor.focus_handle(cx), cx);
            } else {
                window.focus(&self.content_focus, cx);
            }
        }
        if let Err(err) = self.store.set_sidebar_hidden(!self.show_sidebar) {
            eprintln!("draftmux: saving sidebar visibility: {err:#}");
        }
        cx.notify();
    }

    /// Hiding the only visible pane shows the other one instead.
    fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_editor = !self.show_editor;
        if self.show_editor {
            window.focus(&self.editor.focus_handle(cx), cx);
        } else {
            self.show_preview = true;
            // Keys would otherwise go on editing text that isn't on screen.
            if self.editor.focus_handle(cx).contains_focused(window, cx) {
                self.focus_sidebar(window, cx);
            }
        }
        cx.notify();
    }

    fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_preview && !self.show_editor {
            self.toggle_editor(window, cx);
        }
        self.show_preview = !self.show_preview;
        cx.notify();
    }

    /// Empties the editor after the selected draft left the list.
    fn deselect(&mut self, cx: &mut Context<Self>) {
        self.preview_work.invalidate();
        self.autosave = Task::ready(());
        self.set_preview(None, cx);
        self.editor.update(cx, |editor, cx| editor.set_text("", cx));
        self.chat.update(cx, |chat, cx| chat.set_draft(None, cx));
        self.sync_selection_draft(cx);
    }

    fn chat_event(&mut self, event: &ChatEvent, cx: &mut Context<Self>) {
        match event {
            ChatEvent::Send(text) => {
                // pi reads the draft from disk.
                let saved = self
                    .flush()
                    .map(|()| self.editor.read(cx).text().to_owned())
                    .map_err(|err| format!("{err:#}"));
                let text = text.clone();
                self.chat
                    .update(cx, |chat, cx| chat.prompt(text, saved, cx));
            }
            ChatEvent::FirstDraft(text) => {
                let Some(id) = self.drafts.current_id().map(str::to_owned) else {
                    return;
                };
                if let Err(err) = self.drafts.first_draft_requested(&id) {
                    eprintln!("draftmux: recording the first-draft request: {err:#}");
                }
                // Text the user typed while pi was starting is kept, and the
                // request is left in the composer for them to send or drop.
                if !self.editor.read(cx).text().trim().is_empty() {
                    let text = text.clone();
                    self.chat.update(cx, |chat, cx| chat.offer(&text, cx));
                } else {
                    let saved = self
                        .flush()
                        .map(|()| self.editor.read(cx).text().to_owned())
                        .map_err(|err| format!("{err:#}"));
                    let text = text.clone();
                    self.chat
                        .update(cx, |chat, cx| chat.prompt(text, saved, cx));
                }
            }
            ChatEvent::DraftWritten { id, text } => {
                if self.drafts.current_id() == Some(id.as_str()) {
                    // Shown now rather than on the next poll. Nothing is
                    // pending while the agent runs, so there's no conflict.
                    self.reload_outside_edit(cx);
                } else if let Err(err) = self.drafts.touched(id, text) {
                    eprintln!("draftmux: recording the agent's edit: {err:#}");
                }
            }
            ChatEvent::Running(running) => {
                let running = *running;
                let writing = running
                    || self
                        .drafts
                        .current()
                        .is_some_and(|draft| draft.meta.first_draft_pending);
                self.preview.update(cx, |preview, cx| {
                    if preview.writing != writing {
                        preview.writing = writing;
                        cx.notify();
                    }
                });
                self.editor
                    .update(cx, |editor, cx| editor.set_read_only(running, cx));
                self.formatter
                    .update(cx, |formatter, cx| formatter.set_agent_running(running, cx));
                // Selection actions are unavailable while the editor is
                // read-only.
                if running {
                    self.selection
                        .update(cx, |selection, cx| selection.dismiss(cx));
                }
            }
        }
        cx.notify();
    }

    fn edited(&mut self, text: String, cx: &mut Context<Self>) {
        let _timing = editor::timing::span("edit-subscribers");
        let Some(target) = self.drafts.current().map(|draft| draft.meta.target) else {
            return;
        };
        self.drafts.edited(&text, Instant::now());
        self.schedule_save(cx);
        self.preview_selection
            .update(cx, |selection, cx| selection.clear(cx));
        if self.preview_work.request(target, text) {
            self.preview_task = cx.spawn(async move |this, cx| {
                loop {
                    // Coalesce bursts without postponing work indefinitely during typing.
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let Ok(Some(request)) = this.update(cx, |this, _| this.preview_work.take())
                    else {
                        break;
                    };
                    let revision = request.revision;
                    let (document, source) = cx
                        .background_executor()
                        .spawn(async move {
                            (
                                markup_view::preview(request.target, &request.text),
                                request.text,
                            )
                        })
                        .await;
                    if this
                        .update(cx, |this, cx| {
                            if this.preview_work.is_current(revision)
                                && this.editor.read(cx).text() == source
                            {
                                this.set_preview(Some(document), cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    }

    fn set_preview(&mut self, preview: Option<Document>, cx: &mut Context<Self>) {
        let target = self.drafts.current().map(|draft| draft.meta.target);
        self.preview_source = if preview.is_some() {
            self.editor.read(cx).text().to_owned()
        } else {
            String::new()
        };
        self.preview.update(cx, |this, cx| {
            if preview.is_none() {
                this.cache.reset();
            }
            this.fields = markup::frontmatter::parse(&self.preview_source).fields;
            this.copied_field = None;
            this.copied_task = Task::ready(());
            this.document = preview;
            this.target = target;
            cx.notify();
        });
        self.preview_map = None;
        self.preview_selection
            .update(cx, |selection, cx| selection.reset(cx));
    }

    /// Selects in the editor the markdown of the text selected in the
    /// preview, so the chat and selection actions can target it.
    fn preview_selected(&mut self, cx: &mut Context<Self>) {
        let selection = self.preview_selection.read(cx);
        let Some((start, end)) = selection.range() else {
            // Clearing the preview's selection clears the editor's copy of
            // it, unless the editor has selected something else since.
            if let Some(mirrored) = self.mirrored.take()
                && self.editor.read(cx).selected_range() == mirrored
            {
                self.editor.update(cx, |editor, cx| {
                    editor.select_range(mirrored.start..mirrored.start, cx)
                });
            }
            return;
        };
        let map = self.preview_map.get_or_insert_with(|| {
            SourceMap::new(
                markup::frontmatter::parse(&self.preview_source).body,
                selection.paragraphs(),
            )
        });
        let body_offset = markup::frontmatter::parse(&self.preview_source).body_offset;
        if let Some(range) = map
            .range(start, end)
            .map(|range| range.start + body_offset..range.end + body_offset)
            .and_then(|range| {
                preview_work::unchanged_range(
                    &self.preview_source,
                    self.editor.read(cx).text(),
                    range,
                )
            })
        {
            self.mirrored = Some(range.clone());
            self.editor
                .update(cx, |editor, cx| editor.select_range(range, cx));
        } else {
            self.preview_selection
                .update(cx, |selection, cx| selection.clear(cx));
        }
    }

    /// Counts a change made to the selected draft's file outside the editor
    /// as activity, and loads it into the editor when no edit is pending.
    /// Returns the file's text when an edit is pending instead.
    fn reload_outside_edit(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let id = self.drafts.current_id()?.to_owned();
        // A file that can't be read is reported by the next save.
        let disk = self.drafts.changed_on_disk().ok().flatten()?;
        if let Err(err) = self.drafts.touched(&id, &disk) {
            eprintln!("draftmux: recording the outside edit: {err:#}");
        }
        if self.drafts.deadline().is_some() {
            return Some(disk);
        }
        self.load_outside_edit(disk, cx);
        cx.notify();
        None
    }

    fn load_outside_edit(&mut self, disk: String, cx: &mut Context<Self>) {
        self.drafts.adopt(disk.clone());
        self.editor.update(cx, |editor, cx| {
            crate::changes::replace_all(editor, &disk, cx)
        });
        // The file already holds the text, so loading isn't an edit to save.
        self.drafts.loaded(self.editor.read(cx).text());
    }

    /// Reloads an outside change to the selected draft, or asks which
    /// version to keep when the user has unsaved edits.
    fn check_outside_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.asking_outside_edit {
            return;
        }
        let Some(disk) = self.reload_outside_edit(cx) else {
            return;
        };
        let Some(id) = self.drafts.current_id().map(str::to_owned) else {
            return;
        };
        self.asking_outside_edit = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "This draft was changed outside Draftmux",
            Some(
                "You have edits that aren't saved yet. Use the new version, or \
                 keep your edits and replace it? Using the new version can be \
                 undone.",
            ),
            // The default can be undone; keeping the edits replaces the
            // other change for good.
            &["Use New Version", "Keep My Edits"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let answer = answer.await.ok();
            this.update(cx, |this, cx| {
                this.asking_outside_edit = false;
                // The answer is about these edits to this draft and this
                // version of it. If a retried save wrote the edits meanwhile,
                // or the file changed again, the next poll starts over.
                if this.drafts.current_id() != Some(id.as_str())
                    || this.drafts.deadline().is_none()
                    || this.drafts.changed_on_disk().ok().flatten().as_ref() != Some(&disk)
                {
                    return;
                }
                match answer {
                    Some(0) => this.load_outside_edit(disk, cx),
                    Some(1) => {
                        this.drafts.adopt(disk);
                        this.flush().ok();
                    }
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let Some(deadline) = self.drafts.deadline() else {
            return;
        };
        self.autosave = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(deadline.saturating_duration_since(Instant::now()))
                .await;
            this.update(cx, |this, cx| this.poll_save(cx)).ok();
        });
    }

    fn poll_save(&mut self, cx: &mut Context<Self>) {
        match self.drafts.poll(Instant::now()) {
            Ok(()) => self.save_error = None,
            Err(err) => {
                self.save_failed(&err);
                // The text stays pending; try again after another delay.
                let text = self.editor.read(cx).text().to_owned();
                self.drafts.edited(&text, Instant::now());
                self.schedule_save(cx);
            }
        }
        cx.notify();
    }

    fn flush(&mut self) -> Result<()> {
        let result = self.drafts.flush();
        match &result {
            Ok(()) => self.save_error = None,
            Err(err) => self.save_failed(err),
        }
        result
    }

    fn save_failed(&mut self, err: &anyhow::Error) {
        // Not a failure to report: the user is asked which version to keep.
        if err.is::<Conflict>() {
            return;
        }
        eprintln!("draftmux: saving draft: {err:#}");
        self.save_error = Some(format!("Couldn't save: {err:#}").into());
    }

    /// Saves before the window closes or the app quits. On failure, shows the
    /// error and returns false so the unsaved text stays in the editor.
    pub fn flush_before_exit(&mut self, cx: &mut Context<Self>) -> bool {
        match self.flush() {
            Ok(()) => true,
            Err(_) => {
                cx.notify();
                false
            }
        }
    }

    fn set_target(&mut self, target: Target, cx: &mut Context<Self>) {
        if let Err(err) = self.drafts.set_target(target) {
            self.show_error(format!("Couldn't change the target: {err:#}"), cx);
            return;
        }
        self.preview_work.invalidate();
        self.set_target_markup(target, cx);
        let preview = markup_view::preview(target, self.editor.read(cx).text());
        self.set_preview(Some(preview), cx);
        self.sync_selection_draft(cx);
        cx.notify();
    }

    /// Plain text drafts get no markdown highlighting, and only GitHub and
    /// rich text drafts get Format.
    fn set_target_markup(&mut self, target: Target, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_markdown(target != Target::Plain, cx)
        });
        let hidden = matches!(target, Target::Plain | Target::Slack);
        self.formatter
            .update(cx, |formatter, cx| formatter.set_hidden(hidden, cx));
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.drafts.current().map(|draft| draft.meta.target) else {
            return;
        };
        let text = self.editor.read(cx).text().to_owned();
        let confirmation = copy::copy(target, &text, cx);
        self.show_toast(confirmation, cx);
        let serial = self.copied.as_ref().map_or(0, |(serial, _)| serial + 1);
        let reset = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(copy::CONFIRMATION).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        });
        self.copied = Some((serial, reset));
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let current = self.drafts.current();
        let title: SharedString = match current {
            Some(draft) => self
                .drafts
                .rows()
                .iter()
                .find(|row| row.draft.id == draft.id)
                .map_or_else(|| "Untitled draft".into(), |row| row.title.clone().into()),
            None => "Draftmux".into(),
        };
        let subtitle = match (&self.save_error, current) {
            (Some(error), _) => Some((error.clone(), palette.danger)),
            // The sample's directory is just where it was created.
            (None, Some(draft)) if draft.meta.is_sample() => None,
            (None, Some(draft)) => Some((
                tilde(&draft.meta.cwd, std::env::home_dir().as_deref()).into(),
                palette.faint,
            )),
            (None, None) => None,
        };

        div()
            .id("toolbar")
            .h(TOOLBAR_HEIGHT)
            .flex_none()
            .flex()
            // gpui's macOS backend ignores control areas; the handlers below
            // move the window.
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.should_move = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.should_move = false),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.should_move = false))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            // macOS traffic lights sit over this segment.
            .child(
                sidebar_column(&palette)
                    .when(!self.show_sidebar, |bar| {
                        bar.w(px(100.))
                            .bg(palette.toolbar)
                            .border_r_0()
                            .border_b_1()
                            .border_color(palette.hairline)
                    })
                    .children(crate::platform::toolbar_items(&palette)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl(px(18.))
                    .pr(px(12.))
                    .bg(palette.toolbar)
                    .border_b_1()
                    .border_color(palette.hairline)
                    .child(
                        div()
                            .id("toggle-sidebar")
                            .h(px(24.))
                            .px(px(6.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .hover(|style| style.bg(palette.hover))
                            .active(|style| style.opacity(0.8))
                            .tooltip(crate::tooltip::text(if self.show_sidebar {
                                editor::by_platform!("Hide Sidebar (⌘\\)", "Hide Sidebar (Ctrl+\\)")
                            } else {
                                editor::by_platform!("Show Sidebar (⌘\\)", "Show Sidebar (Ctrl+\\)")
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.toggle_sidebar(window, cx);
                            }))
                            .child(
                                svg()
                                    .path(crate::assets::SIDEBAR_ICON)
                                    .size(px(16.))
                                    .text_color(palette.muted),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(subtitle.map(|(text, color)| {
                        div()
                            .min_w(px(40.))
                            .truncate()
                            .text_size(px(12.))
                            .text_color(color)
                            .child(text)
                    }))
                    .child(div().flex_1())
                    .children(self.update_button(&palette, cx))
                    .when_some(current.map(|draft| draft.meta.target), |bar, target| {
                        bar.child(self.toolbar_actions(target, &palette, cx))
                    }),
            )
    }

    /// Restarts into an installed update, or opens the page of one this copy
    /// can't install itself.
    fn update_button(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (label, tooltip, ready): (SharedString, SharedString, bool) =
            match &Updater::global(cx).read(cx).status {
                update::Status::Ready(version) => (
                    "Restart to Update".into(),
                    format!("Draftmux {version} is installed").into(),
                    true,
                ),
                update::Status::Blocked { reason, .. } => {
                    ("Update Available".into(), reason.clone().into(), false)
                }
                _ => return None,
            };
        Some(
            div()
                .id("update")
                .flex_none()
                .h(px(22.))
                .px(px(9.))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .bg(palette.accent_soft)
                .text_color(palette.accent)
                .hover(|style| style.opacity(0.85))
                .active(|style| style.opacity(0.7))
                .tooltip(crate::tooltip::text(tooltip))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if !ready {
                        Updater::global(cx).read(cx).open_release_page(cx);
                    } else if this.flush_before_exit(cx) {
                        cx.restart();
                    }
                }))
                .child(label),
        )
    }

    /// Draft actions at the toolbar's right end. Their mouse-downs don't reach
    /// the toolbar, so clicking them never moves or zooms the window.
    fn toolbar_actions(
        &self,
        target: Target,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.formatter.clone())
            .child(
                div()
                    .id("toggle-editor")
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .group("toggle-editor")
                    .hover(|style| style.bg(palette.hover))
                    .active(|style| style.opacity(0.8))
                    .tooltip(crate::tooltip::text(if self.show_editor {
                        editor::by_platform!("Hide Markdown (⌥⌘E)", "Hide Markdown (Ctrl+Alt+E)")
                    } else {
                        editor::by_platform!("Show Markdown (⌥⌘E)", "Show Markdown (Ctrl+Alt+E)")
                    }))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_editor(window, cx)))
                    .child(
                        svg()
                            .path(EDITOR_PANE_ICON)
                            .size(px(16.))
                            .text_color(if self.show_editor {
                                palette.muted
                            } else {
                                palette.faint
                            })
                            .group_hover("toggle-editor", |style| style.text_color(palette.text)),
                    ),
            )
            .child(
                div()
                    .id("toggle-preview")
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .group("toggle-preview")
                    .hover(|style| style.bg(palette.hover))
                    .active(|style| style.opacity(0.8))
                    .tooltip(crate::tooltip::text(if self.show_preview {
                        editor::by_platform!("Hide Preview (⌥⌘P)", "Hide Preview (Ctrl+Alt+P)")
                    } else {
                        editor::by_platform!("Show Preview (⌥⌘P)", "Show Preview (Ctrl+Alt+P)")
                    }))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx)))
                    .child(
                        svg()
                            .path(PREVIEW_ICON)
                            .size(px(16.))
                            .text_color(if self.show_preview {
                                palette.muted
                            } else {
                                palette.faint
                            })
                            .group_hover("toggle-preview", |style| style.text_color(palette.text)),
                    ),
            )
            .child(target_switch(target, palette, cx))
            .child(copy::button(
                palette,
                self.copied.as_ref().map(|(serial, _)| *serial),
                {
                    let this = cx.weak_entity();
                    move |cx| {
                        this.update(cx, |this, cx| this.copy(cx)).ok();
                    }
                },
            ))
    }
}

/// The segmented control with every target.
fn target_switch(
    selected: Target,
    palette: &Palette,
    cx: &mut Context<Chrome>,
) -> impl IntoElement {
    let palette = *palette;
    let segment = |target: Target| -> AnyElement {
        let label = target.label();
        let segment = div()
            .id(label)
            .h(px(20.))
            .px(px(9.))
            .flex()
            .items_center()
            .rounded(px(5.5))
            .text_size(px(12.))
            .child(label);
        if target != selected {
            return segment
                .active(|style| style.opacity(0.8))
                .on_click(cx.listener(move |this, _, _, cx| this.set_target(target, cx)))
                .into_any_element();
        }
        segment
            .with_animation(
                ("segment-on", target as usize),
                Animation::new(SEGMENT_FADE).with_easing(ease_out_quint()),
                move |segment, delta| {
                    segment.bg(palette.segment_on.opacity(delta)).shadow(vec![
                        shadow(palette.segment_ring.opacity(delta), 0., 0., 0.5),
                        shadow(palette.segment_drop.opacity(delta), 1., 2.5, 0.),
                    ])
                },
            )
            .into_any_element()
    };
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(7.))
        .bg(palette.segment_track)
        .children(Target::ALL.iter().map(|&target| segment(target)))
}

/// A box shadow of `color` moved down by `y`. A zero blur with a spread draws
/// a hairline ring.
pub(crate) fn shadow(color: Hsla, y: f32, blur: f32, spread: f32) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
    }
}

/// The sidebar strip, shared by the toolbar and the body so the two read as
/// one column.
fn sidebar_column(palette: &Palette) -> gpui::Div {
    div()
        .w(SIDEBAR_WIDTH)
        .flex_none()
        .bg(palette.sidebar)
        .border_r_1()
        .border_color(palette.sidebar_edge)
}

/// `width` kept within the chat's minimum and the room the window leaves it
/// beside the sidebar and the content.
fn clamp_chat_width(width: Pixels, viewport_width: Pixels, show_sidebar: bool) -> Pixels {
    let sidebar_width = if show_sidebar { SIDEBAR_WIDTH } else { px(0.) };
    let max = viewport_width - sidebar_width - CONTENT_MIN_WIDTH;
    width.min(max).max(CHAT_MIN_WIDTH)
}

/// A strip over the chat's left edge that drags to resize it; a double click
/// restores the default width.
fn chat_resize_handle(cx: &mut Context<Chrome>) -> impl IntoElement {
    div()
        .id("chat-resize")
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-3.))
        .w(px(6.))
        .cursor(CursorStyle::ResizeLeftRight)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_drag(ChatResize, |_, _, _, cx| cx.new(|_| Empty))
        .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
            if event.click_count() == 2 {
                this.resize_chat(
                    clamp_chat_width(CHAT_WIDTH, window.viewport_size().width, this.show_sidebar),
                    cx,
                );
            }
        }))
}

fn pane(bg: Hsla, palette: &Palette) -> gpui::Div {
    div()
        .h_full()
        .min_w_0()
        .bg(bg)
        .border_color(palette.hairline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_width_reserves_space_only_for_a_visible_sidebar() {
        assert_eq!(clamp_chat_width(px(900.), px(1000.), true), px(452.));
        assert_eq!(clamp_chat_width(px(900.), px(1000.), false), px(680.));
        for visible in [true, false] {
            assert_eq!(clamp_chat_width(CHAT_WIDTH, px(1200.), visible), CHAT_WIDTH);
            assert_eq!(
                clamp_chat_width(px(100.), px(1000.), visible),
                CHAT_MIN_WIDTH
            );
        }
    }
}
