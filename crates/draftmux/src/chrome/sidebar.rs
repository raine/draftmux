//! The drafts list, its Archived section and the toast.

use std::time::Duration;

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, FontWeight, KeyBinding,
    MouseButton, MouseDownEvent, Pixels, Point, SharedString, Task, Transformation, actions,
    anchored, deferred, div, ease_out_quint, percentage, prelude::*, px, svg,
};

use super::{Chrome, shadow, sidebar_column};
use crate::assets::{
    ARCHIVE_ICON, CHEVRON_ICON, HELP_ICON, PLUS_ICON, RESET_ICON, TOAST_DONE_ICON, TOAST_ERROR_ICON,
};
use crate::drafts::{Row, relative_time, tilde};
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
/// How many archived drafts the Archived section lists until asked for all.
const ARCHIVED_SHOWN: usize = 20;

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
    /// Distinguishes successive toasts, so each one's text fades in.
    serial: usize,
    /// The serial of the toast that rose into view. A toast replacing a
    /// visible one keeps it, so the card stays put and only its text
    /// changes.
    entrance: usize,
    error: bool,
    text: SharedString,
    /// What Undo undoes, when the toast offers it.
    undo: Option<ToastUndo>,
    /// Fades the toast out and hides it when it ends; `None` while the
    /// pointer is over the toast, so it stays until the pointer leaves.
    dismiss: Option<Task<()>>,
    /// Whether the toast is fading out.
    leaving: bool,
}

/// An archived draft's row, folding shut where it was.
pub(super) struct LeavingRow {
    index: usize,
    row: Row,
    since: Instant,
}

impl LeavingRow {
    pub(super) fn folding(&self) -> bool {
        self.since.elapsed() < crate::fold::DURATION
    }
}

/// The Archived section at the bottom of the drafts list, listing the most
/// recently active archived drafts.
#[derive(Default)]
pub(super) struct ArchivedSection {
    open: bool,
    /// Lists every archived draft, not just the first [`ARCHIVED_SHOWN`].
    all: bool,
    /// The drafts the section listed when it last rendered.
    seen: RefCell<Option<HashSet<String>>>,
    /// Restored drafts' rows, folding shut where they were.
    pub(super) leaving: Vec<LeavingRow>,
}

impl ArchivedSection {
    /// How many of `len` archived drafts are listed.
    fn shown(&self, len: usize) -> usize {
        if self.all {
            len
        } else {
            len.min(ARCHIVED_SHOWN)
        }
    }
}

#[derive(Clone)]
pub(super) enum ToastUndo {
    /// Unarchives the draft.
    Archive(String),
    /// Undoes the last edit to the selected draft.
    Edit,
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
    Restore,
}

impl Chrome {
    fn move_sidebar_selection(
        &mut self,
        forward: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.session.drafts().rows();
        let current = rows
            .iter()
            .position(|row| Some(row.draft.id.as_str()) == self.session.drafts().current_id());
        if let Some(index) = adjacent_row(current, rows.len(), forward) {
            let id = rows[index].draft.id.clone();
            self.select(&id, window, cx).ok();
            self.focus_sidebar(window, cx);
        }
    }

    pub(super) fn focus_sidebar(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if !self.show_sidebar {
            window.focus(&self.content_focus, cx);
            return;
        }
        window.focus(&self.sidebar_focus, cx);
        if let Some(index) = self
            .session
            .drafts()
            .rows()
            .iter()
            .position(|row| Some(row.draft.id.as_str()) == self.session.drafts().current_id())
        {
            self.sidebar_scroll.scroll_to_item(index);
        }
        self.row_menu = None;
        cx.notify();
    }

    fn archive_sidebar_selection(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(id) = self.session.drafts().current_id().map(str::to_owned) {
            self.archive(&id, window, cx);
            self.focus_sidebar(window, cx);
        }
    }

    pub(super) fn sidebar(
        &self,
        bar: &ScrollbarState,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = *theme::palette(cx);
        let now = Utc::now();
        let home = std::env::home_dir();
        let rows = self.fold_rows(
            self.session.drafts().rows(),
            &self.seen_rows,
            &self.leaving_rows,
            false,
            now,
            home.as_deref(),
            &palette,
            window,
            cx,
        );
        let archived = self.archived_section(now, home.as_deref(), &palette, window, cx);
        let listed = !self.session.drafts().rows().is_empty() || archived.is_some();

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
                    .justify_between()
                    .pl(px(16.))
                    .pr(px(10.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.faint)
                    .child("Drafts")
                    .child(new_draft_button(&palette, cx)),
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
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            .children(rows)
                            // Keeps the Archived section at the bottom of a
                            // short list.
                            .when_some(archived, |list, archived| {
                                list.child(div().flex_1().min_h(px(8.))).child(archived)
                            })
                            // A spacer avoids making an empty list scrollable
                            // with vertical padding on the scroll container.
                            .when(listed, |list| list.child(div().h(px(11.)).flex_none())),
                    )
                    .child(scrollbar(Source::Handle(self.sidebar_scroll.clone()), bar)),
            )
            .child(help_button(&palette))
            .children(self.row_menu(&palette, cx))
    }

    /// `rows` folded open. Rows that weren't listed last time unfold, except
    /// on the first render, which shows the list as it is; `leaving` rows
    /// fold shut where they were.
    #[allow(clippy::too_many_arguments)]
    fn fold_rows(
        &self,
        rows: &[Row],
        seen: &RefCell<Option<HashSet<String>>>,
        leaving: &[LeavingRow],
        archived: bool,
        now: DateTime<Utc>,
        home: Option<&Path>,
        palette: &theme::Palette,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let prefix = if archived {
            "archived-fold"
        } else {
            "draft-fold"
        };
        let ids: HashSet<String> = rows.iter().map(|row| row.draft.id.clone()).collect();
        let seen = seen.replace(Some(ids.clone()));
        let mut elements: Vec<AnyElement> = rows
            .iter()
            .map(|row| {
                let entering = seen
                    .as_ref()
                    .is_some_and(|seen| !seen.contains(&row.draft.id));
                let element = self.draft_row(row, archived, now, home, palette, cx);
                crate::fold::fold(
                    SharedString::from(format!("{prefix}-{}", row.draft.id)),
                    true,
                    entering,
                    move || div().pb(px(1.)).child(element).into_any_element(),
                    window,
                    cx,
                )
            })
            .collect();
        for leaving in leaving {
            if ids.contains(&leaving.row.draft.id) {
                continue;
            }
            let element = self.draft_row(&leaving.row, archived, now, home, palette, cx);
            let folded = crate::fold::fold(
                SharedString::from(format!("{prefix}-{}", leaving.row.draft.id)),
                false,
                false,
                move || div().pb(px(1.)).child(element).into_any_element(),
                window,
                cx,
            );
            elements.insert(leaving.index.min(elements.len()), folded);
        }
        elements
    }

    /// The collapsible Archived section, while there are archived drafts.
    fn archived_section(
        &self,
        now: DateTime<Utc>,
        home: Option<&Path>,
        palette: &theme::Palette,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let section = &self.archived_section;
        let archived = self.session.drafts().archived();
        if archived.is_empty() && section.leaving.is_empty() {
            return None;
        }
        let palette = *palette;
        let shown = section.shown(archived.len());
        let rows = self.fold_rows(
            &archived[..shown],
            &section.seen,
            &section.leaving,
            true,
            now,
            home,
            &palette,
            window,
            cx,
        );
        let more = (archived.len() > ARCHIVED_SHOWN).then(|| {
            div()
                .id("archived-more")
                .flex_none()
                .mt(px(2.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .text_size(px(12.))
                .text_color(palette.muted)
                .hover(|style| style.text_color(palette.text))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.archived_section.all = !this.archived_section.all;
                    cx.notify();
                }))
                .child(if section.all {
                    "Show fewer".to_owned()
                } else {
                    format!("Show all {}", archived.len())
                })
        });
        let chevron = svg()
            .flex_none()
            .path(CHEVRON_ICON)
            .size(px(10.))
            .text_color(palette.faint);
        let header = div()
            .id("archived-header")
            .flex_none()
            .h(px(30.))
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(8.))
            .rounded(px(6.))
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(palette.faint)
            .hover(|style| style.text_color(palette.muted))
            .on_click(cx.listener(|this, _, _, cx| {
                let section = &mut this.archived_section;
                section.open = !section.open;
                // Reopening starts from the most recent drafts again.
                section.all = false;
                cx.notify();
            }))
            .child("Archived")
            .child(if section.open {
                chevron
                    .with_transformation(Transformation::rotate(percentage(0.25)))
                    .into_any_element()
            } else {
                chevron.into_any_element()
            })
            .child(
                div()
                    .ml_auto()
                    .font_weight(FontWeight::NORMAL)
                    .child(archived.len().to_string()),
            );
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .child(header)
                .child(crate::fold::fold(
                    "archived-section",
                    section.open,
                    false,
                    move || {
                        div()
                            .flex()
                            .flex_col()
                            .children(rows)
                            .children(more)
                            .into_any_element()
                    },
                    window,
                    cx,
                ))
                .into_any_element(),
        )
    }

    /// One row of the drafts list, or with `archived` of the Archived
    /// section, which offers Restore in place of Archive and can't be
    /// selected.
    fn draft_row(
        &self,
        row: &Row,
        archived: bool,
        now: DateTime<Utc>,
        home: Option<&Path>,
        palette: &theme::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let palette = *palette;
        let current = self.session.drafts().current_id();
        let id = row.draft.id.clone();
        let selected = !archived && current == Some(id.as_str());
        let menu_open = self.row_menu.as_ref().is_some_and(|menu| menu.id == id);
        let group: SharedString = if archived {
            format!("archived-row-{id}")
        } else {
            format!("draft-row-{id}")
        }
        .into();
        let (icon, tooltip) = if archived {
            (RESET_ICON, "Restore")
        } else {
            (ARCHIVE_ICON, "Archive")
        };
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
            .when(!archived, |row| {
                row.on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.select(&id, window, cx).ok();
                        this.focus_sidebar(window, cx);
                    }
                }))
            })
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
                                    .child(markup::replace_shortcodes(&row.title)),
                            )
                            // Wide enough that the title stops short of the
                            // archive or restore button, which replaces the
                            // time on hover.
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
                                            .child(markup::replace_shortcodes(&tilde(
                                                &row.draft.meta.cwd,
                                                home,
                                            ))),
                                    )
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "{}-{id}",
                        tooltip.to_lowercase()
                    )))
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
                    .tooltip(crate::tooltip::text(tooltip))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if archived {
                            this.restore(&id, window, cx);
                        } else {
                            this.archive(&id, window, cx);
                        }
                    }))
                    .child(svg().path(icon).size(px(14.)).text_color(palette.muted)),
            )
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
            detail: None,
            heading: false,
        };
        let items = [
            item("Copy Draft ID", RowAction::CopyId),
            item(crate::platform::REVEAL_LABEL, RowAction::Reveal),
            MenuItem {
                label: SharedString::default(),
                checked: false,
                enabled: false,
                action: None,
                detail: None,
                heading: false,
            },
            if self
                .session
                .drafts()
                .archived()
                .iter()
                .any(|row| row.draft.id == menu.id)
            {
                item("Restore", RowAction::Restore)
            } else {
                item("Archive", RowAction::Archive)
            },
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
                if let Some(row) = self
                    .session
                    .drafts()
                    .rows()
                    .iter()
                    .chain(self.session.drafts().archived())
                    .find(|row| row.draft.id == id)
                {
                    cx.reveal_path(&row.draft.dir);
                }
            }
            RowAction::Archive => self.archive(id, window, cx),
            RowAction::Restore => self.restore(id, window, cx),
        }
        cx.notify();
    }

    fn archive(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let was_current = self.session.drafts().current_id() == Some(id);
        let leaving = self
            .session
            .drafts()
            .rows()
            .iter()
            .position(|row| row.draft.id == id)
            .map(|index| LeavingRow {
                index,
                row: Row {
                    draft: self.session.drafts().rows()[index].draft.clone(),
                    title: self.session.drafts().rows()[index].title.clone(),
                },
                since: Instant::now(),
            });
        let next = match self.session.archive(id) {
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
        self.leaving_rows.extend(leaving);
        self.present_toast(
            "Draft archived".into(),
            false,
            Some(ToastUndo::Archive(id.to_owned())),
            cx,
        );
    }

    fn undo_archive(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.toast = None;
        if self.unarchive(id, cx) && self.session.drafts().current().is_none() {
            self.select(id, window, cx).ok();
        }
        cx.notify();
    }

    /// Restores `id` from the Archived section and selects it.
    fn restore(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.unarchive(id, cx) && self.select(id, window, cx).is_ok() {
            self.focus_sidebar(window, cx);
        }
        cx.notify();
    }

    /// Unarchives `id`, folding its row in the Archived section shut. Shows
    /// the error and returns false if that fails.
    fn unarchive(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let archived = self.session.drafts().archived();
        let leaving = archived
            .iter()
            .take(self.archived_section.shown(archived.len()))
            .position(|row| row.draft.id == id)
            .map(|index| LeavingRow {
                index,
                row: Row {
                    draft: archived[index].draft.clone(),
                    title: archived[index].title.clone(),
                },
                since: Instant::now(),
            });
        if let Err(err) = self.session.drafts_mut().unarchive(id) {
            self.show_error(format!("Couldn't restore the draft: {err:#}"), cx);
            return false;
        }
        self.archived_section.leaving.extend(leaving);
        true
    }

    fn undo_edit(&mut self, cx: &mut Context<Self>) {
        self.toast = None;
        self.editor.update(cx, |editor, cx| editor.undo(cx));
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

    /// Confirms an edit to the selected draft, offering to undo it.
    pub(super) fn show_undoable_edit(
        &mut self,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.present_toast(text.into(), false, Some(ToastUndo::Edit), cx);
    }

    fn present_toast(
        &mut self,
        text: SharedString,
        error: bool,
        undo: Option<ToastUndo>,
        cx: &mut Context<Self>,
    ) {
        let serial = self.toast.as_ref().map_or(0, |toast| toast.serial + 1);
        let entrance = match &self.toast {
            Some(toast) if !toast.leaving => toast.entrance,
            _ => serial,
        };
        self.toast = Some(Toast {
            serial,
            entrance,
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
            .child({
                let text = div().min_w_0().child(toast.text.clone());
                if toast.serial == toast.entrance {
                    text.into_any_element()
                } else {
                    text.with_animation(
                        ("toast-text", toast.serial),
                        Animation::new(TOAST_FADE_IN).with_easing(ease_out_quint()),
                        |text, delta| text.opacity(delta),
                    )
                    .into_any_element()
                }
            })
            .children(toast.undo.clone().map(|undo| {
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
                    .on_click(cx.listener(move |this, _, window, cx| match &undo {
                        ToastUndo::Archive(id) => this.undo_archive(id, window, cx),
                        ToastUndo::Edit => this.undo_edit(cx),
                    }))
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
                    ("toast-in", toast.entrance),
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

/// A faint button in the sidebar's bottom corner that opens Help.
fn new_draft_button(palette: &theme::Palette, cx: &mut Context<Chrome>) -> impl IntoElement {
    div()
        .id("new-draft")
        .group("new-draft")
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .hover(|style| style.bg(palette.hover))
        .active(|style| style.opacity(0.8))
        .tooltip(crate::tooltip::text(editor::by_platform!(
            "New draft (⌘N)",
            "New draft (Ctrl+N)"
        )))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _, window, cx| this.new_draft(window, cx)))
        .child(
            svg()
                .path(PLUS_ICON)
                .size(px(14.))
                .text_color(palette.faint)
                .group_hover("new-draft", |style| style.text_color(palette.muted)),
        )
}

fn help_button(palette: &theme::Palette) -> gpui::Div {
    div().flex_none().px(px(8.)).pt(px(4.)).pb(px(8.)).child(
        div()
            .id("open-help")
            .group("open-help")
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .hover(|style| style.bg(palette.hover))
            .active(|style| style.opacity(0.8))
            .tooltip(crate::tooltip::text(editor::by_platform!(
                "Help (⌘/)",
                "Help (Ctrl+/)"
            )))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| crate::help_window::open(cx))
            .child(
                svg()
                    .path(HELP_ICON)
                    .size(px(16.))
                    .text_color(palette.faint)
                    .group_hover("open-help", |style| style.text_color(palette.muted)),
            ),
    )
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
