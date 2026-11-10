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
    ]
);

const KEY_CONTEXT: &str = "Editor";

/// Binds the platform text-editing shortcuts. Call once at startup.
pub fn init(cx: &mut App) {
    cx.bind_keys(bindings());
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
        KeyBinding::new("cmd-z", Undo, ctx),
        KeyBinding::new("cmd-shift-z", Redo, ctx),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, ctx),
        KeyBinding::new("escape", DismissMenu, ctx),
    ]
}

#[cfg(target_os = "linux")]
fn bindings() -> Vec<KeyBinding> {
    linux_bindings()
}

#[cfg(any(test, target_os = "linux"))]
fn linux_bindings() -> Vec<KeyBinding> {
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
        KeyBinding::new("ctrl-z", Undo, ctx),
        KeyBinding::new("ctrl-shift-z", Redo, ctx),
        KeyBinding::new("ctrl-y", Redo, ctx),
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
    pub read_only_text: Hsla,
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
            read_only_text: rgb(0x8a8a94).into(),
            heading: rgb(0x1d1d1f).into(),
            code: rgb(0xa8336a).into(),
            link: rgb(0x0a6cf5).into(),
            muted: rgba(0x3c3c43a8).into(),
            faint: rgba(0x3c3c4373).into(),
            caret: rgb(0x0a6cf5).into(),
            selection: rgb(0xb3d4ff).into(),
            inactive_selection: rgb(0xdcdce0).into(),
            flash: rgba(0xffd60a59).into(),
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
    /// What the last events reported, to emit only real changes.
    reported_revision: u64,
    reported_selection: Range<usize>,
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
            caret_on: true,
            painted_focused: false,
            blinking: false,
            blink_epoch: 0,
            flash: None,
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

    /// Replaces the whole text as one undo step, keeping the selection where
    /// the text around it is unchanged. Applies in read-only mode too.
    pub fn replace_all(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.replace_all(text);
        self.after_change(None, false, cx);
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
        }
        let selection = self.selected_range();
        let selection_changed = selection != self.reported_selection;
        self.reported_revision = revision;
        self.reported_selection = selection.clone();
        if edited {
            cx.emit(EditorEvent::Edited);
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
        f(&mut self.buffer);
        self.after_change(Some(window), true, cx);
    }

    /// Runs a movement that needs the laid-out rows. Before the first paint
    /// there are none, and the movement is skipped.
    fn row_movement(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Buffer, &Layout),
    ) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
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
        let granularity = Granularity::from_click_count(event.click_count);
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

    fn dismiss_menu(&mut self, _: &DismissMenu, _: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.is_some() {
            self.close_context_menu(cx);
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
        div()
            .id("editor")
            .size_full()
            .key_context(KEY_CONTEXT)
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
            .on_action(cx.listener(|this, _: &Undo, window, cx| {
                this.user_edit(window, cx, |b| {
                    b.undo();
                });
            }))
            .on_action(cx.listener(|this, _: &Redo, window, cx| {
                this.user_edit(window, cx, |b| {
                    b.redo();
                });
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

#[cfg(test)]
mod keymap_tests {
    use super::*;
    use gpui::Action;
    #[test]
    fn linux_keys_are_unique_and_do_not_use_super() {
        let bindings = linux_bindings();
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
