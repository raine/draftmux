//! The editor entity: actions, mouse handling, native text input and the
//! public API.

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Font, Hsla, KeyBinding, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, SharedString, UTF16Selection, Window,
    actions, anchored, deferred, div, font, prelude::*, px, rgb, rgba,
};

pub use crate::buffer::TrackedId;
use crate::buffer::{Affinity, Buffer, Selection, offset_from_utf16, offset_to_utf16};
use crate::element::{EditorElement, Layout};
use crate::highlight::{Styles, highlight};
use crate::movement::{Granularity, drag_selection, is_word};

actions!(
    editor,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Newline,
        Tab,
        Indent,
        Outdent,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocumentStart,
        DocumentEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        PageUp,
        PageDown,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        ShowCharacterPalette,
        DismissMenu,
        AcceptGhost,
        AcceptGhostWord,
        DismissGhost,
    ]
);

const KEY_CONTEXT: &str = "Editor";
/// Added to the editor's key context while a ghost shows, and while it has
/// text; see [`Editor::set_ghost`].
const GHOST_CONTEXT: &str = "ghost";
const GHOST_TEXT_CONTEXT: &str = "ghost_text";

/// Binds the platform text-editing shortcuts. Call once at startup.
pub fn init(cx: &mut App) {
    cx.bind_keys(bindings());
    cx.bind_keys(ghost_bindings());
}

/// Bound after the editor's own keys, so at the same depth these win while
/// their context is present, and Tab and word-right keep their meaning
/// otherwise.
fn ghost_bindings() -> Vec<KeyBinding> {
    let ghost = format!("{KEY_CONTEXT} && {GHOST_CONTEXT}");
    let text = format!("{KEY_CONTEXT} && {GHOST_TEXT_CONTEXT}");
    vec![
        KeyBinding::new("escape", DismissGhost, Some(&ghost)),
        KeyBinding::new("tab", AcceptGhost, Some(&text)),
        KeyBinding::new(
            by_platform!("alt-right", "ctrl-right"),
            AcceptGhostWord,
            Some(&text),
        ),
    ]
}

#[cfg(target_os = "macos")]
fn bindings() -> Vec<KeyBinding> {
    let ctx = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("shift-backspace", Backspace, ctx),
        KeyBinding::new("ctrl-h", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("ctrl-d", Delete, ctx),
        KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("alt-delete", DeleteWordRight, ctx),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
        KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
        KeyBinding::new("ctrl-k", DeleteToLineEnd, ctx),
        KeyBinding::new("enter", Newline, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("tab", Tab, ctx),
        KeyBinding::new("shift-tab", Outdent, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("ctrl-b", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("ctrl-f", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("ctrl-p", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("ctrl-n", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("alt-left", WordLeft, ctx),
        KeyBinding::new("alt-right", WordRight, ctx),
        KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
        KeyBinding::new("cmd-left", LineStart, ctx),
        KeyBinding::new("home", LineStart, ctx),
        KeyBinding::new("ctrl-a", LineStart, ctx),
        KeyBinding::new("cmd-right", LineEnd, ctx),
        KeyBinding::new("end", LineEnd, ctx),
        KeyBinding::new("ctrl-e", LineEnd, ctx),
        KeyBinding::new("ctrl-shift-a", SelectLineStart, ctx),
        KeyBinding::new("ctrl-shift-e", SelectLineEnd, ctx),
        KeyBinding::new("cmd-shift-left", SelectLineStart, ctx),
        KeyBinding::new("shift-home", SelectLineStart, ctx),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, ctx),
        KeyBinding::new("shift-end", SelectLineEnd, ctx),
        KeyBinding::new("cmd-up", DocumentStart, ctx),
        KeyBinding::new("cmd-down", DocumentEnd, ctx),
        KeyBinding::new("cmd-shift-up", SelectDocumentStart, ctx),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, ctx),
        KeyBinding::new("pageup", PageUp, ctx),
        KeyBinding::new("pagedown", PageDown, ctx),
        KeyBinding::new("cmd-]", Indent, ctx),
        KeyBinding::new("cmd-[", Outdent, ctx),
        KeyBinding::new("cmd-a", SelectAll, ctx),
        KeyBinding::new("cmd-c", Copy, ctx),
        KeyBinding::new("cmd-x", Cut, ctx),
        KeyBinding::new("cmd-v", Paste, ctx),
        // Without a context, so the host can undo from outside an editor.
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, ctx),
        KeyBinding::new("escape", DismissMenu, ctx),
    ]
}

/// Linux and Windows share Ctrl-based bindings.
#[cfg(not(target_os = "macos"))]
fn bindings() -> Vec<KeyBinding> {
    ctrl_bindings()
}

#[cfg(any(test, not(target_os = "macos")))]
fn ctrl_bindings() -> Vec<KeyBinding> {
    let ctx = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("shift-backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("ctrl-delete", DeleteWordRight, ctx),
        KeyBinding::new("enter", Newline, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("tab", Tab, ctx),
        KeyBinding::new("shift-tab", Outdent, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("ctrl-left", WordLeft, ctx),
        KeyBinding::new("ctrl-right", WordRight, ctx),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
        KeyBinding::new("home", LineStart, ctx),
        KeyBinding::new("end", LineEnd, ctx),
        KeyBinding::new("shift-home", SelectLineStart, ctx),
        KeyBinding::new("shift-end", SelectLineEnd, ctx),
        KeyBinding::new("ctrl-home", DocumentStart, ctx),
        KeyBinding::new("ctrl-end", DocumentEnd, ctx),
        KeyBinding::new("ctrl-shift-home", SelectDocumentStart, ctx),
        KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, ctx),
        KeyBinding::new("pageup", PageUp, ctx),
        KeyBinding::new("pagedown", PageDown, ctx),
        KeyBinding::new("ctrl-]", Indent, ctx),
        KeyBinding::new("ctrl-[", Outdent, ctx),
        KeyBinding::new("ctrl-a", SelectAll, ctx),
        KeyBinding::new("ctrl-c", Copy, ctx),
        KeyBinding::new("ctrl-x", Cut, ctx),
        KeyBinding::new("ctrl-v", Paste, ctx),
        // Without a context, so the host can undo from outside an editor.
        KeyBinding::new("ctrl-z", Undo, None),
        KeyBinding::new("ctrl-shift-z", Redo, None),
        KeyBinding::new("ctrl-y", Redo, None),
        KeyBinding::new("escape", DismissMenu, ctx),
    ]
}

/// One item of the editor's context menu; `action: None` is a separator.
pub struct ContextMenuItem {
    pub label: SharedString,
    pub action: Option<Box<dyn Action>>,
    pub enabled: bool,
}

/// Draws the context menu's panel from its items. Picking an item should
/// dispatch its action, which reaches the editor since it has focus.
pub type ContextMenuRenderer =
    Rc<dyn Fn(Vec<ContextMenuItem>, &mut Window, &mut App) -> AnyElement>;

/// Emitted after the change it describes has been applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// The text changed.
    Edited,
    /// The selected byte range changed, including to or from empty.
    SelectionChanged(Range<usize>),
    /// The extra selections changed; see [`Editor::extra_selections`].
    ExtraSelectionsChanged,
    /// The user moved this text from the ghost into the buffer, by
    /// accepting it or typing it; see [`Editor::set_ghost`].
    GhostAccepted(String),
    /// The ghost went away by the user's doing: dismissed, or an edit or
    /// caret movement other than accepting it.
    GhostDismissed,
}

/// Display-only text after a buffer offset; see [`Editor::set_ghost`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ghost {
    pub offset: usize,
    /// Empty while waiting for text, shown as three rising dots.
    pub text: String,
}

/// Fonts, colours and metrics. The default is the light draft editor.
#[derive(Clone, PartialEq)]
pub struct EditorStyle {
    pub font: Font,
    pub font_size: Pixels,
    pub line_height: Pixels,
    pub padding_x: Pixels,
    pub padding_top: Pixels,
    pub padding_bottom: Pixels,
    pub text: Hsla,
    /// The color of all text while read-only. Without one, text keeps its
    /// usual colors, faded.
    pub read_only_text: Option<Hsla>,
    pub heading: Hsla,
    pub code: Hsla,
    pub link: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub caret: Hsla,
    pub selection: Hsla,
    pub inactive_selection: Hsla,
    /// The tint of text that [`Editor::flash`] points out.
    pub flash: Hsla,
    /// The tint of the range [`Editor::set_marked`] points out; a mark
    /// that is only a position is a bar in this color, opaque.
    pub marked: Hsla,
    /// The dots after the caret while the ghost waits for text, faded and
    /// raised in a wave.
    pub ghost_waiting: Hsla,
    /// The tint of the ranges [`Editor::track`] keeps, which are underlined
    /// in the caret's color; an empty one is a bar in that color.
    pub tracked: Hsla,
    /// Size the editor to its text, up to the available width, instead of
    /// filling its parent and scrolling. Scroll events then pass through to
    /// what contains it.
    pub fit_content: bool,
}

impl Default for EditorStyle {
    fn default() -> Self {
        Self {
            // gpui doesn't resolve "SF Mono" on macOS.
            font: font(crate::platform::MONO),
            font_size: px(12.5),
            line_height: px(12.5 * 1.65),
            padding_x: px(24.),
            padding_top: px(20.),
            padding_bottom: px(40.),
            text: rgb(0x2a2a2e).into(),
            read_only_text: None,
            heading: rgb(0x1d1d1f).into(),
            code: rgb(0xa8336a).into(),
            link: rgb(0x0a6cf5).into(),
            muted: rgba(0x3c3c43a8).into(),
            faint: rgba(0x3c3c4373).into(),
            caret: rgb(0x0a6cf5).into(),
            selection: rgb(0xb3d4ff).into(),
            inactive_selection: rgb(0xdcdce0).into(),
            flash: rgba(0xffd60a59).into(),
            marked: rgba(0x0a6cf524).into(),
            ghost_waiting: rgba(0x3c3c4373).into(),
            tracked: rgba(0x0a6cf512).into(),
            fit_content: false,
        }
    }
}

pub struct Editor {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) style: EditorStyle,
    pub(crate) buffer: Buffer,
    pub(crate) read_only: bool,
    pub(crate) scroll_y: Pixels,
    /// Set when the caret moved, so the next layout scrolls it into view.
    pub(crate) autoscroll: bool,
    /// The last painted layout; may be a revision behind the buffer.
    pub(crate) layout: Option<Rc<Layout>>,
    /// Markdown style spans over the buffer text, in byte offsets; empty
    /// when markdown highlighting is off.
    pub(crate) highlights: Vec<(Range<usize>, Styles)>,
    pub(crate) markdown: bool,
    /// A mouse drag selection is in progress.
    selecting: bool,
    /// What the drag selects by, and the unit it started on.
    drag: (Granularity, Range<usize>),
    /// Where the dragging pointer last was, in window coordinates.
    drag_pointer: Point<Pixels>,
    /// Set while a drag is held above or below the visible text, which
    /// keeps scrolling and selecting toward the pointer.
    drag_scrolling: bool,
    /// Ranges selected besides the selection, in document order; see
    /// [`Editor::extra_selections`].
    extra_selections: Vec<Range<usize>>,
    /// What the last events reported, to emit only real changes.
    reported_revision: u64,
    reported_selection: Range<usize>,
    reported_extra_selections: Vec<Range<usize>>,
    /// Whether the blinking caret is in its visible phase.
    pub(crate) caret_on: bool,
    /// Set by each paint of the focused editor; the blink stops when a
    /// period passes without one.
    painted_focused: bool,
    blinking: bool,
    /// Bumped when the caret moves or the text changes, which ends the
    /// running blink so the caret stays solid while the user types.
    blink_epoch: u64,
    /// Where the open context menu was requested, in window coordinates.
    context_menu: Option<Point<Pixels>>,
    menu_renderer: Option<ContextMenuRenderer>,
    pub(crate) flash: Option<Flash>,
    /// Text pointed out until it is cleared or the text changes.
    pub(crate) marked: Option<Range<usize>>,
    /// Set when the mark changed, so the next layout scrolls it into view.
    pub(crate) reveal_marked: bool,
    /// Text the next layout scrolls into view.
    pub(crate) reveal: Option<Range<usize>>,
    pub(crate) ghost: Option<Ghost>,
    /// When the ghost started waiting, for the dots' wave.
    pub(crate) ghost_waiting: Option<Instant>,
    /// Set while text moves from the ghost into the buffer, so that edit
    /// keeps the rest of the ghost.
    accepting_ghost: bool,
}

/// Text pointed out by a tint that fades away.
pub(crate) struct Flash {
    pub(crate) ranges: Vec<Range<usize>>,
    pub(crate) started: Instant,
    /// The buffer revision the ranges are for; any later edit ends the
    /// flash.
    pub(crate) revision: u64,
}

/// How long [`Editor::flash`]'s tint takes to fade away.
pub(crate) const FLASH_DURATION: Duration = Duration::from_millis(900);

/// The opacity of a flash tint that started at `started`, faded by its
/// square so it lingers before it goes; `None` once it has gone.
pub fn flash_alpha(started: Instant) -> Option<f32> {
    let progress = started.elapsed().as_secs_f32() / FLASH_DURATION.as_secs_f32();
    (progress < 1.).then_some(1. - progress * progress)
}

/// How long the caret stays on, and then off, while blinking.
const BLINK_PERIOD: Duration = Duration::from_millis(530);
/// How often a drag held outside the text scrolls it by a row.
const DRAG_SCROLL_PERIOD: Duration = Duration::from_millis(50);

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Editor {
    pub fn new(text: &str, cx: &mut Context<Self>) -> Self {
        let buffer = Buffer::new(text);
        Self {
            focus_handle: cx.focus_handle(),
            style: EditorStyle::default(),
            reported_revision: buffer.revision(),
            highlights: highlight(buffer.text()),
            buffer,
            markdown: true,
            read_only: false,
            scroll_y: Pixels::ZERO,
            autoscroll: false,
            layout: None,
            selecting: false,
            drag: (Granularity::Char, 0..0),
            drag_pointer: Point::default(),
            drag_scrolling: false,
            context_menu: None,
            menu_renderer: None,
            reported_selection: 0..0,
            extra_selections: Vec::new(),
            reported_extra_selections: Vec::new(),
            caret_on: true,
            painted_focused: false,
            blinking: false,
            blink_epoch: 0,
            flash: None,
            marked: None,
            reveal_marked: false,
            reveal: None,
            ghost: None,
            ghost_waiting: None,
            accepting_ghost: false,
        }
    }

    /// Called by each paint of the focused editor: starts the caret
    /// blinking after a period without caret movement or edits.
    pub(crate) fn painted_focused(&mut self, cx: &mut Context<Self>) {
        self.painted_focused = true;
        if self.blinking {
            return;
        }
        self.blinking = true;
        let epoch = self.blink_epoch;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_PERIOD).await;
                let go = this
                    .update(cx, |this, cx| this.blink(epoch, cx))
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        })
        .detach();
    }

    /// Toggles the caret; false ends the blink.
    fn blink(&mut self, epoch: u64, cx: &mut Context<Self>) -> bool {
        if epoch != self.blink_epoch {
            return false;
        }
        if !self.painted_focused {
            self.blinking = false;
            self.caret_on = true;
            return false;
        }
        self.painted_focused = false;
        self.caret_on = !self.caret_on;
        cx.notify();
        true
    }

    /// Shows the caret solid and restarts the blink from the beginning.
    fn reset_blink(&mut self) {
        self.blink_epoch += 1;
        self.blinking = false;
        self.caret_on = true;
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    /// Enables the right-click menu (Cut, Copy, Paste, Select All), drawn by
    /// `renderer`.
    pub fn set_context_menu(&mut self, renderer: ContextMenuRenderer) {
        self.menu_renderer = Some(renderer);
    }

    pub fn set_style(&mut self, style: EditorStyle, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    /// Loads `text` with an empty undo history and the caret at the start.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.set_text(text);
        self.scroll_y = Pixels::ZERO;
        self.after_change(None, false, cx);
    }

    /// The selection as an ordered byte range.
    pub fn selected_range(&self) -> Range<usize> {
        self.buffer.selection().range()
    }

    /// Selects `range`, scrolling it into view.
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.buffer.select_range(range, false);
        self.after_change(None, true, cx);
    }

    /// Ranges selected besides the selection, in document order and not
    /// overlapping each other. Dragging with the secondary modifier (⌘ on
    /// macOS) adds the selection to them and starts a new one; a plain click
    /// or Escape drops them, and so does any edit, which would leave them
    /// pointing at other text.
    pub fn extra_selections(&self) -> &[Range<usize>] {
        &self.extra_selections
    }

    /// Every selected range in document order: the extra selections, less
    /// those the selection overlaps, and the selection unless it's empty.
    pub fn selected_ranges(&self) -> Vec<Range<usize>> {
        all_ranges(&self.extra_selections, self.selected_range())
    }

    /// Replaces the extra selections; empty ranges are dropped.
    pub fn set_extra_selections(&mut self, ranges: Vec<Range<usize>>, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        let mut extra = Vec::new();
        for range in ranges {
            add_range(&mut extra, range.start.min(len)..range.end.min(len));
        }
        self.extra_selections = extra;
        self.after_change(None, false, cx);
    }

    /// Adds the selection to the extra selections and collapses it to its
    /// end, so the next selection adds to them. Nothing happens when it's
    /// empty.
    pub fn add_selection(&mut self, cx: &mut Context<Self>) {
        let range = self.selected_range();
        if range.is_empty() {
            return;
        }
        add_range(&mut self.extra_selections, range.clone());
        self.buffer.select_range(range.end..range.end, false);
        self.after_change(None, false, cx);
    }

    /// Window-space rectangles of the selection, one per visual row, for
    /// anchoring popovers. Empty when nothing is selected or the text has
    /// changed since the last frame; query again after the next render.
    pub fn selection_bounds(&self) -> Vec<Bounds<Pixels>> {
        match &self.layout {
            Some(layout)
                if layout.revision == self.buffer.revision()
                    && !self.buffer.selection().is_empty() =>
            {
                layout.range_rects(self.selected_range(), false)
            }
            _ => Vec::new(),
        }
    }

    /// Replaces `range` with `text` as one undo step, leaving the caret after
    /// it. Applies in read-only mode too.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.buffer.replace_range(range, text);
        self.after_change(None, true, cx);
    }

    /// Replaces `range` with `text` as one undo step, keeping the selection
    /// where the text around it is and the view where it is. Applies in
    /// read-only mode too.
    pub fn replace_range_in_place(
        &mut self,
        range: Range<usize>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.buffer.replace_range_in_place(range, text);
        self.after_change(None, false, cx);
    }

    /// Replaces the whole text as one undo step, keeping the selection where
    /// the text around it is unchanged. Applies in read-only mode too.
    pub fn replace_all(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.replace_all(text);
        self.after_change(None, false, cx);
    }

    /// Undoes the last edit, as the Undo action does with the editor
    /// focused. Returns false when read-only or there's nothing to undo.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        self.history_step(None, cx, Buffer::undo)
    }

    /// Redoes the last undone edit; see [`Editor::undo`].
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        self.history_step(None, cx, Buffer::redo)
    }

    /// Tints `ranges` of the current text and fades the tint away, to point
    /// out text that changed without the user typing it. The next edit
    /// ends it early.
    pub fn flash(&mut self, ranges: Vec<Range<usize>>, cx: &mut Context<Self>) {
        self.flash = (!ranges.is_empty()).then(|| Flash {
            ranges,
            started: Instant::now(),
            revision: self.buffer.revision(),
        });
        cx.notify();
    }

    /// Scrolls `range` into view on the next layout, leaving the selection
    /// where it is.
    pub fn reveal(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.reveal = Some(range);
        cx.notify();
    }

    /// The ranges [`Editor::flash`] points out and when it started, while
    /// the tint lasts and the text is unchanged.
    pub fn flashing(&self) -> Option<(&[Range<usize>], Instant)> {
        self.flash
            .as_ref()
            .filter(|flash| flash.revision == self.buffer.revision())
            .filter(|flash| flash_alpha(flash.started).is_some())
            .map(|flash| (&flash.ranges[..], flash.started))
    }

    /// Tints `range` of the current text, or marks its position when it is
    /// empty, and scrolls it into view; `None` clears the mark. Any edit
    /// clears it too.
    pub fn set_marked(&mut self, range: Option<Range<usize>>, cx: &mut Context<Self>) {
        let range = range.filter(|range| {
            range.end <= self.text().len()
                && self.text().is_char_boundary(range.start)
                && self.text().is_char_boundary(range.end)
        });
        if self.marked == range {
            return;
        }
        self.reveal_marked = range.is_some();
        self.marked = range;
        cx.notify();
    }

    /// Keeps `range` on its text as the text changes, tinted, until
    /// [`Editor::untrack`] or an edit that touches it; see
    /// [`Editor::tracked`].
    pub fn track(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> TrackedId {
        cx.notify();
        self.buffer.track(range)
    }

    /// Where the range `id` is now: edits before it move it. `None` once an
    /// edit has touched it, inside it, across either end or at an empty
    /// range's offset, or new text was loaded.
    pub fn tracked(&self, id: TrackedId) -> Option<Range<usize>> {
        self.buffer.tracked(id)
    }

    pub fn untrack(&mut self, id: TrackedId, cx: &mut Context<Self>) {
        self.buffer.untrack(id);
        cx.notify();
    }

    /// Window-space rectangles of the tracked range `id`, one per visual
    /// row, or a caret's for an empty one; empty when it's gone or the text
    /// has changed since the last layout. Elements after the editor see this
    /// frame's layout in their prepaint.
    pub fn tracked_bounds(&self, id: TrackedId) -> Vec<Bounds<Pixels>> {
        let (Some(layout), Some(range)) = (&self.layout, self.buffer.tracked(id)) else {
            return Vec::new();
        };
        if layout.revision != self.buffer.revision() {
            return Vec::new();
        }
        if range.is_empty() {
            return vec![layout.caret_bounds(range.start, Affinity::Downstream)];
        }
        layout.range_rects(range, false)
    }

    /// The editor's bounds in the window as last laid out; the text sits
    /// inside them, `padding_x` from either side.
    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.layout.as_ref().map(|layout| layout.bounds)
    }

    /// Shows `text` after buffer offset `offset` as faint text that isn't
    /// part of the buffer: undo, [`Editor::text`] and the edit events never
    /// see it. Its line reflows around it. Empty `text` shows a pulsing dot
    /// there instead, for text that is on its way; only the first line of
    /// `text` shows.
    ///
    /// The user accepts it with Tab, or one word with word-right, or by
    /// typing what it says; Escape, any other edit and any caret movement
    /// dismiss it. Each reports an event ([`EditorEvent::GhostAccepted`],
    /// [`EditorEvent::GhostDismissed`]); the ghost methods called by the
    /// host don't. The ghost doesn't count toward a `fit_content` size.
    pub fn set_ghost(&mut self, offset: usize, text: &str, cx: &mut Context<Self>) {
        if offset > self.buffer.len() || !self.text().is_char_boundary(offset) {
            return;
        }
        let text = text.lines().next().unwrap_or_default();
        if text.is_empty() {
            self.ghost_waiting.get_or_insert_with(Instant::now);
        } else {
            self.ghost_waiting = None;
        }
        self.ghost = Some(Ghost {
            offset,
            text: text.to_owned(),
        });
        cx.notify();
    }

    /// The ghost, if one shows.
    pub fn ghost(&self) -> Option<&Ghost> {
        self.ghost.as_ref()
    }

    /// Removes the ghost.
    pub fn clear_ghost(&mut self, cx: &mut Context<Self>) {
        if self.ghost.take().is_some() {
            self.ghost_waiting = None;
            cx.notify();
        }
    }

    /// Moves all of the ghost's text into the buffer as one undo step and
    /// removes it.
    pub fn accept_ghost(&mut self, cx: &mut Context<Self>) {
        if let Some(len) = self.ghost.as_ref().map(|ghost| ghost.text.len()) {
            self.take_ghost(len, None, cx);
            self.clear_ghost(cx);
        }
    }

    /// Moves the ghost's first `len` bytes into the buffer as one undo step
    /// and returns them; with `window`, as the user's input. The rest stays
    /// a ghost after them.
    fn take_ghost(
        &mut self,
        len: usize,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if len == 0 || self.read_only {
            return None;
        }
        let ghost = self.ghost.as_mut()?;
        let taken: String = ghost.text.drain(..len).collect();
        let at = ghost.offset;
        ghost.offset += taken.len();
        if ghost.text.is_empty() {
            self.ghost_waiting = Some(Instant::now());
        }
        self.buffer.replace_range(at..at, &taken);
        self.accepting_ghost = true;
        self.after_change(window, true, cx);
        self.accepting_ghost = false;
        Some(taken)
    }

    /// Removes the ghost as the user's doing, reporting it.
    fn dismiss_ghost(&mut self, cx: &mut Context<Self>) {
        if self.ghost.is_some() {
            self.clear_ghost(cx);
            cx.emit(EditorEvent::GhostDismissed);
        }
    }

    fn accept_ghost_action(
        &mut self,
        _: &AcceptGhost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(len) = self.ghost.as_ref().map(|g| g.text.len()) else {
            return;
        };
        let taken = self.take_ghost(len, Some(window), cx);
        self.clear_ghost(cx);
        if let Some(text) = taken {
            cx.emit(EditorEvent::GhostAccepted(text));
        }
    }

    fn accept_ghost_word(
        &mut self,
        _: &AcceptGhostWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(len) = self.ghost.as_ref().map(|g| next_word_len(&g.text)) else {
            return;
        };
        if let Some(text) = self.take_ghost(len, Some(window), cx) {
            cx.emit(EditorEvent::GhostAccepted(text));
        }
    }

    /// Caret bounds in window coordinates, for anchoring popovers at an
    /// empty selection; `None` when the text changed since the last frame.
    pub fn caret_bounds(&self) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        if layout.revision != self.buffer.revision() {
            return None;
        }
        let selection = self.buffer.selection();
        Some(layout.caret_bounds(selection.head, selection.affinity))
    }

    /// The viewport height, content height and scroll offset as last laid
    /// out, for drawing a scrollbar. `None` before the first frame.
    pub fn scroll_metrics(&self) -> Option<(Pixels, Pixels, Pixels)> {
        let layout = self.layout.as_ref()?;
        Some((
            layout.bounds.size.height,
            layout.content_height,
            self.scroll_y,
        ))
    }

    /// Scrolls to `y` from the top; clamped at the next layout.
    pub fn set_scroll_y(&mut self, y: Pixels, cx: &mut Context<Self>) {
        self.scroll_y = y;
        cx.notify();
    }

    /// A mouse drag selection is in progress; the selection isn't final.
    pub fn is_selecting(&self) -> bool {
        self.selecting
    }

    pub fn is_context_menu_open(&self) -> bool {
        self.context_menu.is_some()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Blocks user edits; selection, copying and the replace methods still
    /// work.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if read_only {
            self.buffer.end_composition();
            self.dismiss_ghost(cx);
        }
        self.read_only = read_only;
        self.after_change(None, false, cx);
    }

    /// Turns markdown highlighting on or off, for text that isn't markdown.
    pub fn set_markdown(&mut self, markdown: bool, cx: &mut Context<Self>) {
        if self.markdown == markdown {
            return;
        }
        self.markdown = markdown;
        self.rehighlight();
        cx.notify();
    }

    fn rehighlight(&mut self) {
        let _timing = crate::timing::span("editor-highlight");
        self.highlights = if self.markdown {
            highlight(self.buffer.text())
        } else {
            highlight(self.buffer.text())
                .into_iter()
                .filter(|(_, style)| {
                    style.contains(Styles::FIELD) || style.contains(Styles::FIELD_KEY)
                })
                .collect()
        };
    }

    /// Reports what changed since the last call and schedules a repaint.
    fn after_change(
        &mut self,
        window: Option<&mut Window>,
        autoscroll: bool,
        cx: &mut Context<Self>,
    ) {
        let revision = self.buffer.revision();
        let edited = revision != self.reported_revision;
        if edited {
            self.rehighlight();
            self.extra_selections.clear();
            self.marked = None;
        }
        let selection = self.selected_range();
        let selection_changed = selection != self.reported_selection;
        if (edited || selection_changed) && !self.accepting_ghost {
            self.dismiss_ghost(cx);
        }
        let extra_changed = self.extra_selections != self.reported_extra_selections;
        self.reported_revision = revision;
        self.reported_selection = selection.clone();
        if extra_changed {
            self.reported_extra_selections = self.extra_selections.clone();
        }
        if edited {
            cx.emit(EditorEvent::Edited);
        }
        if extra_changed {
            cx.emit(EditorEvent::ExtraSelectionsChanged);
        }
        if selection_changed {
            cx.emit(EditorEvent::SelectionChanged(selection));
        }
        if let Some(window) = window {
            if edited {
                window.notify_text_input_changed(&self.focus_handle);
            }
            if selection_changed {
                window.notify_text_selection_changed(&self.focus_handle);
            }
            window.invalidate_character_coordinates();
        }
        if edited || selection_changed {
            self.reset_blink();
        }
        self.autoscroll |= autoscroll;
        cx.notify();
    }

    /// Runs a caret movement, then reports it.
    fn movement(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Buffer),
    ) {
        self.dismiss_ghost(cx);
        f(&mut self.buffer);
        self.after_change(Some(window), true, cx);
    }

    /// Runs a movement that needs the laid-out rows. Before the first paint
    /// there are none, and the movement is skipped. Rows are the buffer's:
    /// a layout that still shows a ghost is laid out again without it.
    fn row_movement(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Buffer, &Layout),
    ) {
        self.dismiss_ghost(cx);
        let Some(mut layout) = self.layout.clone() else {
            return;
        };
        if layout.has_ghost() {
            layout = Rc::new(crate::element::relayout(self, &layout, window));
            self.layout = Some(layout.clone());
        }
        f(&mut self.buffer, &layout);
        self.after_change(Some(window), true, cx);
    }

    /// Runs a user edit unless read-only.
    fn user_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Buffer),
    ) {
        if self.read_only {
            return;
        }
        f(&mut self.buffer);
        self.after_change(Some(window), true, cx);
    }

    /// Undoes or redoes one step unless read-only. Returns whether it did.
    fn history_step(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Buffer) -> bool,
    ) -> bool {
        if self.read_only || !f(&mut self.buffer) {
            return false;
        }
        self.after_change(window, true, cx);
        true
    }

    fn page_rows(&self) -> isize {
        self.layout.as_ref().map_or(1, |layout| {
            ((layout.bounds.size.height / layout.line_height) as isize - 1).max(1)
        })
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        self.after_change(Some(window), false, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.buffer.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if let Some(text) = self.buffer.cut() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.after_change(Some(window), true, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .filter(|text| !text.is_empty())
        {
            self.user_edit(window, cx, |b| b.paste(&text));
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn hit(&self, position: Point<Pixels>) -> Option<(usize, Affinity)> {
        Some(self.layout.as_ref()?.hit(position))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some((offset, affinity)) = self.hit(event.position) else {
            return;
        };
        self.dismiss_ghost(cx);
        let granularity = Granularity::from_click_count(event.click_count);
        // The secondary modifier keeps the selection and adds a new one;
        // a plain press drops the extra ones. Shift extends the selection
        // and leaves them.
        if event.modifiers.secondary(cx) {
            let range = self.selected_range();
            if !range.is_empty() {
                add_range(&mut self.extra_selections, range);
            }
        } else if !event.modifiers.shift {
            self.extra_selections.clear();
        }
        if granularity == Granularity::Char {
            let anchor = if event.modifiers.shift {
                self.buffer.selection().anchor
            } else {
                offset
            };
            self.buffer.set_selection(Selection {
                anchor,
                head: offset,
                affinity,
            });
            self.drag = (granularity, anchor..anchor);
        } else {
            let unit = granularity.unit(self.buffer.text(), offset);
            self.buffer.select_range(unit.clone(), false);
            self.drag = (granularity, unit);
        }
        self.selecting = true;
        self.after_change(Some(window), false, cx);
    }

    /// Opens the context menu. As in AppKit, a right-click outside the
    /// selection first selects the word under the pointer, or places the
    /// caret there when it isn't on a word.
    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu_renderer.is_none() {
            return;
        }
        window.focus(&self.focus_handle, cx);
        if let Some((offset, affinity)) = self.hit(event.position) {
            let selection = self.buffer.selection().range();
            if selection.is_empty() || !(selection.start..=selection.end).contains(&offset) {
                let text = self.buffer.text();
                let word = Granularity::Word.unit(text, offset);
                if is_word(&text[word.clone()]) {
                    self.buffer.select_range(word, false);
                } else {
                    self.buffer.set_selection(Selection {
                        anchor: offset,
                        head: offset,
                        affinity,
                    });
                }
            }
        }
        self.context_menu = Some(event.position);
        cx.stop_propagation();
        self.after_change(Some(window), false, cx);
    }

    fn context_menu_items(&self, cx: &App) -> Vec<ContextMenuItem> {
        let selected = !self.buffer.selection().is_empty();
        let item = |label: &'static str, action: Box<dyn Action>, enabled| ContextMenuItem {
            label: label.into(),
            action: Some(action),
            enabled,
        };
        let mut items = Vec::new();
        if !self.read_only {
            items.push(item("Cut", Box::new(Cut), selected));
        }
        items.push(item("Copy", Box::new(Copy), selected));
        if !self.read_only {
            let can_paste = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| !text.is_empty());
            items.push(item("Paste", Box::new(Paste), can_paste));
        }
        items.push(ContextMenuItem {
            label: SharedString::default(),
            action: None,
            enabled: false,
        });
        items.push(item("Select All", Box::new(SelectAll), true));
        items
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Closes the context menu, or else drops the extra selections.
    fn dismiss_menu(&mut self, _: &DismissMenu, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.is_some() {
            self.close_context_menu(cx);
        } else if !self.extra_selections.is_empty() {
            self.extra_selections.clear();
            self.after_change(Some(window), false, cx);
        } else {
            cx.propagate();
        }
    }

    fn render_context_menu(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = self.context_menu?;
        let renderer = self.menu_renderer.clone()?;
        let panel = renderer(self.context_menu_items(cx), window, cx);
        Some(
            deferred(
                anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .occlude()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.close_context_menu(cx);
                            }))
                            // Items handle the click first, then the menu closes.
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.close_context_menu(cx)),
                            )
                            .child(panel),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Extends a drag selection. Called for every mouse move in the window,
    /// so the drag keeps going when the pointer leaves the editor.
    pub(crate) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        self.drag_pointer = event.position;
        self.extend_drag(window, cx);
        if self.drag_outside() && !self.drag_scrolling {
            self.drag_scrolling = true;
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(DRAG_SCROLL_PERIOD).await;
                    let go = this
                        .update_in(cx, |this, window, cx| {
                            if !this.selecting || !this.drag_outside() {
                                return false;
                            }
                            this.extend_drag(window, cx);
                            true
                        })
                        .unwrap_or(false);
                    if !go {
                        break;
                    }
                }
                this.update(cx, |this, _| this.drag_scrolling = false).ok();
            })
            .detach();
        }
    }

    /// Whether the dragging pointer is above or below the visible text of a
    /// scrolling editor.
    fn drag_outside(&self) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        let y = self.drag_pointer.y;
        !self.style.fit_content && (y < layout.bounds.top() || y > layout.bounds.bottom())
    }

    /// Selects from the drag's origin to the pointer, scrolling the head into
    /// view; held outside the text, each call reaches one row further.
    fn extend_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((head, affinity)) = self.hit(self.drag_pointer) else {
            return;
        };
        let selection = match &self.drag {
            (Granularity::Char, origin) => Selection {
                anchor: origin.start,
                head,
                affinity,
            },
            (granularity, origin) => {
                drag_selection(self.buffer.text(), origin.clone(), head, *granularity)
            }
        };
        self.buffer.set_selection(selection);
        self.after_change(Some(window), true, cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.selecting = false;
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = self.style.line_height;
        self.scroll_y -= event.delta.pixel_delta(line_height).y;
        cx.notify();
    }

    /// Typing `text` over `range` types the ghost's next characters: it
    /// goes at the ghost with nothing selected or composing.
    fn typed_through(&self, range: Option<Range<usize>>, text: &str) -> bool {
        let Some(ghost) = &self.ghost else {
            return false;
        };
        let at = ghost.offset..ghost.offset;
        let range = range.unwrap_or(self.selected_range());
        !text.is_empty()
            && self.buffer.marked().is_none()
            && range == at
            && self.selected_range() == at
            && ghost.text.starts_with(text)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        offset_from_utf16(text, range.start)..offset_from_utf16(text, range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        offset_to_utf16(text, range.start)..offset_to_utf16(text, range.end)
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        adjusted_range.replace(self.range_to_utf16(&range));
        Some(self.buffer.text()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.buffer.selection();
        Some(UTF16Selection {
            range: self.range_to_utf16(&selection.range()),
            reversed: selection.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked()
            .map(|range| self.range_to_utf16(&range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.end_composition();
        self.after_change(Some(window), false, cx);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // macOS delivers typed text here even when input is not accepted.
        if self.read_only {
            return;
        }
        crate::timing::input(cx.entity_id());
        let _timing = crate::timing::span("input-handler");
        let range = range_utf16.map(|r| self.range_from_utf16(&r));
        if self.typed_through(range.clone(), text) {
            if let Some(taken) = self.take_ghost(text.len(), Some(window), cx) {
                cx.emit(EditorEvent::GhostAccepted(taken));
            }
            return;
        }
        self.buffer.insert_in(range, text);
        self.after_change(Some(window), true, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = range_utf16.map(|r| self.range_from_utf16(&r));
        // The selected range is relative to the new text, in UTF-16.
        let selected = new_selected_range_utf16
            .map(|r| offset_from_utf16(new_text, r.start)..offset_from_utf16(new_text, r.end));
        self.buffer.compose(range, new_text, selected);
        self.after_change(Some(window), true, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Native input asks before the next paint, so a layout one revision
        // behind is still the best answer: a composition's start is unchanged.
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        if range.is_empty() {
            return Some(layout.caret_bounds(range.start, Affinity::Downstream));
        }
        layout.range_rects(range, false).into_iter().next()
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        self.layout
            .as_ref()
            .map_or_else(Vec::new, |layout| layout.range_rects(range, false))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let (offset, _) = self.hit(point)?;
        Some(offset_to_utf16(self.buffer.text(), offset))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.range_from_utf16(&range_utf16);
        self.buffer.select_range(range, false);
        self.after_change(Some(window), true, cx);
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(offset_to_utf16(self.buffer.text(), self.buffer.len()))
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.read_only
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let context_menu = self.render_context_menu(window, cx);
        let context = match &self.ghost {
            Some(ghost) if !ghost.text.is_empty() => {
                format!("{KEY_CONTEXT} {GHOST_CONTEXT} {GHOST_TEXT_CONTEXT}")
            }
            Some(_) => format!("{KEY_CONTEXT} {GHOST_CONTEXT}"),
            None => KEY_CONTEXT.to_owned(),
        };
        div()
            .id("editor")
            .size_full()
            .key_context(context.as_str())
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|this, _: &Backspace, window, cx| {
                this.user_edit(window, cx, Buffer::backspace);
            }))
            .on_action(cx.listener(|this, _: &Delete, window, cx| {
                this.user_edit(window, cx, Buffer::delete_forward);
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, window, cx| {
                this.user_edit(window, cx, Buffer::delete_word_left);
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, window, cx| {
                this.user_edit(window, cx, Buffer::delete_word_right);
            }))
            .on_action(cx.listener(|this, _: &Newline, window, cx| {
                this.user_edit(window, cx, |b| b.insert("\n"));
            }))
            .on_action(cx.listener(|this, _: &Tab, window, cx| {
                this.user_edit(window, cx, Buffer::tab);
            }))
            .on_action(cx.listener(|this, _: &Indent, window, cx| {
                this.user_edit(window, cx, Buffer::indent_lines);
            }))
            .on_action(cx.listener(|this, _: &Outdent, window, cx| {
                this.user_edit(window, cx, Buffer::outdent_lines);
            }))
            // With nothing to undo here, the host may undo something else.
            .on_action(cx.listener(|this, _: &Undo, window, cx| {
                if !this.history_step(Some(window), cx, Buffer::undo) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Redo, window, cx| {
                if !this.history_step(Some(window), cx, Buffer::redo) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteToLineStart, window, cx| {
                if !this.read_only {
                    this.row_movement(window, cx, |b, rows| b.delete_to_row_start(rows));
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteToLineEnd, window, cx| {
                if !this.read_only {
                    this.row_movement(window, cx, |b, rows| b.delete_to_row_end(rows));
                }
            }))
            .on_action(cx.listener(|this, _: &Left, window, cx| {
                this.movement(window, cx, |b| b.move_left(false));
            }))
            .on_action(cx.listener(|this, _: &Right, window, cx| {
                this.movement(window, cx, |b| b.move_right(false));
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, window, cx| {
                this.movement(window, cx, |b| b.move_left(true));
            }))
            .on_action(cx.listener(|this, _: &SelectRight, window, cx| {
                this.movement(window, cx, |b| b.move_right(true));
            }))
            .on_action(cx.listener(|this, _: &WordLeft, window, cx| {
                this.movement(window, cx, |b| b.move_word_left(false));
            }))
            .on_action(cx.listener(|this, _: &WordRight, window, cx| {
                this.movement(window, cx, |b| b.move_word_right(false));
            }))
            .on_action(cx.listener(|this, _: &SelectWordLeft, window, cx| {
                this.movement(window, cx, |b| b.move_word_left(true));
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, window, cx| {
                this.movement(window, cx, |b| b.move_word_right(true));
            }))
            .on_action(cx.listener(|this, _: &DocumentStart, window, cx| {
                this.movement(window, cx, |b| b.move_to_start(false));
            }))
            .on_action(cx.listener(|this, _: &DocumentEnd, window, cx| {
                this.movement(window, cx, |b| b.move_to_end(false));
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentStart, window, cx| {
                this.movement(window, cx, |b| b.move_to_start(true));
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentEnd, window, cx| {
                this.movement(window, cx, |b| b.move_to_end(true));
            }))
            .on_action(cx.listener(|this, _: &Up, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_vertically(rows, -1, false));
            }))
            .on_action(cx.listener(|this, _: &Down, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_vertically(rows, 1, false));
            }))
            .on_action(cx.listener(|this, _: &SelectUp, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_vertically(rows, -1, true));
            }))
            .on_action(cx.listener(|this, _: &SelectDown, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_vertically(rows, 1, true));
            }))
            .on_action(cx.listener(|this, _: &LineStart, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_to_row_start(rows, false));
            }))
            .on_action(cx.listener(|this, _: &LineEnd, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_to_row_end(rows, false));
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_to_row_start(rows, true));
            }))
            .on_action(cx.listener(|this, _: &SelectLineEnd, window, cx| {
                this.row_movement(window, cx, |b, rows| b.move_to_row_end(rows, true));
            }))
            .on_action(cx.listener(|this, _: &PageUp, window, cx| {
                let delta = -this.page_rows();
                this.row_movement(window, cx, |b, rows| b.move_vertically(rows, delta, false));
            }))
            .on_action(cx.listener(|this, _: &PageDown, window, cx| {
                let delta = this.page_rows();
                this.row_movement(window, cx, |b, rows| b.move_vertically(rows, delta, false));
            }))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_action(cx.listener(Self::dismiss_menu))
            .on_action(cx.listener(Self::accept_ghost_action))
            .on_action(cx.listener(Self::accept_ghost_word))
            .on_action(cx.listener(|this, _: &DismissGhost, _, cx| this.dismiss_ghost(cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when(!self.style.fit_content, |editor| {
                editor.on_scroll(cx.listener(Self::on_scroll))
            })
            .child(EditorElement {
                editor: cx.entity(),
            })
            .children(context_menu)
    }
}

/// The length of `text`'s first word with the whitespace before it and the
/// punctuation after it, or of its leading punctuation when it starts with
/// some: what accepting one word of a ghost takes.
fn next_word_len(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut len = 0;
    // Something other than whitespace was taken: the next space or word
    // ends the word.
    let mut content = false;
    for segment in text.split_word_bounds() {
        let space = segment.chars().all(char::is_whitespace);
        if content && (space || is_word(segment)) {
            break;
        }
        content |= !space;
        len += segment.len();
    }
    len
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Adds non-empty `range` to `ranges`, replacing the ones it overlaps and
/// keeping them in document order.
fn add_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    ranges.retain(|other| !overlaps(other, &range));
    let at = ranges.partition_point(|other| other.start < range.start);
    ranges.insert(at, range);
}

/// `extra`, less the ranges `selection` overlaps, with `selection` unless
/// it's empty, in document order.
fn all_ranges(extra: &[Range<usize>], selection: Range<usize>) -> Vec<Range<usize>> {
    let mut ranges = extra.to_vec();
    add_range(&mut ranges, selection);
    ranges
}

#[cfg(test)]
mod range_tests {
    use super::*;

    #[test]
    fn added_ranges_stay_ordered_and_replace_overlaps() {
        let mut ranges = Vec::new();
        add_range(&mut ranges, 10..20);
        add_range(&mut ranges, 0..5);
        add_range(&mut ranges, 30..40);
        assert_eq!(ranges, [0..5, 10..20, 30..40]);
        // Touching ranges don't overlap.
        add_range(&mut ranges, 5..10);
        assert_eq!(ranges, [0..5, 5..10, 10..20, 30..40]);
        add_range(&mut ranges, 15..35);
        assert_eq!(ranges, [0..5, 5..10, 15..35]);
        add_range(&mut ranges, 3..3);
        assert_eq!(ranges, [0..5, 5..10, 15..35]);
    }

    #[test]
    fn the_selection_replaces_the_extra_ranges_it_overlaps() {
        let extra = [0..5, 10..20];
        assert_eq!(all_ranges(&extra, 7..7), [0..5, 10..20]);
        assert_eq!(all_ranges(&extra, 6..8), [0..5, 6..8, 10..20]);
        assert_eq!(all_ranges(&extra, 18..30), [0..5, 18..30]);
        assert_eq!(all_ranges(&extra[..1], 6..8), [0..5, 6..8]);
    }
}

#[cfg(test)]
mod keymap_tests {
    use super::*;
    use gpui::Action;
    #[test]
    fn ctrl_keys_are_unique_and_do_not_use_super() {
        let bindings = ctrl_bindings();
        let mut keys = std::collections::HashSet::new();
        for binding in &bindings {
            let key = format!("{:?}", binding.keystrokes());
            assert!(keys.insert(key.clone()), "duplicate binding: {key}");
            assert!(
                !binding
                    .keystrokes()
                    .iter()
                    .any(|key| key.modifiers().platform)
            );
        }
        let select_all = bindings
            .iter()
            .find(|binding| binding.action().name() == SelectAll.name())
            .unwrap();
        assert!(select_all.keystrokes()[0].modifiers().control);
        assert_eq!(select_all.keystrokes()[0].key(), "a");
    }
}

#[cfg(test)]
mod ghost_tests {
    use std::cell::RefCell;

    use gpui::{Entity, TestAppContext, VisualTestContext, point};

    use super::*;

    fn editor<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Editor>, &'a mut VisualTestContext) {
        cx.update(init);
        let (editor, cx) = cx.add_window_view(|_, cx| Editor::new(text, cx));
        cx.update(|window, cx| {
            let focus = editor.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
        });
        cx.run_until_parked();
        (editor, cx)
    }

    fn events(
        editor: &Entity<Editor>,
        cx: &mut VisualTestContext,
    ) -> Rc<RefCell<Vec<EditorEvent>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(editor, move |_, event: &EditorEvent, _| {
                if matches!(
                    event,
                    EditorEvent::GhostAccepted(_) | EditorEvent::GhostDismissed
                ) {
                    events.borrow_mut().push(event.clone());
                }
            })
            .detach();
        });
        events
    }

    fn ghost_at_end(editor: &Entity<Editor>, text: &str, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| {
                let end = editor.text().len();
                editor.select_range(end..end, cx);
                editor.set_ghost(end, text, cx);
            })
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn tab_and_word_right_accept_only_while_a_ghost_has_text(cx: &mut TestAppContext) {
        let (editor, cx) = editor("The fox", cx);
        let events = events(&editor, cx);
        ghost_at_end(&editor, " jumps over, the dog.", cx);
        cx.simulate_keystrokes(by_platform!("alt-right", "ctrl-right"));
        cx.simulate_keystrokes(by_platform!("alt-right", "ctrl-right"));
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.text(), "The fox jumps over,");
            assert_eq!(editor.ghost().unwrap().text, " the dog.");
            assert_eq!(editor.ghost().unwrap().offset, editor.text().len());
        });
        cx.simulate_keystrokes("tab");
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.text(), "The fox jumps over, the dog.");
            assert!(editor.ghost().is_none());
        });
        assert_eq!(
            *events.borrow(),
            [
                EditorEvent::GhostAccepted(" jumps".into()),
                EditorEvent::GhostAccepted(" over,".into()),
                EditorEvent::GhostAccepted(" the dog.".into()),
            ]
        );
        // Each accept is its own undo step.
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| {
                assert!(editor.undo(cx));
                assert_eq!(editor.text(), "The fox jumps over,");
                assert!(editor.undo(cx));
                assert!(editor.undo(cx));
                assert_eq!(editor.text(), "The fox");
            })
        });

        // Without a ghost, the keys do what they always do.
        cx.simulate_keystrokes("tab");
        editor.read_with(cx, |editor, _| assert_ne!(editor.text(), "The fox"));
        cx.update(|_, cx| editor.update(cx, |editor, cx| editor.select_range(0..0, cx)));
        cx.simulate_keystrokes(by_platform!("alt-right", "ctrl-right"));
        editor.read_with(cx, |editor, _| assert_eq!(editor.selected_range(), 3..3));
    }

    #[gpui::test]
    fn accepting_does_not_merge_with_typing(cx: &mut TestAppContext) {
        let (editor, cx) = editor("", cx);
        cx.simulate_input("fox");
        ghost_at_end(&editor, " jumps", cx);
        cx.simulate_keystrokes("tab");
        cx.update(|_, cx| {
            editor.update(cx, |editor, cx| {
                assert_eq!(editor.text(), "fox jumps");
                assert!(editor.undo(cx));
                assert_eq!(editor.text(), "fox");
            })
        });
    }

    #[gpui::test]
    fn typing_the_ghost_keeps_it_and_anything_else_dismisses_it(cx: &mut TestAppContext) {
        let (editor, cx) = editor("The fox", cx);
        let events = events(&editor, cx);
        ghost_at_end(&editor, " jumps", cx);
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, " j", window, cx);
                assert_eq!(editor.text(), "The fox j");
                assert_eq!(editor.ghost().unwrap().text, "umps");
                editor.replace_text_in_range(None, "x", window, cx);
                assert_eq!(editor.text(), "The fox jx");
                assert!(editor.ghost().is_none());
            })
        });
        cx.run_until_parked();
        assert_eq!(
            *events.borrow(),
            [
                EditorEvent::GhostAccepted(" j".into()),
                EditorEvent::GhostDismissed
            ]
        );

        // Escape, and moving the caret, dismiss it too; host calls report
        // nothing.
        events.borrow_mut().clear();
        ghost_at_end(&editor, " more", cx);
        cx.simulate_keystrokes("escape");
        ghost_at_end(&editor, " more", cx);
        cx.simulate_keystrokes("left");
        ghost_at_end(&editor, "", cx);
        // A waiting ghost takes Escape but not Tab.
        cx.simulate_keystrokes("escape");
        ghost_at_end(&editor, " more", cx);
        cx.update(|_, cx| editor.update(cx, |editor, cx| editor.clear_ghost(cx)));
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec![EditorEvent::GhostDismissed; 3]);
    }

    #[gpui::test]
    fn a_waiting_ghost_makes_room_before_the_rest_of_its_line(cx: &mut TestAppContext) {
        let (editor, cx) = editor("ab cd\nef", cx);
        let x = |offset, cx: &mut VisualTestContext| {
            editor.read_with(cx, |editor, _| {
                let layout = editor.layout.clone().unwrap();
                layout.caret_bounds(offset, Affinity::Downstream).origin.x
            })
        };
        let wait = |offset, cx: &mut VisualTestContext| {
            cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_ghost(offset, "", cx)));
            cx.run_until_parked();
        };
        cx.run_until_parked();
        let (c, e) = (x(3, cx), x(6, cx));
        // Mid-line, the text after the caret moves right.
        wait(2, cx);
        assert!(x(3, cx) > c);
        // At the end of a line, nothing moves.
        cx.update(|_, cx| editor.update(cx, |editor, cx| editor.clear_ghost(cx)));
        wait(5, cx);
        assert_eq!(x(3, cx), c);
        assert_eq!(x(6, cx), e);
        editor.read_with(cx, |editor, _| {
            assert!(!editor.layout.clone().unwrap().has_ghost())
        });
    }

    #[gpui::test]
    fn the_ghost_is_laid_out_without_moving_buffer_offsets(cx: &mut TestAppContext) {
        let (editor, cx) = editor("ab\ncd", cx);
        let caret = |offset, cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                editor.update(cx, |editor, cx| editor.select_range(offset..offset, cx))
            });
            cx.run_until_parked();
            editor.read_with(cx, |editor, _| editor.caret_bounds().unwrap())
        };
        let before = caret(2, cx);
        cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_ghost(1, "XYZ", cx)));
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| {
            let layout = editor.layout.clone().unwrap();
            assert!(layout.has_ghost());
            // The caret at the ghost sits before it; text after it moves
            // right by its width, and a hit inside it lands at it.
            let at = layout.caret_bounds(1, Affinity::Downstream);
            let after = layout.caret_bounds(2, Affinity::Downstream);
            assert!(after.origin.x > before.origin.x);
            assert_eq!(layout.hit(at.origin + point(px(1.), px(1.))).0, 1);
            assert_eq!(layout.hit(after.origin + point(px(0.), px(1.))).0, 2);
            // Ranges leave the ghost out.
            let rects = layout.range_rects(0..2, false);
            assert_eq!(rects.len(), 2);
            assert_eq!(rects[0].right(), at.origin.x);
            assert_eq!(rects[1].right(), after.origin.x);
            assert!(rects[1].left() > at.origin.x);
        });
        // Dismissed but not yet drawn again: row movement uses the buffer's
        // rows, not the ghost's.
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.select_range(1..1, cx);
                editor.set_ghost(1, "XYZ", cx);
            });
            window.refresh();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.row_movement(window, cx, |b, rows| b.move_to_row_end(rows, false));
                assert_eq!(editor.selected_range(), 2..2);
            })
        });
    }

    #[test]
    fn a_word_takes_its_space_and_punctuation() {
        assert_eq!(next_word_len(" jumps over"), 6);
        assert_eq!(next_word_len("jumps"), 5);
        assert_eq!(next_word_len(" over, the"), 6);
        assert_eq!(next_word_len(", the"), 1);
        assert_eq!(next_word_len("  it's done."), 6);
        assert_eq!(next_word_len(" done."), 6);
        assert_eq!(next_word_len(""), 0);
    }
}
