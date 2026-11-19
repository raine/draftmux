//! Lays out, paints and hit-tests the editor's soft-wrapped text.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, DispatchPhase, ElementId, ElementInputHandler,
    Entity, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, LayoutId,
    MouseMoveEvent, Pixels, Point, SharedString, Style, TextAlign, TextRun, UnderlineStyle, Window,
    WrappedLine, fill, point, prelude::*, px, relative, size,
};

use crate::buffer::Affinity;
use crate::editor::{Editor, EditorStyle, flash_alpha};
use crate::highlight::Styles;
use crate::movement::RowGeometry;

/// Inputs that can change shaped text or its row geometry. Selection and scroll
/// are intentionally absent: they only reposition or decorate that geometry.
#[derive(Clone, PartialEq)]
struct LayoutKey {
    revision: u64,
    wrap_width: Pixels,
    style: EditorStyle,
    marked: Option<Range<usize>>,
    read_only: bool,
    markdown: bool,
    /// The ghost's buffer offset and text: while waiting before more of
    /// the line, [`WAITING_GAP`] to keep its dots clear of that text.
    ghost: Option<(usize, String)>,
}

impl LayoutKey {
    fn new(editor: &Editor, wrap_width: Pixels) -> Self {
        Self {
            revision: editor.buffer.revision(),
            wrap_width,
            style: editor.style.clone(),
            marked: editor.buffer.marked(),
            read_only: editor.read_only,
            markdown: editor.markdown,
            ghost: editor.ghost.as_ref().and_then(|ghost| {
                if !ghost.text.is_empty() {
                    return Some((ghost.offset, ghost.text.clone()));
                }
                let rest = &editor.buffer.text()[ghost.offset..];
                (!rest.is_empty() && !rest.starts_with('\n'))
                    .then(|| (ghost.offset, WAITING_GAP.to_owned()))
            }),
        }
    }
}

/// The text as last laid out, in window coordinates.
///
/// A ghost (see [`Editor::set_ghost`]) is shaped into its line, so rows and
/// lines are in display offsets: buffer offsets, with those after the ghost
/// shifted by its length. The public methods take and return buffer
/// offsets. [`RowGeometry`] works in display offsets and is only used on a
/// layout without a ghost, where the two are the same.
#[derive(Clone)]
pub(crate) struct Layout {
    key: LayoutKey,
    /// Buffer revision the layout was built from.
    pub revision: u64,
    pub bounds: Bounds<Pixels>,
    /// Window position of the text's top-left corner, after scrolling.
    pub origin: Point<Pixels>,
    pub line_height: Pixels,
    pub wrap_width: Pixels,
    /// Height of the text plus padding, before scrolling.
    pub content_height: Pixels,
    lines: Rc<Vec<Line>>,
    rows: Rc<Vec<Row>>,
    /// The ghost's buffer offset and byte length.
    ghost: Option<(usize, usize)>,
}

struct Line {
    start: usize,
    /// Top of the line relative to `origin`.
    y: Pixels,
    shaped: WrappedLine,
}

struct Row {
    range: Range<usize>,
    line: usize,
    /// Top of the row relative to `origin`.
    y: Pixels,
}

impl Layout {
    fn len(&self) -> usize {
        self.rows.last().map_or(0, |row| row.range.end)
    }

    pub fn has_ghost(&self) -> bool {
        self.ghost.is_some()
    }

    /// The display offset of buffer offset `offset`; at the ghost, before
    /// it unless `after_ghost`.
    fn to_display(&self, offset: usize, after_ghost: bool) -> usize {
        match self.ghost {
            Some((at, len)) if offset > at || (offset == at && after_ghost) => offset + len,
            _ => offset,
        }
    }

    /// The buffer offset of display offset `offset`; inside the ghost, the
    /// ghost's.
    fn to_buffer(&self, offset: usize) -> usize {
        match self.ghost {
            Some((at, len)) if offset > at => offset.saturating_sub(len).max(at),
            _ => offset,
        }
    }

    /// x of `offset` on `row`, relative to `origin`.
    fn x_on_row(&self, row: &Row, offset: usize) -> Pixels {
        let line = &self.lines[row.line];
        let layout = &line.shaped.unwrapped_layout;
        layout.x_for_index(offset - line.start) - layout.x_for_index(row.range.start - line.start)
    }

    /// The caret position closest to a window position.
    pub fn hit(&self, position: Point<Pixels>) -> (usize, Affinity) {
        let local = position - self.origin;
        let row = self
            .rows
            .iter()
            .rposition(|row| row.y <= local.y)
            .unwrap_or(0);
        if local.y < Pixels::ZERO {
            return (0, Affinity::Downstream);
        }
        if local.y >= self.rows[row].y + self.line_height && row == self.rows.len() - 1 {
            return (self.to_buffer(self.len()), Affinity::Downstream);
        }
        let offset = self.offset_at(row, local.x.as_f32());
        let affinity = self.affinity_on_row(row, offset);
        let buffer = self.to_buffer(offset);
        if buffer != offset || self.ghost.is_some_and(|(at, _)| at == offset) {
            // At or inside the ghost: before it, where its caret goes.
            return (buffer, Affinity::Downstream);
        }
        (buffer, affinity)
    }

    pub fn caret_bounds(&self, offset: usize, affinity: Affinity) -> Bounds<Pixels> {
        let offset = self.to_display(offset, false).min(self.len());
        let row = &self.rows[self.row_for(offset, affinity)];
        let x = self.x_on_row(row, offset);
        Bounds::new(
            self.origin + point(x, row.y),
            size(px(0.), self.line_height),
        )
    }

    /// One rectangle per visual row covered by `range`. With `to_edge`, rows
    /// the range continues past extend to the wrap width, as selections do.
    pub fn range_rects(&self, range: Range<usize>, to_edge: bool) -> Vec<Bounds<Pixels>> {
        match self.ghost {
            Some((at, _)) if range.start < at && at < range.end => {
                let mut rects =
                    self.display_rects(self.to_display(range.start, false)..at, to_edge);
                rects.extend(self.display_rects(
                    self.to_display(at, true)..self.to_display(range.end, false),
                    to_edge,
                ));
                rects
            }
            _ if range.is_empty() => {
                let at = self.to_display(range.start, false);
                self.display_rects(at..at, to_edge)
            }
            _ => self.display_rects(
                self.to_display(range.start, true)..self.to_display(range.end, false),
                to_edge,
            ),
        }
    }

    fn display_rects(&self, range: Range<usize>, to_edge: bool) -> Vec<Bounds<Pixels>> {
        let range = range.start.min(self.len())..range.end.min(self.len());
        let mut rects = Vec::new();
        for row in self.rows.iter() {
            if row.range.end < range.start || row.range.start > range.end {
                continue;
            }
            if row.range.end == range.start && !(range.is_empty() && row.range.is_empty()) {
                continue;
            }
            if row.range.start == range.end && !range.is_empty() {
                continue;
            }
            let start = range.start.max(row.range.start);
            let end = range.end.min(row.range.end);
            let x0 = self.x_on_row(row, start);
            let mut x1 = self.x_on_row(row, end);
            if to_edge && range.end > row.range.end {
                x1 = x1.max(self.wrap_width);
            }
            rects.push(Bounds::from_corners(
                self.origin + point(x0, row.y),
                self.origin + point(x1, row.y + self.line_height),
            ));
        }
        rects
    }
}

impl RowGeometry for Layout {
    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn row_range(&self, row: usize) -> Range<usize> {
        self.rows[row].range.clone()
    }

    fn position(&self, offset: usize, affinity: Affinity) -> (usize, f32) {
        let offset = offset.min(self.len());
        let ix = self.row_for(offset, affinity);
        let x = self.x_on_row(&self.rows[ix], offset);
        (ix, x.as_f32())
    }

    fn offset_at(&self, row: usize, x: f32) -> usize {
        let row = &self.rows[row];
        let line = &self.lines[row.line];
        let y = row.y - line.y + self.line_height / 2.;
        let index = line
            .shaped
            .closest_index_for_position(point(px(x), y), self.line_height)
            .unwrap_or_else(|ix| ix);
        (line.start + index).clamp(row.range.start, row.range.end)
    }
}

pub(crate) struct EditorElement {
    pub editor: Entity<Editor>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Rc<Layout>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        if self.editor.read(cx).style.fit_content {
            let editor = self.editor.clone();
            let layout = window.request_measured_layout(
                Style::default(),
                move |known, available, window, cx| {
                    let width = known.width.or(match available.width {
                        AvailableSpace::Definite(width) => Some(width),
                        _ => None,
                    });
                    fit_size(editor.read(cx), width, window)
                },
            );
            return (layout, ());
        }
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Rc<Layout> {
        let _timing = crate::timing::span("editor-layout");
        let editor = self.editor.read(cx);
        let style = editor.style.clone();
        let wrap_width = (bounds.size.width - style.padding_x * 2.).max(px(1.));

        let key = LayoutKey::new(editor, wrap_width);
        let mut layout =
            if let Some(previous) = editor.layout.as_ref().filter(|layout| layout.key == key) {
                (**previous).clone()
            } else {
                shape(editor, key, bounds, window)
            };
        // Scroll and window position can change without reshaping the text.
        layout.bounds = bounds;
        layout.origin = Point::default();
        let max_scroll = (layout.content_height - bounds.size.height).max(Pixels::ZERO);
        let selection = editor.buffer.selection();
        let mut scroll_y = editor.scroll_y;
        let len = editor.buffer.len();
        let shown = editor
            .reveal
            .clone()
            .or_else(|| editor.marked.clone().filter(|_| editor.reveal_marked))
            .filter(|range| range.end <= len);
        if let Some(marked) = shown {
            let start = layout.caret_bounds(marked.start, Affinity::Downstream);
            let end = layout.caret_bounds(marked.end, Affinity::Upstream);
            let top = start.origin.y + style.padding_top;
            let bottom = end.origin.y + style.padding_top + style.line_height;
            let margin = style.line_height * 2.;
            if top - margin < scroll_y || bottom - top + margin * 2. > bounds.size.height {
                scroll_y = top - margin;
            } else if bottom + margin > scroll_y + bounds.size.height {
                scroll_y = bottom + margin - bounds.size.height;
            }
        } else if editor.autoscroll {
            let caret = layout.caret_bounds(selection.head, selection.affinity);
            let top = caret.origin.y + style.padding_top;
            let bottom = top + style.line_height;
            let margin = style.line_height;
            if top - margin < scroll_y {
                scroll_y = top - margin;
            } else if bottom + margin > scroll_y + bounds.size.height {
                scroll_y = bottom + margin - bounds.size.height;
            }
        }
        scroll_y = scroll_y.clamp(Pixels::ZERO, max_scroll);
        layout.origin = bounds.origin + point(style.padding_x, style.padding_top - scroll_y);

        let layout = Rc::new(layout);
        self.editor.update(cx, |editor, _| {
            editor.scroll_y = scroll_y;
            editor.autoscroll = false;
            editor.reveal_marked = false;
            editor.reveal = None;
            editor.layout = Some(layout.clone());
        });
        layout
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        layout: &mut Rc<Layout>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let editor = self.editor.read(cx);
        let style = editor.style.clone();
        let focus_handle = editor.focus_handle.clone();
        let selection = editor.buffer.selection();
        let selected_ranges = editor.selected_ranges();
        // As in AppKit, an inactive window shows no caret and a gray selection.
        let focused = focus_handle.is_focused(window) && window.is_window_active();
        let caret_on = editor.caret_on;
        let read_only = editor.read_only;
        let marked = editor.marked.clone();
        let waiting = editor
            .ghost
            .as_ref()
            .zip(editor.ghost_waiting)
            .map(|(ghost, since)| (ghost.offset, since));
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        let flash = editor
            .flash
            .as_ref()
            .filter(|flash| flash.revision == layout.revision)
            .and_then(|flash| {
                flash_alpha(flash.started).map(|alpha| (flash.ranges.clone(), alpha))
            });
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some((ranges, alpha)) = flash {
                let color = style.flash.opacity(alpha);
                for range in ranges {
                    for rect in layout.range_rects(range, false) {
                        if rect.bottom() >= bounds.top() && rect.top() <= bounds.bottom() {
                            window.paint_quad(fill(rect, color));
                        }
                    }
                }
                window.request_animation_frame();
            }
            if let Some(marked) = marked {
                if marked.is_empty() {
                    let at = layout.caret_bounds(marked.start, Affinity::Downstream);
                    window.paint_quad(fill(
                        Bounds::new(
                            at.origin - point(px(1.), px(0.)),
                            size(px(2.), at.size.height),
                        ),
                        style.marked.opacity(1.),
                    ));
                } else {
                    for rect in layout.range_rects(marked, false) {
                        window.paint_quad(fill(rect, style.marked));
                    }
                }
            }
            let color = if focused {
                style.selection
            } else {
                style.inactive_selection
            };
            // The extra selections, less those the selection replaces, then
            // the selection.
            for range in selected_ranges {
                for rect in layout.range_rects(range, true) {
                    if rect.bottom() >= bounds.top() && rect.top() <= bounds.bottom() {
                        window.paint_quad(fill(rect, color));
                    }
                }
            }

            for line in layout.lines.iter() {
                let top = layout.origin.y + line.y;
                let height = layout.line_height * (line.shaped.wrap_boundaries().len() + 1);
                if top + height < bounds.top() || top > bounds.bottom() {
                    continue;
                }
                line.shaped
                    .paint(
                        point(layout.origin.x, top),
                        layout.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
            }

            if focused && selection.is_empty() && caret_on && !read_only {
                let caret = layout.caret_bounds(selection.head, selection.affinity);
                window.paint_quad(fill(
                    Bounds::new(caret.origin, size(px(2.), caret.size.height)),
                    style.caret,
                ));
            }
            if let Some((offset, since)) = waiting {
                let caret = layout.caret_bounds(offset, Affinity::Downstream);
                // In ems, so they fit WAITING_GAP at any zoom.
                let em = style.font_size;
                let diameter = em * 0.32;
                let top = caret.origin.y + (caret.size.height - diameter) / 2.;
                for dot in 0..3 {
                    let lift = wave(since, dot);
                    let x = caret.origin.x + em * 0.48 + (diameter + em * 0.24) * dot as f32;
                    let origin = point(x, top - em * 0.16 * lift);
                    window.paint_quad(
                        fill(
                            Bounds::new(origin, size(diameter, diameter)),
                            style.ghost_waiting.opacity(0.3 + 0.7 * lift),
                        )
                        .corner_radii(diameter / 2.),
                    );
                }
                window.request_animation_frame();
            }
        });
        crate::timing::painted(self.editor.entity_id());
        if focused {
            self.editor
                .update(cx, |editor, cx| editor.painted_focused(cx));
        }
        if self.editor.read(cx).is_selecting() {
            let editor = self.editor.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    editor.update(cx, |editor, cx| editor.on_mouse_move(event, window, cx));
                }
            });
        }
    }
}

/// `editor` laid out again as it is now, at `previous`'s width and position.
pub(crate) fn relayout(editor: &Editor, previous: &Layout, window: &mut Window) -> Layout {
    let key = LayoutKey::new(editor, previous.wrap_width);
    let mut layout = shape(editor, key, previous.bounds, window);
    layout.origin = previous.origin;
    layout
}

/// Shapes `editor`'s text, with its ghost, into lines and rows.
fn shape(editor: &Editor, key: LayoutKey, bounds: Bounds<Pixels>, window: &mut Window) -> Layout {
    let style = &key.style.clone();
    let wrap_width = key.wrap_width;
    let revision = key.revision;
    let buffer = editor.buffer.text();
    let mut runs = text_runs(
        buffer,
        &editor.highlights,
        editor.buffer.marked(),
        editor.read_only,
        style,
    );
    let text = match &key.ghost {
        Some((at, ghost)) => {
            runs = splice_run(runs, *at, ghost.len(), style);
            SharedString::from(format!("{}{ghost}{}", &buffer[..*at], &buffer[*at..]))
        }
        None => SharedString::from(buffer.to_string()),
    };
    let ghost = key.ghost.as_ref().map(|(at, ghost)| (*at, ghost.len()));
    let shaped = window
        .text_system()
        .shape_text(text, style.font_size, &runs, Some(wrap_width), None)
        .unwrap_or_default();

    let mut lines = Vec::with_capacity(shaped.len());
    let mut rows = Vec::new();
    let mut start = 0;
    let mut y = Pixels::ZERO;
    for (line_ix, shaped) in shaped.into_iter().enumerate() {
        let layout = &shaped.unwrapped_layout;
        let mut row_start = start;
        let mut row_y = y;
        for boundary in shaped.wrap_boundaries() {
            let end = start + layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index;
            rows.push(Row {
                range: row_start..end,
                line: line_ix,
                y: row_y,
            });
            row_start = end;
            row_y += style.line_height;
        }
        rows.push(Row {
            range: row_start..start + shaped.len(),
            line: line_ix,
            y: row_y,
        });
        let len = shaped.len();
        lines.push(Line { start, y, shaped });
        start += len + 1;
        y = row_y + style.line_height;
    }

    let content_height = y + style.padding_top + style.padding_bottom;
    Layout {
        key,
        revision,
        bounds,
        origin: Point::default(),
        line_height: style.line_height,
        wrap_width,
        content_height,
        lines: Rc::new(lines),
        rows: Rc::new(rows),
        ghost,
    }
}

/// `runs` split at `at` around a faint run of `len` bytes for the ghost.
fn splice_run(runs: Vec<TextRun>, at: usize, len: usize, style: &EditorStyle) -> Vec<TextRun> {
    let ghost = TextRun {
        len,
        font: style.font.clone(),
        color: style.faint,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut spliced = Vec::with_capacity(runs.len() + 2);
    let mut start = 0;
    let mut placed = false;
    for run in runs {
        let end = start + run.len;
        if !placed && at < end {
            if at > start {
                spliced.push(TextRun {
                    len: at - start,
                    ..run.clone()
                });
            }
            spliced.push(ghost.clone());
            spliced.push(TextRun {
                len: end - at,
                ..run
            });
            placed = true;
        } else {
            spliced.push(run);
        }
        start = end;
    }
    if !placed {
        spliced.push(ghost);
    }
    spliced
}

/// The space a waiting ghost takes before more of its line: about 2.4 ems
/// in the draft's monospaced font, around 1.9 ems of dots.
const WAITING_GAP: &str = "    ";

/// How long one wave across the waiting ghost's dots takes, in seconds.
const WAVE: f32 = 1.2;
/// How far apart the dots' waves start, in seconds.
const WAVE_STAGGER: f32 = 0.16;
/// The share of each wave a dot spends rising and falling, resting after.
const WAVE_RISE: f32 = 0.6;

/// How far waiting dot `dot` has risen, from 0 at rest to 1 at the top.
fn wave(since: std::time::Instant, dot: usize) -> f32 {
    let t = (since.elapsed().as_secs_f32() - WAVE_STAGGER * dot as f32).rem_euclid(WAVE) / WAVE;
    if t < WAVE_RISE {
        (t / WAVE_RISE * std::f32::consts::PI).sin()
    } else {
        0.
    }
}

/// The size of `editor`'s text and padding: as wide as its longest line,
/// up to `max_width`, and as tall as its lines wrapped to that width.
fn fit_size(editor: &Editor, max_width: Option<Pixels>, window: &mut Window) -> gpui::Size<Pixels> {
    let style = &editor.style;
    let text = SharedString::from(editor.buffer.text().to_string());
    let runs = text_runs(&text, &editor.highlights, None, editor.read_only, style);
    let pad_x = style.padding_x * 2.;
    let shape = |wrap: Option<Pixels>, window: &mut Window| {
        window
            .text_system()
            .shape_text(text.clone(), style.font_size, &runs, wrap, None)
            .unwrap_or_default()
    };
    let longest = shape(None, window)
        .iter()
        .map(|line| line.unwrapped_layout.width)
        .fold(Pixels::ZERO, Pixels::max);
    let width = match max_width {
        // Rounded up so the measured width doesn't wrap the longest line.
        Some(max) => (longest.ceil() + pad_x).min(max),
        None => longest.ceil() + pad_x,
    };
    let rows: usize = shape(Some((width - pad_x).max(px(1.))), window)
        .iter()
        .map(|line| line.wrap_boundaries().len() + 1)
        .sum();
    size(
        width,
        style.line_height * rows.max(1) as f32 + style.padding_top + style.padding_bottom,
    )
}

/// Text runs covering all of `text`, styled by the markdown highlights, with
/// the IME composition underlined.
fn text_runs(
    text: &str,
    highlights: &[(Range<usize>, Styles)],
    marked: Option<Range<usize>>,
    read_only: bool,
    style: &EditorStyle,
) -> Vec<TextRun> {
    let mut cuts = vec![0, text.len()];
    for (range, _) in highlights {
        cuts.extend([range.start, range.end]);
    }
    if let Some(marked) = &marked {
        cuts.extend([marked.start, marked.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans = highlights.iter().peekable();
    cuts.windows(2)
        .map(|w| {
            let (start, end) = (w[0], w[1]);
            while spans.peek().is_some_and(|(r, _)| r.end <= start) {
                spans.next();
            }
            let styles = spans
                .peek()
                .filter(|(r, _)| r.start <= start)
                .map_or(Styles::default(), |(_, s)| *s);
            let underline = marked
                .as_ref()
                .filter(|m| m.start <= start && end <= m.end)
                .map(|_| UnderlineStyle {
                    color: None,
                    thickness: px(1.),
                    wavy: false,
                });
            TextRun {
                len: end - start,
                font: font_for(styles, style),
                color: color_for(styles, read_only, style),
                background_color: None,
                underline,
                strikethrough: None,
            }
        })
        .collect()
}

fn font_for(styles: Styles, style: &EditorStyle) -> Font {
    let mut font = style.font.clone();
    if styles.contains(Styles::HEADING) || styles.contains(Styles::STRONG) {
        font.weight = FontWeight::BOLD;
    }
    if styles.contains(Styles::EMPHASIS) {
        font.style = FontStyle::Italic;
    }
    font
}

/// How opaque text is while read-only, when the style has no color for it.
const READ_ONLY_OPACITY: f32 = 0.7;

fn color_for(styles: Styles, read_only: bool, style: &EditorStyle) -> Hsla {
    if read_only && let Some(color) = style.read_only_text {
        return color;
    }
    let color = if styles.contains(Styles::FIELD_KEY) {
        style.muted
    } else if styles.contains(Styles::FIELD) {
        style.faint
    } else if styles.contains(Styles::CODE) {
        style.code
    } else if styles.contains(Styles::LINK) {
        style.link
    } else if styles.contains(Styles::HEADING) {
        style.heading
    } else if styles.contains(Styles::STRIKETHROUGH) {
        style.muted
    } else {
        style.text
    };
    if read_only {
        color.opacity(READ_ONLY_OPACITY)
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> LayoutKey {
        LayoutKey {
            revision: 1,
            wrap_width: px(400.),
            style: EditorStyle::default(),
            marked: None,
            read_only: false,
            markdown: true,
            ghost: None,
        }
    }

    #[test]
    fn invalidates_geometry_for_every_shaping_input() {
        let original = key();
        let changes: [fn(&mut LayoutKey); 10] = [
            |key| key.ghost = Some((0, "ghost".into())),
            |key| key.revision += 1,
            |key| key.wrap_width += px(1.),
            |key| key.marked = Some(0..1),
            |key| key.read_only = true,
            |key| key.markdown = false,
            |key| key.style.font_size += px(1.),
            |key| key.style.line_height += px(1.),
            |key| key.style.padding_top += px(1.),
            |key| key.style.text = gpui::red(),
        ];
        assert!(original == original.clone());
        for change in changes {
            let mut changed = original.clone();
            change(&mut changed);
            assert!(original != changed);
        }
    }

    #[test]
    fn repositioning_shares_rows_and_shaped_lines() {
        let layout = Layout {
            key: key(),
            revision: 1,
            bounds: Bounds::default(),
            origin: point(px(12.), px(-20.)),
            line_height: px(20.),
            wrap_width: px(400.),
            content_height: px(100.),
            lines: Rc::new(Vec::new()),
            rows: Rc::new(Vec::new()),
            ghost: None,
        };
        let mut moved = layout.clone();
        moved.bounds.origin = point(px(100.), px(200.));
        moved.origin = Point::default();
        assert!(Rc::ptr_eq(&layout.lines, &moved.lines));
        assert!(Rc::ptr_eq(&layout.rows, &moved.rows));
        assert_eq!(layout.origin, point(px(12.), px(-20.)));
        assert_eq!(layout.bounds.origin, Point::default());
    }
}
