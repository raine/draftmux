//! The main window: unified toolbar, translucent drafts sidebar, editor,
//! preview and chat columns of the "Native" prototype variant
//! (history/docs/prototype/index.html).
//!
//! The window uses `WindowBackgroundAppearance::Blurred`, so anything painted
//! with alpha below 1 shows the blurred desktop. The root paints nothing; the
//! sidebar column paints a translucent tint, opaque with Reduce Transparency,
//! and the content panes are opaque.

mod sidebar;

use std::time::{Duration, Instant};

use anyhow::Result;
use draftmux_storage::{Store, Target};
use editor::{Editor, EditorEvent};
use gpui::{
    App, BoxShadow, Context, Entity, Focusable, FontWeight, Hsla, KeyBinding, MouseButton, Pixels,
    Point, ScrollHandle, SharedString, Subscription, Task, Window, WindowControlArea, actions, div,
    point, prelude::*, px, svg,
};
use markup::Document;

use crate::app_settings::AppSettings;
use crate::assets::PREVIEW_ICON;
use crate::chat::{Chat, ChatEvent};
use crate::copy::{self, Copy};
use crate::drafts::{Drafts, tilde};
use crate::format::{Format, Formatter};
use crate::markup_view;
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::selection_popover::SelectionPopover;
use crate::theme::{self, Palette};

actions!(chrome, [TogglePreview]);

const TOOLBAR_HEIGHT: Pixels = px(52.);
const SIDEBAR_WIDTH: Pixels = px(228.);
const CHAT_WIDTH: Pixels = px(340.);
/// Origin of the close button; vertically centres the buttons in the toolbar.
pub const TRAFFIC_LIGHT_POSITION: Point<Pixels> = point(px(20.), px(19.));
/// How often relative times in the sidebar are redrawn.
const CLOCK_TICK: Duration = Duration::from_secs(30);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        editor::secondary!("alt-p"),
        TogglePreview,
        None,
    )]);
}

pub struct Chrome {
    /// Set on toolbar mouse-down; the first mouse-move then hands the drag to
    /// the window server, so a plain click never starts a move.
    should_move: bool,
    drafts: Drafts,
    /// Shows the selected draft; reused across drafts.
    editor: Entity<Editor>,
    formatter: Entity<Formatter>,
    chat: Entity<Chat>,
    selection: Entity<SelectionPopover>,
    /// The selected draft's text parsed for its target.
    preview: Option<Document>,
    show_preview: bool,
    /// Why the selected draft's last save failed; cleared by the next save.
    save_error: Option<SharedString>,
    toast: Option<sidebar::Toast>,
    sidebar_scroll: ScrollHandle,
    preview_scroll: ScrollHandle,
    scrollbars: [ScrollbarState; 3],
    /// Wakes up when the pending edit is due to be saved.
    autosave: Task<()>,
    _clock: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Render for Chrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let [sidebar_bar, editor_bar, preview_bar] = &self.scrollbars;
        let content = if self.drafts.current().is_some() {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .child(
                    pane(palette.pane, &palette)
                        .flex_1()
                        .relative()
                        .child(SelectionPopover::wrap(
                            &self.selection,
                            self.editor.clone(),
                            cx,
                        ))
                        .child(scrollbar(Source::Editor(self.editor.clone()), editor_bar)),
                )
                .when(self.show_preview, |row| {
                    row.child(
                        pane(palette.pane, &palette)
                            .flex_1()
                            .relative()
                            .border_l_1()
                            .child(self.preview_body(window, cx))
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
            .on_action(cx.listener(|this, _: &TogglePreview, _, cx| this.toggle_preview(cx)))
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
            .child(self.toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(sidebar_bar, cx))
                    .child(content)
                    .child(
                        pane(palette.chat, &palette)
                            .w(CHAT_WIDTH)
                            .flex_none()
                            .border_l_1()
                            .child(self.chat.clone()),
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
        let chat = cx.new(Chat::new);
        let selection = cx.new(|cx| SelectionPopover::new(editor.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe(&chat, |this, _, event, cx| this.chat_event(event, cx)),
            cx.subscribe(&editor, |this, editor, event, cx| {
                if *event == EditorEvent::Edited {
                    this.edited(editor.read(cx).text().to_owned(), cx);
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

        let mut chrome = Self {
            should_move: false,
            drafts: Drafts::new(store),
            editor,
            formatter,
            chat,
            selection,
            preview: None,
            show_preview: true,
            save_error: None,
            toast: None,
            sidebar_scroll: ScrollHandle::new(),
            preview_scroll: ScrollHandle::new(),
            scrollbars: Default::default(),
            autosave: Task::ready(()),
            _clock: clock,
            _subscriptions: subscriptions,
        };
        if let Some(home) = std::env::home_dir() {
            let settings = AppSettings::global(cx).read(cx).settings.clone();
            if let Err(err) = chrome.drafts.first_launch_sample(&settings, &home) {
                eprintln!("draftmux: creating the sample draft: {err:#}");
            }
        }
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
            Err(err) => self.show_toast(
                format!("Couldn't create the sample draft: {err:#}"),
                None,
                cx,
            ),
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
                self.show_toast(format!("Couldn't open the draft: {err:#}"), None, cx);
                return Err(err);
            }
        };
        self.save_error = None;
        self.autosave = Task::ready(());
        if let Some(draft) = self.drafts.current() {
            let (dir, target) = (draft.dir.clone(), draft.meta.target);
            self.formatter
                .update(cx, |formatter, cx| formatter.set_draft_dir(dir, cx));
            self.set_plain_text(target, cx);
        }
        self.sync_selection_draft(cx);
        self.editor
            .update(cx, |editor, cx| editor.set_text(&text, cx));
        self.drafts.loaded(self.editor.read(cx).text());
        let draft = self.drafts.current().cloned();
        self.chat
            .update(cx, |chat, cx| chat.set_draft(draft.as_ref(), cx));
        self.preview_scroll.set_offset(Point::default());
        window.focus(&self.editor.focus_handle(cx), cx);
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

    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.show_preview = !self.show_preview;
        cx.notify();
    }

    /// Empties the editor after the selected draft left the list.
    fn deselect(&mut self, cx: &mut Context<Self>) {
        self.autosave = Task::ready(());
        self.preview = None;
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
            ChatEvent::DraftWritten(text) => {
                self.editor
                    .update(cx, |editor, cx| editor.replace_all(text, cx));
                // The file already holds the text, so loading isn't an edit
                // to save.
                self.drafts.loaded(self.editor.read(cx).text());
                self.drafts.refresh();
            }
            ChatEvent::Running(running) => {
                let running = *running;
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
        let Some(target) = self.drafts.current().map(|draft| draft.meta.target) else {
            return;
        };
        self.preview = Some(markup_view::preview(target, &text));
        self.drafts.edited(&text, Instant::now());
        self.schedule_save(cx);
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
        eprintln!("draftmux: saving draft: {err:#}");
        self.save_error = Some(format!("Couldn't save: {err:#}").into());
    }

    /// Saves before the window closes or the app quits. On failure, shows the
    /// error and returns false so the unsaved text stays in the editor.
    fn flush_before_exit(&mut self, cx: &mut Context<Self>) -> bool {
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
            self.show_toast(format!("Couldn't change the target: {err:#}"), None, cx);
            return;
        }
        self.set_plain_text(target, cx);
        self.preview = Some(markup_view::preview(target, self.editor.read(cx).text()));
        self.sync_selection_draft(cx);
        cx.notify();
    }

    /// Plain text drafts get no markdown highlighting or Format.
    fn set_plain_text(&mut self, target: Target, cx: &mut Context<Self>) {
        let plain_text = target == Target::Plain;
        self.editor
            .update(cx, |editor, cx| editor.set_markdown(!plain_text, cx));
        self.formatter
            .update(cx, |formatter, cx| formatter.set_plain_text(plain_text, cx));
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.drafts.current().map(|draft| draft.meta.target) else {
            return;
        };
        let text = self.editor.read(cx).text().to_owned();
        let confirmation = copy::copy(target, &text, cx);
        self.show_toast(confirmation.to_owned(), None, cx);
    }

    fn preview_body(&self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme::get(cx);
        let target = self.drafts.current().map(|draft| draft.meta.target);
        let (body, background) = match (&self.preview, target) {
            (Some(doc), Some(target)) if !doc.blocks.is_empty() => {
                let style = markup_view::style(target, theme.dark);
                (
                    markup_view::render_preview(
                        doc,
                        &style,
                        "preview-body",
                        &self.preview_scroll,
                        window,
                        cx,
                    ),
                    Some(style.background),
                )
            }
            _ => (
                div()
                    .text_size(px(14.))
                    .text_color(theme.palette.faint)
                    .child("Nothing to preview")
                    .into_any_element(),
                None,
            ),
        };
        // The body sits on its platform's own page color, as it does there.
        div()
            .id("preview")
            .size_full()
            .when_some(background, |this, background| this.bg(background))
            .overflow_y_scroll()
            .track_scroll(&self.preview_scroll)
            .px_6()
            .pt_5()
            .pb_10()
            .child(body)
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
            .child(sidebar_column(&palette).children(crate::platform::toolbar_items(&palette)))
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
                    .when_some(current.map(|draft| draft.meta.target), |bar, target| {
                        bar.child(self.toolbar_actions(target, &palette, cx))
                    }),
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
                    .id("toggle-preview")
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .group("toggle-preview")
                    .hover(|style| style.bg(palette.hover))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_preview(cx)))
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
            .child(copy::button(palette, {
                let this = cx.weak_entity();
                move |cx| {
                    this.update(cx, |this, cx| this.copy(cx)).ok();
                }
            }))
    }
}

/// The segmented control with every target.
fn target_switch(
    selected: Target,
    palette: &Palette,
    cx: &mut Context<Chrome>,
) -> impl IntoElement {
    let segment = |target: Target| {
        let label = target.label();
        let on = target == selected;
        div()
            .id(label)
            .h(px(20.))
            .px(px(9.))
            .flex()
            .items_center()
            .rounded(px(5.5))
            .text_size(px(12.))
            .when(on, |segment| {
                segment.bg(palette.segment_on).shadow(vec![
                    shadow(palette.segment_ring, 0., 0., 0.5),
                    shadow(palette.segment_drop, 1., 2.5, 0.),
                ])
            })
            .when(!on, |segment| {
                segment.on_click(cx.listener(move |this, _, _, cx| this.set_target(target, cx)))
            })
            .child(label)
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

fn pane(bg: Hsla, palette: &Palette) -> gpui::Div {
    div()
        .h_full()
        .min_w_0()
        .bg(bg)
        .border_color(palette.hairline)
}
