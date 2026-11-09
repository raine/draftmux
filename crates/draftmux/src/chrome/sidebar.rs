//! The drafts list and the toast.

use std::time::Duration;

use chrono::Utc;
use gpui::{
    Animation, AnimationExt, App, ClipboardItem, Context, FontWeight, KeyBinding, MouseButton,
    MouseDownEvent, Pixels, Point, SharedString, Task, actions, anchored, deferred, div,
    ease_out_quint, prelude::*, px, svg,
};

use super::{Chrome, shadow, sidebar_column};
use crate::assets::{ARCHIVE_ICON, TOAST_DONE_ICON, TOAST_ERROR_ICON};
use crate::drafts::{relative_time, tilde};
use crate::menu::{MenuItem, menu_panel};
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::theme;

const TOAST_DURATION: Duration = Duration::from_secs(4);
/// Errors stay longer: they're longer and worth reading.
const ERROR_TOAST_DURATION: Duration = Duration::from_secs(6);
const TOAST_FADE_IN: Duration = Duration::from_millis(180);
const TOAST_FADE_OUT: Duration = Duration::from_millis(150);
/// How far a toast rises as it fades in.
const TOAST_RISE: f32 = 6.;
const KEY_CONTEXT: &str = "DraftSidebar";

actions!(sidebar, [PreviousDraft, NextDraft, ArchiveDraft]);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", PreviousDraft, Some(KEY_CONTEXT)),
        KeyBinding::new("down", NextDraft, Some(KEY_CONTEXT)),
        KeyBinding::new(
            editor::secondary!("backspace"),
            ArchiveDraft,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new("delete", ArchiveDraft, Some(KEY_CONTEXT)),
    ]);
}

fn adjacent_row(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(index) if forward => index.saturating_add(1).min(len - 1),
        Some(index) => index.saturating_sub(1).min(len - 1),
        None if forward => 0,
        None => len - 1,
    })
}

pub(super) struct Toast {
    /// Distinguishes successive toasts, so each one animates in.
    serial: usize,
    error: bool,
    text: SharedString,
    /// Draft to unarchive when Undo is clicked.
    undo: Option<String>,
    /// Fades the toast out and hides it when it ends; `None` while the
    /// pointer is over the toast, so it stays until the pointer leaves.
    dismiss: Option<Task<()>>,
    /// Whether the toast is fading out.
    leaving: bool,
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
    fn move_sidebar_selection(
        &mut self,
        forward: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.drafts.rows();
        let current = rows
            .iter()
            .position(|row| Some(row.draft.id.as_str()) == self.drafts.current_id());
        if let Some(index) = adjacent_row(current, rows.len(), forward) {
            let id = rows[index].draft.id.clone();
            self.select(&id, window, cx).ok();
            self.focus_sidebar(window, cx);
        }
    }

    pub(super) fn focus_sidebar(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        window.focus(&self.sidebar_focus, cx);
        if let Some(index) = self
            .drafts
            .rows()
            .iter()
            .position(|row| Some(row.draft.id.as_str()) == self.drafts.current_id())
        {
            self.sidebar_scroll.scroll_to_item(index);
        }
        self.row_menu = None;
        cx.notify();
    }

    fn archive_sidebar_selection(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(id) = self.drafts.current_id().map(str::to_owned) {
            self.archive(&id, window, cx);
            self.focus_sidebar(window, cx);
        }
    }

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
                .flex_none()
                .flex()
                .gap(px(8.))
                .px(px(8.))
                .py(px(7.))
                .rounded(px(6.))
                .when(selected, |row| row.bg(palette.selected_row))
                // As in Finder and Mail, the right-clicked row gets a ring
                // without being selected. An overlay, since a shadow would
                // also fill the row and a border would shift its content.
                .when(menu_open, |row| {
                    row.child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(px(6.))
                            .border_2()
                            .border_color(palette.accent),
                    )
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
                        this.focus_sidebar(window, cx);
                    }
                }))
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
                                // Wide enough that the title stops short of the
                                // archive button, which replaces the time on hover.
                                .child(
                                    div()
                                        .flex_none()
                                        .min_w(px(20.))
                                        .flex()
                                        .justify_end()
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
                                // The sample's directory is just where it was
                                // created, not a project it came from.
                                .map(|line| {
                                    if row.draft.meta.is_sample() {
                                        line.child(sample_badge(&palette))
                                    } else {
                                        line.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(px(12.))
                                                .text_color(palette.muted)
                                                .child(tilde(&row.draft.meta.cwd, home.as_deref())),
                                        )
                                    }
                                }),
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
                        .opacity(0.)
                        .group_hover(group, |style| style.opacity(1.))
                        // Translucent, so it darkens whatever the row shows.
                        .hover(|style| style.bg(palette.selected_row))
                        .active(|style| style.opacity(0.8))
                        .tooltip(crate::tooltip::text("Archive"))
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
                            .key_context(KEY_CONTEXT)
                            .track_focus(&self.sidebar_focus)
                            .on_action(cx.listener(|this, _: &PreviousDraft, window, cx| {
                                this.move_sidebar_selection(false, window, cx);
                            }))
                            .on_action(cx.listener(|this, _: &NextDraft, window, cx| {
                                this.move_sidebar_selection(true, window, cx);
                            }))
                            .on_action(cx.listener(|this, _: &ArchiveDraft, window, cx| {
                                this.archive_sidebar_selection(window, cx);
                            }))
                            .size_full()
                            .px(px(8.))
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            .children(rows)
                            // A spacer avoids making an empty list scrollable
                            // with vertical padding on the scroll container.
                            .when(!self.drafts.rows().is_empty(), |list| {
                                list.child(div().h(px(11.)).flex_none())
                            }),
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
            enabled: true,
            action: Some(action),
        };
        let items = [
            item("Copy Draft ID", RowAction::CopyId),
            item(crate::platform::REVEAL_LABEL, RowAction::Reveal),
            MenuItem {
                label: SharedString::default(),
                checked: false,
                enabled: false,
                action: None,
            },
            item("Archive", RowAction::Archive),
        ];
        let this = cx.entity().downgrade();
        let id = menu.id.clone();
        let dismiss = cx.entity().downgrade();
        let closing_menu = menu.clone();
        let panel = menu_panel(
            palette,
            &items,
            move |action, window, cx| {
                this.update(cx, |this, cx| this.row_action(&id, action, window, cx))
                    .ok();
            },
            move |_, cx| {
                dismiss
                    .update(cx, |this, cx| {
                        if this.row_menu.as_ref() == Some(&closing_menu) {
                            this.row_menu = None;
                            cx.notify();
                        }
                    })
                    .ok();
            },
        );
        Some(deferred(
            anchored()
                .position(menu.position)
                .snap_to_window_with_margin(px(8.))
                .child(panel),
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
                self.show_toast("Draft ID copied", cx);
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
                self.show_error(format!("Couldn't archive the draft: {err:#}"), cx);
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
        self.present_toast("Draft archived".into(), false, Some(id.to_owned()), cx);
    }

    fn undo_archive(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.toast = None;
        if let Err(err) = self.drafts.unarchive(id) {
            self.show_error(format!("Couldn't restore the draft: {err:#}"), cx);
            return;
        }
        if self.drafts.current().is_none() {
            self.select(id, window, cx).ok();
        }
        cx.notify();
    }

    /// Confirms that something worked, replacing any earlier toast.
    pub(super) fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.present_toast(text.into(), false, None, cx);
    }

    /// Reports a failure, replacing any earlier toast.
    pub(super) fn show_error(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.present_toast(text.into(), true, None, cx);
    }

    fn present_toast(
        &mut self,
        text: SharedString,
        error: bool,
        undo: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let serial = self.toast.as_ref().map_or(0, |toast| toast.serial + 1);
        self.toast = Some(Toast {
            serial,
            error,
            text,
            undo,
            dismiss: Some(Self::dismiss_toast_later(error, cx)),
            leaving: false,
        });
        cx.notify();
    }

    /// Fades the toast out after it has been shown for a while, then hides
    /// it.
    fn dismiss_toast_later(error: bool, cx: &mut Context<Self>) -> Task<()> {
        let duration = if error {
            ERROR_TOAST_DURATION
        } else {
            TOAST_DURATION
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let faded = this.update(cx, |this, cx| {
                if let Some(toast) = &mut this.toast {
                    toast.leaving = true;
                }
                cx.notify();
            });
            if faded.is_err() {
                return;
            }
            cx.background_executor().timer(TOAST_FADE_OUT).await;
            this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            })
            .ok();
        })
    }

    /// Pauses the toast's countdown while the pointer is over it, so it
    /// doesn't vanish while the user reaches for Undo, and restarts it after.
    fn toast_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        let Some(error) = self.toast.as_ref().map(|toast| toast.error) else {
            return;
        };
        let dismiss = (!hovered).then(|| Self::dismiss_toast_later(error, cx));
        if let Some(toast) = &mut self.toast {
            toast.dismiss = dismiss;
            toast.leaving = false;
        }
        cx.notify();
    }

    /// The current toast, centred above the bottom of the window. It rises
    /// slightly as it fades in, unless Reduce Motion is on.
    pub(super) fn toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let toast = self.toast.as_ref()?;
        let palette = *theme::palette(cx);
        let (icon, icon_color) = if toast.error {
            (TOAST_ERROR_ICON, palette.danger)
        } else {
            (TOAST_DONE_ICON, palette.toast_done)
        };
        let card = div()
            .id("toast")
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.toast_hovered(*hovered, cx)))
            .max_w(px(440.))
            .flex()
            .items_center()
            .gap(px(8.))
            .py(px(8.))
            .pl(px(11.))
            .pr(px(if toast.undo.is_some() { 6. } else { 14. }))
            .rounded(px(10.))
            .bg(palette.toast)
            .text_color(palette.text)
            .text_size(px(13.))
            .line_height(px(18.))
            .shadow(vec![
                shadow(palette.toast_ring, 0., 0., 0.5),
                shadow(palette.toast_drop, 8., 24., 0.),
            ])
            .child(
                svg()
                    .flex_none()
                    .size(px(16.))
                    .path(icon)
                    .text_color(icon_color),
            )
            .child(div().min_w_0().child(toast.text.clone()))
            .children(toast.undo.clone().map(|id| {
                div()
                    .id("toast-undo")
                    .flex_none()
                    .ml(px(4.))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .text_color(palette.accent)
                    .font_weight(FontWeight::MEDIUM)
                    .hover(|style| style.bg(palette.hover))
                    .active(|style| style.opacity(0.7))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.undo_archive(&id, window, cx)),
                    )
                    .child("Undo")
            }));
        let rise = if crate::platform::reduce_motion() {
            0.
        } else {
            TOAST_RISE
        };
        let layer = div()
            .absolute()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(card);
        const BOTTOM: f32 = 20.;
        Some(if toast.leaving {
            layer
                .bottom(px(BOTTOM))
                .with_animation(
                    ("toast-out", toast.serial),
                    Animation::new(TOAST_FADE_OUT),
                    |layer, delta| layer.opacity(1. - delta),
                )
                .into_any_element()
        } else {
            layer
                .with_animation(
                    ("toast-in", toast.serial),
                    Animation::new(TOAST_FADE_IN).with_easing(ease_out_quint()),
                    move |layer, delta| {
                        layer
                            .bottom(px(BOTTOM - rise * (1. - delta)))
                            .opacity(delta)
                    },
                )
                .into_any_element()
        })
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

#[cfg(test)]
mod tests {
    use super::adjacent_row;

    #[test]
    fn sidebar_navigation_moves_without_wrapping() {
        assert_eq!(adjacent_row(Some(1), 3, false), Some(0));
        assert_eq!(adjacent_row(Some(1), 3, true), Some(2));
        assert_eq!(adjacent_row(Some(0), 3, false), Some(0));
        assert_eq!(adjacent_row(Some(2), 3, true), Some(2));
        assert_eq!(adjacent_row(Some(0), 1, false), Some(0));
        assert_eq!(adjacent_row(Some(0), 1, true), Some(0));
    }

    #[test]
    fn sidebar_navigation_handles_empty_and_unselected_lists() {
        assert_eq!(adjacent_row(None, 0, false), None);
        assert_eq!(adjacent_row(None, 0, true), None);
        assert_eq!(adjacent_row(None, 3, false), Some(2));
        assert_eq!(adjacent_row(None, 3, true), Some(0));
    }
}
