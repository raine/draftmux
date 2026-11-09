//! The drafts list and the archive toast.

use std::time::Duration;

use chrono::Utc;
use draftmux_storage::Target;
use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, Hsla, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Task, anchored, deferred, div, img, prelude::*, px, svg,
};

use super::{Chrome, shadow, sidebar_column};
use crate::assets::{ARCHIVE_ICON, GITHUB_ICON, PLAIN_TEXT_ICON, RICH_TEXT_ICON, SLACK_ICON};
use crate::drafts::{relative_time, tilde};
use crate::menu::{MenuItem, menu_panel};
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::theme;

const TOAST_DURATION: Duration = Duration::from_secs(4);

pub(super) struct Toast {
    text: SharedString,
    /// Draft to unarchive when Undo is clicked.
    undo: Option<String>,
    _dismiss: Task<()>,
}

/// A right-clicked draft row's menu, at the click.
#[derive(Clone, PartialEq)]
pub(super) struct RowMenu {
    id: String,
    position: Point<Pixels>,
}

#[derive(Clone, Copy)]
enum RowAction {
    CopyId,
    Reveal,
    Archive,
}

impl Chrome {
    pub(super) fn sidebar(&self, bar: &ScrollbarState, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let now = Utc::now();
        let home = std::env::home_dir();
        let current = self.drafts.current_id();
        // Ids follow the draft, not its position, so a press that straddles
        // a reorder can't land on another draft.
        let rows = self.drafts.rows().iter().map(|row| {
            let id = row.draft.id.clone();
            let selected = current == Some(id.as_str());
            let menu_open = self.row_menu.as_ref().is_some_and(|menu| menu.id == id);
            let group: SharedString = format!("draft-row-{id}").into();
            div()
                .id(group.clone())
                .group(group.clone())
                .relative()
                .flex()
                .gap(px(8.))
                .px(px(8.))
                .py(px(7.))
                .rounded(px(6.))
                .when(selected, |row| row.bg(palette.selected_row))
                // As in Finder and Mail, the right-clicked row gets a ring
                // without being selected.
                .when(menu_open, |row| {
                    row.shadow(vec![shadow(palette.accent, 0., 0., 2.)])
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener({
                        let id = id.clone();
                        move |this, event: &MouseDownEvent, _, cx| {
                            this.row_menu = Some(RowMenu {
                                id: id.clone(),
                                position: event.position,
                            });
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }),
                )
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.select(&id, window, cx).ok();
                    }
                }))
                .child(
                    div()
                        .size(px(16.))
                        .h(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(platform_icon(row.draft.meta.target, palette.text)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(13.))
                                        .line_height(px(18.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(row.title.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(11.))
                                        .text_color(palette.faint)
                                        .group_hover(group.clone(), |style| style.opacity(0.))
                                        .child(relative_time(row.draft.meta.updated_at, now)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .when(row.draft.meta.is_sample(), |line| {
                                    line.child(sample_badge(&palette))
                                })
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.))
                                        .text_color(palette.muted)
                                        .child(tilde(&row.draft.meta.cwd, home.as_deref())),
                                ),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("archive-{id}")))
                        .absolute()
                        .top(px(5.))
                        .right(px(6.))
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .bg(if selected {
                            palette.archive_button_selected
                        } else {
                            palette.archive_button
                        })
                        .opacity(0.)
                        .group_hover(group, |style| style.opacity(1.))
                        .hover(|style| style.bg(palette.archive_button_hover))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.archive(&id, window, cx)),
                        )
                        .child(
                            svg()
                                .path(ARCHIVE_ICON)
                                .size(px(14.))
                                .text_color(palette.muted),
                        ),
                )
        });

        sidebar_column(&palette)
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(16.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.faint)
                    .child("Drafts"),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("drafts")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            // Padding goes on an inner column: gpui sizes an
                            // empty scroll container's content to the
                            // container and adds its padding, which would
                            // make an empty list scrollable.
                            .child(
                                div()
                                    .px(px(8.))
                                    .pb(px(12.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .children(rows),
                            ),
                    )
                    .child(scrollbar(Source::Handle(self.sidebar_scroll.clone()), bar)),
            )
            .children(self.row_menu(&palette, cx))
    }

    fn row_menu(
        &self,
        palette: &theme::Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let menu = self.row_menu.clone()?;
        let item = |label: &str, action| MenuItem {
            label: SharedString::from(label.to_owned()),
            checked: false,
            action: Some(action),
        };
        let items = [
            item("Copy Draft ID", RowAction::CopyId),
            item(crate::platform::REVEAL_LABEL, RowAction::Reveal),
            MenuItem {
                label: SharedString::default(),
                checked: false,
                action: None,
            },
            item("Archive", RowAction::Archive),
        ];
        let this = cx.entity().downgrade();
        let id = menu.id.clone();
        let panel = menu_panel(palette, &items, move |action, window, cx| {
            this.update(cx, |this, cx| this.row_action(&id, action, window, cx))
                .ok();
        });
        Some(deferred(
            anchored()
                .position(menu.position)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        // A right-click on another row replaces the menu
                        // before this runs, so only close the menu it shows.
                        .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                            if this.row_menu.as_ref() == Some(&menu) {
                                this.row_menu = None;
                                cx.notify();
                            }
                        }))
                        .child(panel),
                ),
        ))
    }

    fn row_action(
        &mut self,
        id: &str,
        action: RowAction,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.row_menu = None;
        match action {
            RowAction::CopyId => {
                cx.write_to_clipboard(ClipboardItem::new_string(id.to_owned()));
                self.show_toast("Draft ID copied".into(), None, cx);
            }
            RowAction::Reveal => {
                if let Some(row) = self.drafts.rows().iter().find(|row| row.draft.id == id) {
                    cx.reveal_path(&row.draft.dir);
                }
            }
            RowAction::Archive => self.archive(id, window, cx),
        }
        cx.notify();
    }

    fn archive(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let was_current = self.drafts.current_id() == Some(id);
        let next = match self.drafts.archive(id) {
            Ok(next) => next,
            Err(err) => {
                self.show_toast(format!("Couldn't archive the draft: {err:#}"), None, cx);
                return;
            }
        };
        if was_current {
            match next {
                Some(next) => {
                    if self.select(&next, window, cx).is_err() {
                        self.deselect(cx);
                    }
                }
                None => self.deselect(cx),
            }
        }
        self.show_toast("Draft archived".into(), Some(id.to_owned()), cx);
    }

    fn undo_archive(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.toast = None;
        if let Err(err) = self.drafts.unarchive(id) {
            self.show_toast(format!("Couldn't restore the draft: {err:#}"), None, cx);
            return;
        }
        if self.drafts.current().is_none() {
            self.select(id, window, cx).ok();
        }
        cx.notify();
    }

    /// Shows `text` for a few seconds, replacing any earlier toast.
    pub(super) fn show_toast(
        &mut self,
        text: String,
        undo: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let dismiss = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            })
            .ok();
        });
        self.toast = Some(Toast {
            text: text.into(),
            undo,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    pub(super) fn toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let toast = self.toast.as_ref()?;
        let palette = *theme::palette(cx);
        Some(
            div()
                .id("toast")
                .occlude()
                .absolute()
                .left(px(16.))
                .bottom(px(16.))
                .max_w(px(420.))
                .flex()
                .items_center()
                .gap_3()
                .py(px(8.))
                .pl(px(12.))
                .pr(px(if toast.undo.is_some() { 8. } else { 12. }))
                .rounded(px(9.))
                .bg(palette.toast)
                .text_color(palette.toast_text)
                .text_size(px(13.))
                .shadow(vec![shadow(palette.toast_drop, 10., 30., 0.)])
                .child(div().min_w_0().child(toast.text.clone()))
                .children(toast.undo.clone().map(|id| {
                    div()
                        .id("toast-undo")
                        .flex_none()
                        .px(px(9.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .bg(palette.toast_button)
                        .hover(|style| style.bg(palette.toast_button_hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.undo_archive(&id, window, cx)
                        }))
                        .child("Undo")
                })),
        )
    }
}

/// "Sample" before the sample draft's meta line.
fn sample_badge(palette: &theme::Palette) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(palette.accent_soft)
        .text_color(palette.accent)
        .text_size(px(10.))
        .line_height(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .child("Sample")
}

/// The target's mark: Slack's keeps its own colours, the others take the
/// text colour.
fn platform_icon(target: Target, text: Hsla) -> AnyElement {
    let tinted = |path| {
        svg()
            .path(path)
            .size(px(15.))
            .text_color(text)
            .into_any_element()
    };
    match target {
        Target::Github => tinted(GITHUB_ICON),
        Target::Slack => img(SLACK_ICON).size(px(15.)).into_any_element(),
        Target::Rich => tinted(RICH_TEXT_ICON),
        Target::Plain => tinted(PLAIN_TEXT_ICON),
    }
}
