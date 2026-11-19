//! Renders a [`markup::Document`] as GPUI elements.
//!
//! The preview pane and chat messages share the same block renderer and
//! [`MarkupStyle`]. The preview caches measured blocks and selection text;
//! its footnotes ([`render_preview`]) also keep navigation state. Callers parse
//! when the text changes and keep the document; they own scrolling, padding
//! and backgrounds around the body.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use draftmux_core::decorations::{Decoration, DecorationKind, Side};
use draftmux_storage::Target;
use gpui::{
    AnyElement, App, Bounds, Div, ElementId, Entity, FontWeight, HighlightStyle, Hsla, Pixels,
    Point, ScrollHandle, SharedString, StyledText, Task, Window, div, point, prelude::*, px,
    relative, rgba, svg,
};
use markup::frontmatter::{FieldMap, Fields};
use markup::highlight::Token;
use markup::slack::{CodeLanguage, DiffRole};
use markup::source_map::SourceMap;
use markup::{AlertKind, Alignment, Block, Document, List, Span, Table};

use crate::assets::{
    ALERT_CAUTION_ICON, ALERT_IMPORTANT_ICON, ALERT_NOTE_ICON, ALERT_TIP_ICON, ALERT_WARNING_ICON,
    CHECK_ICON, LINK_ICON, SLACK_COPY_ICON,
};
use crate::block_cache::BlockCache;
use crate::inline_text::{Chip, Icon, InlineText, MarkHandlers, Segment, SegmentMark, is_emoji};
use crate::selectable::{Select, TextRegistry, TextSelection};
use crate::theme::Palette;

/// Visual parameters for one target platform.
pub struct MarkupStyle {
    /// The platform's page background behind the body.
    pub background: Hsla,
    pub font_family: SharedString,
    pub mono_family: SharedString,
    pub text_size: Pixels,
    /// Relative to `text_size`.
    pub line_height: f32,
    /// Heading font sizes for levels 1 to 6, relative to `text_size`.
    pub heading_scale: [f32; 6],
    /// Levels up to this one get a rule underneath.
    pub heading_rule_max_level: u8,
    pub heading_weight: FontWeight,
    /// Relative to the heading's size.
    pub heading_line_height: f32,
    /// Level 6 headings take `muted_color`.
    pub muted_h6: bool,
    /// Spaces blocks with collapsing margins instead of the gaps below.
    pub margins: Option<Margins>,
    pub block_gap: Pixels,
    pub heading_gap: Pixels,
    pub strong_weight: FontWeight,
    /// Gap between items of a tight list.
    pub tight_item_gap: Pixels,
    /// Gap between two blocks of running text (paragraphs, headings and
    /// lists); other blocks are `block_gap` apart.
    pub text_gap: Pixels,
    /// Offset of an item's text from the list's left edge.
    pub list_indent: Pixels,
    pub bullet: Bullet,
    /// Space between an ordered item's number and its text.
    pub ordered_marker_gap: Pixels,
    pub text_color: Hsla,
    pub muted_color: Hsla,
    pub link_color: Hsla,
    pub link_underline: bool,
    pub border_color: Hsla,
    pub heading_rule_color: Hsla,
    pub inline_code: InlineBox,
    /// How `@name` mentions show; `None` leaves them as text.
    pub mention: Option<InlineBox>,
    /// How links labelled with their own URL show, with a link icon and a
    /// shortened label ([`url_label`]); `None` shows them as other links.
    pub url_slug: Option<InlineBox>,
    /// Curl straight quotes as Slack does when the message is pasted
    /// ([`markup::slack::smart_quotes`]).
    pub smart_quotes: bool,
    /// Emoji size relative to the text.
    pub emoji_scale: f32,
    /// Height of an empty line inside a paragraph; `None` gives it a full
    /// line.
    pub blank_line: Option<Pixels>,
    pub code_background: Hsla,
    pub code_text_size: Pixels,
    pub code_line_height: f32,
    pub code_padding: Pixels,
    pub code_border: Option<Hsla>,
    /// Wrap long code lines instead of scrolling them sideways.
    pub code_wraps: bool,
    /// How code blocks with a language show; `None` shows them as others.
    pub language_code: Option<LanguageCode>,
    /// Colors of code in a language with a grammar; `None` leaves code
    /// uncolored.
    pub syntax: Option<SyntaxColors>,
    pub radius: Pixels,
    pub quote_bar_width: Pixels,
    pub quote_bar_color: Hsla,
    pub quote_bar_radius: Pixels,
    pub quote_padding: Pixels,
    pub quote_text_color: Hsla,
    /// How ordered lists number their items at list depths 0, 1 and 2,
    /// repeating deeper down.
    pub numbering: [Numbering; 3],
    pub table_stripe: Hsla,
    /// Horizontal and vertical padding of a table cell.
    pub table_cell_padding: (Pixels, Pixels),
    /// Space above and below a thematic break.
    pub rule_padding: Pixels,
    pub rule_height: Pixels,
    pub rule_color: Hsla,
    pub checkbox_checked: Hsla,
    pub checkbox_unchecked_background: Hsla,
    pub checkbox_unchecked_border: Hsla,
    pub alert_colors: AlertColors,
    /// Render what Slack posts instead of the document as written
    /// ([`markup::slack::posted`]).
    pub slack_fallbacks: bool,
    /// Colors of the footnotes [`render_preview`] puts on blocks Slack posts
    /// differently; `None` hides them.
    pub notes: Option<NoteColors>,
    /// Draws a review's changes on the text ([`redline`]); `None` outside
    /// review.
    pub redline: Option<Rc<Redline>>,
    /// Called with the [`SourceMap`] path of a task list item whose
    /// checkbox is clicked; `None` leaves checkboxes inert.
    pub on_task: Option<TaskClick>,
}

/// Toggles the task list item at a [`SourceMap`] path.
pub type TaskClick = Rc<dyn Fn(&[usize], &mut Window, &mut App)>;

/// How a redline's changes are drawn and what the pointer does to them:
/// insertions on a green tint, deletions struck through on a red one. Kept
/// changes lose the tint, insertions underlined and deletions faded; the
/// selected change is outlined, and the hovered one more faintly. Where
/// a change has no text on this side, a bar ([`POINT`]) marks its place
/// while it is selected or hovered.
pub struct Redline {
    /// Where the document's changes are.
    pub marks: Rc<Marks>,
    pub selected: Option<usize>,
    pub hovered: Option<usize>,
    /// Whether each hunk is kept.
    pub kept: Vec<bool>,
    pub colors: RedlineColors,
    pub handlers: Rc<MarkHandlers>,
}

#[derive(Clone, Copy)]
pub struct RedlineColors {
    pub added: Hsla,
    pub added_background: Hsla,
    pub removed: Hsla,
    pub removed_background: Hsla,
    pub selected: Hsla,
    /// The bars where text was added or removed.
    pub added_point: Hsla,
    pub removed_point: Hsla,
}

impl RedlineColors {
    pub fn new(palette: &Palette) -> Self {
        Self {
            added: palette.diff_added,
            added_background: palette.added_bg,
            removed: palette.diff_removed,
            removed_background: palette.removed_bg,
            selected: palette.accent,
            added_point: palette.added,
            removed_point: palette.removed,
        }
    }
}

/// What a point is drawn on: a hair space, shown as a bar.
pub const POINT: &str = "\u{200A}";

/// A change drawn on text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// The change, by index in the comparison's hunks.
    pub hunk: usize,
    /// Drawn as removed text rather than added text.
    pub deleted: bool,
    /// A bar where the change's text is missing, rather than the text.
    pub point: bool,
}

/// A mark on part of the text of the node at `path` in a document's
/// [`SourceMap`]. An empty `text` is a point between two characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMark {
    pub path: Vec<usize>,
    pub text: Range<usize>,
    pub mark: Mark,
}

/// Where a redline's changes show: on the text of each block the preview
/// draws, and on the frontmatter's fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    /// By top-level block; each mark's path starts with the block's index.
    pub blocks: Vec<Vec<TextMark>>,
    /// By field index, in text order.
    pub fields: Vec<(usize, Mark)>,
}

impl Marks {
    /// The marks on top-level block `ix`.
    pub fn block(&self, ix: usize) -> &[TextMark] {
        self.blocks.get(ix).map_or(&[], Vec::as_slice)
    }

    /// The marks on field `ix`.
    pub fn field(&self, ix: usize) -> impl Iterator<Item = Mark> + '_ {
        self.fields
            .iter()
            .filter(move |(field, _)| *field == ix)
            .map(|(_, mark)| *mark)
    }
}

impl Redline {
    fn style(&self, segment: &mut Segment, mark: Mark) {
        let point = mark.point;
        let colors = self.colors;
        let kept = self.kept.get(mark.hunk).copied().unwrap_or(false);
        let (color, background) = if mark.deleted {
            (colors.removed, colors.removed_background)
        } else {
            (colors.added, colors.added_background)
        };
        segment.color = Some(if mark.deleted && kept {
            color.opacity(0.6)
        } else {
            color
        });
        segment.strike |= mark.deleted && !point;
        let ring = if self.selected == Some(mark.hunk) {
            Some(colors.selected)
        } else {
            (self.hovered == Some(mark.hunk)).then(|| colors.selected.opacity(0.5))
        };
        segment.mark = Some(SegmentMark {
            hunk: mark.hunk,
            background: (!kept && !point).then_some(background),
            ring,
            underline: (kept && !mark.deleted).then(|| color.opacity(0.45)),
            bar: (point && ring.is_some()).then(|| {
                let bar = match mark.deleted {
                    true => colors.removed_point,
                    false => colors.added_point,
                };
                bar.opacity(if kept { 0.6 } else { 1. })
            }),
        });
    }
}

/// Block margins as a browser gives unstyled HTML: siblings are as far
/// apart as the larger of their margins. Paragraphs, quotes and top-level
/// lists have a margin of the text size, code blocks of the code size, and
/// tables none.
pub struct Margins {
    /// Heading margins for levels 1 to 6, relative to the heading's size.
    pub heading: [f32; 6],
    pub rule: Pixels,
}

#[derive(Clone, Copy)]
pub struct NoteColors {
    /// A footnote's number, on its block and in the list.
    pub mark: Hsla,
    /// Behind a hovered number and a block or note jumped to.
    pub highlight: Hsla,
    pub text: Hsla,
    /// The list's heading.
    pub label: Hsla,
    /// Above the list.
    pub rule: Hsla,
}

#[derive(Clone, Copy)]
pub enum Numbering {
    Decimal,
    /// a, b, … z, aa.
    Alpha,
    /// Lowercase roman numerals.
    Roman,
}

/// Bullets are a disc, a ring and a square by depth, like CSS list markers.
pub struct Bullet {
    pub size: Pixels,
    /// Offset of the bullet's left edge from the list's left edge.
    pub left: Pixels,
    /// Offset of the bullet's top edge from the top of the item's first
    /// line.
    pub top: Pixels,
    pub ring_width: Pixels,
}

/// Colors of Slack's code block for a fence with a language: a header with
/// the language and a copy button, line numbers, and diff colors.
pub struct LanguageCode {
    pub background: Hsla,
    pub border: Hsla,
    pub label: Hsla,
    pub icon: Hsla,
    pub line_number: Hsla,
    pub text: Hsla,
    /// Text in a language Slack doesn't know, which it leaves unhighlighted.
    pub unknown_text: Hsla,
    /// Text and background of deleted and inserted diff lines, and of file
    /// names and hunk ranges.
    pub deleted: (Hsla, Hsla),
    pub inserted: (Hsla, Hsla),
    pub header: (Hsla, Hsla),
    /// Diff punctuation such as `@@` and `---`.
    pub muted: Hsla,
}

/// Colors of highlighted code by [`Token`] class.
pub struct SyntaxColors {
    pub comment: Hsla,
    pub constant: Hsla,
    pub entity: Hsla,
    pub tag: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub variable: Hsla,
    pub heading: Hsla,
    pub list_marker: Hsla,
    pub inserted: Hsla,
    pub deleted: Hsla,
    pub changed: Hsla,
    pub diff_range: Hsla,
    pub invalid: Hsla,
}

impl SyntaxColors {
    /// Primer's `prettylights` syntax colors, as GitHub highlights code.
    fn github(dark: bool) -> Self {
        let c = |light: u32, dark_hex: u32| -> Hsla {
            rgba(if dark { dark_hex } else { light }).into()
        };
        Self {
            comment: c(0x59636eff, 0x9198a1ff),
            constant: c(0x0550aeff, 0x79c0ffff),
            entity: c(0x6639baff, 0xd2a8ffff),
            tag: c(0x116329ff, 0x7ee787ff),
            keyword: c(0xcf222eff, 0xff7b72ff),
            string: c(0x0a3069ff, 0xa5d6ffff),
            variable: c(0x953800ff, 0xffa657ff),
            heading: c(0x0550aeff, 0x1f6febff),
            list_marker: c(0x3b2300ff, 0xf2cc60ff),
            inserted: c(0x116329ff, 0xaff5b4ff),
            deleted: c(0x82071eff, 0xffdcd7ff),
            changed: c(0x953800ff, 0xffdfb6ff),
            diff_range: c(0x8250dfff, 0xd2a8ffff),
            invalid: c(0x82071eff, 0xf85149ff),
        }
    }

    /// `segment` drawn as `token`.
    fn style(&self, segment: &mut Segment, token: Token) {
        let (color, bold) = match token {
            Token::Comment => (Some(self.comment), false),
            Token::Constant => (Some(self.constant), false),
            Token::Entity => (Some(self.entity), false),
            Token::Tag => (Some(self.tag), false),
            Token::Keyword => (Some(self.keyword), false),
            Token::String => (Some(self.string), false),
            Token::Variable => (Some(self.variable), false),
            Token::RegexpEscape => (Some(self.tag), true),
            Token::Heading => (Some(self.heading), true),
            Token::ListMarker => (Some(self.list_marker), false),
            Token::Bold => (None, true),
            Token::Italic => (None, false),
            Token::Inserted => (Some(self.inserted), false),
            Token::Deleted => (Some(self.deleted), false),
            Token::Changed => (Some(self.changed), false),
            Token::DiffRange => (Some(self.diff_range), true),
            Token::Invalid => (Some(self.invalid), false),
        };
        segment.color = color;
        segment.weight = bold.then_some(FontWeight::BOLD);
        segment.italic = token == Token::Italic;
    }
}

impl LanguageCode {
    fn highlight(&self, role: DiffRole) -> HighlightStyle {
        let (color, background) = match role {
            DiffRole::Deleted => self.deleted,
            DiffRole::Inserted => self.inserted,
            DiffRole::Header => self.header,
            DiffRole::Muted => {
                return HighlightStyle {
                    color: Some(self.muted),
                    ..Default::default()
                };
            }
        };
        HighlightStyle {
            color: Some(color),
            background_color: Some(background),
            ..Default::default()
        }
    }
}

/// A run drawn in a box of its own, such as inline code.
pub struct InlineBox {
    /// Font size relative to the surrounding text.
    pub scale: f32,
    /// `None` keeps the surrounding color.
    pub color: Option<Hsla>,
    pub chip: Chip,
}

pub struct AlertColors {
    pub note: Hsla,
    pub tip: Hsla,
    pub important: Hsla,
    pub warning: Hsla,
    pub caution: Hsla,
}

impl AlertColors {
    fn get(&self, kind: AlertKind) -> Hsla {
        match kind {
            AlertKind::Note => self.note,
            AlertKind::Tip => self.tip,
            AlertKind::Important => self.important,
            AlertKind::Warning => self.warning,
            AlertKind::Caution => self.caution,
        }
    }
}

impl Chip {
    fn zoomed(self, zoom: f32) -> Self {
        Self {
            pad_x: self.pad_x * zoom,
            pad_top: self.pad_top * zoom,
            pad_bottom: self.pad_bottom * zoom,
            icon: self.icon.map(|icon| Icon {
                size: icon.size * zoom,
                gap: icon.gap * zoom,
                ..icon
            }),
            ..self
        }
    }
}

impl InlineBox {
    fn zoomed(self, zoom: f32) -> Self {
        Self {
            chip: self.chip.zoomed(zoom),
            ..self
        }
    }
}

impl MarkupStyle {
    /// The style with its text and the space between and around blocks
    /// scaled by `zoom`. Rules, borders and corners keep their size.
    pub fn zoomed(self, zoom: f32) -> Self {
        let (pad_x, pad_y) = self.table_cell_padding;
        Self {
            text_size: self.text_size * zoom,
            margins: self.margins.map(|margins| Margins {
                rule: margins.rule * zoom,
                ..margins
            }),
            block_gap: self.block_gap * zoom,
            heading_gap: self.heading_gap * zoom,
            tight_item_gap: self.tight_item_gap * zoom,
            text_gap: self.text_gap * zoom,
            list_indent: self.list_indent * zoom,
            bullet: Bullet {
                size: self.bullet.size * zoom,
                left: self.bullet.left * zoom,
                top: self.bullet.top * zoom,
                ring_width: self.bullet.ring_width,
            },
            ordered_marker_gap: self.ordered_marker_gap * zoom,
            inline_code: self.inline_code.zoomed(zoom),
            mention: self.mention.map(|mention| mention.zoomed(zoom)),
            url_slug: self.url_slug.map(|slug| slug.zoomed(zoom)),
            blank_line: self.blank_line.map(|line| line * zoom),
            code_text_size: self.code_text_size * zoom,
            code_padding: self.code_padding * zoom,
            quote_padding: self.quote_padding * zoom,
            table_cell_padding: (pad_x * zoom, pad_y * zoom),
            rule_padding: self.rule_padding * zoom,
            ..self
        }
    }

    /// GitHub's markdown body in Primer's light or dark colors.
    pub fn github(dark: bool) -> Self {
        let c = |light: u32, dark_hex: u32| -> Hsla {
            rgba(if dark { dark_hex } else { light }).into()
        };
        Self {
            background: c(0xffffffff, 0x0d1117ff),
            font_family: editor::platform::ui_font(),
            mono_family: crate::theme::MONO.into(),
            text_size: px(14.),
            line_height: 1.5,
            heading_scale: [2., 1.5, 1.25, 1., 0.875, 0.85],
            heading_rule_max_level: 2,
            heading_weight: FontWeight::SEMIBOLD,
            heading_line_height: 1.25,
            muted_h6: true,
            margins: None,
            block_gap: px(16.),
            heading_gap: px(24.),
            strong_weight: FontWeight::SEMIBOLD,
            tight_item_gap: px(3.5),
            text_gap: px(16.),
            list_indent: px(28.),
            bullet: Bullet {
                size: px(5.),
                left: px(14.),
                top: px(9.),
                ring_width: px(1.),
            },
            ordered_marker_gap: px(6.),
            text_color: c(0x1f2328ff, 0xe6edf3ff),
            muted_color: c(0x59636eff, 0x9198a1ff),
            link_color: c(0x0969daff, 0x4493f8ff),
            link_underline: false,
            border_color: c(0xd1d9e0ff, 0x3d444dff),
            heading_rule_color: c(0xd1d9e0b3, 0x3d444db3),
            // `code` is 85% with .2em .4em padding in its own size.
            inline_code: InlineBox {
                scale: 0.85,
                color: None,
                chip: Chip {
                    background: c(0x818b981f, 0x656c7633),
                    border: None,
                    radius: px(6.),
                    pad_x: px(14. * 0.85 * 0.4),
                    pad_top: px(14. * 0.85 * 0.2),
                    pad_bottom: px(14. * 0.85 * 0.2),
                    icon: None,
                },
            },
            mention: None,
            url_slug: None,
            smart_quotes: false,
            emoji_scale: 1.,
            blank_line: None,
            code_background: c(0xf6f8faff, 0x151b23ff),
            code_text_size: px(11.9),
            code_line_height: 1.45,
            code_padding: px(16.),
            code_border: None,
            code_wraps: false,
            language_code: None,
            syntax: Some(SyntaxColors::github(dark)),
            radius: px(6.),
            quote_bar_width: px(3.5),
            quote_bar_color: c(0xd1d9e0ff, 0x3d444dff),
            quote_bar_radius: px(0.),
            quote_padding: px(14.),
            quote_text_color: c(0x59636eff, 0x9198a1ff),
            numbering: [Numbering::Decimal, Numbering::Roman, Numbering::Alpha],
            table_stripe: c(0xf6f8faff, 0x151b23ff),
            table_cell_padding: (px(13.), px(6.)),
            rule_padding: px(8.),
            rule_height: px(1.),
            rule_color: c(0xd1d9e0ff, 0x3d444dff),
            checkbox_checked: c(0x0969daff, 0x4493f8ff),
            // The browser's own checkbox.
            checkbox_unchecked_background: c(0xffffffff, 0x3b3b3bff),
            checkbox_unchecked_border: c(0x767676ff, 0x858585ff),
            alert_colors: AlertColors {
                note: c(0x0969daff, 0x4493f8ff),
                tip: c(0x1a7f37ff, 0x3fb950ff),
                important: c(0x8250dfff, 0xab7df8ff),
                warning: c(0x9a6700ff, 0xd29922ff),
                caution: c(0xd1242fff, 0xf85149ff),
            },
            slack_fallbacks: false,
            notes: None,
            redline: None,
            on_task: None,
        }
    }

    /// A Rich text draft as Apple Mail shows the HTML Copy writes: WebKit's
    /// default layout in Mail's 13px Helvetica message font, plus the
    /// HTML's own styles for code, quotes and tables. Dark colors are the
    /// light ones passed through WebKit's `apple-invert-lightness()` filter
    /// (a 180° hue rotation, then a fixed matrix), as Mail shows messages
    /// that declare no dark colors.
    pub fn rich_text(dark: bool) -> Self {
        let c = |light: u32, dark_hex: u32| -> Hsla {
            rgba(if dark { dark_hex } else { light }).into()
        };
        let transparent = gpui::transparent_black();
        let text_color = c(0x000000ff, 0xffffffff);
        let border_color = c(0xd0d7deff, 0x4e5459ff);
        let github = Self::github(dark);
        Self {
            background: c(0xffffffff, 0x000000ff),
            font_family: editor::platform::RICH_SANS.into(),
            mono_family: editor::platform::MONO.into(),
            text_size: px(13.),
            // Helvetica's normal line height, 15px at 13px.
            line_height: 15. / 13.,
            heading_scale: [2., 1.5, 1.17, 1., 0.83, 0.67],
            heading_rule_max_level: 0,
            heading_weight: FontWeight::BOLD,
            heading_line_height: 1.15,
            muted_h6: false,
            margins: Some(Margins {
                heading: [0.67, 0.83, 1., 1.33, 1.67, 2.33],
                rule: px(6.5),
            }),
            block_gap: px(13.),
            strong_weight: FontWeight::BOLD,
            tight_item_gap: px(0.),
            list_indent: px(40.),
            bullet: Bullet {
                size: px(4.),
                left: px(25.),
                top: px(5.5),
                ring_width: px(1.),
            },
            text_color,
            muted_color: text_color,
            link_color: c(0x0000eeff, 0x5656ffff),
            link_underline: true,
            border_color,
            inline_code: InlineBox {
                scale: 1.,
                color: None,
                chip: Chip {
                    background: transparent,
                    border: None,
                    radius: px(0.),
                    pad_x: px(0.),
                    pad_top: px(0.),
                    pad_bottom: px(0.),
                    icon: None,
                },
            },
            code_background: transparent,
            // 90% of the text.
            code_text_size: px(11.7),
            // Menlo's normal line height, 14px at 11.7px.
            code_line_height: 14. / 11.7,
            code_padding: px(8.),
            code_border: Some(border_color),
            code_wraps: true,
            radius: px(4.),
            quote_bar_width: px(3.),
            quote_bar_color: border_color,
            quote_padding: px(8.),
            quote_text_color: text_color,
            numbering: [Numbering::Decimal, Numbering::Alpha, Numbering::Roman],
            table_stripe: transparent,
            table_cell_padding: (px(10.), px(4.)),
            rule_padding: px(0.),
            rule_height: px(2.),
            rule_color: c(0x9a9a9aff, 0x848484ff),
            syntax: None,
            ..github
        }
    }

    /// Slack's light or dark message text. Spacing between lines comes from
    /// the newlines kept in the text, so blocks sit only a small margin apart.
    pub fn slack(dark: bool) -> Self {
        let c = |light: u32, dark_hex: u32| -> Hsla {
            rgba(if dark { dark_hex } else { light }).into()
        };
        let text_color = c(0x1d1c1dff, 0xd1d2d3ff);
        let border_color = c(0x1d1c1d21, 0x4a4c50ff);
        let link_color = c(0x1264a3ff, 0x2ba5ceff);
        let code_background = c(0x1d1c1d0a, 0xe8e8e80a);
        let code_border = c(0x1d1c1d21, 0xe8e8e821);
        let app = if dark {
            Palette::dark()
        } else {
            Palette::light()
        };
        let tag = |color: Hsla, background: Hsla, icon: Option<Icon>| InlineBox {
            scale: 1.,
            color: Some(color),
            chip: Chip {
                background,
                border: None,
                radius: px(3.),
                pad_x: px(2.),
                pad_top: px(1.),
                pad_bottom: px(1.),
                icon,
            },
        };
        Self {
            background: c(0xffffffff, 0x1a1d21ff),
            font_family: "Lato".into(),
            mono_family: "Roboto Mono".into(),
            text_size: px(15.),
            line_height: 1.46668,
            // Headings are replaced by bold lines.
            heading_scale: [1.; 6],
            heading_rule_max_level: 0,
            heading_weight: FontWeight::SEMIBOLD,
            heading_line_height: 1.25,
            muted_h6: true,
            margins: None,
            block_gap: px(4.),
            heading_gap: px(4.),
            strong_weight: FontWeight::BOLD,
            tight_item_gap: px(0.),
            // Message lines run on; only quotes and code blocks have
            // margins.
            text_gap: px(0.),
            list_indent: px(28.),
            bullet: Bullet {
                size: px(7.),
                left: px(7.25),
                top: px(9.),
                ring_width: px(1.5),
            },
            ordered_marker_gap: px(12.),
            text_color,
            muted_color: c(0x616061ff, 0xababadff),
            link_color,
            link_underline: false,
            border_color,
            heading_rule_color: border_color,
            // Roboto Mono 12px in a bordered chip.
            inline_code: InlineBox {
                scale: 0.8,
                color: Some(c(0xc01343ff, 0xe8912dff)),
                chip: Chip {
                    background: code_background,
                    border: Some(code_border),
                    radius: px(3.),
                    pad_x: px(3.),
                    pad_top: px(2.),
                    pad_bottom: px(1.),
                    icon: None,
                },
            },
            mention: Some(tag(
                c(0x1264a3ff, 0xdea700ff),
                c(0xffc6002e, 0xa073002e),
                None,
            )),
            url_slug: Some(tag(
                link_color,
                c(0x36c5f021, 0x03669640),
                Some(Icon {
                    path: LINK_ICON.into(),
                    size: px(15.),
                    gap: px(4.),
                }),
            )),
            smart_quotes: true,
            // Emoji are 22px images.
            emoji_scale: 1.2,
            // An empty line is an 8px paragraph break.
            blank_line: Some(px(8.)),
            code_background,
            code_text_size: px(12.),
            code_line_height: 1.50001,
            code_padding: px(8.),
            code_border: Some(code_border),
            code_wraps: true,
            syntax: None,
            language_code: Some(LanguageCode {
                background: c(0xf8f8f8ff, 0x212428ff),
                border: c(0x1d1c1d0f, 0xf8f8f80f),
                label: c(0x7c7a7fff, 0x797c81ff),
                icon: c(0x454447ff, 0xb9babdff),
                line_number: c(0x454447ff, 0xb9babdff),
                text: c(0x1d1c1dff, 0xf8f8f8ff),
                unknown_text: text_color,
                deleted: (c(0xad2e12ff, 0xdf847cff), c(0xffd6d5ff, 0x5a1005ff)),
                inserted: (c(0x137700ff, 0x81bc69ff), c(0xd7f5c8ff, 0x123111ff)),
                header: (c(0x2840a9ff, 0x899bdcff), c(0xdce3faff, 0x0a165aff)),
                muted: c(0x5e5d60ff, 0x9a9b9eff),
            }),
            radius: px(4.),
            quote_bar_width: px(4.),
            quote_bar_color: c(0xddddddff, 0xb9babdff),
            quote_bar_radius: px(8.),
            quote_padding: px(12.),
            quote_text_color: text_color,
            numbering: [Numbering::Decimal, Numbering::Alpha, Numbering::Roman],
            table_stripe: c(0x1d1c1d0a, 0x232529ff),
            table_cell_padding: (px(13.), px(6.)),
            rule_padding: px(8.),
            rule_height: px(1.),
            rule_color: border_color,
            checkbox_checked: link_color,
            checkbox_unchecked_background: c(0xffffffff, 0x1a1d21ff),
            checkbox_unchecked_border: border_color,
            alert_colors: AlertColors {
                note: border_color,
                tip: border_color,
                important: border_color,
                warning: border_color,
                caution: border_color,
            },
            slack_fallbacks: true,
            // The app's own text colors, as the footnotes aren't part of
            // the message.
            notes: Some(NoteColors {
                mark: c(0xc58a00ff, 0xe6b84aff),
                highlight: c(0xf5be2824, 0xe6b84a24),
                text: app.muted,
                label: app.faint,
                rule: app.hairline,
            }),
            redline: None,
            on_task: None,
        }
    }
}

/// Registers the bundled fonts the styles use. Call once at startup.
pub fn init(cx: &mut App) {
    cx.background_executor()
        .spawn(async { markup::highlight::load() })
        .detach();
    let fonts = [
        include_bytes!("../assets/fonts/lato/Lato-Regular.ttf").as_slice(),
        include_bytes!("../assets/fonts/lato/Lato-Italic.ttf"),
        include_bytes!("../assets/fonts/lato/Lato-Bold.ttf"),
        include_bytes!("../assets/fonts/lato/Lato-BoldItalic.ttf"),
        include_bytes!("../assets/fonts/roboto-mono/RobotoMono-Regular.ttf"),
        include_bytes!("../assets/fonts/roboto-mono/RobotoMono-Italic.ttf"),
        include_bytes!("../assets/fonts/roboto-mono/RobotoMono-Bold.ttf"),
        include_bytes!("../assets/fonts/roboto-mono/RobotoMono-BoldItalic.ttf"),
    ];
    cx.text_system()
        .add_fonts(fonts.into_iter().map(Cow::Borrowed).collect())
        .expect("bundled fonts are valid");
}

/// Makes a body's text selectable as body `body` of a surface's selection,
/// highlighted in `color`.
#[derive(Clone)]
pub struct SelectBody {
    pub state: Entity<TextSelection>,
    pub body: usize,
    pub color: Hsla,
    pub registry: Option<TextRegistry>,
}

/// Renders `doc` at the width its parent gives it. `id` must be unique among
/// bodies shown together, such as chat messages in one list.
pub fn render(
    doc: &Document,
    style: &MarkupStyle,
    id: impl Into<ElementId>,
    select: Option<SelectBody>,
) -> AnyElement {
    let mut renderer = Renderer::new(style, select);
    let blocks = renderer.body(doc);
    root(style).id(id).children(blocks).into_any_element()
}

/// [`render`] with the text in its parent's color, for a body on a surface
/// whose text color changes, such as a highlighted row. Bullets, links and
/// other marks keep the style's colors.
pub fn render_in_parent_color(
    doc: &Document,
    style: &MarkupStyle,
    id: impl Into<ElementId>,
) -> AnyElement {
    let mut renderer = Renderer::new(style, None);
    let blocks = renderer.body(doc);
    uncolored_root(style)
        .id(id)
        .children(blocks)
        .into_any_element()
}

/// The text [`render`] draws for `doc`: each text's path in the
/// document's [`SourceMap`] and its segments, with any redline marks.
#[cfg(any(test, feature = "capture"))]
pub fn drawn(doc: &Document, style: &MarkupStyle) -> Vec<(Vec<usize>, Vec<Segment>)> {
    let mut renderer = Renderer::new(style, None);
    renderer.body(doc);
    renderer.drawn
}

/// Renders `doc` like [`render`], with a numbered footnote on each block
/// Slack posts differently and the notes listed below the body. Clicking a
/// number scrolls `scroll`, the handle of the container the body scrolls in,
/// to its note and highlights it; clicking a note does the same for its block.
pub fn render_preview(
    doc: &Document,
    style: Rc<MarkupStyle>,
    scroll: &ScrollHandle,
    select: Option<SelectBody>,
    cache: (&mut BlockCache, Pixels),
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (cache, width) = cache;
    let (Some(colors), true) = (style.notes, style.slack_fallbacks) else {
        let posted;
        let blocks = if style.slack_fallbacks {
            posted = markup::slack::posted(doc)
                .into_iter()
                .map(|(block, _)| block)
                .collect::<Vec<_>>();
            &posted
        } else {
            &doc.blocks
        };
        cache.prepare(blocks, redline_marks(&style), width);
        let renderer = Renderer::new(&style, None);
        let blocks = blocks
            .iter()
            .enumerate()
            .map(|(ix, block)| {
                let gap = renderer.gap_before(
                    block,
                    ix.checked_sub(1).map(|i| &blocks[i]),
                    style.block_gap,
                );
                cached_block(cache, ix, gap, style.clone(), select.clone(), |block| block)
            })
            .collect::<Vec<_>>();
        return root(&style)
            .id("preview-body")
            .children(blocks)
            .into_any_element();
    };
    let state = window.use_keyed_state("footnotes", cx, |_, _| Footnotes::default());
    let flash = state.read(cx).flash;
    let posted = markup::slack::posted(doc);
    // The block index and text of each note, in footnote order.
    let noted: Vec<(usize, &str)> = posted
        .iter()
        .enumerate()
        .filter_map(|(ix, (_, note))| note.map(|note| (ix, note)))
        .collect();
    let posted: Vec<Block> = posted.into_iter().map(|(block, _)| block).collect();
    cache.prepare(&posted, redline_marks(&style), width);
    let renderer = Renderer::new(&style, None);
    let blocks = posted
        .iter()
        .enumerate()
        .map(|(ix, block)| {
            let gap = renderer.gap_before(
                block,
                ix.checked_sub(1).map(|i| &posted[i]),
                style.block_gap,
            );
            let n = noted.iter().position(|&(noted, _)| noted == ix);
            let state = state.clone();
            let scroll = scroll.clone();
            cached_block(
                cache,
                ix,
                gap,
                style.clone(),
                select.clone(),
                move |block| {
                    let Some(n) = n else {
                        return block;
                    };
                    let state = state.clone();
                    let scroll = scroll.clone();
                    div()
                        .relative()
                        .when(flash == Some(Flash::Block(ix)), |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .top(px(-3.))
                                    .bottom(px(-3.))
                                    .left(px(-6.))
                                    .right(px(-6.))
                                    .rounded(px(6.))
                                    .bg(colors.highlight),
                            )
                        })
                        .child(block)
                        .child(
                            div()
                                .id(("footnote-ref", n))
                                .absolute()
                                .top(px(1.))
                                .right(px(-18.))
                                .px(px(3.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .font_family(editor::platform::ui_font())
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .line_height(relative(1.))
                                .text_color(colors.mark)
                                .cursor_pointer()
                                .hover(|this| this.bg(colors.highlight))
                                .on_click(move |_, _, cx| jump(&state, &scroll, Flash::Note(n), cx))
                                .child(SharedString::from((n + 1).to_string())),
                        )
                        .into_any_element()
                },
            )
        })
        .collect::<Vec<_>>();
    let notes = noted.iter().enumerate().map(|(n, &(ix, note))| {
        let state = state.clone();
        let scroll = scroll.clone();
        div()
            .flex()
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .pt(px(1.))
                    .text_right()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.mark)
                    .child(format!("{}.", n + 1)),
            )
            .child(
                div()
                    .id(("footnote", n))
                    .flex_1()
                    .min_w_0()
                    .px(px(4.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .when(flash == Some(Flash::Note(n)), |this| {
                        this.bg(colors.highlight)
                    })
                    .cursor_pointer()
                    .hover(|this| this.text_color(style.text_color))
                    .on_click(move |_, _, cx| jump(&state, &scroll, Flash::Block(ix), cx))
                    .child(note),
            )
    });
    let section = (!noted.is_empty()).then(|| {
        div()
            .mt(px(28.))
            .pt(px(10.))
            .border_t_1()
            .border_color(colors.rule)
            .font_family(editor::platform::ui_font())
            .text_size(px(12.))
            .line_height(relative(1.5))
            .text_color(colors.text)
            .child(
                div()
                    .mb(px(4.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.label)
                    .child("When pasted into Slack"),
            )
            .child(
                div()
                    .on_children_prepainted({
                        let state = state.clone();
                        move |bounds, _, cx| state.update(cx, |this, _| this.notes = bounds)
                    })
                    .children(notes),
            )
    });
    root(&style)
        .on_children_prepainted(move |bounds, _, cx| {
            state.update(cx, |this, _| this.blocks = bounds)
        })
        .id("preview-body")
        .children(blocks)
        .children(section)
        .into_any_element()
}

/// The marks `style` draws, if it draws a redline.
fn redline_marks(style: &MarkupStyle) -> Option<&Marks> {
    style.redline.as_ref().map(|redline| &*redline.marks)
}

/// Each cached top-level block owns a selection body and local element ids.
fn cached_block(
    cache: &BlockCache,
    index: usize,
    gap: Pixels,
    style: Rc<MarkupStyle>,
    select: Option<SelectBody>,
    decorate: impl FnOnce(AnyElement) -> AnyElement + 'static,
) -> AnyElement {
    cache.wrap(index, move |entry| {
        entry.texts.borrow_mut().clear();
        let select = select.map(|mut select| {
            select.body = index;
            select.registry = Some(entry.texts.clone());
            select
        });
        let mut renderer = Renderer::new(&style, select);
        renderer.scrolls = Some(entry.scrolls.clone());
        renderer.path = vec![index];
        renderer.marks = &entry.marks;
        let block = renderer.block(&entry.block);
        entry
            .scrolls
            .borrow_mut()
            .retain(|id, _| *id <= renderer.next_id);
        root(&style)
            .id(("preview-block", index))
            .child(div().w_full().pt(gap).child(decorate(block)))
            .into_any_element()
    })
}

/// How long a block or note jumped to stays highlighted.
const FLASH: Duration = Duration::from_millis(1400);

/// Where the preview's footnotes were last drawn, and what is highlighted.
#[derive(Default)]
struct Footnotes {
    /// Window bounds of the body's children: the blocks, then the list.
    blocks: Vec<Bounds<Pixels>>,
    notes: Vec<Bounds<Pixels>>,
    flash: Option<Flash>,
    /// Clears `flash`; replacing it cancels the previous one.
    unflash: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Flash {
    Block(usize),
    /// The n-th note, from 0.
    Note(usize),
}

/// Scrolls `target` into view and highlights it for a moment.
fn jump(state: &Entity<Footnotes>, scroll: &ScrollHandle, target: Flash, cx: &mut App) {
    state.update(cx, |this, cx| {
        let bounds = match target {
            Flash::Block(ix) => this.blocks.get(ix),
            Flash::Note(n) => this.notes.get(n),
        };
        if let Some(bounds) = bounds {
            scroll.set_offset(reveal(scroll.bounds(), *bounds, scroll.offset()));
        }
        this.flash = Some(target);
        this.unflash = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            this.update(cx, |this, cx| {
                this.flash = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    });
}

/// The scroll offset that brings `target` into `viewport` with the least
/// movement, aligning its top when it doesn't fit. Both are window bounds at
/// `offset`.
fn reveal(
    viewport: Bounds<Pixels>,
    target: Bounds<Pixels>,
    offset: Point<Pixels>,
) -> Point<Pixels> {
    let shift = if target.top() < viewport.top() || target.size.height > viewport.size.height {
        viewport.top() - target.top()
    } else if target.bottom() > viewport.bottom() {
        viewport.bottom() - target.bottom()
    } else {
        px(0.)
    };
    point(offset.x, offset.y + shift)
}

/// The body's container, in the style's text settings.
fn root(style: &MarkupStyle) -> Div {
    uncolored_root(style).text_color(style.text_color)
}

fn uncolored_root(style: &MarkupStyle) -> Div {
    div()
        .w_full()
        .min_w_0()
        .font_family(style.font_family.clone())
        .text_size(style.text_size)
        .line_height(relative(style.line_height))
}

/// Parses `text` for previewing it on `target`. With [`style`], the one
/// place a target chooses its parser and style.
pub fn preview(target: Target, text: &str) -> Document {
    let _timing = editor::timing::span("preview-parse");
    let draft = markup::frontmatter::parse(text);
    let text = draft.body;
    match target {
        Target::Github => markup::github::parse(text),
        Target::GithubFile => markup::github::parse_file(text),
        Target::Slack => markup::slack::parse(text),
        Target::Rich => markup::rich_text::pasted(&markup::github::parse(text)),
        Target::Plain => markup::plain::parse(text),
    }
}

/// Whether a GitHub comment preview looks like prose wrapped at a fixed
/// width, as a repository file often is: at least two lines inside
/// paragraphs break after [`WRAPPED_LINE`] characters or more.
pub fn looks_wrapped(document: &Document) -> bool {
    fn wrapped_lines(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                Block::Paragraph(spans) => {
                    let text: String = spans.iter().map(|span| span.text.as_str()).collect();
                    let mut lines: Vec<_> = text.split('\n').collect();
                    lines.pop();
                    lines
                        .iter()
                        .filter(|line| line.chars().count() >= WRAPPED_LINE)
                        .count()
                }
                Block::List(list) => list
                    .items
                    .iter()
                    .map(|item| wrapped_lines(&item.blocks))
                    .sum(),
                Block::Quote(blocks) | Block::Alert { blocks, .. } => wrapped_lines(blocks),
                _ => 0,
            })
            .sum()
    }
    wrapped_lines(&document.blocks) >= 2
}

/// The shortest line, in characters, that [`looks_wrapped`] counts as
/// wrapped rather than broken on purpose.
const WRAPPED_LINE: usize = 50;

/// A draft parsed for previewing on a target, with where its text came
/// from.
pub struct Mapped {
    pub document: Rc<Document>,
    pub fields: Fields,
    /// The map of the blocks the preview draws: for Slack, of what Slack
    /// posts ([`markup::slack::posted_mapped`]).
    map: SourceMap,
    field_map: FieldMap,
    /// Where the body starts in the draft, after its frontmatter.
    body_offset: usize,
}

/// [`preview`], with its source maps. Mapping costs about a third more
/// than parsing, so only a review asks for it.
pub fn preview_mapped(target: Target, text: &str) -> Mapped {
    let _timing = editor::timing::span("preview-parse");
    let (draft, field_map) = markup::frontmatter::parse_mapped(text);
    let body = draft.body;
    let (document, map) = match target {
        Target::Github => markup::github::parse_mapped(body),
        Target::GithubFile => markup::github::parse_file_mapped(body),
        Target::Slack => {
            let (document, map) = markup::slack::parse_mapped(body);
            let map = markup::slack::posted_mapped(&document, &map).1;
            (document, map)
        }
        Target::Rich => {
            let (document, map) = markup::github::parse_mapped(body);
            markup::rich_text::pasted_mapped(&document, &map)
        }
        Target::Plain => markup::plain::parse_mapped(body),
    };
    Mapped {
        document: Rc::new(document),
        fields: draft.fields,
        map,
        field_map,
        body_offset: draft.body_offset,
    }
}

/// One side of a review: the side's document, and where its changes show.
pub struct Redlined {
    /// The side's [`Mapped::document`], with an empty paragraph at the end
    /// to hold the points that have no text to stand by.
    pub document: Rc<Document>,
    pub marks: Rc<Marks>,
}

/// Finds where `side`'s decorations show in `mapped`, that side's text
/// parsed for the preview. A change in the frontmatter marks the fields it
/// touches; one in the body marks the text its source map finds there, or
/// a point where it has no text, such as a heading's level or a link's URL.
pub fn redline(mapped: &Mapped, decorations: &[Decoration], side: Side) -> Redlined {
    let offset = mapped.body_offset;
    let mut marks = Marks {
        blocks: vec![Vec::new(); mapped.map.blocks.len()],
        fields: Vec::new(),
    };
    let mut alone = Vec::new();
    for decoration in decorations
        .iter()
        .filter(|decoration| decoration.side == side)
    {
        let range = decoration.range.clone();
        let mark = Mark {
            hunk: decoration.hunk,
            // A point shows where the other side's text is.
            deleted: match decoration.kind {
                DecorationKind::Deleted => true,
                DecorationKind::Inserted => false,
                DecorationKind::Point => side == Side::After,
            },
            point: decoration.kind == DecorationKind::Point,
        };
        let fields = mapped.field_map.locate(range.clone());
        for &field in &fields {
            marks.fields.push((field, mark));
        }
        if range.end <= offset && !(range.is_empty() && range.start >= offset) {
            if fields.is_empty() {
                alone.push(Mark {
                    point: true,
                    ..mark
                });
            }
            continue;
        }
        let body = range.start.max(offset) - offset..range.end.max(offset) - offset;
        let fragments = mapped.map.locate(body);
        if fragments.is_empty() {
            alone.push(mark);
        }
        for fragment in fragments {
            let mark = Mark {
                point: mark.point || fragment.text.is_empty(),
                ..mark
            };
            marks.blocks[fragment.path[0]].push(TextMark {
                path: fragment.path,
                text: fragment.text,
                mark,
            });
        }
    }
    let document = if alone.is_empty() {
        mapped.document.clone()
    } else {
        let mut document = (*mapped.document).clone();
        let ix = document.blocks.len();
        document.blocks.push(Block::Paragraph(Vec::new()));
        marks.blocks.resize(ix, Vec::new());
        marks.blocks.push(
            alone
                .into_iter()
                .map(|mark| TextMark {
                    path: vec![ix],
                    text: 0..0,
                    mark,
                })
                .collect(),
        );
        Rc::new(document)
    };
    Redlined {
        document,
        marks: Rc::new(marks),
    }
}

/// Field `index`'s `value` with the review's changes to it drawn.
pub fn field_value(value: &str, index: usize, redline: &Redline) -> InlineText {
    InlineText::new(field_segments(value, index, redline)).marks(Some(redline.handlers.clone()))
}

/// Field maps locate whole values, not decoded scalar substrings. A single
/// text change can tint the value; multiple text changes use separate points
/// so each hunk has its own hit target and kept/selected state. Empty values
/// also use points, since an empty segment has no drawable hit target.
pub fn field_segments(value: &str, index: usize, redline: &Redline) -> Vec<Segment> {
    let mut segment = Segment {
        text: SharedString::from(value.to_owned()),
        ..Segment::default()
    };
    let marks: Vec<Mark> = redline.marks.field(index).collect();
    let whole = !value.is_empty() && marks.iter().filter(|mark| !mark.point).count() == 1;
    if whole {
        redline.style(
            &mut segment,
            *marks.iter().find(|mark| !mark.point).unwrap(),
        );
    }
    let points = marks
        .iter()
        .filter(|mark| mark.point || !whole)
        .map(|&mark| {
            let mut point = Segment {
                text: POINT.into(),
                ..Segment::default()
            };
            redline.style(
                &mut point,
                Mark {
                    point: true,
                    ..mark
                },
            );
            point
        });
    std::iter::once(segment).chain(points).collect()
}

/// The preview style for `target` in the light or dark appearance.
pub fn style(target: Target, dark: bool) -> MarkupStyle {
    match target {
        Target::Github | Target::GithubFile => MarkupStyle::github(dark),
        Target::Slack => MarkupStyle::slack(dark),
        Target::Rich => MarkupStyle::rich_text(dark),
        // The system font; nothing in plain text is styled.
        Target::Plain => MarkupStyle::github(dark),
    }
}

struct Renderer<'a> {
    style: &'a MarkupStyle,
    /// Element ids are scoped by the body's root id, so a counter suffices.
    next_id: u64,
    scrolls: Option<Rc<RefCell<BTreeMap<u64, ScrollHandle>>>>,
    list_depth: usize,
    /// Rendering the blocks of a tight list's item, whose text has no
    /// paragraph margins.
    tight_item: bool,
    /// Font size of the heading whose text is being rendered.
    heading_size: Option<Pixels>,
    select: Option<SelectBody>,
    /// Selectable paragraphs rendered so far, which numbers the next.
    paragraphs: usize,
    /// The node being rendered, as a path in the document's
    /// [`SourceMap`].
    path: Vec<usize>,
    /// The redline's marks on the top-level block being rendered.
    marks: &'a [TextMark],
    /// Each text's path and segments, as drawn.
    #[cfg(any(test, feature = "capture"))]
    drawn: Vec<(Vec<usize>, Vec<Segment>)>,
}

/// A segment, and the part of its node's text it shows.
struct Placed {
    segment: Segment,
    source: Range<usize>,
    /// The node's text where the segment shows something else for it, as a
    /// curled quote for a straight one. Equal in characters.
    replaced: Option<String>,
    /// Shows something other than its text, such as a mention's name, so a
    /// mark takes it whole.
    whole: bool,
}

impl Placed {
    fn new(segment: Segment, source: Range<usize>) -> Self {
        Self {
            segment,
            source,
            replaced: None,
            whole: false,
        }
    }

    /// Where `at`, an offset in the node's text inside this segment, is in
    /// the segment's text.
    fn local(&self, at: usize) -> usize {
        let at = at - self.source.start;
        match &self.replaced {
            None => at,
            Some(text) => {
                let chars = text[..at].chars().count();
                let shown = &self.segment.text;
                shown
                    .char_indices()
                    .nth(chars)
                    .map_or(shown.len(), |(ix, _)| ix)
            }
        }
    }
}

/// The characters `segments` show as written, by where they are in the
/// node's text.
fn shown_chars(segments: &[Placed]) -> Vec<(usize, char)> {
    segments
        .iter()
        .filter(|placed| placed.replaced.is_none() && !placed.whole)
        .flat_map(|placed| {
            placed
                .segment
                .text
                .char_indices()
                .map(|(at, c)| (placed.source.start + at, c))
        })
        .collect()
}

/// `range` without the whitespace that ends it, unless that is all of it.
fn trim_end(shown: &[(usize, char)], range: Range<usize>) -> Range<usize> {
    let mut end = range.end;
    while let Some(&(at, _)) = shown
        .iter()
        .find(|(at, c)| at + c.len_utf8() == end && *at >= range.start)
        .filter(|(_, c)| c.is_whitespace())
    {
        end = at;
    }
    match range.start < end {
        true => range.start..end,
        false => range,
    }
}

/// Where `at`, an offset in `shown`, is in `text`, which has as many
/// characters.
fn unreplaced(text: &str, shown: &str, at: usize) -> usize {
    if text.len() == shown.len() {
        return at;
    }
    let chars = shown[..at].chars().count();
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(ix, _)| ix)
}

impl<'a> Renderer<'a> {
    fn new(style: &'a MarkupStyle, select: Option<SelectBody>) -> Self {
        Self {
            style,
            next_id: 0,
            scrolls: None,
            list_depth: 0,
            tight_item: false,
            heading_size: None,
            select,
            paragraphs: 0,
            path: Vec::new(),
            marks: &[],
            #[cfg(any(test, feature = "capture"))]
            drawn: Vec::new(),
        }
    }

    /// `doc`'s blocks, or for Slack what Slack posts.
    fn body(&mut self, doc: &Document) -> Vec<AnyElement> {
        let gap = self.style.block_gap;
        if self.style.slack_fallbacks {
            let posted = markup::slack::posted(doc);
            self.decorated_blocks(posted.iter().map(|(block, _)| block), gap, |_, block| block)
        } else {
            self.blocks(&doc.blocks, gap)
        }
    }

    /// What makes the next paragraph selectable, in reading order.
    fn select(&mut self) -> Option<Select> {
        let body = self.select.as_ref()?;
        self.paragraphs += 1;
        Some(Select {
            state: body.state.clone(),
            key: (body.body, self.paragraphs),
            color: body.color,
            registry: body.registry.clone(),
        })
    }

    /// `text` in the surrounding style, selectable like other paragraphs.
    fn plain(&mut self, text: &str) -> InlineText {
        let segment = Segment {
            text: SharedString::from(text.to_owned()),
            ..Segment::default()
        };
        self.inline(vec![Placed::new(segment, 0..text.len())])
            .selectable(self.select())
    }

    /// `text`, code in `lang`, in the surrounding style, highlighted when the
    /// style has [`MarkupStyle::syntax`] colors and `lang` a grammar.
    fn code(&mut self, lang: Option<&str>, text: &str) -> InlineText {
        let highlighted = self
            .style
            .syntax
            .as_ref()
            .zip(lang)
            .and_then(|(colors, lang)| Some((colors, markup::highlight::highlight(lang, text)?)));
        let Some((colors, tokens)) = highlighted else {
            return self.plain(text);
        };
        let placed = |range: Range<usize>| {
            let segment = Segment {
                text: SharedString::from(text[range.clone()].to_owned()),
                ..Segment::default()
            };
            Placed::new(segment, range)
        };
        let mut segments = Vec::with_capacity(tokens.len() * 2 + 1);
        let mut at = 0;
        for (range, token) in tokens.iter() {
            if at < range.start {
                segments.push(placed(at..range.start));
            }
            let mut token_segment = placed(range.clone());
            colors.style(&mut token_segment.segment, *token);
            segments.push(token_segment);
            at = range.end;
        }
        if at < text.len() || segments.is_empty() {
            segments.push(placed(at..text.len()));
        }
        self.inline(segments).selectable(self.select())
    }

    /// The text of the node being rendered, in `segments`, with the
    /// redline's marks on it drawn.
    fn inline(&mut self, segments: Vec<Placed>) -> InlineText {
        let all = self.marks;
        let marks: Vec<&TextMark> = all.iter().filter(|mark| mark.path == self.path).collect();
        let segments = self.marked(segments, &marks);
        #[cfg(any(test, feature = "capture"))]
        self.drawn.push((self.path.clone(), segments.clone()));
        let handlers = self
            .style
            .redline
            .as_ref()
            .map(|redline| redline.handlers.clone());
        InlineText::new(segments).marks(handlers)
    }

    /// `segments` split where `marks` start and end, with each part a mark
    /// covers drawn as that mark, and a [`POINT`] at each empty mark. A mark
    /// with visible text leaves the whitespace that ends it untinted.
    fn marked(&self, segments: Vec<Placed>, marks: &[&TextMark]) -> Vec<Segment> {
        let Some(redline) = self.style.redline.as_deref().filter(|_| !marks.is_empty()) else {
            return segments.into_iter().map(|placed| placed.segment).collect();
        };
        let (mut points, ranges): (Vec<&TextMark>, Vec<&TextMark>) =
            marks.iter().partition(|mark| mark.text.is_empty());
        let shown = shown_chars(&segments);
        let ranges: Vec<TextMark> = ranges
            .into_iter()
            .map(|mark| TextMark {
                text: trim_end(&shown, mark.text.clone()),
                ..mark.clone()
            })
            .collect();
        points.sort_by_key(|point| point.text.start);
        let mut points = points.into_iter().peekable();
        let point = |base: &Segment, mark: Mark| {
            let mut segment = Segment {
                text: POINT.into(),
                strike: false,
                chip: None,
                link: None,
                ..base.clone()
            };
            redline.style(
                &mut segment,
                Mark {
                    point: true,
                    ..mark
                },
            );
            segment
        };
        let covering = |range: Range<usize>| {
            ranges
                .iter()
                .find(|mark| mark.text.start <= range.start && range.end <= mark.text.end)
                .map(|mark| mark.mark)
        };
        let mut out = Vec::with_capacity(segments.len() + marks.len() * 2);
        for placed in segments {
            let source = placed.source.clone();
            while let Some(at) = points.next_if(|point| point.text.start <= source.start) {
                out.push(point(&placed.segment, at.mark));
            }
            if placed.whole {
                let mut segment = placed.segment;
                let touched = ranges
                    .iter()
                    .find(|mark| mark.text.start < source.end && source.start < mark.text.end);
                if let Some(mark) = touched {
                    redline.style(&mut segment, mark.mark);
                }
                out.push(segment);
                continue;
            }
            let mut cuts: Vec<usize> = ranges
                .iter()
                .flat_map(|mark| [mark.text.start, mark.text.end])
                .chain(points.clone().map(|point| point.text.start))
                .filter(|&at| source.start < at && at < source.end)
                .collect();
            cuts.sort_unstable();
            cuts.dedup();
            let mut start = source.start;
            for end in cuts.into_iter().chain([source.end]) {
                if start < end || source.is_empty() {
                    let text = &placed.segment.text[placed.local(start)..placed.local(end)];
                    let mut segment = Segment {
                        text: text.to_owned().into(),
                        ..placed.segment.clone()
                    };
                    if let Some(mark) = covering(start..end) {
                        redline.style(&mut segment, mark);
                    }
                    out.push(segment);
                }
                if end < source.end {
                    while let Some(at) = points.next_if(|point| point.text.start <= end) {
                        out.push(point(&placed.segment, at.mark));
                    }
                }
                start = end;
            }
        }
        let base = out.last().cloned().unwrap_or_default();
        out.extend(points.map(|at| point(&base, at.mark)));
        out
    }

    fn id(&mut self) -> ElementId {
        self.next_id += 1;
        ElementId::Integer(self.next_id)
    }

    fn scroll_handle(&self) -> Option<ScrollHandle> {
        self.scrolls.as_ref().map(|scrolls| {
            scrolls
                .borrow_mut()
                .entry(self.next_id)
                .or_default()
                .clone()
        })
    }

    fn gap_before(&self, block: &Block, prev: Option<&Block>, gap: Pixels) -> Pixels {
        // Language code blocks retain their own margin next to quotes and code.
        let separate = |block: &Block| {
            self.style.language_code.is_some() && matches!(block, Block::Code { lang: Some(_), .. })
        };
        let margined = |block: &Block| matches!(block, Block::Code { .. } | Block::Quote(_));
        match (&self.style.margins, prev) {
            (Some(margins), Some(prev)) => {
                self.margin(margins, prev).max(self.margin(margins, block))
            }
            (Some(_), None) => px(0.),
            (None, _) => match block {
                _ if prev.is_none() && separate(block) => gap,
                _ if prev.is_none() => px(0.),
                Block::Heading { .. } => self.style.heading_gap,
                _ if prev.is_some_and(is_text) && is_text(block) => self.style.text_gap.min(gap),
                _ if prev.is_some_and(|prev| {
                    separate(block) && margined(prev) || separate(prev) && margined(block)
                }) =>
                {
                    gap * 2.
                }
                _ => gap,
            },
        }
    }

    /// Renders sibling blocks, separated by `gap` (wider above headings).
    fn blocks(&mut self, blocks: &[Block], gap: Pixels) -> Vec<AnyElement> {
        self.decorated_blocks(blocks, gap, |_, block| block)
    }

    /// Renders sibling blocks like [`Self::blocks`], passing each through
    /// `decorate` with its index before it is spaced.
    fn decorated_blocks<'b>(
        &mut self,
        blocks: impl IntoIterator<Item = &'b Block>,
        gap: Pixels,
        mut decorate: impl FnMut(usize, AnyElement) -> AnyElement,
    ) -> Vec<AnyElement> {
        let mut prev: Option<&Block> = None;
        let top = self.path.is_empty();
        blocks
            .into_iter()
            .enumerate()
            .map(|(ix, block)| {
                let gap = self.gap_before(block, prev, gap);
                prev = Some(block);
                if top && let Some(redline) = self.style.redline.as_deref() {
                    self.marks = redline.marks.block(ix);
                }
                self.path.push(ix);
                let block = self.block(block);
                self.path.pop();
                div()
                    .w_full()
                    .pt(gap)
                    .child(decorate(ix, block))
                    .into_any_element()
            })
            .collect()
    }

    /// `block`'s margin among its siblings with [`MarkupStyle::margins`].
    fn margin(&self, margins: &Margins, block: &Block) -> Pixels {
        let style = self.style;
        match block {
            Block::Heading { level, .. } => {
                let level = (*level as usize).clamp(1, 6) - 1;
                style.text_size * style.heading_scale[level] * margins.heading[level]
            }
            Block::List(_) if self.list_depth > 0 => px(0.),
            Block::Paragraph(_) if self.tight_item => px(0.),
            Block::Code { .. } => style.code_text_size,
            Block::Table(_) => px(0.),
            Block::Rule => margins.rule,
            _ => style.text_size,
        }
    }

    fn block(&mut self, block: &Block) -> AnyElement {
        let style = self.style;
        match block {
            Block::Paragraph(spans) => self.text(spans),
            Block::Heading { level, spans } => {
                let scale = style.heading_scale[(*level as usize).clamp(1, 6) - 1];
                self.heading_size = Some(style.text_size * scale);
                let text = self.text(spans);
                self.heading_size = None;
                div()
                    .text_size(style.text_size * scale)
                    .line_height(relative(style.heading_line_height))
                    .font_weight(style.heading_weight)
                    .when(*level == 6 && style.muted_h6, |this| {
                        this.text_color(style.muted_color)
                    })
                    .when(*level <= style.heading_rule_max_level, |this| {
                        this.pb(style.text_size * scale * 0.3)
                            .border_b_1()
                            .border_color(style.heading_rule_color)
                    })
                    .child(text)
                    .into_any_element()
            }
            Block::List(list) => self.list(list),
            Block::Quote(blocks) => div()
                .relative()
                .pl(style.quote_bar_width + style.quote_padding)
                .pr(style.quote_padding)
                .text_color(style.quote_text_color)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(style.quote_bar_width)
                        .rounded(style.quote_bar_radius)
                        .bg(style.quote_bar_color),
                )
                .children(self.blocks(blocks, style.block_gap))
                .into_any_element(),
            Block::Alert { kind, blocks } => {
                let color = style.alert_colors.get(*kind);
                div()
                    .border_l(style.quote_bar_width)
                    .border_color(color)
                    .py(px(8.))
                    .px(px(16.))
                    .child(
                        // A one-line-high title with its octicon, a
                        // paragraph's margin above the body.
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .mb(px(16.))
                            .line_height(relative(1.))
                            .text_color(color)
                            .font_weight(FontWeight::MEDIUM)
                            .child(
                                svg()
                                    .path(alert_icon(*kind))
                                    .size(px(16.))
                                    .flex_none()
                                    .text_color(color),
                            )
                            .child(kind.label()),
                    )
                    .children(self.blocks(blocks, style.block_gap))
                    .into_any_element()
            }
            Block::Code {
                lang: Some(lang),
                text,
            } if style.language_code.is_some() => self.language_code(lang, text),
            Block::Code { lang, text } if style.code_wraps => self
                .code_box()
                .font_family(style.mono_family.clone())
                .text_size(style.code_text_size)
                .line_height(relative(style.code_line_height))
                .child(self.code(lang.as_deref(), text))
                .into_any_element(),
            Block::Code { lang, text } => self
                .code_box()
                .id(self.id())
                .when_some(self.scroll_handle(), |div, scroll| {
                    div.track_scroll(&scroll)
                })
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .flex()
                .child(
                    // Unshrunk so long lines widen the content and scroll.
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .font_family(style.mono_family.clone())
                        .text_size(style.code_text_size)
                        .line_height(relative(style.code_line_height))
                        .child(self.code(lang.as_deref(), text)),
                )
                .into_any_element(),
            Block::Table(table) => self.table(table),
            Block::Rule => div()
                .py(style.rule_padding)
                .child(div().h(style.rule_height).bg(style.rule_color))
                .into_any_element(),
            Block::Html(html) => div()
                .text_color(style.muted_color)
                .font_family(style.mono_family.clone())
                .text_size(style.code_text_size)
                .child(self.plain(html))
                .into_any_element(),
        }
    }

    /// A code block with a language as Slack draws it: inset in the message,
    /// a header with the language's name and a copy button, line numbers
    /// unless Slack doesn't know the language, and diff colors. Lines never
    /// wrap; the code scrolls sideways under fixed line numbers.
    fn language_code(&mut self, lang: &str, text: &str) -> AnyElement {
        let style = self.style;
        let colors = style
            .language_code
            .as_ref()
            .expect("only called with a language code style");
        let language = markup::slack::code_language(lang);
        let label = match language {
            CodeLanguage::Known(name) => name,
            CodeLanguage::Plain | CodeLanguage::Unknown => lang,
        };
        let lines: Vec<&str> = text.split('\n').collect();
        // An 18px line whose text is 16px high, the height a run's
        // background covers in Slack.
        let line = || div().h(px(18.)).pt(px(1.)).line_height(px(16.));
        let numbers = (language != CodeLanguage::Unknown).then(|| {
            div()
                .flex_none()
                .w(px(35.))
                .pr(px(10.))
                .mr(px(10.))
                .flex()
                .flex_col()
                .items_end()
                .text_color(colors.line_number)
                .children((1..=lines.len()).map(|n| line().child(n.to_string())))
        });
        let all = self.marks;
        let marks: Vec<&TextMark> = all.iter().filter(|mark| mark.path == self.path).collect();
        let mut at = 0;
        let mut code_lines = Vec::with_capacity(lines.len());
        for text in &lines {
            let source = at..at + text.len();
            at = source.end + 1;
            // A line with changes shows them, without its diff colors.
            let on_line: Vec<&TextMark> = marks
                .iter()
                .filter(|mark| match mark.text.is_empty() {
                    true => source.contains(&mark.text.start) || mark.text.start == source.end,
                    false => mark.text.start < source.end && source.start < mark.text.end,
                })
                .copied()
                .collect();
            if !on_line.is_empty() {
                let segment = Segment {
                    text: SharedString::from(text.to_string()),
                    ..Segment::default()
                };
                let segments = self.marked(vec![Placed::new(segment, source)], &on_line);
                #[cfg(any(test, feature = "capture"))]
                self.drawn.push((self.path.clone(), segments.clone()));
                let handlers = style
                    .redline
                    .as_ref()
                    .map(|redline| redline.handlers.clone());
                code_lines.push(line().child(InlineText::new(segments).marks(handlers)));
                continue;
            }
            #[cfg(any(test, feature = "capture"))]
            self.drawn.push((
                self.path.clone(),
                vec![Segment {
                    text: SharedString::from(text.to_string()),
                    ..Segment::default()
                }],
            ));
            let highlights = if lang == "diff" {
                markup::slack::diff_line(text)
                    .into_iter()
                    .map(|(range, role)| (range, colors.highlight(role)))
                    .collect()
            } else {
                Vec::new()
            };
            code_lines
                .push(line().child(StyledText::new(text.to_string()).with_highlights(highlights)));
        }
        let code = div()
            .flex_none()
            .pr(px(20.))
            .text_color(if language == CodeLanguage::Unknown {
                colors.unknown_text
            } else {
                colors.text
            })
            .children(code_lines);
        let header = div()
            .h(px(40.))
            .pl(px(12.))
            .pr(px(4.))
            .flex()
            .items_center()
            .justify_between()
            .text_size(px(13.))
            .line_height(px(18.))
            .font_weight(FontWeight::BOLD)
            .text_color(colors.label)
            .child(SharedString::from(label.to_owned()))
            .child(
                div()
                    .size(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path(SLACK_COPY_ICON)
                            .size(px(16.))
                            .text_color(colors.icon),
                    ),
            );
        div()
            .px(px(8.))
            .py(px(12.))
            .child(
                div()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.background)
                    .child(header)
                    .child(
                        div()
                            .flex()
                            .pl(px(8.))
                            .pb(px(16.))
                            .font_family(style.mono_family.clone())
                            .text_size(px(12.))
                            .whitespace_nowrap()
                            .children(numbers)
                            .child(
                                div()
                                    .id(self.id())
                                    .when_some(self.scroll_handle(), |div, scroll| {
                                        div.track_scroll(&scroll)
                                    })
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_x_scroll()
                                    .restrict_scroll_to_axis()
                                    .flex()
                                    .child(code),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn code_box(&self) -> gpui::Div {
        let style = self.style;
        div()
            .p(style.code_padding)
            .rounded(style.radius)
            .bg(style.code_background)
            .when_some(style.code_border, |this, color| {
                this.border_1().border_color(color)
            })
    }

    fn list(&mut self, list: &List) -> AnyElement {
        let style = self.style;
        let (item_gap, inner_gap) = if list.tight {
            (style.tight_item_gap, px(0.))
        } else {
            (style.block_gap, style.block_gap)
        };
        let bullet_kind = self.list_depth % 3;
        let numbering = style.numbering[self.list_depth % 3];
        self.list_depth += 1;
        let outer_tight = std::mem::replace(&mut self.tight_item, list.tight);
        let items = list
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                self.path.push(ix);
                let marker = match (item.task, list.start) {
                    // GitHub pulls the checkbox 1.4em left of the text.
                    (Some(checked), _) => div()
                        .pl(style.list_indent - style.text_size * 1.4)
                        .child(self.checkbox(checked))
                        .into_any_element(),
                    (None, Some(start)) => div()
                        .flex()
                        .justify_end()
                        .pr(style.ordered_marker_gap)
                        .child(format!("{}.", ordered_marker(start + ix as u64, numbering)))
                        .into_any_element(),
                    (None, None) => bullet(&style.bullet, bullet_kind, style.text_color),
                };
                let blocks = self.blocks(&item.blocks, inner_gap);
                self.path.pop();
                div()
                    .when(ix > 0, |this| this.pt(item_gap))
                    .flex()
                    .child(div().w(style.list_indent).flex_none().child(marker))
                    .child(div().flex_1().min_w_0().flex().flex_col().children(blocks))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        self.list_depth -= 1;
        self.tight_item = outer_tight;
        div().flex().flex_col().children(items).into_any_element()
    }

    /// The checkbox of the task list item at `self.path`, which toggles
    /// it when the style takes clicks and no redline is drawn.
    fn checkbox(&mut self, checked: bool) -> AnyElement {
        let style = self.style;
        let size = px(13.);
        // Centered on the text rather than the line box, whose center sits
        // above the text's because of the space reserved for descenders.
        let top = (style.text_size * style.line_height - size) / 2. + px(1.);
        let check = div()
            .size(size)
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.))
            .border_1()
            .when_else(
                checked,
                |this| {
                    this.bg(style.checkbox_checked)
                        .border_color(style.checkbox_checked)
                        .child(
                            svg()
                                .path(CHECK_ICON)
                                .size(px(11.))
                                .text_color(gpui::white()),
                        )
                },
                |this| {
                    this.bg(style.checkbox_unchecked_background)
                        .border_color(style.checkbox_unchecked_border)
                },
            );
        let check = match style.on_task.clone().filter(|_| style.redline.is_none()) {
            Some(on_task) => {
                let path = self.path.clone();
                check
                    .id(self.id())
                    .cursor_pointer()
                    // Keeps the click from starting a text selection.
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        on_task(&path, window, cx);
                    })
                    .into_any_element()
            }
            None => check.into_any_element(),
        };
        div()
            .h(style.text_size * style.line_height)
            .pt(top)
            .child(check)
            .into_any_element()
    }

    fn table(&mut self, table: &Table) -> AnyElement {
        let style = self.style;
        let columns = table.alignments.len();
        let rows = std::iter::once(&table.header).chain(&table.rows);
        let mut cells = Vec::new();
        for (row_ix, row) in rows.enumerate() {
            for (col_ix, spans) in row.iter().enumerate() {
                let (pad_x, pad_y) = style.table_cell_padding;
                let cell = div()
                    .px(pad_x)
                    .py(pad_y)
                    .border_r_1()
                    .border_b_1()
                    .border_color(style.border_color)
                    .when(row_ix == 0, |this| {
                        this.border_t_1().font_weight(style.strong_weight)
                    })
                    .when(col_ix == 0, |this| this.border_l_1())
                    // Body rows alternate, starting unshaded.
                    .when(row_ix > 0 && row_ix % 2 == 0, |this| {
                        this.bg(style.table_stripe)
                    });
                let cell = match table.alignments.get(col_ix) {
                    // Header cells default to centered, as in browsers.
                    Some(Alignment::None) if row_ix == 0 => cell.text_center(),
                    Some(Alignment::Center) => cell.text_center(),
                    Some(Alignment::Right) => cell.text_right(),
                    _ => cell,
                };
                self.path.extend([row_ix, col_ix]);
                cells.push(cell.child(self.text(spans)));
                self.path.truncate(self.path.len() - 2);
            }
        }
        div()
            .id(self.id())
            .when_some(self.scroll_handle(), |div, scroll| {
                div.track_scroll(&scroll)
            })
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .flex()
            .child(
                // Like GitHub, the table takes its max-content width and
                // scrolls instead of wrapping cells.
                div()
                    .flex_none()
                    .grid()
                    .grid_cols_max_content(columns as u16)
                    .children(cells),
            )
            .into_any_element()
    }

    /// Inline content as one wrapped paragraph; web and mail links open on
    /// click.
    fn text(&mut self, spans: &[Span]) -> AnyElement {
        let segments = self.segments(spans);
        self.inline(segments)
            .blank_line(self.style.blank_line)
            .selectable(self.select())
            .into_any_element()
    }

    /// How `spans` show, each part placed in their text.
    fn segments(&self, spans: &[Span]) -> Vec<Placed> {
        let style = self.style;
        let curled;
        let shown = if style.smart_quotes {
            curled = smart_quotes(spans);
            &curled
        } else {
            spans
        };
        let mut segments = Vec::new();
        let mut at = 0;
        for (span, shown) in spans.iter().zip(shown) {
            let source = at..at + span.text.len();
            at = source.end;
            let replaced = (shown.text != span.text).then(|| span.text.clone());
            let base = Segment {
                weight: span.style.strong.then_some(style.strong_weight),
                italic: span.style.emph,
                strike: span.style.strike,
                link: span
                    .link
                    .as_deref()
                    .filter(|url| is_openable(url))
                    .map(|url| url.to_owned().into()),
                ..Segment::default()
            };
            let boxed = |inline: &InlineBox, text: String| Segment {
                text: text.into(),
                scale: Some(inline.scale),
                color: inline.color,
                chip: Some(inline.chip.clone()),
                ..base.clone()
            };
            if span.style.code {
                // Code in a heading keeps the heading's size, with less
                // padding.
                let heading_code = self.heading_size.map(|size| InlineBox {
                    scale: 1.,
                    color: style.inline_code.color,
                    chip: Chip {
                        pad_x: size * 0.2,
                        pad_top: px(0.),
                        pad_bottom: px(0.),
                        ..style.inline_code.chip.clone()
                    },
                });
                let segment = Segment {
                    family: Some(style.mono_family.clone()),
                    ..boxed(
                        heading_code.as_ref().unwrap_or(&style.inline_code),
                        shown.text.clone(),
                    )
                };
                segments.push(Placed {
                    replaced,
                    ..Placed::new(segment, source)
                });
            } else if let Some(url) = &shown.link {
                match &style.url_slug {
                    Some(slug) if shown.text == *url => segments.push(Placed {
                        whole: true,
                        ..Placed::new(boxed(slug, url_label(url)), source)
                    }),
                    _ => segments.push(Placed {
                        replaced,
                        ..Placed::new(
                            Segment {
                                text: shown.text.clone().into(),
                                color: Some(style.link_color),
                                underline: style.link_underline,
                                ..base
                            },
                            source,
                        )
                    }),
                }
            } else {
                for (run, kind) in runs(&shown.text, style.mention.is_some()) {
                    let start = run.as_ptr() as usize - shown.text.as_ptr() as usize;
                    let run_source = source.start + unreplaced(&span.text, &shown.text, start)
                        ..source.start + unreplaced(&span.text, &shown.text, start + run.len());
                    let replaced = replaced.as_ref().map(|_| {
                        span.text[run_source.start - source.start..run_source.end - source.start]
                            .to_owned()
                    });
                    let placed = |segment| Placed {
                        replaced,
                        ..Placed::new(segment, run_source)
                    };
                    segments.push(match kind {
                        Run::Text => placed(Segment {
                            text: run.to_owned().into(),
                            ..base.clone()
                        }),
                        Run::Emoji => placed(Segment {
                            text: run.to_owned().into(),
                            scale: Some(style.emoji_scale),
                            ..base.clone()
                        }),
                        Run::Mention => Placed {
                            whole: true,
                            ..placed(boxed(style.mention.as_ref().unwrap(), mention_label(run)))
                        },
                    });
                }
            }
        }
        segments
    }
}

/// `spans` with quotes curled across span boundaries, leaving code and URLs
/// shown as themselves alone.
fn smart_quotes(spans: &[Span]) -> Vec<Span> {
    let verbatim = |span: &Span| span.style.code || span.link.as_ref() == Some(&span.text);
    let text: Vec<char> = spans
        .iter()
        .flat_map(|span| {
            let masked = verbatim(span);
            span.text
                .chars()
                .map(move |c| if masked { '\u{fffc}' } else { c })
        })
        .collect();
    let mut curled = markup::slack::smart_quotes(&text).into_iter();
    spans
        .iter()
        .map(|span| {
            let chars = curled.by_ref().take(span.text.chars().count());
            Span {
                text: if verbatim(span) {
                    chars.for_each(drop);
                    span.text.clone()
                } else {
                    chars.collect()
                },
                ..span.clone()
            }
        })
        .collect()
}

#[derive(Debug, PartialEq)]
enum Run {
    Text,
    Emoji,
    /// `@name` at the start of a word, or a Slack mention in angle
    /// brackets: `<@U123>`, `<@U123|name>`, `<#C123|channel>`, `<!here>`.
    Mention,
}

/// How a mention run shows: Slack's bracketed forms as `@name`, `#channel`
/// or `@here`, preferring the label after `|`. Unlabeled users get fictional
/// preview names; the document and clipboard retain the Slack IDs.
fn mention_label(run: &str) -> String {
    let Some(inner) = run.strip_prefix('<').and_then(|r| r.strip_suffix('>')) else {
        return run.to_owned();
    };
    let (sigil, rest) = inner.split_at(1);
    let (id, label) = rest.split_once('|').unwrap_or((rest, ""));
    if sigil == "@" && label.is_empty() {
        return preview_user_name(id);
    }
    let name = if label.is_empty() { id } else { label };
    let sigil = if sigil == "#" { "#" } else { "@" };
    // `<!subteam^S123|@team>` labels already carry the sigil.
    format!("{sigil}{}", name.strip_prefix(sigil).unwrap_or(name))
}

/// Stable fictional names with a nod to drafting and writing.
fn preview_user_name(id: &str) -> String {
    const FIRST: [&str; 16] = [
        "Ada", "Alex", "Amir", "Clara", "Elliot", "Felix", "Iris", "Jamie", "Jules", "Leo", "Maya",
        "Nora", "Oscar", "Robin", "Sam", "Zoe",
    ];
    const LAST: [&str; 16] = [
        "Archer", "Blake", "Booker", "Byron", "Ellis", "Folio", "Gray", "Hart", "Lane", "Page",
        "Parker", "Penn", "Quill", "Reed", "Wilde", "Wren",
    ];
    // FNV-1a fixes the mapping across processes, platforms and Rust versions.
    let hash = id.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    let first = FIRST[(hash % FIRST.len() as u64) as usize];
    let last = LAST[(hash / FIRST.len() as u64 % LAST.len() as u64) as usize];
    format!("@{first} {last}")
}

/// Length of a Slack mention in angle brackets at the start of `text`.
fn bracketed_mention(text: &str) -> Option<usize> {
    let rest = text.strip_prefix('<')?;
    if !rest.starts_with(['@', '#', '!']) {
        return None;
    }
    let end = rest.find(|c: char| c == '>' || c == '<' || c.is_whitespace() && c != ' ')?;
    let inner = &rest[1..end];
    let id = inner.split_once('|').map_or(inner, |(id, _)| id);
    let valid = rest[end..].starts_with('>')
        && !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '^' | '_' | '-'));
    valid.then_some(end + 2)
}

/// Splits plain text into emoji, mentions (when `mentions`) and other
/// text.
fn runs(text: &str, mentions: bool) -> Vec<(&str, Run)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    let mut prev: Option<char> = None;
    while let Some((ix, c)) = chars.next() {
        let run = if is_emoji(c) {
            let mut end = ix + c.len_utf8();
            // Variation selectors, joiners and the pictographs they join.
            while let Some(&(at, n)) = chars.peek() {
                if !(is_emoji(n) || matches!(n, '\u{fe0f}' | '\u{200d}' | '\u{20e3}')) {
                    break;
                }
                end = at + n.len_utf8();
                chars.next();
            }
            Some((end, Run::Emoji))
        } else if let Some(len) = (c == '<' && mentions)
            .then(|| bracketed_mention(&text[ix..]))
            .flatten()
        {
            let end = ix + len;
            while chars.peek().is_some_and(|&(at, _)| at < end) {
                chars.next();
            }
            Some((end, Run::Mention))
        } else if c == '@' && mentions && !prev.is_some_and(|p| p.is_alphanumeric()) {
            let name = text[ix + 1..]
                .find(|n: char| !(n.is_alphanumeric() || matches!(n, '.' | '_' | '-')))
                .map_or(text.len(), |len| ix + 1 + len);
            let end = ix + 1 + text[ix + 1..name].trim_end_matches(['.', '-']).len();
            (end > ix + 1).then(|| {
                while chars.peek().is_some_and(|&(at, _)| at < end) {
                    chars.next();
                }
                (end, Run::Mention)
            })
        } else {
            None
        };
        if let Some((end, kind)) = run {
            if start < ix {
                out.push((&text[start..ix], Run::Text));
            }
            out.push((&text[ix..end], kind));
            start = end;
            prev = text[..end].chars().next_back();
        } else {
            prev = Some(c);
        }
    }
    if start < text.len() {
        out.push((&text[start..], Run::Text));
    }
    out
}

/// Slack's label for a link shown as its URL: without the scheme, `www.`,
/// port and trailing slash, with a fragment and tracking-style queries
/// (`ref`, `id` or a bare key) collapsed to `#…` and `?…`. Slack resolves the
/// label on its servers, so this approximates it.
fn url_label(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, _)) => (rest, "#…"),
        None => (rest, ""),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => {
            let collapse = query.split('&').any(|pair| match pair.split_once('=') {
                Some((key, _)) => matches!(key, "ref" | "id"),
                None => true,
            });
            (
                rest,
                if collapse {
                    "?…".to_owned()
                } else {
                    format!("?{query}")
                },
            )
        }
        None => (rest, String::new()),
    };
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = match host.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => host,
    };
    format!("{host}{}{query}{fragment}", path.trim_end_matches('/'))
}

fn alert_icon(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Note => ALERT_NOTE_ICON,
        AlertKind::Tip => ALERT_TIP_ICON,
        AlertKind::Important => ALERT_IMPORTANT_ICON,
        AlertKind::Warning => ALERT_WARNING_ICON,
        AlertKind::Caution => ALERT_CAUTION_ICON,
    }
}

fn is_text(block: &Block) -> bool {
    matches!(
        block,
        Block::Paragraph(_) | Block::Heading { .. } | Block::List(_)
    )
}

/// A disc, ring or square (`kind` 0, 1, 2) in CSS list marker style.
fn bullet(bullet: &Bullet, kind: usize, color: Hsla) -> AnyElement {
    let shape = div().size(bullet.size).ml(bullet.left).mt(bullet.top);
    match kind {
        1 => shape
            .rounded_full()
            .border(bullet.ring_width)
            .border_color(color),
        0 => shape.rounded_full().bg(color),
        _ => shape.bg(color),
    }
    .into_any_element()
}

/// The marker for item `number` of an ordered list; numbers out of the
/// numbering's range fall back to decimal.
fn ordered_marker(number: u64, numbering: Numbering) -> String {
    match numbering {
        Numbering::Alpha if number > 0 => {
            let mut n = number;
            let mut letters = Vec::new();
            while n > 0 {
                n -= 1;
                letters.push(b'a' + (n % 26) as u8);
                n /= 26;
            }
            letters.iter().rev().map(|&b| b as char).collect()
        }
        Numbering::Roman if (1..4000).contains(&number) => {
            let mut n = number;
            let mut out = String::new();
            for (value, numeral) in [
                (1000, "m"),
                (900, "cm"),
                (500, "d"),
                (400, "cd"),
                (100, "c"),
                (90, "xc"),
                (50, "l"),
                (40, "xl"),
                (10, "x"),
                (9, "ix"),
                (5, "v"),
                (4, "iv"),
                (1, "i"),
            ] {
                while n >= value {
                    out.push_str(numeral);
                    n -= value;
                }
            }
            out
        }
        _ => number.to_string(),
    }
}

/// Only web and mail links open. Drafts are agent-written, so other schemes
/// (`file:`, custom app URLs) and relative links stay inert.
fn is_openable(url: &str) -> bool {
    ["https://", "http://", "mailto:"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn wrapped_paragraphs_look_wrapped() {
        let wrapped = "Draftmux gives the message its own window. Hand it off from\n\
                       Claude Code or pi, and an agent forked from the session writes\n\
                       the first draft.\n";
        let github = |text| super::preview(Target::Github, text);
        assert!(super::looks_wrapped(&github(wrapped)));
        let item = format!("- {}", wrapped.replace('\n', "\n  "));
        assert!(super::looks_wrapped(&github(&item)));
        // Short lines are broken on purpose, and one long break is not a
        // pattern.
        assert!(!super::looks_wrapped(&github(
            "Thanks!\nI'll fix it today.\n"
        )));
        assert!(!super::looks_wrapped(&github(
            "Draftmux gives the message its own window. Hand it off from\nhere.\n"
        )));
        assert!(!super::looks_wrapped(&super::preview(
            Target::GithubFile,
            wrapped
        )));
    }

    #[test]
    fn every_preview_format_excludes_frontmatter() {
        for target in draftmux_storage::Target::ALL {
            let source = "---\ntitle: Secret title\ncc: private@example.com\n---\nBody";
            assert_eq!(
                super::preview(*target, source),
                super::preview(*target, "Body")
            );
        }
    }

    use std::rc::Rc;

    use super::Numbering::{Alpha, Decimal, Roman};
    use super::{
        Run, Segment, is_openable, mention_label, ordered_marker, reveal, runs, url_label,
    };
    use draftmux_core::decorations::Side;
    use draftmux_storage::Target;
    use gpui::{Bounds, point, px, size};

    #[test]
    fn reveals_with_least_scrolling() {
        let viewport = Bounds::new(point(px(0.), px(100.)), size(px(300.), px(200.)));
        let at =
            |top: f32, height: f32| Bounds::new(point(px(10.), px(top)), size(px(50.), px(height)));
        let offset = point(px(0.), px(-40.));
        // Visible: stays put.
        assert_eq!(reveal(viewport, at(150., 20.), offset), offset);
        // Above: its top meets the viewport's.
        assert_eq!(
            reveal(viewport, at(60., 20.), offset),
            point(px(0.), px(0.))
        );
        // Below: its bottom meets the viewport's.
        assert_eq!(
            reveal(viewport, at(310., 20.), offset),
            point(px(0.), px(-70.))
        );
        // Taller than the viewport: its top meets the viewport's.
        assert_eq!(
            reveal(viewport, at(250., 400.), offset),
            point(px(0.), px(-190.))
        );
    }

    #[test]
    fn numbers_ordered_items() {
        assert_eq!(ordered_marker(3, Decimal), "3");
        assert_eq!(ordered_marker(1, Alpha), "a");
        assert_eq!(ordered_marker(27, Alpha), "aa");
        assert_eq!(ordered_marker(4, Roman), "iv");
        assert_eq!(ordered_marker(14, Roman), "xiv");
        assert_eq!(ordered_marker(0, Alpha), "0");
        assert_eq!(ordered_marker(4000, Roman), "4000");
    }

    #[test]
    fn splits_emoji_and_mentions() {
        assert_eq!(
            runs("Hi @raine. 🎉👍🏽 a@b.c", true),
            [
                ("Hi ", Run::Text),
                ("@raine", Run::Mention),
                (". ", Run::Text),
                ("🎉👍🏽", Run::Emoji),
                (" a@b.c", Run::Text),
            ]
        );
        assert_eq!(runs("@raine", false), [("@raine", Run::Text)]);
    }

    #[test]
    fn splits_slack_bracketed_mentions() {
        assert_eq!(
            runs(
                "Thanks <@W010J2WH545>, see <#C1|dev team> <!here> <@ x> <a>",
                true
            ),
            [
                ("Thanks ", Run::Text),
                ("<@W010J2WH545>", Run::Mention),
                (", see ", Run::Text),
                ("<#C1|dev team>", Run::Mention),
                (" ", Run::Text),
                ("<!here>", Run::Mention),
                (" <@ x> <a>", Run::Text),
            ]
        );
        assert_eq!(mention_label("<@W010J2WH545>"), "@Jules Penn");
        assert_eq!(mention_label("<@U1|raine>"), "@raine");
        assert_eq!(mention_label("<#C1|dev team>"), "#dev team");
        assert_eq!(mention_label("<#C1>"), "#C1");
        assert_eq!(mention_label("<!here>"), "@here");
        assert_eq!(mention_label("<!subteam^S1|@eng>"), "@eng");
        assert_eq!(mention_label("@raine"), "@raine");
    }

    #[test]
    fn preview_user_names_are_stable_and_display_only() {
        let source = "Thanks <@W010J2WH545> and <@U066F0DKH7G>, <@W010J2WH545>!";
        let parts = runs(source, true);
        let names: Vec<_> = parts
            .iter()
            .filter(|(_, kind)| *kind == Run::Mention)
            .map(|(run, _)| mention_label(run))
            .collect();
        assert_eq!(names, ["@Jules Penn", "@Zoe Lane", "@Jules Penn"]);
        assert_eq!(
            parts.iter().map(|(run, _)| *run).collect::<String>(),
            source
        );
        assert_eq!(mention_label("<@W010J2WH545|>"), "@Jules Penn");
        assert_eq!(mention_label("<@W010J2WH545|Real Name>"), "@Real Name");
        assert_eq!(mention_label("<@U1|@raine>"), "@raine");
        assert_eq!(runs(source, false), [(source, Run::Text)]);
    }

    #[test]
    fn labels_urls_like_slack() {
        assert_eq!(url_label("https://example.com"), "example.com");
        assert_eq!(
            url_label("https://www.example.com:8080/a/"),
            "example.com/a"
        );
        assert_eq!(
            url_label("https://example.com/docs/setup?ref=slack"),
            "example.com/docs/setup?…"
        );
        assert_eq!(
            url_label("http://example.com/s?q=rust"),
            "example.com/s?q=rust"
        );
        assert_eq!(url_label("https://example.com/a#intro"), "example.com/a#…");
        assert_eq!(url_label("mailto:a@example.com"), "mailto:a@example.com");
    }

    #[test]
    fn opens_only_web_and_mail_links() {
        assert!(is_openable("https://example.com"));
        assert!(is_openable("HTTP://example.com"));
        assert!(is_openable("mailto:a@example.com"));
        assert!(!is_openable("file:///etc/hosts"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("docs/design.md"));
        assert!(!is_openable("#section"));
        assert!(!is_openable("ht"));
    }

    /// The review fixtures: `<name>.before.md` and `<name>.after.md` pairs.
    fn review_fixtures() -> Vec<(String, String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/review");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                name.strip_suffix(".before.md").map(str::to_owned)
            })
            .collect();
        names.sort();
        assert!(!names.is_empty());
        names
            .into_iter()
            .map(|name| {
                let read = |side: &str| {
                    std::fs::read_to_string(dir.join(format!("{name}.{side}.md"))).unwrap()
                };
                let (before, after) = (read("before"), read("after"));
                (name, before, after)
            })
            .collect()
    }

    /// A redline style for `target` drawing `marks`, nothing kept or
    /// selected.
    fn redline_style(target: Target, marks: Rc<super::Marks>, hunks: usize) -> super::MarkupStyle {
        use super::{Redline, RedlineColors};
        use crate::inline_text::MarkHandlers;
        let mut style = super::style(target, false);
        style.redline = Some(Rc::new(Redline {
            marks,
            selected: None,
            hovered: None,
            kept: vec![false; hunks],
            colors: RedlineColors::new(&crate::theme::Palette::light()),
            handlers: Rc::new(MarkHandlers {
                click: Rc::new(|_, _, _| {}),
                hover: Rc::new(|_, _| {}),
                placed: Rc::new(|_, _| {}),
            }),
        }));
        style
    }

    /// Whether `segment` is a point: marked, with no tint, while nothing is
    /// kept.
    fn is_point(segment: &Segment) -> bool {
        segment
            .mark
            .as_ref()
            .is_some_and(|mark| mark.background.is_none())
    }

    /// Each side of every review fixture on every target: the side's text,
    /// what the preview shows for it, and its redline.
    fn review_sides(
        mut each: impl FnMut(&str, Target, Side, &str, &super::Mapped, &super::Redlined, usize),
    ) {
        for (name, before, after) in review_fixtures() {
            let comparison = draftmux_core::diff::compare(&before, &after);
            let decorations = draftmux_core::decorations::decorations(&comparison);
            for target in Target::ALL.iter().copied() {
                for (side, text) in [(Side::Before, &before), (Side::After, &after)] {
                    let mapped = super::preview_mapped(target, text);
                    let redlined = super::redline(&mapped, &decorations, side);
                    each(
                        &name,
                        target,
                        side,
                        text,
                        &mapped,
                        &redlined,
                        comparison.hunks.len(),
                    );
                }
            }
        }
    }

    #[test]
    fn redlines_draw_the_text_they_decorate() {
        let mut failures = Vec::new();
        review_sides(|name, target, side, text, mapped, redlined, hunks| {
            let at = format!("{name} {side:?} {target:?}");
            let plain = super::preview(target, text);
            if *mapped.document != plain {
                failures.push(format!("{at}: the mapped parse isn't the preview's"));
            }
            let style = redline_style(target, redlined.marks.clone(), hunks);
            for (ix, (_, value)) in mapped.fields.iter().enumerate() {
                let segments = super::field_segments(value, ix, style.redline.as_ref().unwrap());
                let shown: String = segments
                    .iter()
                    .filter(|segment| !is_point(segment))
                    .map(|segment| segment.text.as_ref())
                    .collect();
                if shown != *value {
                    failures.push(format!("{at}: field {ix}: {shown:?} != {value:?}"));
                }
            }
            let marked: Vec<(Vec<usize>, String)> = super::drawn(&redlined.document, &style)
                .into_iter()
                // The paragraph holding points with no text to stand by.
                .filter(|(path, _)| path[0] < plain.blocks.len())
                .map(|(path, segments)| {
                    let text = segments
                        .iter()
                        .filter(|segment| !is_point(segment))
                        .map(|segment| segment.text.as_ref())
                        .collect();
                    (path, text)
                })
                .collect();
            let unmarked: Vec<(Vec<usize>, String)> =
                super::drawn(&plain, &super::style(target, false))
                    .into_iter()
                    .map(|(path, segments)| {
                        (
                            path,
                            segments
                                .iter()
                                .map(|segment| segment.text.as_ref())
                                .collect(),
                        )
                    })
                    .collect();
            if marked != unmarked {
                failures.push(format!(
                    "{at}:\n  marked   {marked:?}\n  unmarked {unmarked:?}"
                ));
            }
        });
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn redlines_draw_every_change_on_both_sides() {
        let mut failures = Vec::new();
        let mut checked = 0;
        review_sides(|name, target, side, _, mapped, redlined, hunks| {
            let style = redline_style(target, redlined.marks.clone(), hunks);
            let mut shown = vec![false; hunks];
            for (_, segments) in super::drawn(&redlined.document, &style) {
                for mark in segments.iter().filter_map(|segment| segment.mark.as_ref()) {
                    shown[mark.hunk] = true;
                }
            }
            for (ix, (_, value)) in mapped.fields.iter().enumerate() {
                for segment in super::field_segments(value, ix, style.redline.as_ref().unwrap()) {
                    if !segment.text.is_empty()
                        && let Some(mark) = segment.mark
                    {
                        shown[mark.hunk] = true;
                    }
                }
            }
            for (hunk, shown) in shown.into_iter().enumerate() {
                checked += 1;
                if !shown {
                    failures.push(format!("{name} {side:?} {target:?}: hunk {hunk}"));
                }
            }
        });
        assert!(checked > 500, "{checked} hunks checked");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn frontmatter_hunks_have_independent_selection_and_kept_state() {
        let before = include_str!("../fixtures/review/frontmatter-multiple.before.md");
        let after = include_str!("../fixtures/review/frontmatter-multiple.after.md");
        let comparison = draftmux_core::diff::compare(before, after);
        assert_eq!(comparison.hunks.len(), 2);
        let decorations = draftmux_core::decorations::decorations(&comparison);
        for (side, text) in [(Side::Before, before), (Side::After, after)] {
            let mapped = super::preview_mapped(Target::Github, text);
            let redlined = super::redline(&mapped, &decorations, side);
            let mut style = redline_style(Target::Github, redlined.marks, 2);
            let redline = Rc::get_mut(style.redline.as_mut().unwrap()).unwrap();
            redline.kept[0] = true;
            for selected in 0..2 {
                redline.selected = Some(selected);
                let segments = super::field_segments(&mapped.fields["subject"], 0, redline);
                assert_eq!(segments[0].text.as_ref(), mapped.fields["subject"]);
                assert!(segments[0].mark.is_none(), "no hunk owns the whole value");
                let marks: Vec<_> = segments.iter().filter_map(|s| s.mark.as_ref()).collect();
                assert_eq!(marks.iter().map(|m| m.hunk).collect::<Vec<_>>(), [0, 1]);
                for mark in marks {
                    assert_eq!(mark.ring.is_some(), mark.hunk == selected);
                    assert_eq!(mark.bar.is_some(), mark.hunk == selected);
                }
            }
        }
    }

    #[test]
    fn only_a_point_draws_a_bar() {
        use super::{Mark, Redline, RedlineColors, Segment};
        use crate::inline_text::MarkHandlers;
        use crate::theme::Palette;
        let redline = Redline {
            marks: Rc::default(),
            selected: Some(0),
            hovered: None,
            kept: vec![false],
            colors: RedlineColors::new(&Palette::light()),
            handlers: Rc::new(MarkHandlers {
                click: Rc::new(|_, _, _| {}),
                hover: Rc::new(|_, _| {}),
                placed: Rc::new(|_, _| {}),
            }),
        };
        let bar = |point| {
            let mut segment = Segment {
                text: super::POINT.into(),
                ..Segment::default()
            };
            let mark = Mark {
                hunk: 0,
                deleted: true,
                point,
            };
            redline.style(&mut segment, mark);
            (segment.mark.unwrap().bar.is_some(), segment.strike)
        };
        assert_eq!(bar(true), (true, false));
        assert_eq!(bar(false), (false, true));
    }

    /// `after`'s side of its comparison with `before`, as drawn on
    /// `target`: each text with its marks, `[…]` for a mark's text and `|`
    /// for a point.
    fn drawn_after(target: Target, before: &str, after: &str) -> Vec<String> {
        let comparison = draftmux_core::diff::compare(before, after);
        let decorations = draftmux_core::decorations::decorations(&comparison);
        let mapped = super::preview_mapped(target, after);
        let redlined = super::redline(&mapped, &decorations, Side::After);
        let style = redline_style(target, redlined.marks.clone(), comparison.hunks.len());
        super::drawn(&redlined.document, &style)
            .into_iter()
            .map(|(_, segments)| {
                segments
                    .iter()
                    .map(|segment| match &segment.mark {
                        None => segment.text.to_string(),
                        Some(_) if is_point(segment) => "|".to_owned(),
                        Some(_) => format!("[{}]", segment.text),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn review_of_a_draft_with_noncharacters_shows_them_unchanged() {
        let before = "One \u{FDD0}\u{FDE0} two &#xFDD1; three\n";
        let after = "One \u{FDD0}\u{FDE0} two &#xFDD1; four\n";
        assert_eq!(
            drawn_after(Target::Github, before, after),
            ["One \u{FDD0}\u{FDE0} two \u{FDD1} [four]"]
        );
        assert_eq!(
            drawn_after(Target::Plain, before, after),
            ["One \u{FDD0}\u{FDE0} two &#xFDD1; [four]"]
        );
    }

    #[test]
    fn slack_marks_curled_quotes_by_character_and_mentions_whole() {
        assert_eq!(
            drawn_after(
                Target::Slack,
                "Say \"hi there\" to all of the team, then <@U1> once more.\n",
                "Say \"hey there\" to all of the team, then <@U2> once more.\n"
            ),
            ["Say [\u{201C}hey] there\u{201D} to all of the team, then [@Iris Lane] once more."]
        );
    }

    #[test]
    fn a_mark_leaves_the_space_after_its_words_untinted() {
        assert_eq!(
            drawn_after(Target::Github, "Run the tests.\n", "Run the full tests.\n"),
            ["Run the [full] tests."]
        );
        // A change of only whitespace keeps it.
        assert_eq!(
            drawn_after(Target::Github, "Left right.\n", "Left  right.\n"),
            ["Left|[  ]right."]
        );
    }

    #[test]
    fn slack_code_boxes_show_changes() {
        let before = "```rust\nlet a = 1;\nlet b = 2;\n```\n";
        let after = "```rust\nlet a = 1;\nlet b = 3;\n```\n";
        assert_eq!(
            drawn_after(Target::Slack, before, after),
            ["let a = 1;", "let b = [3;]"]
        );
    }

    #[test]
    fn github_code_is_highlighted_under_changes() {
        let before = "```rust\nlet a = 1;\nlet b = 2;\n```\n";
        let after = "```rust\nlet a = 1;\nlet b = 3;\n```\n";
        assert_eq!(
            drawn_after(Target::Github, before, after),
            ["let a = 1;\nlet b = [3][;]"]
        );
        let doc = super::preview_mapped(Target::Github, after).document;
        let style = super::MarkupStyle::github(false);
        let segments = &super::drawn(&doc, &style)[0].1;
        let keyword = segments
            .iter()
            .find(|segment| segment.text == "let")
            .expect("`let` is its own segment");
        assert_eq!(keyword.color, Some(style.syntax.as_ref().unwrap().keyword));
    }

    #[test]
    fn an_empty_side_shows_a_point_on_its_own() {
        assert_eq!(drawn_after(Target::Github, "Hello\n", ""), ["|"]);
    }

    #[test]
    fn a_frontmatter_change_marks_its_field() {
        let before = "---\ntitle: Old\nto: a@b.c\n---\nBody\n";
        let after = "---\ntitle: New\nto: a@b.c\n---\nBody\n";
        let comparison = draftmux_core::diff::compare(before, after);
        let decorations = draftmux_core::decorations::decorations(&comparison);
        for (side, text) in [(Side::Before, before), (Side::After, after)] {
            let mapped = super::preview_mapped(Target::Github, text);
            let redlined = super::redline(&mapped, &decorations, side);
            assert_eq!(
                redlined.marks.fields,
                [(
                    0,
                    super::Mark {
                        hunk: 0,
                        deleted: side == Side::Before,
                        point: false
                    }
                )]
            );
            assert!(redlined.marks.blocks.iter().all(Vec::is_empty));
        }
    }
}
