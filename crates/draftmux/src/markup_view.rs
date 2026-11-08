//! Renders a [`markup::Document`] as GPUI elements.
//!
//! Rendering is a pure function of the document and a [`MarkupStyle`], so the
//! preview pane and chat messages draw bodies the same way. Callers parse when
//! the text changes and keep the document; they own scrolling, padding and
//! backgrounds around the body.

use std::borrow::Cow;

use draftmux_storage::Target;
use gpui::{
    AnyElement, App, ElementId, FontWeight, Hsla, Pixels, SharedString, div, prelude::*, px,
    relative, rgba,
};
use markup::{AlertKind, Alignment, Block, Document, List, Span, Table};

use crate::assets::LINK_ICON;
use crate::inline_text::{Chip, Icon, InlineText, Segment, is_emoji};

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
    pub radius: Pixels,
    pub quote_bar_width: Pixels,
    pub quote_bar_color: Hsla,
    pub quote_bar_radius: Pixels,
    pub quote_padding: Pixels,
    pub quote_text_color: Hsla,
    /// Number nested ordered lists 1., a., i. by depth instead of 1. at
    /// every depth.
    pub ordered_markers_by_depth: bool,
    pub table_stripe: Hsla,
    pub checkbox_checked: Hsla,
    pub alert_colors: AlertColors,
    /// Render what Slack posts instead of the document as written
    /// ([`markup::slack::posted`]).
    pub slack_fallbacks: bool,
    /// Colors of the note above each replaced block; `None` hides notes.
    pub notes: Option<NoteColors>,
}

#[derive(Clone, Copy)]
pub struct NoteColors {
    pub background: Hsla,
    pub border: Hsla,
    pub text: Hsla,
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

impl MarkupStyle {
    /// GitHub's markdown body in Primer's light or dark colors.
    pub fn github(dark: bool) -> Self {
        let c = |light: u32, dark_hex: u32| -> Hsla {
            rgba(if dark { dark_hex } else { light }).into()
        };
        Self {
            background: c(0xffffffff, 0x0d1117ff),
            font_family: ".SystemUIFont".into(),
            mono_family: "Menlo".into(),
            text_size: px(14.),
            line_height: 1.5,
            heading_scale: [2., 1.5, 1.25, 1., 0.875, 0.85],
            heading_rule_max_level: 2,
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
            radius: px(6.),
            quote_bar_width: px(3.5),
            quote_bar_color: c(0xd1d9e0ff, 0x3d444dff),
            quote_bar_radius: px(0.),
            quote_padding: px(14.),
            quote_text_color: c(0x59636eff, 0x9198a1ff),
            ordered_markers_by_depth: false,
            table_stripe: c(0xf6f8faff, 0x151b23ff),
            checkbox_checked: c(0x0969daff, 0x4493f8ff),
            alert_colors: AlertColors {
                note: c(0x0969daff, 0x4493f8ff),
                tip: c(0x1a7f37ff, 0x3fb950ff),
                important: c(0x8250dfff, 0xab7df8ff),
                warning: c(0x9a6700ff, 0xd29922ff),
                caution: c(0xd1242fff, 0xf85149ff),
            },
            slack_fallbacks: false,
            notes: None,
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
            radius: px(4.),
            quote_bar_width: px(4.),
            quote_bar_color: c(0xddddddff, 0xb9babdff),
            quote_bar_radius: px(8.),
            quote_padding: px(12.),
            quote_text_color: text_color,
            ordered_markers_by_depth: true,
            table_stripe: c(0x1d1c1d0a, 0x232529ff),
            checkbox_checked: link_color,
            alert_colors: AlertColors {
                note: border_color,
                tip: border_color,
                important: border_color,
                warning: border_color,
                caution: border_color,
            },
            slack_fallbacks: true,
            notes: Some(NoteColors {
                background: c(0xfff8e6ff, 0xffd60a14),
                border: c(0xf4e2b0ff, 0xffd60a33),
                text: c(0x7a5a00ff, 0xe6c35cff),
            }),
        }
    }
}

/// Registers the bundled fonts the styles use. Call once at startup.
pub fn init(cx: &mut App) {
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

/// Renders `doc` at the width its parent gives it. `id` must be unique among
/// bodies shown together, such as chat messages in one list.
pub fn render(doc: &Document, style: &MarkupStyle, id: impl Into<ElementId>) -> AnyElement {
    let mut renderer = Renderer {
        style,
        next_id: 0,
        list_depth: 0,
    };
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .font_family(style.font_family.clone())
        .text_size(style.text_size)
        .line_height(relative(style.line_height))
        .text_color(style.text_color)
        .children(if style.slack_fallbacks {
            let posted = markup::slack::posted(doc);
            let notes = style.notes;
            renderer.noted_blocks(
                posted.iter().map(|(block, note)| (block, note.zip(notes))),
                style.block_gap,
            )
        } else {
            renderer.blocks(&doc.blocks, style.block_gap)
        })
        .into_any_element()
}

/// Parses `text` for previewing it on `target`. With [`style`], the one
/// place a target chooses its parser and style.
pub fn preview(target: Target, text: &str) -> Document {
    match target {
        Target::Github => markup::github::parse(text),
        Target::Slack => markup::slack::parse(text),
        Target::Email => markup::plain::parse(text),
    }
}

/// The preview style for `target` in the light or dark appearance.
pub fn style(target: Target, dark: bool) -> MarkupStyle {
    match target {
        Target::Github => MarkupStyle::github(dark),
        Target::Slack => MarkupStyle::slack(dark),
        // An email body in the system font; nothing in it is styled.
        Target::Email => MarkupStyle::github(dark),
    }
}

struct Renderer<'a> {
    style: &'a MarkupStyle,
    /// Element ids are scoped by the body's root id, so a counter suffices.
    next_id: u64,
    list_depth: usize,
}

impl Renderer<'_> {
    fn id(&mut self) -> ElementId {
        self.next_id += 1;
        ElementId::Integer(self.next_id)
    }

    /// Renders sibling blocks, separated by `gap` (wider above headings).
    fn blocks(&mut self, blocks: &[Block], gap: Pixels) -> Vec<AnyElement> {
        self.noted_blocks(blocks.iter().map(|block| (block, None)), gap)
    }

    /// Renders sibling blocks like [`Self::blocks`], each with an optional
    /// conversion note above it.
    fn noted_blocks<'b>(
        &mut self,
        blocks: impl IntoIterator<Item = (&'b Block, Option<(&'static str, NoteColors)>)>,
        gap: Pixels,
    ) -> Vec<AnyElement> {
        let mut after_text = false;
        blocks
            .into_iter()
            .enumerate()
            .map(|(ix, (block, note))| {
                let gap = match block {
                    _ if ix == 0 => px(0.),
                    Block::Heading { .. } => self.style.heading_gap,
                    _ if after_text && is_text(block) => self.style.text_gap,
                    _ => gap,
                };
                after_text = is_text(block);
                div()
                    .pt(gap)
                    .when_some(note, |this, (note, colors)| {
                        this.child(conversion_note(note, colors))
                    })
                    .child(self.block(block))
                    .into_any_element()
            })
            .collect()
    }

    fn block(&mut self, block: &Block) -> AnyElement {
        let style = self.style;
        match block {
            Block::Paragraph(spans) => self.text(spans),
            Block::Heading { level, spans } => {
                let scale = style.heading_scale[(*level as usize).clamp(1, 6) - 1];
                div()
                    .text_size(style.text_size * scale)
                    .line_height(relative(1.25))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(*level <= style.heading_rule_max_level, |this| {
                        this.pb(style.text_size * scale * 0.3)
                            .border_b_1()
                            .border_color(style.heading_rule_color)
                    })
                    .child(self.text(spans))
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
                        div()
                            .mb(px(4.))
                            .text_color(color)
                            .font_weight(FontWeight::MEDIUM)
                            .child(kind.label()),
                    )
                    .children(self.blocks(blocks, style.block_gap))
                    .into_any_element()
            }
            Block::Code { text, .. } if style.code_wraps => self
                .code_box()
                .font_family(style.mono_family.clone())
                .text_size(style.code_text_size)
                .line_height(relative(style.code_line_height))
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            Block::Code { text, .. } => self
                .code_box()
                .id(self.id())
                .overflow_x_scroll()
                .flex()
                .child(
                    // Unshrunk so long lines widen the content and scroll.
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .font_family(style.mono_family.clone())
                        .text_size(style.code_text_size)
                        .line_height(relative(style.code_line_height))
                        .child(SharedString::from(text.clone())),
                )
                .into_any_element(),
            Block::Table(table) => self.table(table),
            Block::Rule => div()
                .py(px(8.))
                .child(div().h(px(3.5)).bg(style.border_color))
                .into_any_element(),
            Block::Html(html) => div()
                .text_color(style.muted_color)
                .font_family(style.mono_family.clone())
                .text_size(style.code_text_size)
                .child(SharedString::from(html.clone()))
                .into_any_element(),
        }
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
        let marker_depth = if style.ordered_markers_by_depth {
            self.list_depth
        } else {
            0
        };
        self.list_depth += 1;
        let items = list
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let marker = match (item.task, list.start) {
                    (Some(checked), _) => self.checkbox(checked),
                    (None, Some(start)) => div()
                        .flex()
                        .justify_end()
                        .pr(style.ordered_marker_gap)
                        .child(format!(
                            "{}.",
                            ordered_marker(start + ix as u64, marker_depth)
                        ))
                        .into_any_element(),
                    (None, None) => bullet(&style.bullet, bullet_kind, style.text_color),
                };
                div()
                    .when(ix > 0, |this| this.pt(item_gap))
                    .flex()
                    .child(div().w(style.list_indent).flex_none().child(marker))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .children(self.blocks(&item.blocks, inner_gap)),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        self.list_depth -= 1;
        div().flex().flex_col().children(items).into_any_element()
    }

    fn checkbox(&self, checked: bool) -> AnyElement {
        let style = self.style;
        div()
            .h(style.text_size * style.line_height)
            .flex()
            .items_center()
            .child(
                div()
                    .size(px(13.))
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
                                .text_color(gpui::white())
                                .text_size(px(10.))
                                .line_height(px(13.))
                                .font_weight(FontWeight::BOLD)
                                .child("✓")
                        },
                        |this| this.border_color(style.muted_color),
                    ),
            )
            .into_any_element()
    }

    fn table(&mut self, table: &Table) -> AnyElement {
        let style = self.style;
        let columns = table.alignments.len();
        let rows = std::iter::once(&table.header).chain(&table.rows);
        let mut cells = Vec::new();
        for (row_ix, row) in rows.enumerate() {
            for (col_ix, spans) in row.iter().enumerate() {
                let cell = div()
                    .px(px(13.))
                    .py(px(6.))
                    .border_r_1()
                    .border_b_1()
                    .border_color(style.border_color)
                    .when(row_ix == 0, |this| {
                        this.border_t_1().font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(col_ix == 0, |this| this.border_l_1())
                    // Body rows alternate, starting unshaded.
                    .when(row_ix > 0 && row_ix % 2 == 0, |this| {
                        this.bg(style.table_stripe)
                    });
                let cell = match table.alignments.get(col_ix) {
                    Some(Alignment::Center) => cell.text_center(),
                    Some(Alignment::Right) => cell.text_right(),
                    _ => cell,
                };
                cells.push(cell.child(self.text(spans)));
            }
        }
        div()
            .id(self.id())
            .overflow_x_scroll()
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
        let style = self.style;
        let curled;
        let spans = if style.smart_quotes {
            curled = smart_quotes(spans);
            &curled
        } else {
            spans
        };
        let mut segments = Vec::new();
        for span in spans {
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
                color: inline.color.or(base.color),
                chip: Some(inline.chip.clone()),
                ..base.clone()
            };
            if span.style.code {
                segments.push(Segment {
                    family: Some(style.mono_family.clone()),
                    ..boxed(&style.inline_code, span.text.clone())
                });
            } else if let Some(url) = &span.link {
                match &style.url_slug {
                    Some(slug) if span.text == *url => {
                        segments.push(boxed(slug, url_label(url)));
                    }
                    _ => segments.push(Segment {
                        text: span.text.clone().into(),
                        color: Some(style.link_color),
                        ..base
                    }),
                }
            } else {
                for (run, kind) in runs(&span.text, style.mention.is_some()) {
                    segments.push(match kind {
                        Run::Text => Segment {
                            text: run.to_owned().into(),
                            ..base.clone()
                        },
                        Run::Emoji => Segment {
                            text: run.to_owned().into(),
                            scale: Some(style.emoji_scale),
                            ..base.clone()
                        },
                        Run::Mention => boxed(style.mention.as_ref().unwrap(), run.to_owned()),
                    });
                }
            }
        }
        InlineText::new(segments)
            .blank_line(style.blank_line)
            .into_any_element()
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
    /// `@name` at the start of a word.
    Mention,
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

/// Says how a block changes on its way to the platform, above the
/// replacement block.
fn conversion_note(note: &'static str, colors: NoteColors) -> impl IntoElement {
    div().flex().mb(px(4.)).child(
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(6.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .text_color(colors.text)
            .font_family(".SystemUIFont")
            .text_size(px(12.))
            .line_height(relative(1.4))
            .child(note),
    )
}

/// The marker for item `number` of an ordered list nested `depth` deep:
/// decimal, then lowercase letters, then lowercase roman numerals.
fn ordered_marker(number: u64, depth: usize) -> String {
    match depth % 3 {
        1 if number > 0 => {
            let mut n = number;
            let mut letters = Vec::new();
            while n > 0 {
                n -= 1;
                letters.push(b'a' + (n % 26) as u8);
                n /= 26;
            }
            letters.iter().rev().map(|&b| b as char).collect()
        }
        2 if (1..4000).contains(&number) => {
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
    use super::{Run, is_openable, ordered_marker, runs, url_label};

    #[test]
    fn numbers_nested_ordered_lists_by_depth() {
        assert_eq!(ordered_marker(3, 0), "3");
        assert_eq!(ordered_marker(1, 1), "a");
        assert_eq!(ordered_marker(27, 1), "aa");
        assert_eq!(ordered_marker(4, 2), "iv");
        assert_eq!(ordered_marker(14, 2), "xiv");
        assert_eq!(ordered_marker(2, 3), "2");
        assert_eq!(ordered_marker(0, 1), "0");
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
}
