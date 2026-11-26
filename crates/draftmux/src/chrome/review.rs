//! Reviewing what the agent's edits changed in the selected draft: the
//! toolbar's Changes button, a redline in place of the preview under a bar
//! of controls, and single keys that step through, revert and keep changes.
//!
//! Each draft keeps its history of agent edits
//! ([`History`](draftmux_storage::History)). Review compares the draft as it
//! is with the text before one of those edits, the latest by default; what
//! is compared, kept and selected is the session's
//! ([`ReviewState`](draftmux_core::review::ReviewState)).

use std::rc::Rc;
use std::time::Duration;

use chrono::Utc;
use draftmux_core::decorations::Side;
use draftmux_core::diff::Hunk;
use draftmux_core::history::Recorded;
use draftmux_core::review::{Compared, Origin};
use draftmux_core::session::Outcome;
use draftmux_storage::Target;
use gpui::{
    AnimationExt, AnyElement, App, Context, Entity, Focusable, FontWeight, KeyBinding, MouseButton,
    Pixels, Point, SharedString, StyleRefinement, Task, Transformation, Window, actions, anchored,
    deferred, div, linear_color_stop, linear_gradient, percentage, point, prelude::*, px, svg,
};

use super::preview::{Preview, PreviewRedline};
use super::{Chrome, shadow};
use crate::assets::CHEVRON_ICON;
use crate::drafts::relative_time;
use crate::markup_view::{self, Mapped, Redlined};
use crate::menu::{MenuItem, menu_panel};
use crate::scrollbar::{ScrollbarState, Source, scrollbar};
use crate::theme::Palette;

actions!(
    review,
    [
        ToggleReview,
        NextChange,
        PreviousChange,
        RevertChange,
        KeepChange,
        EndReview
    ]
);

const KEY_CONTEXT: &str = "Review";
/// The bar of review controls over the redline.
pub(super) const BAR_HEIGHT: Pixels = px(40.);
/// The name over each column.
pub(super) const COLUMN_HEAD_HEIGHT: Pixels = px(28.);
/// How long the pointer rests on a change before it is said where it came
/// from.
const ORIGIN_DELAY: Duration = Duration::from_millis(500);
/// How long the Changes button takes to fade in.
const BUTTON_FADE: Duration = Duration::from_millis(200);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(editor::secondary!("shift-e"), ToggleReview, None),
        KeyBinding::new("]", NextChange, Some(KEY_CONTEXT)),
        KeyBinding::new("[", PreviousChange, Some(KEY_CONTEXT)),
        KeyBinding::new("r", RevertChange, Some(KEY_CONTEXT)),
        KeyBinding::new("k", KeepChange, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", EndReview, Some(KEY_CONTEXT)),
    ]);
}

/// Review, while it is open.
#[derive(Default)]
pub(super) struct Review {
    menu_open: bool,
    /// The change under the pointer.
    hovered: Option<usize>,
    /// Where the hovered change came from, once the pointer has rested on
    /// it, and where the pointer was.
    origin: Option<(SharedString, Point<Pixels>)>,
    show_origin: Option<Task<()>>,
    /// A change to scroll into view in both columns, with a serial for each
    /// request.
    reveal: Option<(usize, u64)>,
    /// Both columns' documents and what they were made from.
    documents: Option<Documents>,
    /// The text before the compared edits, parsed for the target. It
    /// changes only with the baseline.
    before: Option<Parsed>,
    /// The draft parsed for the target, which the preview shows when
    /// review closes.
    after: Option<Parsed>,
}

/// A text parsed for a target.
#[derive(Clone)]
struct Parsed {
    text: String,
    target: Target,
    mapped: Rc<Mapped>,
}

impl Parsed {
    /// `text` parsed for `target`, reusing `last` if it is that.
    fn reuse(last: &mut Option<Parsed>, text: &str, target: Target) -> Rc<Mapped> {
        match last {
            Some(parsed) if parsed.target == target && parsed.text == text => parsed.mapped.clone(),
            _ => {
                let mapped = Rc::new(markup_view::preview_mapped(target, text));
                *last = Some(Parsed {
                    text: text.to_owned(),
                    target,
                    mapped: mapped.clone(),
                });
                mapped
            }
        }
    }
}

/// The two columns: the text before the compared edits, and now.
#[derive(Clone)]
pub(super) struct Documents {
    before: Column,
    after: Column,
    compared: Rc<Compared>,
    target: Target,
}

/// One column's redline and frontmatter.
#[derive(Clone)]
struct Column {
    redlined: Rc<Redlined>,
    fields: markup::frontmatter::Fields,
}

impl Column {
    fn new(mapped: &Mapped, compared: &Compared, side: Side) -> Self {
        Self {
            redlined: Rc::new(markup_view::redline(mapped, &compared.decorations, side)),
            fields: mapped.fields.clone(),
        }
    }
}

/// How both columns draw the changes.
struct Marks {
    selected: Option<usize>,
    hovered: Option<usize>,
    reveal: Option<(usize, u64)>,
    kept: Vec<bool>,
}

impl Chrome {
    /// Compares the newly selected draft with its history and leaves
    /// review.
    pub(super) fn load_changes(&mut self, cx: &mut Context<Self>) {
        self.review = None;
        self.sync_review(cx);
    }

    /// An agent turn added an edit to the selected draft's history, which
    /// the comparison now starts from.
    /// Records an option the user applied from the selection popover as an
    /// edit in the history, as the agent's edits are.
    pub(super) fn option_applied(
        &mut self,
        before: String,
        text: &str,
        prompt: &str,
        cx: &mut Context<Self>,
    ) {
        match self
            .session
            .option_applied(before, text, prompt, Utc::now())
        {
            Ok(Recorded::Added) => self.edit_recorded(cx),
            Ok(Recorded::Ignored) => {}
            Err(err) => eprintln!("draftmux: saving the edit history: {err:#}"),
        }
    }

    pub(super) fn edit_recorded(&mut self, cx: &mut Context<Self>) {
        if let Some(review) = &mut self.review {
            review.menu_open = false;
        }
        self.sync_review(cx);
    }

    /// Brings the comparison, the Changes count and the redline up to date
    /// with the draft and the review.
    pub(super) fn sync_review(&mut self, cx: &mut Context<Self>) {
        let compared = self.session.sync_review();
        let target = self
            .session
            .drafts()
            .current()
            .map(|draft| draft.meta.target);
        let sides = match (&mut self.review, compared, target) {
            (Some(review), Some(compared), Some(target)) => {
                let documents = match &review.documents {
                    Some(documents)
                        if Rc::ptr_eq(&documents.compared, &compared)
                            && documents.target == target =>
                    {
                        documents.clone()
                    }
                    _ => {
                        let before = Parsed::reuse(&mut review.before, &compared.base, target);
                        let after = Parsed::reuse(&mut review.after, &compared.text, target);
                        let documents = Documents {
                            before: Column::new(&before, &compared, Side::Before),
                            after: Column::new(&after, &compared, Side::After),
                            compared: compared.clone(),
                            target,
                        };
                        review.documents = Some(documents.clone());
                        documents
                    }
                };
                let marks = Marks {
                    selected: self.session.review().selected(),
                    hovered: review.hovered,
                    reveal: review.reveal,
                    kept: self.session.review().kept(),
                };
                Some((documents, marks))
            }
            _ => None,
        };
        let (before, after) = match sides {
            Some((documents, marks)) => (
                Some(self.preview_redline(documents.before, &marks, cx)),
                Some(self.preview_redline(documents.after, &marks, cx)),
            ),
            None => (None, None),
        };
        for (preview, redline) in [(&self.before_preview, before), (&self.preview, after)] {
            preview.update(cx, |preview, cx| {
                if redline.is_some() {
                    preview.target = target;
                }
                if preview.redline.is_some() || redline.is_some() {
                    // A selection only lasts as long as the text it's in.
                    let document = |redline: Option<&PreviewRedline>| {
                        redline.map(|redline| Rc::as_ptr(&redline.document))
                    };
                    if document(preview.redline.as_ref()) != document(redline.as_ref()) {
                        preview
                            .redline_selection
                            .update(cx, |selection, cx| selection.reset(cx));
                    }
                    preview.redline = redline;
                    cx.notify();
                }
            });
        }
        cx.notify();
    }

    fn preview_redline(
        &self,
        column: Column,
        marks: &Marks,
        cx: &mut Context<Self>,
    ) -> PreviewRedline {
        let on_click = {
            let this = cx.weak_entity();
            Rc::new(move |hunk, window: &mut Window, cx: &mut App| {
                this.update(cx, |this, cx| this.click_change(hunk, window, cx))
                    .ok();
            })
        };
        let on_hover = {
            let this = cx.weak_entity();
            Rc::new(
                move |hit: Option<(usize, Point<Pixels>)>, _: &mut Window, cx: &mut App| {
                    this.update(cx, |this, cx| this.change_hovered(hit, cx))
                        .ok();
                },
            )
        };
        PreviewRedline {
            document: column.redlined.document.clone(),
            marks: column.redlined.marks.clone(),
            fields: column.fields,
            selected: marks.selected,
            hovered: marks.hovered,
            kept: marks.kept.clone(),
            reveal: marks.reveal,
            on_click,
            on_hover,
        }
    }

    /// The selected change, while review is open.
    fn selected(&self) -> Option<usize> {
        self.review.as_ref()?;
        self.session.review().selected()
    }

    fn selected_hunk(&self) -> Option<&Hunk> {
        self.session.review().hunks().get(self.selected()?)
    }

    pub(super) fn toggle_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.is_some() {
            self.close_review(window, cx);
        } else {
            self.open_review(window, cx);
        }
    }

    fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.log().edits().is_empty() || self.session.drafts().current().is_none() {
            return;
        }
        self.close_find(window, cx);
        self.show_preview = true;
        self.review = Some(Review::default());
        self.session.review_mut().select(None);
        // Review's keys are single letters, which the editor would type.
        window.focus(&self.review_focus, cx);
        self.sync_review(cx);
    }

    pub(super) fn close_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.review.take() else {
            return;
        };
        self.catch_up_preview(review.after, cx);
        self.session.review_mut().select(None);
        self.editor
            .update(cx, |editor, cx| editor.set_marked(None, cx));
        if self.review_focus.contains_focused(window, cx) {
            if self.show_editor {
                window.focus(&self.editor.focus_handle(cx), cx);
            } else {
                window.focus(&self.content_focus, cx);
            }
        }
        self.sync_review(cx);
    }

    /// Gives the preview the text edited while review showed in its place,
    /// from review's parse of it when that is current.
    fn catch_up_preview(&mut self, parsed: Option<Parsed>, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text();
        let Some(target) = self
            .session
            .drafts()
            .current()
            .map(|draft| draft.meta.target)
        else {
            return;
        };
        if self.preview_source == text && self.preview.read(cx).target == Some(target) {
            return;
        }
        let document = match parsed {
            Some(parsed) if parsed.text == text && parsed.target == target => {
                (*parsed.mapped.document).clone()
            }
            _ => markup_view::preview(target, text),
        };
        self.set_preview(Some(document), cx);
    }

    /// Selects change `selected`, or nothing, in the redline and the editor.
    fn select_change(&mut self, selected: Option<usize>, cx: &mut Context<Self>) {
        if self.review.is_none() {
            return;
        }
        self.session.review_mut().select(selected);
        self.mark_selected(cx);
        self.sync_review(cx);
    }

    /// Points out the selected change's text in the editor, or where its
    /// removed text was.
    fn mark_selected(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text();
        let range = self.selected_hunk().map(|hunk| {
            let span = &text[hunk.new.clone()];
            let start = hunk.new.start + (span.len() - span.trim_start().len());
            let end = (hunk.new.end - (span.len() - span.trim_end().len())).max(start);
            start..end
        });
        self.editor
            .update(cx, |editor, cx| editor.set_marked(range, cx));
    }

    fn click_change(&mut self, hunk: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.is_none() {
            return;
        }
        let selected = (self.selected() != Some(hunk)).then_some(hunk);
        window.focus(&self.review_focus, cx);
        self.select_change(selected, cx);
    }

    /// A press outside the changes and the review bar clears the selected
    /// change.
    pub(super) fn pressed_outside_changes(&mut self, cx: &mut Context<Self>) {
        if self.selected().is_some() {
            self.select_change(None, cx);
        }
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.review.is_none() {
            return;
        }
        let Some(selected) = self.session.review().step(forward) else {
            return;
        };
        self.reveal_change(selected);
        self.select_change(Some(selected), cx);
    }

    /// Scrolls change `hunk` into view in both columns on the next frame.
    fn reveal_change(&mut self, hunk: usize) {
        if let Some(review) = &mut self.review {
            let serial = review.reveal.map_or(0, |(_, serial)| serial + 1);
            review.reveal = Some((hunk, serial));
        }
    }

    fn revert_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(at), Some(hunk)) = (self.selected(), self.selected_hunk().cloned()) else {
            return;
        };
        let outcome = self.session.revert(at);
        if !self.apply_review(outcome, cx) {
            return;
        }
        window.focus(&self.review_focus, cx);
        // The next change takes the reverted one's place, once the edit has
        // been compared.
        cx.defer_in(window, |this, _, cx| this.mark_selected(cx));
        self.show_undoable_edit(
            if hunk.is_removal() {
                "Restored the removed text"
            } else if hunk.is_addition() {
                "Removed the added text"
            } else {
                "Reverted the change"
            },
            cx,
        );
    }

    /// Replaces the draft with the before column, dropping every edit
    /// since, the agent's and the user's.
    fn restore_before(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.review().base().is_none() {
            return;
        }
        let restored = format!("Restored the draft {}", self.before_name());
        let outcome = self.session.restore_before();
        if !self.apply_review(outcome, cx) {
            return;
        }
        window.focus(&self.review_focus, cx);
        self.select_change(None, cx);
        self.show_undoable_edit(restored, cx);
    }

    /// Carries out a revert or restore, or shows why it was refused.
    /// Returns whether it went ahead.
    fn apply_review(&mut self, outcome: anyhow::Result<Outcome>, cx: &mut Context<Self>) -> bool {
        match outcome {
            Ok(outcome) => {
                self.apply(outcome, None, cx);
                true
            }
            Err(err) => {
                self.show_error(err.to_string(), cx);
                false
            }
        }
    }

    /// Keeps the selected change and moves on to the next one not kept, or
    /// un-keeps it.
    fn keep_change(&mut self, cx: &mut Context<Self>) {
        let (Some(at), Some(_)) = (self.selected(), self.selected_hunk()) else {
            return;
        };
        let kept = !self.session.review().is_kept(at);
        if let Err(err) = self.session.keep(at, kept) {
            eprintln!("draftmux: saving the reviewed changes: {err:#}");
        }
        let next = kept
            .then(|| self.session.review().next_unkept(at))
            .flatten();
        match next {
            Some(next) => {
                self.reveal_change(next);
                self.select_change(Some(next), cx);
            }
            None => self.sync_review(cx),
        }
    }

    /// Keeps every change in the comparison and closes review.
    fn finish_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.session.finish() {
            eprintln!("draftmux: saving the reviewed changes: {err:#}");
        }
        self.close_review(window, cx);
    }

    fn end_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (&self.review, self.selected()) {
            (Some(_), Some(_)) => self.select_change(None, cx),
            (Some(_), None) => self.close_review(window, cx),
            (None, _) => {}
        }
    }

    fn compare_since(&mut self, since: usize, cx: &mut Context<Self>) {
        self.session.review_mut().compare_since(since);
        if let Some(review) = &mut self.review {
            review.menu_open = false;
        }
        self.select_change(None, cx);
    }

    fn change_hovered(&mut self, hit: Option<(usize, Point<Pixels>)>, cx: &mut Context<Self>) {
        let Some(review) = &mut self.review else {
            return;
        };
        let hovered = hit.map(|(hunk, _)| hunk);
        if review.hovered == hovered {
            return;
        }
        review.hovered = hovered;
        review.origin = None;
        review.show_origin = hit.map(|(hunk, position)| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ORIGIN_DELAY).await;
                this.update(cx, |this, cx| {
                    let origin = this.change_origin(hunk, cx);
                    if let Some(review) = &mut this.review
                        && review.hovered == Some(hunk)
                    {
                        review.origin = origin.map(|origin| (origin.into(), position));
                        cx.notify();
                    }
                })
                .ok();
            })
        });
        // Both columns outline the change under the pointer.
        self.sync_review(cx);
    }

    /// Which agent edit made change `hunk`: the newest edit in the
    /// comparison with a change of its own in it.
    fn change_origin(&self, hunk: usize, cx: &App) -> Option<String> {
        let text = self.editor.read(cx).text();
        Some(match self.session.review().origin(hunk, text)? {
            Origin::Edit(ix) => {
                let edit = &self.session.log().edits()[ix];
                format!(
                    "From “{}” · {}",
                    clip(&edit.prompt, 40),
                    relative_time(edit.at, Utc::now())
                )
            }
            Origin::User => "Your edit".to_owned(),
        })
    }

    /// The toolbar's Changes button, for a draft the agent has edited, with
    /// the number of changes not yet kept.
    pub(super) fn review_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let edits = self.session.log().edits().len();
        if edits == 0 {
            return None;
        }
        let count = self.session.review().unreviewed();
        let open = self.review.is_some();
        let palette = *palette;
        Some(
            div()
                .id("review")
                .flex_none()
                .h(px(24.))
                .pl(px(if count > 0 { 5. } else { 9. }))
                .pr(px(9.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(6.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .when(open, |button| button.bg(palette.selected_row))
                .hover(|style| style.bg(palette.hover))
                .active(|style| style.opacity(0.8))
                .tooltip(crate::tooltip::text(if open {
                    editor::by_platform!("Close Review (⌘⇧E)", "Close Review (Ctrl+Shift+E)")
                } else {
                    editor::by_platform!("Review Changes (⌘⇧E)", "Review Changes (Ctrl+Shift+E)")
                }))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_review(window, cx)))
                .when(count > 0, |button| {
                    button.child(
                        div()
                            .min_w(px(16.))
                            .h(px(16.))
                            .px(px(4.))
                            .flex()
                            .items_center()
                            .rounded(px(8.))
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.on_accent)
                            .bg(palette.accent)
                            // Text alignment centers the number exactly;
                            // flex centering rounds its offset to whole
                            // pixels.
                            .child(div().flex_1().text_center().child(count.to_string())),
                    )
                })
                .child(if count == 1 { "Change" } else { "Changes" })
                // Fades in when the draft gets its first edit, and with
                // each edit after.
                .with_animation(
                    ("review-button", edits),
                    gpui::Animation::new(BUTTON_FADE).with_easing(gpui::ease_out_quint()),
                    |button, delta| button.opacity(delta),
                )
                .into_any_element(),
        )
    }

    /// The preview pane's contents while reviewing: the bar of controls,
    /// with the redline below it. Review's keys work while the focus is in
    /// here.
    pub(super) fn review_pane(
        &self,
        pane: gpui::Div,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        pane.track_focus(&self.review_focus)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| this.step(false, cx)))
            .on_action(
                cx.listener(|this, _: &RevertChange, window, cx| this.revert_change(window, cx)),
            )
            .on_action(cx.listener(|this, _: &KeepChange, _, cx| this.keep_change(cx)))
            .on_action(cx.listener(|this, _: &EndReview, window, cx| this.end_review(window, cx)))
            .child(self.review_bar(palette, cx))
    }

    /// The before column's version, in a sentence: the text before the
    /// compared edits.
    fn before_name(&self) -> String {
        let edits = self.session.log().edits();
        let since = self.session.log().since();
        if since + 1 >= edits.len() {
            let prompt = edits.get(since).map_or("", |edit| edit.prompt.as_str());
            format!("before “{}”", clip(prompt, 30))
        } else if since == 0 {
            format!("before all {} edits", edits.len())
        } else {
            format!("before the last {} edits", edits.len() - since)
        }
    }

    /// A column's name over its preview: the text before the compared
    /// edits, or now.
    pub(super) fn column_head(&self, before: bool, palette: &Palette) -> impl IntoElement {
        let name = if before {
            let name = self.before_name();
            format!("B{}", &name[1..])
        } else {
            "Now".to_owned()
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(COLUMN_HEAD_HEIGHT)
            .flex()
            .items_center()
            .gap(px(7.))
            .px(px(16.))
            .text_size(px(11.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(palette.muted)
            .bg(palette.toolbar)
            .border_b_1()
            .border_color(palette.hairline)
            .child(div().flex_none().size(px(7.)).rounded_full().bg(if before {
                palette.removed
            } else {
                palette.added
            }))
            .child(div().min_w_0().truncate().child(name))
    }

    /// Review's view, in place of the editor and preview: the bar, over the
    /// text before the compared edits and now, side by side.
    pub(super) fn review_columns(&self, palette: &Palette, cx: &mut Context<Self>) -> gpui::Div {
        let column = |preview: &Entity<Preview>, bar: &ScrollbarState, before: bool| {
            let scroll = preview.read(cx).scroll.clone();
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .when(!before, |column| {
                    column.border_l_1().border_color(palette.hairline)
                })
                .child(self.column_head(before, palette))
                .child(
                    div()
                        .absolute()
                        .top(COLUMN_HEAD_HEIGHT)
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(
                            preview.clone().cached(
                                StyleRefinement::default()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                            ),
                        )
                        .child(scrollbar(Source::Handle(scroll), bar)),
                )
        };
        let columns = div()
            .absolute()
            .top(BAR_HEIGHT)
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .child(column(&self.before_preview, &self.before_scrollbar, true))
            .child(column(&self.preview, &self.scrollbars[2], false));
        self.review_pane(
            super::pane(palette.pane, palette)
                .flex_1()
                .relative()
                .child(columns),
            palette,
            cx,
        )
    }

    fn review_bar(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *palette;
        let review = self.review.as_ref();
        let count = self.session.review().hunks().len();
        let selected = self.selected();
        let kept = selected.is_some_and(|at| self.session.review().is_kept(at));
        let menu_open = review.is_some_and(|review| review.menu_open);
        let range = div()
            .id("review-range")
            .flex_none()
            .h(px(24.))
            .pl(px(8.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(6.))
            .font_weight(FontWeight::MEDIUM)
            .when(menu_open, |button| button.bg(palette.hover))
            .hover(|style| style.bg(palette.hover))
            .active(|style| style.opacity(0.8))
            .tooltip(crate::tooltip::text("Compare with an Earlier Version"))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(review) = &mut this.review {
                    review.menu_open = !review.menu_open;
                    cx.notify();
                }
            }))
            .child(self.session.log().range_name())
            .child(
                svg()
                    .path(CHEVRON_ICON)
                    .size(px(9.))
                    .text_color(palette.muted)
                    .with_transformation(Transformation::rotate(percentage(0.25))),
            );
        let range = div().relative().child(range).when(menu_open, |range| {
            range.child(
                div().absolute().top_full().left_0().mt_1().child(deferred(
                    anchored()
                        .snap_to_window_with_margin(px(8.))
                        .child(self.range_menu(&palette, cx)),
                )),
            )
        });
        let changes = (count > 0).then(|| {
            let position: SharedString = match selected {
                Some(at) => format!("{} of {count}", at + 1).into(),
                None => format!("– of {count}").into(),
            };
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .pl(px(4.))
                        .mr(px(2.))
                        .border_l_1()
                        .border_color(palette.hairline)
                        .child(step_button(
                            "review-previous",
                            false,
                            editor::by_platform!("Previous Change ([)", "Previous Change ([)"),
                            &palette,
                            cx.listener(|this, _, _, cx| this.step(false, cx)),
                        ))
                        .child(
                            div()
                                .id("review-position")
                                .px(px(2.))
                                .text_color(palette.muted)
                                .whitespace_nowrap()
                                .when(selected.is_none(), |position| {
                                    position.tooltip(crate::tooltip::text(format!(
                                        "{count} change{}. Click one or press ] to select it.",
                                        if count == 1 { "" } else { "s" }
                                    )))
                                })
                                .child(position),
                        )
                        .child(step_button(
                            "review-next",
                            true,
                            "Next Change (])",
                            &palette,
                            cx.listener(|this, _, _, cx| this.step(true, cx)),
                        )),
                )
                // With no change selected it restores the before column.
                .child(match selected {
                    Some(_) => bar_button("review-revert", "Revert", false, &palette)
                        .tooltip(crate::tooltip::text("Revert This Change (R)"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.revert_change(window, cx)),
                        ),
                    None => bar_button("review-revert", "Restore", false, &palette)
                        .tooltip(crate::tooltip::text("Go Back to the Before Column"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.restore_before(window, cx)),
                        ),
                })
                .child(
                    bar_button(
                        "review-keep",
                        if kept { "Kept" } else { "Keep" },
                        kept,
                        &palette,
                    )
                    .when_else(
                        selected.is_some(),
                        |button| {
                            button
                                .tooltip(crate::tooltip::text(if kept {
                                    "Mark as Not Reviewed (K)"
                                } else {
                                    "Keep This Change (K)"
                                }))
                                .on_click(cx.listener(|this, _, _, cx| this.keep_change(cx)))
                        },
                        |button| button.opacity(0.45),
                    ),
                )
        });
        div()
            .id("review-bar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(BAR_HEIGHT)
            .flex()
            .items_center()
            .gap(px(4.))
            .pl(px(4.))
            .pr(px(8.))
            .bg(palette.toolbar)
            .border_b_1()
            .border_color(palette.hairline)
            .text_size(px(12.))
            // Clicks on the controls never clear the selected change.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(range)
            .children(changes)
            .when(count == 0, |bar| {
                bar.child(
                    div()
                        .pl(px(10.))
                        .border_l_1()
                        .border_color(palette.hairline)
                        .text_color(palette.muted)
                        .child("No changes"),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("review-done")
                    .flex_none()
                    .h(px(24.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.on_accent)
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(palette.primary_top, 0.),
                        linear_color_stop(palette.primary_bottom, 1.),
                    ))
                    .hover(|style| {
                        style.bg(linear_gradient(
                            180.,
                            linear_color_stop(palette.primary_hover_top, 0.),
                            linear_color_stop(palette.primary_hover_bottom, 1.),
                        ))
                    })
                    .shadow(vec![
                        shadow(palette.primary_ring, 0., 0., 0.5),
                        shadow(palette.primary_drop, 1., 1.5, 0.),
                    ])
                    .active(|style| style.opacity(0.8))
                    .tooltip(crate::tooltip::text("Mark All as Reviewed and Close"))
                    .on_click(cx.listener(|this, _, window, cx| this.finish_review(window, cx)))
                    .child("Done"),
            )
    }

    /// "Show Changes Since" each agent edit, newest first, and a way back
    /// to the before column.
    fn range_menu(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let now = Utc::now();
        let edits = self.session.log().edits();
        let mut items = vec![MenuItem {
            label: "Show Changes Since".into(),
            checked: false,
            enabled: false,
            action: None,
            detail: None,
            heading: true,
        }];
        items.extend(edits.iter().enumerate().rev().map(|(ix, edit)| MenuItem {
            label: format!("“{}”", clip(&edit.prompt, 34)).into(),
            checked: ix == self.session.log().since(),
            enabled: true,
            action: Some(Some(ix)),
            detail: Some(relative_time(edit.at, now).into()),
            heading: false,
        }));
        items.push(MenuItem {
            label: SharedString::default(),
            checked: false,
            enabled: false,
            action: None,
            detail: None,
            heading: false,
        });
        items.push(MenuItem {
            label: "Restore the Before Column".into(),
            checked: false,
            enabled: !self.session.review().hunks().is_empty(),
            action: Some(None),
            detail: None,
            heading: false,
        });
        let pick = cx.entity().downgrade();
        let dismiss = cx.entity().downgrade();
        menu_panel(
            palette,
            &items,
            move |action: Option<usize>, window, cx| {
                pick.update(cx, |this, cx| match action {
                    Some(since) => this.compare_since(since, cx),
                    None => {
                        if let Some(review) = &mut this.review {
                            review.menu_open = false;
                        }
                        this.restore_before(window, cx);
                    }
                })
                .ok();
            },
            move |_, cx| {
                dismiss
                    .update(cx, |this, cx| {
                        if let Some(review) = &mut this.review {
                            review.menu_open = false;
                        }
                        cx.notify();
                    })
                    .ok();
            },
        )
    }

    /// Where the change under the pointer came from, beside the pointer.
    pub(super) fn change_origin_tooltip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (text, position) = self.review.as_ref()?.origin.clone()?;
        Some(deferred(
            anchored()
                .position(position + point(px(8.), px(16.)))
                .snap_to_window_with_margin(px(8.))
                .child(crate::tooltip::card(text, cx)),
        ))
    }
}

/// A review bar button with a hairline ring, like a push button, or
/// `plain`, a muted label without the ring.
fn bar_button(
    id: &'static str,
    label: &'static str,
    plain: bool,
    palette: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(24.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .font_weight(FontWeight::MEDIUM)
        // gpui allows one hover style per element, so both looks set it here.
        .when_else(
            plain,
            |button| {
                button
                    .text_color(palette.muted)
                    .hover(|style| style.bg(palette.hover).text_color(palette.text))
            },
            |button| {
                button
                    .bg(palette.control)
                    .shadow(vec![
                        shadow(palette.line_strong, 0., 0., 0.5),
                        shadow(palette.card_drop, 1., 1., 0.),
                    ])
                    .hover(|style| style.bg(palette.control_hover))
            },
        )
        .active(|style| style.opacity(0.8))
        .child(label)
}

fn step_button(
    id: &'static str,
    forward: bool,
    tooltip: &'static str,
    palette: &Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let palette = *palette;
    div()
        .id(id)
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .group(id)
        .hover(|style| style.bg(palette.hover))
        .active(|style| style.opacity(0.8))
        .tooltip(crate::tooltip::text(tooltip))
        .on_click(on_click)
        .child(
            svg()
                .path(CHEVRON_ICON)
                .size(px(10.))
                .text_color(palette.muted)
                .group_hover(id, |style| style.text_color(palette.text))
                .when(!forward, |icon| {
                    icon.with_transformation(Transformation::rotate(percentage(0.5)))
                }),
        )
}

/// `text` on one line, cut to `max` characters with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let mut clipped: String = text.chars().take(max - 1).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_long_prompts_to_one_line() {
        assert_eq!(clip("make it\n  shorter", 40), "make it shorter");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
    }
}
