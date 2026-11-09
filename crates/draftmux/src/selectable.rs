//! Text selection across the paragraphs of rendered markup, the way a browser
//! selects text on a page.
//!
//! A [`TextSelection`] belongs to one surface, such as the preview or the
//! chat. Its paragraphs are [`InlineText`](crate::inline_text::InlineText)s
//! marked selectable with a [`Select`]; each registers its text and a hit test
//! while it prepaints. [`area`] wraps the surface: a press starts a selection
//! at the nearest text, dragging extends it across
//! paragraphs, a double click selects a word and a triple click a paragraph.
//! Dragging past the top or bottom of the surface scrolls it, faster the
//! farther the pointer is. ⌘C copies the selected text and ⌘A selects
//! everything. Pressing outside the
//! surface clears the selection.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, Div, Entity, EventEmitter, FocusHandle, Hsla, KeyBinding,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollHandle, SharedString, Window, actions, canvas, div, point, prelude::*, px,
};

actions!(selectable, [CopySelection, SelectAllText]);

pub const KEY_CONTEXT: &str = "SelectableText";
/// How often a drag past the edge scrolls.
const AUTOSCROLL_TICK: Duration = Duration::from_millis(16);
/// The most a drag past the edge scrolls in one tick.
const AUTOSCROLL_MAX_STEP: Pixels = px(40.);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(editor::secondary!("c"), CopySelection, Some(KEY_CONTEXT)),
        KeyBinding::new(editor::secondary!("a"), SelectAllText, Some(KEY_CONTEXT)),
    ]);
}

/// A paragraph's place on its surface: the body it belongs to, such as a chat
/// message, then its order in the body. Keys order paragraphs as they read.
pub type Key = (usize, usize);

/// Paragraph text collected while constructing a virtualized body, including
/// paragraphs whose elements will not be laid out or painted.
pub type TextRegistry = Rc<RefCell<BTreeMap<Key, SharedString>>>;

/// A byte offset in a paragraph's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub key: Key,
    pub offset: usize,
}

/// What marks an [`InlineText`](crate::inline_text::InlineText) selectable:
/// the surface's selection, the paragraph's key and the highlight colour.
#[derive(Clone)]
pub struct Select {
    pub state: Entity<TextSelection>,
    pub key: Key,
    pub color: Hsla,
    pub registry: Option<TextRegistry>,
}

/// What scrolls the surface, so a drag past its edge can.
#[derive(Clone)]
pub enum Scroller {
    Handle(ScrollHandle),
    List(ListState),
}

impl Scroller {
    fn viewport(&self) -> Bounds<Pixels> {
        match self {
            Scroller::Handle(handle) => handle.bounds(),
            Scroller::List(list) => list.viewport_bounds(),
        }
    }

    /// Moves the view `distance` toward the top, or toward the bottom when
    /// negative.
    fn scroll_by(&self, distance: Pixels) {
        match self {
            // The offset is zero at the top and negative below it.
            Scroller::Handle(handle) => {
                let offset = handle.offset();
                let y = (offset.y + distance).clamp(-handle.max_offset().y, px(0.));
                handle.set_offset(point(offset.x, y));
            }
            // The list scrolls down by a positive distance.
            Scroller::List(list) => list.scroll_by(-distance),
        }
    }
}

/// The selection changed; the range is in each end's paragraph.
pub struct SelectionChanged;

pub struct TextSelection {
    focus: FocusHandle,
    /// The text of every paragraph registered since the content was last
    /// reset, including ones scrolled out of a list since.
    texts: BTreeMap<Key, SharedString>,
    /// Paragraphs painted in the current frame, in paint order.
    painted: Vec<Painted>,
    /// Where the current frame painted the selection, one rectangle per
    /// line fragment.
    rects: Vec<Bounds<Pixels>>,
    anchor: Option<Pos>,
    head: Option<Pos>,
    /// While the button is down: what the drag extends by and the unit it
    /// started on, which stays selected whichever way it goes.
    dragging: Option<(Unit, Pos, Pos)>,
    scroller: Option<Scroller>,
    /// Where the pointer last was during a drag.
    pointer: Point<Pixels>,
    /// Whether a drag past the edge is scrolling.
    autoscrolling: bool,
    /// Whether a press outside the area keeps the selection.
    sticky: bool,
}

struct Painted {
    key: Key,
    bounds: Bounds<Pixels>,
    hit: Rc<dyn Fn(Point<Pixels>) -> usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Unit {
    Char,
    Word,
    Paragraph,
}

impl EventEmitter<SelectionChanged> for TextSelection {}

impl TextSelection {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            texts: BTreeMap::new(),
            painted: Vec::new(),
            rects: Vec::new(),
            anchor: None,
            head: None,
            dragging: None,
            scroller: None,
            pointer: Point::default(),
            autoscrolling: false,
            sticky: false,
        }
    }

    /// Keeps the selection when the user presses outside the area, so it
    /// can be used from elsewhere; a press inside or [`Self::clear`] still
    /// clears it.
    pub fn set_sticky(&mut self, sticky: bool) {
        self.sticky = sticky;
    }

    /// Lets a drag past the surface's edge scroll it with `scroller`.
    pub fn set_scroller(&mut self, scroller: Option<Scroller>) {
        self.scroller = scroller;
    }

    /// Forgets the paragraphs and the selection, for new content whose
    /// paragraphs reuse the old keys.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.texts.clear();
        self.clear(cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.dragging = None;
        if self.anchor.take().is_some() | self.head.take().is_some() {
            cx.emit(SelectionChanged);
            cx.notify();
        }
    }

    /// Publishes every paragraph in a virtualized body before hit testing.
    pub fn set_texts(&mut self, texts: BTreeMap<Key, SharedString>) {
        self.texts = texts;
    }

    /// Called by a selectable paragraph as it prepaints.
    pub(crate) fn register(
        &mut self,
        key: Key,
        text: SharedString,
        bounds: Bounds<Pixels>,
        hit: Rc<dyn Fn(Point<Pixels>) -> usize>,
    ) {
        self.texts.insert(key, text);
        self.painted.push(Painted { key, bounds, hit });
    }

    pub(crate) fn painted_selection(&mut self, rect: Bounds<Pixels>) {
        self.rects.push(rect);
    }

    /// Window-space rectangles of the selection as last painted, in
    /// reading order, for anchoring popovers.
    pub fn selection_rects(&self) -> &[Bounds<Pixels>] {
        &self.rects
    }

    /// Whether the mouse button is down on a selection being made.
    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// The selected start and end, in reading order; `None` when nothing is
    /// selected.
    pub fn range(&self) -> Option<(Pos, Pos)> {
        let (anchor, head) = (self.anchor?, self.head?);
        (anchor != head).then(|| (anchor.min(head), anchor.max(head)))
    }

    /// The selected part of paragraph `key`, whose text is `len` bytes long.
    pub fn range_in(&self, key: Key, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.range()?;
        if key < start.key || key > end.key {
            return None;
        }
        let from = if key == start.key { start.offset } else { 0 };
        let to = if key == end.key { end.offset } else { len };
        let range = from.min(len)..to.min(len);
        (!range.is_empty()).then_some(range)
    }

    /// The text of every registered paragraph, in reading order.
    pub fn paragraphs(&self) -> impl Iterator<Item = (Key, &str)> {
        self.texts.iter().map(|(key, text)| (*key, text.as_ref()))
    }

    /// The selected text, with a line break between paragraphs.
    pub fn selected_text(&self) -> String {
        let Some((start, end)) = self.range() else {
            return String::new();
        };
        let mut out = Vec::new();
        for (key, text) in self.texts.range(start.key..=end.key) {
            if let Some(range) = self.range_in(*key, text.len()) {
                let range = floor_boundary(text, range.start)..floor_boundary(text, range.end);
                out.push(&text[range]);
            } else {
                out.push("");
            }
        }
        // A selection from a gap starts at the end of the paragraph above
        // and adds nothing from it.
        let first = out.iter().position(|text| !text.is_empty()).unwrap_or(0);
        let last = out
            .iter()
            .rposition(|text| !text.is_empty())
            .map_or(0, |ix| ix + 1);
        out[first..last.max(first)].join("\n")
    }

    /// Where `point` falls in the painted paragraphs: in the one whose
    /// lines it is level with, else at the end of the nearest one above it,
    /// else at the start of the first.
    fn pos_at(&self, point: Point<Pixels>) -> Option<Pos> {
        let mut painted: Vec<&Painted> = self.painted.iter().collect();
        painted.sort_by_key(|p| p.key);
        let level = painted
            .iter()
            .filter(|p| p.bounds.top() <= point.y && point.y < p.bounds.bottom())
            .min_by(|a, b| {
                distance_x(a.bounds, point.x)
                    .partial_cmp(&distance_x(b.bounds, point.x))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(p) = level {
            return Some(Pos {
                key: p.key,
                offset: (p.hit)(point),
            });
        }
        let above = painted.iter().rev().find(|p| p.bounds.top() <= point.y);
        match above {
            Some(p) => Some(Pos {
                key: p.key,
                offset: self.texts.get(&p.key).map_or(0, |text| text.len()),
            }),
            None => painted.first().map(|p| Pos {
                key: p.key,
                offset: 0,
            }),
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pos) = self.pos_at(event.position) else {
            self.clear(cx);
            return;
        };
        window.focus(&self.focus, cx);
        let unit = match event.click_count {
            1 => Unit::Char,
            2 => Unit::Word,
            _ => Unit::Paragraph,
        };
        // Shift-click extends the selection from its anchor.
        let (start, end, head) = match self.anchor {
            Some(anchor) if unit == Unit::Char && event.modifiers.shift => (anchor, anchor, pos),
            _ => {
                let (start, end) = self.expand(pos, unit);
                (start, end, end)
            }
        };
        self.dragging = Some((unit, start, end));
        self.anchor = Some(start);
        self.head = Some(head);
        cx.emit(SelectionChanged);
        cx.notify();
    }

    fn mouse_drag(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((unit, start, end)) = self.dragging else {
            return;
        };
        self.pointer = point;
        if !self.autoscrolling && self.autoscroll_step() != px(0.) {
            self.autoscrolling = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_TICK).await;
                    let more = this.update(cx, |this, cx| this.autoscroll(cx));
                    if !matches!(more, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
        }
        let Some(pos) = self.pos_at(point) else {
            return;
        };
        let (anchor, head) = if pos < start {
            (end, self.expand(pos, unit).0)
        } else {
            (start, self.expand(pos, unit).1)
        };
        if (self.anchor, self.head) != (Some(anchor), Some(head)) {
            self.anchor = Some(anchor);
            self.head = Some(head);
            cx.emit(SelectionChanged);
            cx.notify();
        }
    }

    /// How far to scroll this tick: up (positive) with the pointer above the
    /// viewport, down below it, nothing inside it.
    fn autoscroll_step(&self) -> Pixels {
        let Some(viewport) = self.scroller.as_ref().map(Scroller::viewport) else {
            return px(0.);
        };
        let y = self.pointer.y;
        let past = if y < viewport.top() {
            viewport.top() - y
        } else if y > viewport.bottom() {
            viewport.bottom() - y
        } else {
            return px(0.);
        };
        // A quarter of the distance past the edge, so the speed follows the
        // pointer.
        (past / 4.).clamp(-AUTOSCROLL_MAX_STEP, AUTOSCROLL_MAX_STEP)
    }

    /// Scrolls one tick and extends the selection to the text now under the
    /// pointer. Returns whether to keep going.
    fn autoscroll(&mut self, cx: &mut Context<Self>) -> bool {
        let step = self.autoscroll_step();
        let scroller = match (&self.scroller, self.dragging) {
            (Some(scroller), Some(_)) if step != px(0.) => scroller.clone(),
            _ => {
                self.autoscrolling = false;
                return false;
            }
        };
        scroller.scroll_by(step);
        // The paragraphs are where the last frame painted them; the next
        // tick sees them moved.
        self.mouse_drag(self.pointer, cx);
        cx.notify();
        true
    }

    /// The word or paragraph around `pos`, or `pos` itself.
    fn expand(&self, pos: Pos, unit: Unit) -> (Pos, Pos) {
        let text = self.texts.get(&pos.key).map_or("", |text| text.as_ref());
        let range = match unit {
            Unit::Char => pos.offset..pos.offset,
            Unit::Word => word_at(text, pos.offset),
            Unit::Paragraph => 0..text.len(),
        };
        (
            Pos {
                offset: range.start,
                ..pos
            },
            Pos {
                offset: range.end,
                ..pos
            },
        )
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let (Some((first, _)), Some((last, text))) =
            (self.texts.first_key_value(), self.texts.last_key_value())
        else {
            return;
        };
        self.anchor = Some(Pos {
            key: *first,
            offset: 0,
        });
        self.head = Some(Pos {
            key: *last,
            offset: text.len(),
        });
        cx.emit(SelectionChanged);
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
}

/// Wraps `content`, whose paragraphs are selectable with `state`, in what
/// drives the selection: presses, drags, copying and selecting all. Controls
/// in `content` that stop their mouse-down's propagation don't start a
/// selection.
pub fn area(state: &Entity<TextSelection>, cx: &App) -> Div {
    let focus = state.read(cx).focus.clone();
    let frame = state.clone();
    div()
        .track_focus(&focus)
        .key_context(KEY_CONTEXT)
        .on_action({
            let state = state.clone();
            move |_: &CopySelection, _, cx| state.update(cx, |this, cx| this.copy(cx))
        })
        .on_action({
            let state = state.clone();
            move |_: &SelectAllText, _, cx| state.update(cx, |this, cx| this.select_all(cx))
        })
        .on_mouse_down(MouseButton::Left, {
            let state = state.clone();
            move |event, window, cx| state.update(cx, |this, cx| this.mouse_down(event, window, cx))
        })
        .on_mouse_down_out({
            let state = state.clone();
            move |_, _, cx| {
                state.update(cx, |this, cx| {
                    if !this.sticky {
                        this.clear(cx);
                    }
                })
            }
        })
        // Laid out first, so it forgets the last frame's paragraphs before
        // this frame's register; while dragging it follows the pointer
        // anywhere in the window.
        .child(
            canvas(
                {
                    let state = frame.clone();
                    move |_, _, cx| {
                        state.update(cx, |this, _| {
                            this.painted.clear();
                            this.rects.clear();
                        })
                    }
                },
                move |_, _, window, cx| {
                    if frame.read(cx).dragging.is_none() {
                        return;
                    }
                    let state = frame.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, _, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            state.update(cx, |this, cx| this.mouse_drag(event.position, cx));
                        }
                    });
                    let state = frame.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                        state.update(cx, |this, _| this.dragging = None);
                    });
                },
            )
            .absolute()
            .size_0(),
        )
}

fn distance_x(bounds: Bounds<Pixels>, x: Pixels) -> f32 {
    if x < bounds.left() {
        f32::from(bounds.left() - x)
    } else if x > bounds.right() {
        f32::from(x - bounds.right())
    } else {
        0.
    }
}

/// The run of word characters, or of other non-space characters, around
/// `offset`; an empty range between spaces.
fn word_at(text: &str, offset: usize) -> Range<usize> {
    let offset = floor_boundary(text, offset.min(text.len()));
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' || c == '\'' || c == '’' {
            1
        } else if c.is_whitespace() {
            0
        } else {
            2
        }
    };
    // The character after the offset, or before it at the end.
    let Some(here) = text[offset..]
        .chars()
        .next()
        .or_else(|| text[..offset].chars().next_back())
    else {
        return offset..offset;
    };
    let kind = class(here);
    if kind == 0 {
        return offset..offset;
    }
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| class(c) == kind)
        .last()
        .map_or(offset, |(ix, _)| ix);
    let end = text[offset..]
        .char_indices()
        .find(|&(_, c)| class(c) != kind)
        .map_or(text.len(), |(ix, _)| offset + ix);
    start..end
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::word_at;

    #[test]
    fn word_at_finds_words_and_punctuation_runs() {
        let text = "it’s a well-known fact";
        assert_eq!(&text[word_at(text, 1)], "it’s");
        assert_eq!(&text[word_at(text, 9)], "well");
        assert_eq!(&text[word_at(text, 13)], "-");
        assert_eq!(&text[word_at(text, 7)], "a");
        assert_eq!(word_at(text, 6), 6..6);
        assert_eq!(&text[word_at(text, text.len())], "fact");
        assert_eq!(word_at("", 0), 0..0);
    }
}
