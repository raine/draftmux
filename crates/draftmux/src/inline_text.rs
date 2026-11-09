//! A paragraph of inline text whose parts can differ in size and sit in
//! boxes, the way a browser lays out inline elements.
//!
//! gpui's `StyledText` shapes a paragraph at one font size and can only give
//! a run a background, so inline code can't be smaller, padded or bordered
//! like on GitHub and Slack. [`InlineText`] shapes each [`Segment`] at its own
//! size, breaks lines itself (after spaces and hyphens, around emoji, at
//! `\n`, and inside words too long for a line), puts every part of a line on
//! one baseline and draws chips around boxed segments, cut open where they
//! wrap.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, BorderStyle, Bounds, Corners, CursorStyle, DispatchPhase, Edges, Element,
    ElementId, FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseUpEvent, Pixels, ShapedLine,
    SharedString, Size, StrikethroughStyle, TextAlign, TextRun, TransformationMatrix, Window,
    point, px, quad, size,
};

/// Inline text laid out like a browser paragraph, in the surrounding text
/// style.
pub struct InlineText {
    segments: Vec<Segment>,
    blank_line: Option<Pixels>,
    laid: Rc<RefCell<Option<Laid>>>,
}

/// Part of a paragraph with one look. Unset fields keep the surrounding
/// text style.
#[derive(Clone, Default)]
pub struct Segment {
    pub text: SharedString,
    pub family: Option<SharedString>,
    pub weight: Option<FontWeight>,
    pub italic: bool,
    /// Font size relative to the surrounding text.
    pub scale: Option<f32>,
    pub color: Option<Hsla>,
    pub strike: bool,
    pub chip: Option<Chip>,
    /// Opened on click.
    pub link: Option<SharedString>,
}

/// A box around a segment, like a CSS inline element's background, border
/// and padding. Where the segment wraps, the box is cut open.
#[derive(Clone)]
pub struct Chip {
    pub background: Hsla,
    pub border: Option<Hsla>,
    pub radius: Pixels,
    pub pad_x: Pixels,
    pub pad_top: Pixels,
    pub pad_bottom: Pixels,
    /// An icon before the text.
    pub icon: Option<Icon>,
}

#[derive(Clone)]
pub struct Icon {
    pub path: SharedString,
    pub size: Pixels,
    /// Space between the icon and the text.
    pub gap: Pixels,
}

impl Chip {
    fn border_width(&self) -> Pixels {
        if self.border.is_some() {
            px(1.)
        } else {
            px(0.)
        }
    }
}

impl InlineText {
    pub fn new(segments: Vec<Segment>) -> Self {
        Self {
            segments,
            blank_line: None,
            laid: Rc::default(),
        }
    }

    /// Height of an empty line; by default a full line.
    pub fn blank_line(mut self, height: Option<Pixels>) -> Self {
        self.blank_line = height;
        self
    }
}

/// The segments' text between line breaks, each piece shaped at its
/// segment's size.
struct Shaped {
    pieces: Vec<Piece>,
    /// Line breaks after the last piece.
    trailing_breaks: usize,
    line_height: Pixels,
    /// Baseline offset from the top of a line, for the surrounding font.
    baseline: Pixels,
    blank_line: Pixels,
    align: TextAlign,
}

struct Piece {
    segment: usize,
    line: ShapedLine,
    /// Line breaks between the previous piece and this one.
    breaks: usize,
    /// Width a chip adds before and after the text: border, padding, icon.
    lead: Pixels,
    trail: Pixels,
}

struct Laid {
    shaped: Rc<Shaped>,
    wrap: Option<Pixels>,
    lines: Vec<Line>,
}

impl Element for InlineText {
    type RequestLayoutState = ();
    /// Link hitboxes and their URLs.
    type PrepaintState = Vec<(Hitbox, SharedString)>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let shaped = Rc::new(shape(&self.segments, self.blank_line, window, cx));
        let laid = self.laid.clone();
        let layout_id =
            window.request_measured_layout(Default::default(), move |known, available, _, _| {
                // Words are only cut to fit a definite width; min-content
                // keeps them whole, as browsers do.
                let (wrap, cut) = match (known.width, available.width) {
                    (Some(width), _) | (None, AvailableSpace::Definite(width)) => {
                        (Some(width), true)
                    }
                    (None, AvailableSpace::MinContent) => (Some(px(0.)), false),
                    (None, AvailableSpace::MaxContent) => (None, false),
                };
                let lines = break_lines(&shaped, wrap, cut);
                let size = size_of(&lines, &shaped);
                laid.replace(Some(Laid {
                    shaped: shaped.clone(),
                    wrap,
                    lines,
                }));
                size
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let mut hitboxes = Vec::new();
        let mut laid = self.laid.borrow_mut();
        let Some(laid) = laid.as_mut() else {
            return hitboxes;
        };
        if laid.wrap != Some(bounds.size.width) {
            laid.lines = break_lines(&laid.shaped, Some(bounds.size.width), true);
            laid.wrap = Some(bounds.size.width);
        }
        for (line, top, left) in positioned(laid, bounds) {
            for frag in &line.frags {
                let piece = &laid.shaped.pieces[frag.piece];
                if let Some(url) = &self.segments[piece.segment].link {
                    // A chip's box, icon included, is part of the link.
                    let (lead, trail) = frag.edges(piece);
                    let width = piece.line.x_for_index(frag.range.end)
                        - piece.line.x_for_index(frag.range.start);
                    let bounds = Bounds::new(
                        point(left + frag.x - lead, top),
                        size(lead + width + trail, laid.shaped.line_height),
                    );
                    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                    hitboxes.push((hitbox, url.clone()));
                }
            }
        }
        hitboxes
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitboxes: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let laid = self.laid.borrow();
        let Some(laid) = laid.as_ref() else { return };
        let shaped = &laid.shaped;
        // Each piece's fragments come in order, so one cursor per piece
        // cuts them without reshaping.
        let mut cursors: Vec<_> = shaped.pieces.iter().map(|p| p.line.cursor()).collect();
        for (line, top, left) in positioned(laid, bounds) {
            let baseline = top + shaped.baseline;
            for frag in &line.frags {
                let piece = &shaped.pieces[frag.piece];
                let segment = &self.segments[piece.segment];
                let part = cursors[frag.piece].take_until(frag.range.end);
                let x = left + frag.x;
                if let Some(chip) = &segment.chip {
                    paint_chip(chip, segment, piece, frag, &part, x, baseline, window, cx);
                }
                // `paint` centers the text's own ascent and descent in the
                // line; shift it so its baseline lands on the line's.
                let own = (shaped.line_height - part.ascent - part.descent) / 2. + part.ascent;
                part.paint(
                    point(x, baseline - own),
                    shaped.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
            }
        }
        for (hitbox, url) in hitboxes.drain(..) {
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    cx.open_url(&url);
                }
            });
        }
    }
}

impl IntoElement for InlineText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// Lines with their top and left edges in window coordinates.
fn positioned(
    laid: &Laid,
    bounds: Bounds<Pixels>,
) -> impl Iterator<Item = (&Line, Pixels, Pixels)> {
    let shaped = &laid.shaped;
    let mut top = bounds.origin.y;
    laid.lines.iter().map(move |line| {
        let offset = match shaped.align {
            TextAlign::Left => px(0.),
            TextAlign::Center => ((bounds.size.width - line.width) / 2.).max(px(0.)),
            TextAlign::Right => (bounds.size.width - line.width).max(px(0.)),
        };
        let line_top = top;
        top += line_height(line, shaped);
        (line, line_top, bounds.origin.x + offset)
    })
}

fn line_height(line: &Line, shaped: &Shaped) -> Pixels {
    if line.frags.is_empty() {
        shaped.blank_line
    } else {
        shaped.line_height
    }
}

fn size_of(lines: &[Line], shaped: &Shaped) -> Size<Pixels> {
    let width = lines
        .iter()
        .map(|line| line.width)
        .fold(px(0.), Pixels::max);
    let height = lines.iter().map(|line| line_height(line, shaped)).sum();
    size(width.ceil(), height)
}

#[allow(clippy::too_many_arguments)]
fn paint_chip(
    chip: &Chip,
    segment: &Segment,
    piece: &Piece,
    frag: &Frag,
    part: &ShapedLine,
    x: Pixels,
    baseline: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let border = chip.border_width();
    let opens = frag.range.start == 0;
    let closes = frag.range.end == piece.line.len();
    let (lead, trail) = frag.edges(piece);
    let left = x - lead;
    let right = x + part.width + trail;
    let top = baseline - piece.line.ascent - chip.pad_top - border;
    let bottom = baseline + piece.line.descent + chip.pad_bottom + border;
    let radius = |on: bool| if on { chip.radius } else { px(0.) };
    let edge = |on: bool| if on { border } else { px(0.) };
    window.paint_quad(quad(
        Bounds::from_corners(point(left, top), point(right, bottom)),
        Corners {
            top_left: radius(opens),
            bottom_left: radius(opens),
            top_right: radius(closes),
            bottom_right: radius(closes),
        },
        chip.background,
        Edges {
            top: border,
            bottom: border,
            left: edge(opens),
            right: edge(closes),
        },
        chip.border.unwrap_or_default(),
        BorderStyle::Solid,
    ));
    if opens && let Some(icon) = &chip.icon {
        // Centered between the text's ascent and descent.
        let middle = baseline - (piece.line.ascent - piece.line.descent) / 2.;
        let origin = point(left + border + chip.pad_x, middle - icon.size / 2.);
        let color = segment.color.unwrap_or_else(|| window.text_style().color);
        window
            .paint_svg(
                Bounds::new(origin, size(icon.size, icon.size)),
                icon.path.clone(),
                None,
                TransformationMatrix::unit(),
                color,
                cx,
            )
            .ok();
    }
}

fn shape(
    segments: &[Segment],
    blank_line: Option<Pixels>,
    window: &mut Window,
    cx: &App,
) -> Shaped {
    let style = window.text_style();
    let rem = window.rem_size();
    let font_size = style.font_size.to_pixels(rem);
    let line_height = window.pixel_snap(style.line_height.to_pixels(font_size.into(), rem));
    let base_font = style.font();
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&base_font);
    let ascent = text_system.ascent(font_id, font_size);
    // The text system gives descent below the baseline as negative.
    let descent = -text_system.descent(font_id, font_size);
    let mut pieces = Vec::new();
    let mut breaks = 0;
    for (ix, segment) in segments.iter().enumerate() {
        let mut font = base_font.clone();
        if let Some(family) = &segment.family {
            font.family = family.clone();
        }
        if let Some(weight) = segment.weight {
            font.weight = weight;
        }
        if segment.italic {
            font.style = FontStyle::Italic;
        }
        let size = font_size * segment.scale.unwrap_or(1.);
        let (lead, trail) = match &segment.chip {
            Some(chip) => {
                let edge = chip.border_width() + chip.pad_x;
                let icon = chip.icon.as_ref().map_or(px(0.), |i| i.size + i.gap);
                (edge + icon, edge)
            }
            None => (px(0.), px(0.)),
        };
        for (lx, text) in segment.text.split('\n').enumerate() {
            if lx > 0 {
                breaks += 1;
            }
            if text.is_empty() {
                continue;
            }
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: segment.color.unwrap_or(style.color),
                background_color: None,
                underline: None,
                strikethrough: segment.strike.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
            };
            let line = window
                .text_system()
                .shape_line(SharedString::new(text), size, &[run], None);
            pieces.push(Piece {
                segment: ix,
                line,
                breaks: std::mem::take(&mut breaks),
                lead,
                trail,
            });
        }
    }
    Shaped {
        pieces,
        trailing_breaks: breaks,
        line_height,
        baseline: (line_height - ascent - descent) / 2. + ascent,
        blank_line: blank_line.unwrap_or(line_height),
        align: style.text_align,
    }
}

fn break_lines(shaped: &Shaped, wrap: Option<Pixels>, cut: bool) -> Vec<Line> {
    let items: Vec<Item> = shaped
        .pieces
        .iter()
        .map(|piece| Item {
            text: &piece.line.text,
            breaks: piece.breaks,
            lead: piece.lead,
            trail: piece.trail,
            x: Box::new(|ix| piece.line.x_for_index(ix)),
        })
        .collect();
    layout_lines(&items, shaped.trailing_breaks, wrap, cut)
}

/// One shaped piece as the line breaker sees it.
struct Item<'a> {
    text: &'a str,
    breaks: usize,
    lead: Pixels,
    trail: Pixels,
    /// Offset of a byte index from the start of the text.
    x: Box<dyn Fn(usize) -> Pixels + 'a>,
}

impl Item<'_> {
    fn width(&self, range: &Range<usize>) -> Pixels {
        (self.x)(range.end) - (self.x)(range.start)
    }
}

#[derive(Debug, Default)]
struct Line {
    frags: Vec<Frag>,
    /// Width without trailing spaces.
    width: Pixels,
}

#[derive(Debug)]
struct Frag {
    piece: usize,
    range: Range<usize>,
    /// Offset of the text from the line start.
    x: Pixels,
}

impl Frag {
    /// The chip edges drawn before and after this fragment: a piece's lead
    /// where it starts and its trail where it ends.
    fn edges(&self, piece: &Piece) -> (Pixels, Pixels) {
        let lead = if self.range.start == 0 {
            piece.lead
        } else {
            px(0.)
        };
        let trail = if self.range.end == piece.line.len() {
            piece.trail
        } else {
            px(0.)
        };
        (lead, trail)
    }
}

/// Item ranges with no break opportunity between them.
type Word = Vec<(usize, Range<usize>)>;

/// Breaks `items` into lines at most `wrap` wide. With `cut`, a word wider
/// than a line is cut between characters.
fn layout_lines(
    items: &[Item],
    trailing_breaks: usize,
    wrap: Option<Pixels>,
    cut: bool,
) -> Vec<Line> {
    let mut lines = vec![Line::default()];
    let mut x = px(0.);
    for word in words(items) {
        let (first, range) = &word[0];
        if range.start == 0 {
            for _ in 0..items[*first].breaks {
                lines.push(Line::default());
                x = px(0.);
            }
        }
        let (width, hang) = measure(items, &word);
        let fits = wrap.is_none_or(|wrap| x + width - hang <= wrap);
        if !fits && !lines.last().unwrap().frags.is_empty() {
            lines.push(Line::default());
            x = px(0.);
        }
        let parts = match wrap {
            Some(wrap) if cut && width - hang > wrap => cut_word(items, word, wrap),
            _ => vec![word],
        };
        for (ix, part) in parts.into_iter().enumerate() {
            if ix > 0 {
                lines.push(Line::default());
                x = px(0.);
            }
            let (_, hang) = measure(items, &part);
            let line = lines.last_mut().unwrap();
            for (piece, range) in part {
                let item = &items[piece];
                if range.start == 0 {
                    x += item.lead;
                }
                line.frags.push(Frag {
                    piece,
                    range: range.clone(),
                    x,
                });
                x += item.width(&range);
                if range.end == item.text.len() {
                    x += item.trail;
                }
            }
            line.width = x - hang;
        }
    }
    for _ in 0..trailing_breaks {
        lines.push(Line::default());
    }
    lines
}

/// A word's width with chip edges, and the width of its trailing spaces,
/// which may hang past the line end.
fn measure(items: &[Item], word: &Word) -> (Pixels, Pixels) {
    let mut width = px(0.);
    for (piece, range) in word {
        let item = &items[*piece];
        if range.start == 0 {
            width += item.lead;
        }
        width += item.width(range);
        if range.end == item.text.len() {
            width += item.trail;
        }
    }
    let hang = word.last().map_or(px(0.), |(piece, range)| {
        let item = &items[*piece];
        // Spaces inside a chip are part of its box.
        if item.trail > px(0.) && range.end == item.text.len() {
            return px(0.);
        }
        let text = &item.text[range.clone()];
        let spaces = text.len() - text.trim_end_matches(' ').len();
        item.width(&(range.end - spaces..range.end))
    });
    (width, hang)
}

/// Splits items into words, each ending at a break opportunity: after
/// spaces, after a hyphen between letters or digits, at a line break, and
/// before and after emoji.
fn words(items: &[Item]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut word = Word::new();
    for (ix, item) in items.iter().enumerate() {
        let emoji = item.text.chars().next().is_some_and(is_emoji);
        if (item.breaks > 0 || emoji) && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        let mut start = 0;
        for end in break_offsets(item.text) {
            word.push((ix, start..end));
            if end < item.text.len() || item.text.ends_with(' ') || emoji {
                words.push(std::mem::take(&mut word));
            }
            start = end;
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Byte offsets in `text` after which a line may break, ending with the
/// text's length.
fn break_offsets(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev = None;
    let mut chars = text.char_indices().peekable();
    while let Some((ix, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let opportunity = match c {
            ' ' => next.is_some_and(|n| n != ' '),
            '-' => {
                prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric)
            }
            _ => false,
        };
        if opportunity {
            out.push(ix + c.len_utf8());
        }
        prev = Some(c);
    }
    out.push(text.len());
    out
}

/// Cuts a word too wide for a line into parts that fit, between
/// characters.
fn cut_word(items: &[Item], word: Word, wrap: Pixels) -> Vec<Word> {
    let mut parts = vec![Word::new()];
    let mut x = px(0.);
    for (piece, range) in word {
        let item = &items[piece];
        if range.start == 0 {
            x += item.lead;
        }
        let mut start = range.start;
        for (offset, c) in item.text[range.clone()].char_indices() {
            let at = range.start + offset;
            let end = at + c.len_utf8();
            let mut width = item.width(&(at..end));
            if end == item.text.len() {
                width += item.trail;
            }
            // Spaces hang past the line end instead of starting a line.
            if c != ' ' && x + width > wrap && x > px(0.) {
                if at > start {
                    parts.last_mut().unwrap().push((piece, start..at));
                }
                parts.push(Word::new());
                start = at;
                // A chip's lead moves with its first character.
                x = if at == 0 { item.lead } else { px(0.) };
            }
            x += width;
        }
        if start < range.end {
            parts.last_mut().unwrap().push((piece, start..range.end));
        }
    }
    parts.retain(|part| !part.is_empty());
    parts
}

/// Pictographic characters, which render as emoji and lines may break
/// around.
pub fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0x1F000..=0x1FAFF | 0x2300..=0x23FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text with every byte one pixel wide.
    fn item(text: &str) -> Item<'_> {
        Item {
            text,
            breaks: 0,
            lead: px(0.),
            trail: px(0.),
            x: Box::new(|ix| px(ix as f32)),
        }
    }

    fn texts(items: &[Item], lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.frags
                    .iter()
                    .map(|f| &items[f.piece].text[f.range.clone()])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn wraps_after_spaces_and_hyphens() {
        let items = [item("one two three-four five")];
        let lines = layout_lines(&items, 0, Some(px(9.)), true);
        assert_eq!(texts(&items, &lines), ["one two ", "three-", "four five"]);
        // The trailing space hangs past the line end.
        assert_eq!(lines[0].width, px(7.));
    }

    #[test]
    fn words_span_segments() {
        let items = [item("aa b"), item("cc"), item("d ee")];
        let lines = layout_lines(&items, 0, Some(px(6.)), true);
        assert_eq!(texts(&items, &lines), ["aa ", "bccd ", "ee"]);
    }

    #[test]
    fn breaks_around_emoji() {
        let items = [item("ab"), item("🎉"), item("cd")];
        let lines = layout_lines(&items, 0, Some(px(5.)), true);
        assert_eq!(texts(&items, &lines), ["ab", "🎉", "cd"]);
    }

    #[test]
    fn line_breaks_leave_empty_lines() {
        let mut second = item("b");
        second.breaks = 2;
        let items = [item("a"), second];
        let lines = layout_lines(&items, 1, Some(px(10.)), true);
        assert_eq!(texts(&items, &lines), ["a", "", "b", ""]);
    }

    #[test]
    fn cuts_overlong_words_only_to_a_definite_width() {
        let items = [item("ab abcdefgh")];
        let lines = layout_lines(&items, 0, Some(px(3.)), true);
        assert_eq!(texts(&items, &lines), ["ab ", "abc", "def", "gh"]);
        let lines = layout_lines(&items, 0, Some(px(0.)), false);
        assert_eq!(texts(&items, &lines), ["ab ", "abcdefgh"]);
        let lines = layout_lines(&items, 0, None, false);
        assert_eq!(texts(&items, &lines), ["ab abcdefgh"]);
        // A cut word's trailing space hangs.
        let items = [item("abcdef gh")];
        let lines = layout_lines(&items, 0, Some(px(3.)), true);
        assert_eq!(texts(&items, &lines), ["abc", "def ", "gh"]);
    }

    #[test]
    fn chip_edges_take_space() {
        let mut chip = item("code");
        chip.lead = px(2.);
        chip.trail = px(2.);
        let items = [item("ab "), chip];
        let lines = layout_lines(&items, 0, Some(px(9.)), true);
        assert_eq!(texts(&items, &lines), ["ab ", "code"]);
        assert_eq!(lines[1].frags[0].x, px(2.));
        assert_eq!(lines[1].width, px(8.));
        // Cut to fit, the chip's edges count on its first and last line.
        let mut chip = item("code");
        chip.lead = px(2.);
        chip.trail = px(2.);
        let items = [chip];
        let lines = layout_lines(&items, 0, Some(px(6.)), true);
        assert_eq!(texts(&items, &lines), ["cod", "e"]);
    }
}
