//! Find in the draft: a bar over the editor that tints the text matching
//! what is typed into it and steps through the matches.
//!
//! The editor's selection stays where it is while the bar steps, so the
//! selection popover doesn't follow the matches; closing the bar from its
//! field selects the match it was at.

use std::ops::Range;

use editor::{Editor, EditorEvent, EditorStyle};
use gpui::{
    Context, Entity, Focusable, KeyBinding, MouseButton, SharedString, Stateful, Subscription,
    Transformation, Window, actions, div, font, percentage, prelude::*, px, svg,
};

use super::{Chrome, shadow};
use crate::assets::{CHEVRON_ICON, CLOSE_ICON};
use crate::theme::{self, Palette};

actions!(find, [Find, FindNext, FindPrevious, CloseFind]);

const KEY_CONTEXT: &str = "FindBar";
const WIDTH: gpui::Pixels = px(300.);

pub(super) fn init(cx: &mut gpui::App) {
    let field = Some(format!("{KEY_CONTEXT} > Editor"));
    // Bound after the editor's keys, so in the field these win over its
    // newline and its Escape.
    cx.bind_keys([
        KeyBinding::new(editor::secondary!("f"), Find, None),
        KeyBinding::new(editor::secondary!("g"), FindNext, None),
        KeyBinding::new(editor::secondary!("shift-g"), FindPrevious, None),
        KeyBinding::new("enter", FindNext, field.as_deref()),
        KeyBinding::new("shift-enter", FindPrevious, field.as_deref()),
        KeyBinding::new("escape", CloseFind, field.as_deref()),
    ]);
}

pub(super) struct FindBar {
    open: bool,
    /// What to find; kept while the bar is closed.
    field: Entity<Editor>,
    /// Where the editor's text matches the field's, while the bar is open.
    matches: Vec<Range<usize>>,
    /// The match the bar is at.
    current: Option<usize>,
    /// Where the search goes on from: the editor's caret when the bar
    /// opened, then the start of each match it has been at.
    anchor: usize,
    _subscription: Subscription,
}

impl FindBar {
    pub(super) fn new(cx: &mut Context<Chrome>) -> Self {
        let field = theme::editor(field_style, cx);
        field.update(cx, |field, cx| field.set_markdown(false, cx));
        let subscription = cx.subscribe(&field, |this, _, event, cx| {
            if matches!(event, EditorEvent::Edited) {
                this.search(true, cx);
            }
        });
        Self {
            open: false,
            field,
            matches: Vec::new(),
            current: None,
            anchor: 0,
            _subscription: subscription,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }
}

impl Chrome {
    /// Opens the find bar, or goes back to its field, with the field's text
    /// selected to type over. A selection within one line of the editor
    /// becomes what to find.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.drafts().current().is_none() || self.review.is_some() {
            return;
        }
        if !self.show_editor {
            self.toggle_editor(window, cx);
        }
        let editor = self.editor.read(cx);
        let selected = editor.selected_range();
        self.find.anchor = selected.start;
        let seed = Some(&editor.text()[selected])
            .filter(|text| !text.is_empty() && !text.contains('\n'))
            .map(str::to_owned);
        self.find.open = true;
        self.find.field.update(cx, |field, cx| {
            if let Some(seed) = seed.filter(|seed| seed != field.text()) {
                field.set_text(&seed, cx);
            }
            field.select_range(0..field.text().len(), cx);
        });
        window.focus(&self.find.field.focus_handle(cx), cx);
        self.search(true, cx);
    }

    /// Closes the find bar. From its field, the keyboard goes back to the
    /// editor with the match the bar was at selected.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let current = self
            .find
            .current
            .and_then(|ix| self.find.matches.get(ix).cloned());
        self.end_find(cx);
        let editor_focus = self.editor.focus_handle(cx);
        if self.find.field.focus_handle(cx).is_focused(window) {
            if let Some(range) = current {
                self.editor
                    .update(cx, |editor, cx| editor.select_range(range, cx));
            }
            window.focus(&editor_focus, cx);
        }
    }

    /// Closes the find bar where it is no longer shown, leaving the
    /// editor's selection and the focus alone.
    pub(super) fn end_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        self.find.open = false;
        self.find.matches.clear();
        self.find.current = None;
        self.editor
            .update(cx, |editor, cx| editor.set_found(Vec::new(), None, cx));
        cx.notify();
    }

    /// Finds the field's text in the editor's again and goes to the first
    /// match from where the search was; `reveal` scrolls to it. Called when
    /// either text changes.
    pub(super) fn search(&mut self, reveal: bool, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let matches = matches(self.editor.read(cx).text(), self.find.field.read(cx).text());
        let from = matches.partition_point(|range| range.start < self.find.anchor);
        // Past the last match, the search wraps around to the first.
        self.find.current = (!matches.is_empty()).then_some(from % matches.len().max(1));
        self.find.matches = matches;
        self.show_found(reveal, cx);
    }

    /// Goes to the next match, or the previous one, wrapping around; opens
    /// the bar when it is closed.
    pub(super) fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            self.open_find(window, cx);
            return;
        }
        let len = self.find.matches.len();
        let Some(current) = self.find.current.filter(|_| len > 0) else {
            return;
        };
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.find.current = Some(next);
        self.find.anchor = self.find.matches[next].start;
        self.show_found(true, cx);
    }

    /// Tints the matches in the editor.
    fn show_found(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let (matches, current) = (self.find.matches.clone(), self.find.current);
        let at = current.map(|ix| matches[ix].clone());
        self.editor.update(cx, |editor, cx| {
            editor.set_found(matches, current, cx);
            if let Some(range) = at.filter(|_| reveal) {
                editor.reveal(range, cx);
            }
        });
        cx.notify();
    }

    /// The find bar, in the top right corner of the editor's pane.
    pub(super) fn find_bar(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let field = self.find.field.clone();
        let empty = field.read(cx).text().is_empty();
        let focus = field.focus_handle(cx);
        let count: Option<SharedString> = match (self.find.current, self.find.matches.len()) {
            _ if empty => None,
            (Some(current), len) => Some(format!("{} of {len}", current + 1).into()),
            (None, _) => Some("No results".into()),
        };
        let found = !self.find.matches.is_empty();
        let style = theme::zoom_editor(field_style(palette), theme::get(cx).zoom);
        div()
            .id("find-bar")
            .key_context(KEY_CONTEXT)
            .occlude()
            .absolute()
            .top(px(8.))
            .right(px(16.))
            .w(WIDTH)
            .max_w_full()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(4.))
            .rounded(px(9.))
            .bg(palette.menu)
            .shadow(vec![
                shadow(palette.toast_ring, 0., 0., 0.5),
                shadow(palette.toast_drop, 6., 18., 0.),
            ])
            .text_size(px(12.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .mr(px(2.))
                    .py(px(2.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .bg(palette.menu_field)
                    .border_1()
                    .border_color(palette.hairline)
                    .cursor_text()
                    // The field is as wide as its text; a press beside it
                    // still focuses it.
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&focus, cx)
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .child(field)
                            .when(empty, |input| {
                                input.child(
                                    div()
                                        .absolute()
                                        .top(style.padding_top)
                                        .left_0()
                                        .text_size(style.font_size)
                                        .line_height(style.line_height)
                                        .text_color(palette.faint)
                                        .child("Find"),
                                )
                            }),
                    )
                    .children(count.map(|count| {
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .text_color(palette.faint)
                            .child(count)
                    })),
            )
            .child(
                step_button("find-previous", true, found, palette)
                    .tooltip(crate::tooltip::text(editor::by_platform!(
                        "Previous match (⇧↩)",
                        "Previous match (Shift+Enter)"
                    )))
                    .on_click(cx.listener(|this, _, window, cx| this.find_step(false, window, cx))),
            )
            .child(
                step_button("find-next", false, found, palette)
                    .tooltip(crate::tooltip::text(editor::by_platform!(
                        "Next match (↩)",
                        "Next match (Enter)"
                    )))
                    .on_click(cx.listener(|this, _, window, cx| this.find_step(true, window, cx))),
            )
            .child(
                button("find-close", true, palette)
                    .tooltip(crate::tooltip::text("Close (esc)"))
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx)))
                    .child(
                        svg()
                            .path(CLOSE_ICON)
                            .size(px(10.))
                            .text_color(palette.muted),
                    ),
            )
    }
}

/// One of the bar's icon buttons; dimmed and inert unless `enabled`.
fn button(id: &'static str, enabled: bool, palette: &Palette) -> Stateful<gpui::Div> {
    let hover = palette.hover;
    div()
        .id(id)
        .size(px(22.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .when(enabled, |button| {
            button
                .hover(move |style| style.bg(hover))
                .active(|style| style.opacity(0.8))
        })
        .when(!enabled, |button| button.opacity(0.4))
}

/// The button to the previous match, a chevron pointing up, or to the next,
/// pointing down.
fn step_button(
    id: &'static str,
    up: bool,
    enabled: bool,
    palette: &Palette,
) -> Stateful<gpui::Div> {
    button(id, enabled, palette).child(
        svg()
            .path(CHEVRON_ICON)
            .size(px(10.))
            .text_color(palette.muted)
            .with_transformation(Transformation::rotate(percentage(if up {
                0.75
            } else {
                0.25
            }))),
    )
}

/// The find field: the UI font, as tall as its text.
fn field_style(palette: &Palette) -> EditorStyle {
    EditorStyle {
        font: font(editor::platform::ui_font()),
        font_size: px(13.),
        line_height: px(19.),
        padding_x: px(0.),
        padding_top: px(1.),
        padding_bottom: px(1.),
        text: palette.text,
        heading: palette.text,
        fit_content: true,
        ..palette.editor_style()
    }
}

/// Where `text` contains `query`, ignoring case, in order and without
/// overlaps; nowhere for an empty query.
fn matches(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    // Lowercasing can change a character's length in bytes, so the search
    // runs over the folded text and maps what it finds back.
    const INSIDE: usize = usize::MAX;
    let mut folded = String::with_capacity(text.len());
    // For each byte of `folded`, where the character it came from starts in
    // `text`, or `INSIDE` when it isn't the first byte that character gave.
    let mut origin = Vec::with_capacity(text.len() + 1);
    for (at, ch) in text.char_indices() {
        folded.extend(ch.to_lowercase());
        origin.push(at);
        origin.resize(folded.len(), INSIDE);
    }
    origin.push(text.len());
    let query: String = query.chars().flat_map(char::to_lowercase).collect();
    folded
        .match_indices(&query)
        .filter_map(|(start, found)| {
            let (start, end) = (origin[start], origin[start + found.len()]);
            // A match can't split what one character lowercased to.
            (start != INSIDE && end != INSIDE).then_some(start..end)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn finds_every_occurrence_ignoring_case() {
        assert_eq!(
            matches("The cat. THE dog; the end", "the"),
            [0..3, 9..12, 18..21]
        );
        assert_eq!(matches("The cat", "CAT"), [4..7]);
        assert!(matches("The cat", "dog").is_empty());
        assert!(matches("The cat", "").is_empty());
        assert!(matches("", "cat").is_empty());
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(matches("aaaaa", "aa"), [0..2, 2..4]);
    }

    #[test]
    fn matches_span_lines() {
        assert_eq!(matches("one\ntwo", "e\nt"), [2..5]);
    }

    #[test]
    fn ranges_are_in_the_text_where_lowercasing_changes_its_length() {
        // `İ` is two bytes and lowercases to three; `ẞ` is three and
        // lowercases to two.
        let text = "İstanbul STRAẞE café Straße";
        for (query, found) in [("stra", vec!["STRA", "Stra"]), ("ße", vec!["ẞE", "ße"])] {
            let ranges = matches(text, query);
            let texts: Vec<_> = ranges.iter().map(|range| &text[range.clone()]).collect();
            assert_eq!(texts, found);
        }
        assert_eq!(&text[matches(text, "CAFÉ")[0].clone()], "café");
        assert_eq!(
            &text[matches(text, "i\u{307}stanbul")[0].clone()],
            "İstanbul"
        );
        // Half of what `İ` lowercases to isn't a match.
        assert!(matches("İ", "i").is_empty());
    }
}
