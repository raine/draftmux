//! Lays out, paints and hit-tests the editor's soft-wrapped text.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, ElementId, ElementInputHandler, Entity, Font, FontStyle, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, LayoutId, Pixels, Point, SharedString, Style,
    TextAlign, TextRun, UnderlineStyle, Window, WrappedLine, fill, point, prelude::*, px, relative,
    size,
};

use crate::buffer::Affinity;
use crate::editor::{Editor, EditorStyle};
use crate::highlight::Styles;
use crate::movement::RowGeometry;

/// The text as last laid out, in window coordinates.
pub(crate) struct Layout {
    /// Buffer revision the layout was built from.
    pub revision: u64,
    pub bounds: Bounds<Pixels>,
    /// Window position of the text's top-left corner, after scrolling.
    pub origin: Point<Pixels>,
    pub line_height: Pixels,
    pub wrap_width: Pixels,
    /// Height of the text plus padding, before scrolling.
    pub content_height: Pixels,
    lines: Vec<Line>,
    rows: Vec<Row>,
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
            return (self.len(), Affinity::Downstream);
        }
        let offset = self.offset_at(row, local.x.as_f32());
        (offset, self.affinity_on_row(row, offset))
    }

    pub fn caret_bounds(&self, offset: usize, affinity: Affinity) -> Bounds<Pixels> {
        let offset = offset.min(self.len());
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
        let range = range.start.min(self.len())..range.end.min(self.len());
        let mut rects = Vec::new();
        for row in &self.rows {
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
        let style = EditorStyle::default();
        let wrap_width = (bounds.size.width - style.padding_x * 2.).max(px(1.));

        let editor = self.editor.read(cx);
        let text = SharedString::from(editor.buffer.text().to_string());
        let revision = editor.buffer.revision();
        let runs = text_runs(
            &text,
            &editor.highlights,
            editor.buffer.marked(),
            editor.read_only,
            &style,
        );
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

        // Scroll: clamp, then bring the caret into view if it moved.
        let content_height = y + style.padding_top + style.padding_bottom;
        let max_scroll = (content_height - bounds.size.height).max(Pixels::ZERO);
        let selection = editor.buffer.selection();
        let mut scroll_y = editor.scroll_y;
        let autoscroll = editor.autoscroll;
        let mut layout = Layout {
            revision,
            bounds,
            origin: Point::default(),
            line_height: style.line_height,
            wrap_width,
            content_height,
            lines,
            rows,
        };
        if autoscroll {
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
        let style = EditorStyle::default();
        let editor = self.editor.read(cx);
        let focus_handle = editor.focus_handle.clone();
        let selection = editor.buffer.selection();
        let focused = focus_handle.is_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if !selection.is_empty() {
                let color = if focused {
                    style.selection
                } else {
                    style.inactive_selection
                };
                for rect in layout.range_rects(selection.range(), true) {
                    window.paint_quad(fill(rect, color));
                }
            }

            for line in &layout.lines {
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

            if focused && selection.is_empty() {
                let caret = layout.caret_bounds(selection.head, selection.affinity);
                window.paint_quad(fill(
                    Bounds::new(caret.origin, size(px(2.), caret.size.height)),
                    style.caret,
                ));
            }
        });
    }
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

fn color_for(styles: Styles, read_only: bool, style: &EditorStyle) -> Hsla {
    if read_only {
        style.read_only_text
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
    }
}
