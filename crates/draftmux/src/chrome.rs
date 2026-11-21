//! The main window: unified toolbar, translucent drafts sidebar, editor,
//! preview and chat columns.
//!
//! The window uses `WindowBackgroundAppearance::Blurred`, so anything painted
//! with alpha below 1 shows the blurred desktop. The root paints nothing; the
//! sidebar column paints a translucent tint, opaque with Reduce Transparency,
//! and the content panes are opaque.

mod preview;
mod preview_work;
mod review;
mod sidebar;
mod skeleton;
mod update_offer;

use preview::Preview;

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;
use draftmux_core::history::Recorded;
use draftmux_core::outside::{Choice, Question};
use draftmux_core::session::{Effect, Outcome, Session};
use draftmux_storage::{FileSave, Store, Target};
use editor::{Editor, EditorEvent, Redo, Undo};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, BoxShadow, Context, CursorStyle,
    DragMoveEvent, Empty, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding,
    MouseButton, Pixels, Point, PromptLevel, ScrollHandle, SharedString, StyleRefinement,
    Subscription, Task, Transformation, Window, WindowControlArea, actions, anchored, deferred,
    div, ease_out_quint, percentage, point, prelude::*, px, svg,
};
use markup::Document;

use crate::app_settings::AppSettings;
use crate::assets::{CHEVRON_ICON, EDITOR_PANE_ICON, PREVIEW_ICON};
use crate::chat::{Chat, ChatEvent};
use crate::copy::{self, Copy};
use crate::drafts::{self, tilde};
use crate::edit;
use crate::file_watch;
use crate::format::{Format, Formatter, Wrap};
use crate::markup_view;
use crate::menu::{MenuItem, menu_panel};
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::selectable::{Pos, Scroller, SelectionChanged, SelectionStarted, TextSelection};
use crate::selection_popover::{SelectionPopover, SelectionPopoverEvent};
use crate::source_map::SourceMap;
use crate::theme::{self, Palette};
use crate::update::{self, Updater};

actions!(
    chrome,
    [
        ToggleEditor,
        TogglePreview,
        ToggleSidebar,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        SaveToFile
    ]
);

const TOOLBAR_HEIGHT: Pixels = px(52.);
const SIDEBAR_WIDTH: Pixels = px(228.);
const CHAT_WIDTH: Pixels = px(340.);
/// How long the target switch's newly selected segment takes to fade in.
const SEGMENT_FADE: Duration = Duration::from_millis(150);
const CHAT_MIN_WIDTH: Pixels = px(260.);
/// Room the chat never takes from the editor and preview.
const CONTENT_MIN_WIDTH: Pixels = px(320.);
/// Below this, the editor and preview are too narrow to share the room
/// beside the sidebar and chat, so the editor collapses.
const SPLIT_MIN_WIDTH: Pixels = px(640.);
/// The narrowest the divider leaves the editor or the preview, as far as
/// the room they share allows.
const SPLIT_PANE_MIN_WIDTH: Pixels = px(240.);
/// How close to the middle the divider snaps to an even split.
const SPLIT_SNAP: Pixels = px(12.);
/// How long the sidebar, editor and preview take to slide in or out.
const PANE_SLIDE: Duration = Duration::from_millis(200);
/// How long the chat width or the split ratio settles before it is saved.
const RESIZE_SAVE_DELAY: Duration = Duration::from_millis(300);
/// Origin of the close button; vertically centres the buttons in the toolbar.
pub const TRAFFIC_LIGHT_POSITION: Point<Pixels> = point(px(20.), px(19.));
/// How often relative times in the sidebar are redrawn.
const CLOCK_TICK: Duration = Duration::from_secs(30);

pub fn init(cx: &mut App) {
    sidebar::init(cx);
    review::init(cx);
    update_offer::init(cx);
    cx.bind_keys([
        KeyBinding::new(editor::secondary!("\\"), ToggleSidebar, None),
        KeyBinding::new(editor::secondary!("alt-e"), ToggleEditor, None),
        KeyBinding::new(editor::secondary!("alt-p"), TogglePreview, None),
        KeyBinding::new(editor::secondary!("="), ZoomIn, None),
        KeyBinding::new(editor::secondary!("+"), ZoomIn, None),
        KeyBinding::new(editor::secondary!("-"), ZoomOut, None),
        KeyBinding::new(editor::secondary!("0"), ResetZoom, None),
        KeyBinding::new(editor::secondary!("s"), SaveToFile, None),
    ]);
}

/// The drag that resizes the chat column.
struct ChatResize;

/// The drag that moves the divider between the editor and the preview.
struct SplitResize;

pub struct Chrome {
    /// Set on toolbar mouse-down; the first mouse-move then hands the drag to
    /// the window server, so a plain click never starts a move.
    should_move: bool,
    /// The selected draft, its saving and its agent turns.
    session: Session,
    store: Store,
    chat_width: Pixels,
    save_chat_width: Task<()>,
    /// The editor's share of the room it splits with the preview, as the
    /// user set it; see [`editor_share`] for the share it gets.
    split_ratio: f32,
    save_split_ratio: Task<()>,
    /// Shows the selected draft; reused across drafts.
    editor: Entity<Editor>,
    formatter: Entity<Formatter>,
    chat: Entity<Chat>,
    selection: Entity<SelectionPopover>,
    /// The selected draft's text parsed for its target.
    preview: Entity<Preview>,
    /// The text before the compared edits, beside the preview during
    /// review.
    before_preview: Entity<Preview>,
    before_scrollbar: ScrollbarState,
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
    /// When the editor flash last tinted in the preview started.
    mirrored_flash: Option<Instant>,
    /// The selection requests last marked in the preview.
    mirrored_requests: Option<Vec<(editor::TrackedId, Range<usize>)>>,
    /// At least one of the markdown editor and the preview is shown.
    show_editor: bool,
    show_preview: bool,
    /// The editor and preview had too little room to be shown side by side
    /// when last rendered.
    narrow: bool,
    /// The editor was collapsed for a narrow window rather than by the user,
    /// so widening the window shows it again.
    collapsed_for_width: bool,
    toast: Option<sidebar::Toast>,
    /// The update button's card, while it is open.
    update_card: Option<update_offer::UpdateCard>,
    /// Review of those changes, while it is open.
    review: Option<review::Review>,
    /// Holds the focus during review, so its single-key shortcuts don't
    /// reach a text field.
    review_focus: FocusHandle,
    /// The Copy button shows a checkmark while this is set, with a serial
    /// so each copy replays it; the task clears it.
    copied: Option<(usize, Task<()>)>,
    /// Whether the GitHub segment's menu of GitHub targets is open.
    github_menu: bool,
    /// Drafts whose hint to preview them as a GitHub file was dismissed.
    file_hint_dismissed: HashSet<String>,
    /// The sidebar row context menu, while it is open.
    row_menu: Option<sidebar::RowMenu>,
    /// The drafts the sidebar listed when it last rendered.
    seen_rows: RefCell<Option<HashSet<String>>>,
    leaving_rows: Vec<sidebar::LeavingRow>,
    /// The sidebar's Archived section, collapsed until opened.
    archived_section: sidebar::ArchivedSection,
    sidebar_scroll: ScrollHandle,
    sidebar_focus: FocusHandle,
    content_focus: FocusHandle,
    show_sidebar: bool,
    /// The sidebar, editor and preview sliding in and out, once rendered.
    panes: Option<[Slide; 3]>,
    preview_scroll: ScrollHandle,
    scrollbars: [ScrollbarState; 3],
    /// Wakes up when the pending edit is due to be saved.
    autosave: Task<()>,
    _clock: Task<()>,
    _watch_draft: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Render for Chrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = editor::timing::span("chrome-render");
        let palette = *theme::palette(cx);
        self.fit_to_width(window, cx);
        self.leaving_rows.retain(sidebar::LeavingRow::folding);
        self.archived_section
            .leaving
            .retain(sidebar::LeavingRow::folding);
        self.mirror_flash(window, cx);
        self.mirror_requests(window, cx);
        self.selection
            .read(cx)
            .set_panes_shown(self.show_editor, self.show_preview);
        let shown = [self.show_sidebar, self.show_editor, self.show_preview];
        let panes = self.panes.get_or_insert_with(|| shown.map(Slide::new));
        let now = Instant::now();
        let [sidebar_open, editor_open, preview_open] =
            std::array::from_fn(|ix| panes[ix].set(shown[ix], now));
        let [_, editor_slide, preview_slide] = panes.clone();
        if panes.iter().any(|pane| pane.running(now)) {
            window.request_animation_frame();
        }
        // A pane sliding in or out keeps the width it has open, and is
        // clipped rather than rewrapped; the pane beside it gives and takes
        // the room.
        let split = split_width(
            window.viewport_size().width,
            self.chat_width,
            self.show_sidebar,
        );
        let editor_share = editor_share(self.split_ratio, split);
        let [editor_grow, preview_grow] = split_grow(editor_share);
        let open_width = |slide: &Slide, other: &Slide, share: f32| {
            let shared = if slide.shown {
                other.shown
            } else {
                other.was_open()
            };
            if shared { split * share } else { split }
        };
        let editor_width =
            (editor_open < 1.).then(|| open_width(&editor_slide, &preview_slide, editor_share));
        let preview_width = (preview_open < 1.)
            .then(|| open_width(&preview_slide, &editor_slide, 1. - editor_share));
        let both_open = editor_width.is_none() && preview_width.is_none();
        let [sidebar_bar, editor_bar, preview_bar] = &self.scrollbars;
        let reviewing = self.review.is_some();
        let content = if self.session.drafts().current().is_some() {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .on_drag_move(
                    cx.listener(|this, event: &DragMoveEvent<SplitResize>, _, cx| {
                        let bounds = event.bounds;
                        this.resize_split(
                            drag_ratio(event.event.position.x - bounds.left(), bounds.size.width),
                            cx,
                        );
                    }),
                )
                .when(reviewing, |row| {
                    row.child(self.review_columns(&palette, cx))
                })
                .when(!reviewing && editor_open > 0., |row| {
                    row.child(
                        slot(editor_open * editor_grow, false).child(
                            pane(palette.pane, &palette)
                                .when_some(editor_width, |pane, width| pane.w(width).flex_none())
                                .when(editor_width.is_none(), |pane| pane.flex_1())
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
                        ),
                    )
                })
                .when(!reviewing && preview_open > 0., |row| {
                    row.child(
                        slot(preview_open * preview_grow, true).child(
                            pane(palette.pane, &palette)
                                .when_some(preview_width, |pane, width| pane.w(width).flex_none())
                                .when(preview_width.is_none(), |pane| pane.flex_1())
                                .relative()
                                .when(editor_open > 0., |pane| pane.border_l_1())
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
                                ))
                                .when(editor_open > 0. && both_open, |pane| {
                                    pane.child(split_resize_handle(&palette, cx))
                                }),
                        ),
                    )
                })
                // A press anywhere but on a change or the review bar
                // clears the selected change.
                .when(reviewing, |row| {
                    row.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.pressed_outside_changes(cx)),
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
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.zoom(theme::get(cx).zoom + theme::ZOOM_STEP, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.zoom(theme::get(cx).zoom - theme::ZOOM_STEP, cx)
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| this.zoom(1., cx)))
            .on_action(cx.listener(|this, _: &Format, _, cx| {
                if this.session.drafts().current().is_some() {
                    this.formatter
                        .update(cx, |formatter, cx| formatter.format(Wrap::Keep, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &crate::chat::CompactSession, _, cx| {
                this.chat.update(cx, |chat, cx| chat.compact(cx))
            }))
            .on_action(
                cx.listener(|this, _: &SaveToFile, window, cx| {
                    this.save_to_file(false, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &review::ToggleReview, window, cx| {
                this.toggle_review(window, cx)
            }))
            // Reached when the focus is outside the draft editor, or in a
            // text field with nothing to undo, such as the composer after
            // a send: the draft is what the user, or its agent, last changed.
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                this.editor.update(cx, |editor, cx| editor.undo(cx));
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                this.editor.update(cx, |editor, cx| editor.redo(cx));
            }))
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
                    .when(sidebar_open > 0., |row| {
                        let sidebar = self.sidebar(sidebar_bar, window, cx);
                        // Slides out to the left, uncovering the content.
                        row.child(
                            div()
                                .flex_none()
                                .h_full()
                                .flex()
                                .justify_end()
                                .overflow_hidden()
                                .w(SIDEBAR_WIDTH * sidebar_open)
                                .child(sidebar),
                        )
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
            .children(self.toast(cx))
            .children(self.change_origin_tooltip(cx));
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
        if let Some(zoom) = store.text_zoom() {
            theme::set_zoom(zoom, cx);
        }
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
        let selection = cx.new(|cx| {
            SelectionPopover::new(
                editor.clone(),
                preview_selection.clone(),
                store.clone(),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.observe(&preview_selection, |_, _, cx| cx.notify()),
            cx.subscribe(&preview_selection, |this, _, _: &SelectionChanged, cx| {
                this.preview_selected(cx)
            }),
            // A press in the preview with the secondary modifier keeps the
            // editor's selection besides the new one; a plain press drops
            // the kept ones, as in the editor.
            cx.subscribe(
                &preview_selection,
                |this, _, event: &SelectionStarted, cx| {
                    this.editor.update(cx, |editor, cx| {
                        if event.adding {
                            editor.add_selection(cx);
                        } else {
                            editor.set_extra_selections(Vec::new(), cx);
                        }
                    })
                },
            ),
            cx.subscribe(&chat, |this, _, event, cx| this.chat_event(event, cx)),
            cx.subscribe(&selection, |this, _, event, cx| match event {
                SelectionPopoverEvent::Comment(text) => {
                    this.chat.update(cx, |chat, cx| chat.comment(text, cx));
                }
                SelectionPopoverEvent::OptionApplied {
                    before,
                    text,
                    prompt,
                } => this.option_applied(before.clone(), text, prompt, cx),
                SelectionPopoverEvent::ContinueFailed(reason) => {
                    this.show_error(reason.clone(), cx);
                }
            }),
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
                    this.mark_preview(cx);
                }
                EditorEvent::ExtraSelectionsChanged => this.mark_preview(cx),
                EditorEvent::GhostAccepted(_)
                | EditorEvent::GhostDismissed
                | EditorEvent::TrackedClicked(_) => {}
            }),
            // Appearance is app-wide, and this window lives as long as the
            // app, so it reports appearance changes for every window.
            cx.observe_window_appearance(window, |_, window, cx| theme::sync_window(window, cx)),
            // Drafts created or archived by the CLI while the app was in the
            // background. Leaving the window saves pending edits, so the CLI
            // reads them without waiting for autosave; `flush` reports a
            // failure in the window.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.session.drafts_mut().refresh();
                } else {
                    this.session.flush().ok();
                }
                cx.notify();
            }),
            cx.on_app_quit(|this, _| {
                this.session.flush().ok();
                async {}
            }),
            cx.observe(&Updater::global(cx), |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &Updater::global(cx),
                window,
                |this, _, event: &update::Event, window, cx| match event {
                    update::Event::Notice { text, error: true } => {
                        this.show_error(text.clone(), cx);
                    }
                    update::Event::Notice { text, error: false } => {
                        this.show_toast(text.clone(), cx);
                    }
                    update::Event::ShowOffer => {
                        // The card answers the check; "Downloading…" is over.
                        this.toast = None;
                        window.activate_window();
                        this.open_update_card(window, cx);
                    }
                    update::Event::Restart => this.restart_to_update(cx),
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
        let split_ratio = store.split_ratio().unwrap_or(0.5);
        let show_sidebar = !store.sidebar_hidden();
        let preview = new_preview(preview_selection.clone(), preview_scroll.clone(), cx);
        let before_preview = {
            let scroll = ScrollHandle::new();
            let selection = cx.new(|cx| {
                let mut selection = TextSelection::new(cx);
                selection.set_scroller(Some(Scroller::Handle(scroll.clone())));
                selection
            });
            new_preview(selection, scroll, cx)
        };
        // The review columns scroll together.
        preview.update(cx, |preview, _| {
            preview.partner = Some(before_preview.downgrade())
        });
        before_preview.update(cx, |before, _| before.partner = Some(preview.downgrade()));
        let mut chrome = Self {
            should_move: false,
            session: Session::new(store.clone()),
            store,
            chat_width,
            save_chat_width: Task::ready(()),
            split_ratio,
            save_split_ratio: Task::ready(()),
            editor,
            formatter,
            chat,
            selection,
            preview,
            before_preview,
            before_scrollbar: Default::default(),
            preview_work: preview_work::Work::default(),
            preview_task: Task::ready(()),
            preview_source: String::new(),
            preview_selection,
            preview_map: None,
            mirrored: None,
            mirrored_flash: None,
            mirrored_requests: None,
            show_editor: true,
            show_preview: true,
            narrow: false,
            collapsed_for_width: false,
            toast: None,
            update_card: None,
            review: None,
            review_focus: cx.focus_handle(),
            copied: None,
            github_menu: false,
            file_hint_dismissed: HashSet::new(),
            row_menu: None,
            seen_rows: RefCell::default(),
            leaving_rows: Vec::new(),
            archived_section: Default::default(),
            sidebar_scroll: ScrollHandle::new(),
            sidebar_focus: cx.focus_handle(),
            content_focus: cx.focus_handle(),
            show_sidebar,
            panes: None,
            preview_scroll,
            scrollbars: Default::default(),
            autosave: Task::ready(()),
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
        chrome._subscriptions.push(cx.subscribe(
            &chrome.preview,
            |this, _, event: &preview::TaskToggled, cx| this.toggle_task(&event.0, cx),
        ));
        chrome._subscriptions.push(cx.subscribe(
            &chrome.preview,
            |this, _, event: &preview::FileHint, cx| match event {
                preview::FileHint::Accept => this.set_target(Target::GithubFile, cx),
                preview::FileHint::Dismiss => {
                    if let Some(id) = this.session.drafts().current_id() {
                        this.file_hint_dismissed.insert(id.to_owned());
                    }
                    this.preview.update(cx, |preview, cx| {
                        preview.file_hint = false;
                        cx.notify();
                    });
                }
            },
        ));
        if let Some(home) = std::env::home_dir() {
            let settings = AppSettings::global(cx).read(cx).settings.clone();
            if let Err(err) =
                drafts::first_launch_sample(chrome.session.drafts_mut(), &settings, &home)
            {
                eprintln!("draftmux: creating the sample draft: {err:#}");
            }
        }
        window.focus(&chrome.content_focus, cx);
        if let Some(id) = chrome
            .session
            .drafts()
            .rows()
            .first()
            .map(|row| row.draft.id.clone())
        {
            chrome.select(&id, window, cx).ok();
        }
        chrome
    }

    /// Creates a fresh sample draft and selects it.
    fn open_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::global(cx).read(cx).settings.clone();
        let created = std::env::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home directory"))
            .and_then(|home| drafts::create_sample(self.session.drafts_mut(), &settings, &home));
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

    /// Selects `id`, as `draftmux open` asks once it has created the draft,
    /// and `focus` and `restore` ask for one in the drafts list.
    pub fn focus_draft(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.session.drafts_mut().refresh();
        if !self
            .session
            .drafts()
            .rows()
            .iter()
            .any(|row| row.draft.id == id)
        {
            anyhow::bail!("draft {id} isn't in the drafts list");
        }
        self.select(id, window, cx)?;
        cx.notify();
        Ok(())
    }

    /// Loads `id` into the editor and preview, unless it's already selected.
    /// On failure, including failing to save the draft being left, shows the
    /// error and stays put.
    fn select(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        if self.session.drafts().current_id() == Some(id) {
            return Ok(());
        }
        let outcome = match self.session.select(id) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.show_error(format!("Couldn't open the draft: {err:#}"), cx);
                return Err(err);
            }
        };
        self.autosave = Task::ready(());
        self.preview_work.invalidate();
        self.set_preview(None, cx);
        if let Some(draft) = self.session.drafts().current() {
            let (dir, target) = (draft.dir.clone(), draft.meta.target);
            self.formatter
                .update(cx, |formatter, cx| formatter.set_draft_dir(dir, cx));
            self.set_target_markup(target, cx);
        }
        self.sync_selection_draft(cx);
        self.apply(outcome, None, cx);
        self.load_changes(cx);
        let draft = self.session.drafts().current().cloned();
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
        let draft = self.session.drafts().current().cloned();
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
            cx.background_executor().timer(RESIZE_SAVE_DELAY).await;
            if let Err(err) = store.set_chat_width(f32::from(width)) {
                eprintln!("draftmux: saving the chat width: {err:#}");
            }
        });
        cx.notify();
    }

    fn resize_split(&mut self, ratio: f32, cx: &mut Context<Self>) {
        if ratio == self.split_ratio {
            return;
        }
        self.split_ratio = ratio;
        let store = self.store.clone();
        self.save_split_ratio = cx.spawn(async move |_, cx| {
            cx.background_executor().timer(RESIZE_SAVE_DELAY).await;
            if let Err(err) = store.set_split_ratio(ratio) {
                eprintln!("draftmux: saving the split ratio: {err:#}");
            }
        });
        cx.notify();
    }

    /// Scales the editor's, preview's and chat's text and remembers it.
    fn zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = theme::set_zoom(zoom, cx);
        if let Err(err) = self.store.set_text_zoom(zoom) {
            eprintln!("draftmux: saving the text zoom: {err:#}");
        }
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_sidebar = !self.show_sidebar;
        self.row_menu = None;
        if !self.show_sidebar && self.sidebar_focus.contains_focused(window, cx) {
            if self.show_editor && self.session.drafts().current().is_some() {
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

    /// Collapses the editor of a side-by-side layout when the window gets
    /// too narrow for both panes, and shows it again once the window is wide
    /// enough. Only crossing the threshold acts, so the user's own toggles
    /// hold until the next crossing.
    fn fit_to_width(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let narrow = split_width(
            window.viewport_size().width,
            self.chat_width,
            self.show_sidebar,
        ) < SPLIT_MIN_WIDTH;
        if narrow == self.narrow {
            return;
        }
        self.narrow = narrow;
        if narrow {
            if self.show_editor && self.show_preview {
                self.show_editor = false;
                self.collapsed_for_width = true;
                if self.editor.focus_handle(cx).contains_focused(window, cx) {
                    self.focus_sidebar(window, cx);
                }
            }
        } else if std::mem::take(&mut self.collapsed_for_width) {
            self.show_editor = true;
        }
    }

    /// Hiding the only visible pane shows the other one instead.
    fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.collapsed_for_width = false;
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
        self.collapsed_for_width = false;
        if self.show_preview && !self.show_editor {
            self.toggle_editor(window, cx);
        }
        // Review happens in the preview.
        if self.show_preview {
            self.close_review(window, cx);
        }
        self.show_preview = !self.show_preview;
        cx.notify();
    }

    /// Empties the editor after the selected draft left the list.
    fn deselect(&mut self, cx: &mut Context<Self>) {
        self.preview_work.invalidate();
        self.autosave = Task::ready(());
        self.set_preview(None, cx);
        let outcome = self.session.deselect();
        self.apply(outcome, None, cx);
        self.load_changes(cx);
        self.chat.update(cx, |chat, cx| chat.set_draft(None, cx));
        self.sync_selection_draft(cx);
        self.sync_agent(cx);
    }

    fn chat_event(&mut self, event: &ChatEvent, cx: &mut Context<Self>) {
        match event {
            ChatEvent::Send(text) => {
                debug_assert_eq!(self.session.text(), self.editor.read(cx).text());
                let outcome = self.session.send(text.clone(), crate::chat::message(text));
                self.apply(outcome, None, cx);
            }
            ChatEvent::FirstDraft { id, text } => {
                debug_assert_eq!(self.session.text(), self.editor.read(cx).text());
                let outcome = self.session.first_draft_ready(id, text.clone());
                self.apply(outcome, None, cx);
                // The preview stops showing the first draft as still to come.
                self.sync_agent(cx);
            }
            ChatEvent::Accepted { id } => self.session.prompt_accepted(id),
            ChatEvent::DraftWritten { id, text } => {
                let current = self.session.drafts().current_id() == Some(id.as_str());
                // Show the write now if no edit is pending; otherwise the
                // next outside-edit check asks which version to keep.
                let (recorded, outcome) = self.session.agent_wrote(id, text, Utc::now());
                match recorded {
                    Ok(Recorded::Added) if current => self.edit_recorded(cx),
                    Ok(_) => {}
                    Err(err) => eprintln!("draftmux: saving the edit history: {err:#}"),
                }
                self.apply(outcome, None, cx);
            }
            ChatEvent::Recorded { id, record } => {
                let recorded = self
                    .session
                    .drafts_mut()
                    .update_backend(id, |backend| crate::chat::record(backend, record));
                if let Err(err) = recorded {
                    eprintln!("draftmux: recording draft {id}'s Claude Code session: {err:#}");
                }
                // Selection calls use the draft's model and transcript.
                let draft = self.session.drafts().current().cloned();
                self.selection
                    .update(cx, |selection, _| selection.refresh_draft(draft));
            }
            ChatEvent::SwitchAgent { id } => self.switch_agent(id, cx),
            ChatEvent::Running { id, running } => {
                self.session.agent_running(id, *running);
                if self.session.drafts().current_id() != Some(id.as_str()) {
                    return;
                }
                self.sync_agent(cx);
            }
        }
        cx.notify();
    }

    /// Moves draft `id`, which continues no session, to a new, empty
    /// session on the other agent, with the settings for its directory, and
    /// restarts its chat there.
    fn switch_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(draft) = self
            .session
            .drafts()
            .current()
            .filter(|draft| draft.id == id && draft.meta.source.is_none())
            .cloned()
        else {
            return;
        };
        let kind = match draft.meta.backend {
            draftmux_storage::Backend::Claude(_) => edit::AgentKind::Pi,
            draftmux_storage::Backend::Pi(_) => edit::AgentKind::Claude,
        };
        let Some(home) = std::env::home_dir() else {
            self.show_error("Couldn't switch agents: no home directory", cx);
            return;
        };
        let resolved = crate::app_settings::AppSettings::global(cx)
            .read(cx)
            .settings
            .resolve(&draft.meta.cwd, &home);
        let claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
        let switched = edit::empty_session(
            kind,
            &draft.meta.cwd,
            &draft.session_dir(),
            &resolved,
            &home,
            claude_config_dir.as_deref(),
        )
        .and_then(|backend| {
            self.session.drafts_mut().set_backend(
                id,
                backend,
                edit::session_settings(kind, &resolved),
            )
        });
        if let Err(err) = switched {
            self.show_error(format!("Couldn't switch agents: {err:#}"), cx);
            return;
        }
        let draft = self.session.drafts().current().cloned();
        if let Some(draft) = &draft {
            self.chat.update(cx, |chat, cx| chat.restart(draft, cx));
        }
        // Selection calls use the draft's agent.
        self.selection
            .update(cx, |selection, _| selection.refresh_draft(draft));
    }

    /// Shows what the selected draft's agent is doing: the editor is
    /// read-only while it runs, and the preview shows it writing.
    fn sync_agent(&mut self, cx: &mut Context<Self>) {
        let running = self.session.editor_read_only();
        let writing = self.session.writing();
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
        // Selection actions are unavailable while the editor is read-only.
        if running {
            self.selection
                .update(cx, |selection, cx| selection.dismiss(cx));
        }
    }

    fn edited(&mut self, text: String, cx: &mut Context<Self>) {
        let _timing = editor::timing::span("edit-subscribers");
        let Some(target) = self
            .session
            .drafts()
            .current()
            .map(|draft| draft.meta.target)
        else {
            return;
        };
        let outcome = self.session.edited(text.clone(), Instant::now());
        self.apply(outcome, None, cx);
        self.sync_review(cx);
        self.preview_selection
            .update(cx, |selection, cx| selection.clear(cx));
        // Review shows its own parse of the text, which the preview takes
        // when review closes.
        if self.review.is_some() {
            self.preview_work.invalidate();
            return;
        }
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
        let target = self
            .session
            .drafts()
            .current()
            .map(|draft| draft.meta.target);
        let file_hint = target == Some(Target::Github)
            && self
                .session
                .drafts()
                .current_id()
                .is_some_and(|id| !self.file_hint_dismissed.contains(id))
            && preview.as_ref().is_some_and(markup_view::looks_wrapped);
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
            this.file_hint = file_hint;
            cx.notify();
        });
        self.preview_map = None;
        self.mirrored_requests = None;
        self.preview_selection
            .update(cx, |selection, cx| selection.reset(cx));
    }

    /// Checks or unchecks the task list item at `path` in the preview by
    /// editing its checkbox in the editor, as one undoable edit that leaves
    /// the selection and scroll position alone. Does nothing while the
    /// agent is writing, or when the item has changed since the preview
    /// was drawn.
    fn toggle_task(&mut self, path: &[usize], cx: &mut Context<Self>) {
        if self.review.is_some() || self.editor.read(cx).is_read_only() {
            return;
        }
        let draft = markup::frontmatter::parse(&self.preview_source);
        let Some((range, with)) = markup::github::toggle_task(draft.body, path) else {
            return;
        };
        let range = range.start + draft.body_offset..range.end + draft.body_offset;
        let Some(range) =
            preview_work::unchanged_range(&self.preview_source, self.editor.read(cx).text(), range)
        else {
            return;
        };
        self.editor.update(cx, |editor, cx| {
            editor.replace_range_in_place(range, with, cx)
        });
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

    /// Marks the editor's extra selections in the preview, less those its
    /// selection replaces.
    fn mark_preview(&mut self, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        let selection = editor.selected_range();
        let ranges: Vec<_> = editor
            .selected_ranges()
            .into_iter()
            .filter(|range| *range != selection)
            .collect();
        let mut marks = Vec::new();
        // Edits drop the extra selections, so a preview of other text has
        // none to show.
        if !ranges.is_empty() && editor.text() == self.preview_source {
            marks = self.preview_marks(ranges, cx);
        }
        self.preview_selection
            .update(cx, |selection, cx| selection.set_marks(marks, cx));
    }

    /// Where `ranges` of the previewed markdown were rendered in the preview.
    fn preview_marks(&mut self, ranges: Vec<Range<usize>>, cx: &App) -> Vec<(Pos, Pos)> {
        let selection = self.preview_selection.read(cx);
        let map = self.preview_map.get_or_insert_with(|| {
            SourceMap::new(
                markup::frontmatter::parse(&self.preview_source).body,
                selection.paragraphs(),
            )
        });
        let body_offset = markup::frontmatter::parse(&self.preview_source).body_offset;
        ranges
            .into_iter()
            .filter(|range| range.end > body_offset)
            .filter_map(|range| {
                map.rendered(range.start.saturating_sub(body_offset)..range.end - body_offset)
            })
            .collect()
    }

    /// Marks in the preview the passages selection requests wait on, once
    /// the preview shows the text they're tracked in and has published its
    /// paragraphs.
    fn mirror_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        if !self.show_preview || editor.text() != self.preview_source {
            return;
        }
        let requests = editor.tracked_ranges();
        if self.mirrored_requests.as_ref() == Some(&requests) {
            return;
        }
        if !requests.is_empty()
            && self
                .preview_selection
                .read(cx)
                .paragraphs()
                .next()
                .is_none()
        {
            window.request_animation_frame();
            return;
        }
        let marks: Vec<_> = requests
            .iter()
            .filter_map(|(id, range)| {
                let (start, end) = self.preview_marks(vec![range.clone()], cx).pop()?;
                Some((u64::from(*id), start, end))
            })
            .collect();
        self.mirrored_requests = Some(requests);
        let palette = theme::palette(cx);
        let (tint, line) = (
            palette.change_mark.opacity(0.5),
            palette.accent.opacity(0.55),
        );
        // A notification while drawing doesn't redraw the cached preview.
        let selection = self.preview_selection.clone();
        window.on_next_frame(move |_, cx| {
            selection.update(cx, |selection, cx| {
                selection.set_tracked(marks, tint, line, cx)
            });
        });
    }

    /// Tints in the preview what the editor's flash points out, once the
    /// preview shows the flashed text, fading in step with the editor.
    fn mirror_flash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        let Some((ranges, started)) = editor.flashing() else {
            return;
        };
        if self.mirrored_flash == Some(started) {
            return;
        }
        // The preview catches up with the text, and publishes its
        // paragraphs as it paints, a frame or more later.
        if editor.text() != self.preview_source
            || self
                .preview_selection
                .read(cx)
                .paragraphs()
                .next()
                .is_none()
        {
            window.request_animation_frame();
            return;
        }
        let ranges = ranges.to_vec();
        self.mirrored_flash = Some(started);
        let marks = self.preview_marks(ranges, cx);
        let color = theme::palette(cx).change_flash;
        // A notification while drawing doesn't redraw the cached preview,
        // so the tint is handed over before the next frame.
        let selection = self.preview_selection.clone();
        window.on_next_frame(move |_, cx| {
            selection.update(cx, |selection, cx| {
                selection.set_flash(marks, started, color, cx)
            });
        });
    }

    /// Reloads an outside change to the selected draft, or asks which
    /// version to keep when the user has unsaved edits.
    fn check_outside_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let outcome = self.session.check_outside();
        let changed = !outcome.effects.is_empty();
        // A held edit resumes saving with only a deadline.
        self.apply(outcome, Some(window), cx);
        if changed {
            cx.notify();
        }
    }

    fn ask_outside_edit(
        &mut self,
        question: Question,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            let choice = match answer.await.ok() {
                Some(0) => Choice::UseNew,
                Some(1) => Choice::KeepMine,
                _ => Choice::Dismissed,
            };
            this.update(cx, |this, cx| {
                let outcome = this.session.answer(question, choice);
                this.apply(outcome, None, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Carries out `outcome`'s effects in order, then arms the autosave
    /// timer for its deadline. Only [`Session::check_outside`] asks a
    /// question, which needs the `window`.
    fn apply(&mut self, outcome: Outcome, mut window: Option<&mut Window>, cx: &mut Context<Self>) {
        for effect in outcome.effects {
            match effect {
                Effect::LoadText { epoch, text } => {
                    self.editor
                        .update(cx, |editor, cx| editor.set_text(&text, cx));
                    self.session.loaded(epoch, self.editor.read(cx).text());
                }
                Effect::ReplaceText { epoch, text } => {
                    self.editor.update(cx, |editor, cx| {
                        crate::changes::replace_all(editor, &text, cx)
                    });
                    self.session.loaded(epoch, self.editor.read(cx).text());
                }
                Effect::Prompt { text, saved } => {
                    self.chat
                        .update(cx, |chat, cx| chat.prompt(text, saved, cx));
                }
                Effect::Offer(text) => {
                    self.chat.update(cx, |chat, cx| chat.offer(&text, cx));
                }
                Effect::AskOutsideEdit(question) => {
                    let window = window
                        .as_deref_mut()
                        .expect("a question is asked only with the window");
                    self.ask_outside_edit(question, window, cx);
                }
                Effect::ReplaceRange {
                    epoch,
                    revision,
                    range,
                    text,
                } => {
                    if self.session.is_current(epoch, revision) {
                        // One undoable edit, as if typed.
                        self.editor
                            .update(cx, |editor, cx| editor.replace_range(range, &text, cx));
                    }
                }
                Effect::ReplaceAll {
                    epoch,
                    revision,
                    text,
                } => {
                    if self.session.is_current(epoch, revision) {
                        self.editor
                            .update(cx, |editor, cx| editor.replace_all(&text, cx));
                    }
                }
            }
        }
        let Some(deadline) = outcome.deadline else {
            return;
        };
        let epoch = self.session.epoch();
        self.autosave = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(deadline.saturating_duration_since(Instant::now()))
                .await;
            this.update(cx, |this, cx| {
                let outcome = this.session.wake(epoch, Instant::now());
                this.apply(outcome, None, cx);
                cx.notify();
            })
            .ok();
        });
    }

    /// Saves the drafts and restarts into the installed update.
    fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if self.flush_before_exit(cx) {
            cx.restart();
        }
    }

    /// Saves before the window closes or the app quits. On failure, shows the
    /// error and returns false so the unsaved text stays in the editor.
    pub fn flush_before_exit(&mut self, cx: &mut Context<Self>) -> bool {
        match self.session.flush() {
            Ok(()) => true,
            Err(err) => {
                self.show_error(format!("Couldn't close: {err:#}"), cx);
                false
            }
        }
    }

    fn set_target(&mut self, target: Target, cx: &mut Context<Self>) {
        if let Err(err) = self.session.drafts_mut().set_target(target) {
            self.show_error(format!("Couldn't change the target: {err:#}"), cx);
            return;
        }
        self.preview_work.invalidate();
        self.set_target_markup(target, cx);
        let preview = markup_view::preview(target, self.editor.read(cx).text());
        self.set_preview(Some(preview), cx);
        self.sync_selection_draft(cx);
        self.sync_review(cx);
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
        let Some(target) = self
            .session
            .drafts()
            .current()
            .map(|draft| draft.meta.target)
        else {
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

    /// Writes the editor's text to the file the selected draft was opened
    /// from. Unless `overwrite`, a file changed or removed outside the draft
    /// is left alone until the user confirms replacing it.
    fn save_to_file(&mut self, overwrite: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self
            .session
            .drafts()
            .current()
            .and_then(|draft| draft.meta.file.clone())
        else {
            return;
        };
        let name = file_name(&file.path);
        let text = self.editor.read(cx).text().to_owned();
        let detail = match self.session.drafts_mut().save_to_file(&text, overwrite) {
            Ok(Some(FileSave::Saved(_))) => {
                self.show_toast(format!("Saved {name}"), cx);
                cx.notify();
                return;
            }
            Ok(None) => return,
            Ok(Some(FileSave::Changed)) => {
                "It was changed outside this draft since the draft \
                 last read or saved it. Saving replaces those changes."
            }
            Ok(Some(FileSave::Missing)) => {
                "It was removed since the draft last read or saved \
                 it. Saving creates it again."
            }
            Err(err) => {
                self.show_error(format!("Couldn't save {name}: {err:#}"), cx);
                cx.notify();
                return;
            }
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Save {name} anyway?"),
            Some(detail),
            &["Cancel", "Save Anyway"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(1) {
                this.update_in(cx, |this, window, cx| this.save_to_file(true, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let current = self.session.drafts().current();
        let title: SharedString = match current {
            Some(draft) => self
                .session
                .drafts()
                .rows()
                .iter()
                .find(|row| row.draft.id == draft.id)
                .map_or_else(
                    || "Untitled draft".into(),
                    |row| markup::replace_shortcodes(&row.title).into(),
                ),
            None => "Draftmux".into(),
        };
        let subtitle = match (self.session.save_error(), current) {
            (Some(error), _) => Some((SharedString::from(error.to_owned()), palette.danger)),
            // The sample's directory is just where it was created.
            (None, Some(draft)) if draft.meta.is_sample() => None,
            // A draft opened from a file shows the file, and whether it
            // holds the editor's text.
            (None, Some(draft)) => {
                let home = std::env::home_dir();
                let text = match &draft.meta.file {
                    Some(file) => {
                        let path = tilde(&file.path, home.as_deref());
                        let text = self.editor.read(cx).text().as_bytes();
                        if draftmux_storage::sha256(text) == file.sha256 {
                            path
                        } else {
                            format!("{path} · Edited")
                        }
                    }
                    None => tilde(&draft.meta.cwd, home.as_deref()),
                };
                Some((markup::replace_shortcodes(&text).into(), palette.faint))
            }
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

    /// Draft actions at the toolbar's right end. Their mouse-downs don't reach
    /// the toolbar, so clicking them never moves or zooms the window.
    fn toolbar_actions(
        &self,
        target: Target,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_file = self
            .session
            .drafts()
            .current()
            .is_some_and(|draft| draft.meta.file.is_some());
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(self.review_button(palette, cx))
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
            .child(target_switch(target, self.github_menu, palette, cx))
            .children(has_file.then(|| save_button(palette, cx)))
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

/// The toolbar button that writes the draft back to the file it was opened
/// from.
fn save_button(palette: &Palette, cx: &mut Context<Chrome>) -> impl IntoElement {
    div()
        .id("save-to-file")
        .flex_none()
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .hover(|style| style.bg(palette.hover))
        .active(|style| style.opacity(0.8))
        .on_click(cx.listener(|this, _, window, cx| this.save_to_file(false, window, cx)))
        .child("Save")
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::NORMAL)
                .text_color(palette.faint)
                .child(editor::by_platform!("⌘S", "Ctrl+S")),
        )
}

/// The last component of `path`, else the whole path.
fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The segmented control with every target. Both GitHub targets share one
/// segment, whose menu switches between them.
fn target_switch(
    selected: Target,
    github_menu: bool,
    palette: &Palette,
    cx: &mut Context<Chrome>,
) -> impl IntoElement {
    let palette = *palette;
    let is_github = |target: Target| matches!(target, Target::Github | Target::GithubFile);
    let segment = |target: Target, cx: &mut Context<Chrome>| -> AnyElement {
        let github = is_github(target);
        let on = target == selected || github && is_github(selected);
        let label = if on { selected.label() } else { target.label() };
        let segment = div()
            .id(target.name())
            .h(px(20.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(5.5))
            .text_size(px(12.))
            .child(label)
            .when(github, |segment| {
                segment.mr(px(-2.)).child(
                    svg()
                        .path(CHEVRON_ICON)
                        .size(px(8.))
                        .text_color(palette.muted)
                        .with_transformation(Transformation::rotate(percentage(0.25))),
                )
            });
        if !on {
            return segment
                .active(|style| style.opacity(0.8))
                .on_click(cx.listener(move |this, _, _, cx| this.set_target(target, cx)))
                .into_any_element();
        }
        let segment = segment
            .when(github, |segment| {
                segment.on_click(cx.listener(|this, _, _, cx| {
                    this.github_menu = !this.github_menu;
                    cx.notify();
                }))
            })
            .with_animation(
                ("segment-on", target as usize),
                Animation::new(SEGMENT_FADE).with_easing(ease_out_quint()),
                move |segment, delta| {
                    segment.bg(palette.segment_on.opacity(delta)).shadow(vec![
                        shadow(palette.segment_ring.opacity(delta), 0., 0., 0.5),
                        shadow(palette.segment_drop.opacity(delta), 1., 2.5, 0.),
                    ])
                },
            );
        div()
            .relative()
            .child(segment)
            .when(github && github_menu, |group| {
                group.child(
                    div().absolute().top_full().left_0().mt_1().child(deferred(
                        anchored()
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(github_menu_panel(selected, &palette, cx)),
                    )),
                )
            })
            .into_any_element()
    };
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(7.))
        .bg(palette.segment_track)
        .children(
            Target::ALL
                .iter()
                .filter(|&&target| target != Target::GithubFile)
                .map(|&target| segment(target, cx))
                .collect::<Vec<_>>(),
        )
}

/// The GitHub segment's menu: a comment, issue or PR, or a file in a
/// repository.
fn github_menu_panel(
    selected: Target,
    palette: &Palette,
    cx: &mut Context<Chrome>,
) -> impl IntoElement + use<> {
    let item = |label: &'static str, detail: Option<&'static str>, target| MenuItem {
        label: label.into(),
        checked: selected == target,
        enabled: true,
        action: Some(target),
        detail: detail.map(Into::into),
        heading: false,
    };
    let items = [
        item("Comment, Issue or PR", None, Target::Github),
        item("Repository File", Some("README, docs"), Target::GithubFile),
    ];
    let pick = cx.entity().downgrade();
    let dismiss = cx.entity().downgrade();
    menu_panel(
        palette,
        &items,
        move |target, _, cx| {
            pick.update(cx, |this, cx| {
                this.github_menu = false;
                this.set_target(target, cx);
            })
            .ok();
        },
        move |_, cx| {
            dismiss
                .update(cx, |this, cx| {
                    this.github_menu = false;
                    cx.notify();
                })
                .ok();
        },
    )
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

/// The room the editor and preview share beside the sidebar and chat.
fn split_width(viewport_width: Pixels, chat_width: Pixels, show_sidebar: bool) -> Pixels {
    let sidebar_width = if show_sidebar { SIDEBAR_WIDTH } else { px(0.) };
    viewport_width - sidebar_width - clamp_chat_width(chat_width, viewport_width, show_sidebar)
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

/// The editor's share of the `split` room for the user's `ratio`, leaving
/// each pane at least [`SPLIT_PANE_MIN_WIDTH`] when the room allows it.
fn editor_share(ratio: f32, split: Pixels) -> f32 {
    let min = (SPLIT_PANE_MIN_WIDTH / split).clamp(0., 0.5);
    ratio.clamp(min, 1. - min)
}

/// The ratio for the divider dragged to `x` across `width`, snapped to an
/// even split near the middle.
fn drag_ratio(x: Pixels, width: Pixels) -> f32 {
    if (x - width / 2.).abs() <= SPLIT_SNAP {
        return 0.5;
    }
    editor_share(x / width, width)
}

/// Flex grow factors giving the editor `share` of the room and the preview
/// the rest. Neither is below 1, since factors summing to under 1 would
/// leave part of the room empty.
fn split_grow(share: f32) -> [f32; 2] {
    let unit = share.min(1. - share).max(f32::EPSILON);
    [share / unit, (1. - share) / unit]
}

/// A strip over the divider between the editor and the preview that drags
/// to resize them; a double click restores the even split.
fn split_resize_handle(palette: &Palette, cx: &mut Context<Chrome>) -> impl IntoElement {
    div()
        .id("split-resize")
        .group("split-resize")
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-3.))
        .w(px(6.))
        .cursor(CursorStyle::ResizeLeftRight)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_drag(SplitResize, |_, _, _, cx| cx.new(|_| Empty))
        .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
            if event.click_count() == 2 {
                this.resize_split(0.5, cx);
            }
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(2.))
                .w(px(2.))
                .group_hover("split-resize", |style| style.bg(palette.line_strong)),
        )
}

/// A pane's place in the window, sliding open or shut.
#[derive(Clone)]
struct Slide {
    shown: bool,
    /// How open the pane was when the slide started, and when.
    from: f32,
    start: Option<Instant>,
}

impl Slide {
    fn new(shown: bool) -> Self {
        Self {
            shown,
            from: if shown { 1. } else { 0. },
            start: None,
        }
    }

    fn progress(&self, now: Instant) -> Option<f32> {
        let elapsed = now.saturating_duration_since(self.start?);
        let linear = elapsed.as_secs_f32() / PANE_SLIDE.as_secs_f32();
        (linear < 1.).then(|| ease_out_quint()(linear))
    }

    /// How open the pane is, from 0 to 1.
    fn open(&self, now: Instant) -> f32 {
        let target = if self.shown { 1. } else { 0. };
        match self.progress(now) {
            Some(delta) => self.from + (target - self.from) * delta,
            None => target,
        }
    }

    /// Slides toward `shown` from wherever the pane is, and returns how
    /// open it is now.
    fn set(&mut self, shown: bool, now: Instant) -> f32 {
        if shown != self.shown {
            self.from = self.open(now);
            self.shown = shown;
            self.start = (!crate::platform::reduce_motion()).then_some(now);
        }
        self.open(now)
    }

    fn running(&self, now: Instant) -> bool {
        self.progress(now).is_some()
    }

    /// Whether the pane was open when the slide started.
    fn was_open(&self) -> bool {
        self.from > 0.5
    }
}

/// Holds the editor or the preview, taking the room in proportion to
/// `grow`, which follows how open its pane is while it slides. It shows
/// the pane from its start or, with `end`, its end, so a pane wider than
/// the slot is clipped on the side it slides out from.
fn slot(grow: f32, end: bool) -> gpui::Div {
    let mut slot = div().h_full().min_w_0().flex().overflow_hidden();
    let style = slot.style();
    style.flex_grow = Some(grow);
    style.flex_shrink = Some(1.);
    style.flex_basis = Some(gpui::relative(0.).into());
    slot.when(end, |slot| slot.justify_end())
}

fn new_preview(
    selection: Entity<TextSelection>,
    scroll: ScrollHandle,
    cx: &mut Context<Chrome>,
) -> Entity<Preview> {
    let redline_selection = cx.new(|cx| {
        let mut selection = TextSelection::new(cx);
        selection.set_scroller(Some(Scroller::Handle(scroll.clone())));
        selection
    });
    cx.new(|cx| Preview {
        document: None,
        fields: Default::default(),
        copied_field: None,
        copied_task: Task::ready(()),
        target: None,
        _selections: [
            cx.observe(&selection, |_, _, cx| cx.notify()),
            cx.observe(&redline_selection, |_, _, cx| cx.notify()),
        ],
        selection,
        redline_selection,
        scroll,
        cache: Default::default(),
        appearance: None,
        writing: false,
        file_hint: false,
        redline: None,
        partner: None,
        marks: Default::default(),
    })
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

    #[test]
    fn editor_share_keeps_both_panes_at_their_minimum() {
        assert_eq!(editor_share(0.5, px(960.)), 0.5);
        assert_eq!(editor_share(0.7, px(960.)), 0.7);
        assert_eq!(editor_share(0.95, px(960.)), 0.75);
        assert_eq!(editor_share(0.05, px(960.)), 0.25);
        // Too narrow for both minimums: an even split.
        assert_eq!(editor_share(0.8, px(400.)), 0.5);
    }

    #[test]
    fn drag_ratio_snaps_to_an_even_split_near_the_middle() {
        assert_eq!(drag_ratio(px(490.), px(960.)), 0.5);
        assert_eq!(drag_ratio(px(468.), px(960.)), 0.5);
        assert_eq!(drag_ratio(px(528.), px(960.)), 0.55);
        assert_eq!(drag_ratio(px(950.), px(960.)), 0.75);
        assert_eq!(drag_ratio(px(-50.), px(960.)), 0.25);
    }

    #[test]
    fn split_grow_fills_the_room_in_proportion() {
        assert_eq!(split_grow(0.5), [1., 1.]);
        assert_eq!(split_grow(0.75), [3., 1.]);
        assert_eq!(split_grow(0.25), [1., 3.]);
    }

    #[test]
    fn split_width_leaves_out_the_sidebar_and_chat() {
        assert_eq!(split_width(px(1200.), CHAT_WIDTH, true), px(632.));
        assert_eq!(split_width(px(1200.), CHAT_WIDTH, false), px(860.));
        // The chat gives up its width before the content goes under its
        // minimum.
        assert_eq!(split_width(px(1000.), px(600.), true), CONTENT_MIN_WIDTH);
    }
}
