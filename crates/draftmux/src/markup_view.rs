//! Renders a [`markup::Document`] as GPUI elements.
//!
//! Rendering is a pure function of the document and a [`MarkupStyle`], so the
//! preview pane and chat messages draw bodies the same way. Callers parse when
//! the text changes and keep the document; they own scrolling, padding and
//! backgrounds around the body.

use std::borrow::Cow;

use draftmux_storage::Target;
use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveText,
    Pixels, SharedString, StrikethroughStyle, StyledText, Window, div, prelude::*, px, relative,
    rgb, rgba,
};
use markup::{AlertKind, Alignment, Block, Document, List, Span, Table};

/// Visual parameters for one target platform.
pub struct MarkupStyle {
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
    pub list_indent: Pixels,
    pub text_color: Hsla,
    pub muted_color: Hsla,
    pub link_color: Hsla,
    pub border_color: Hsla,
    pub heading_rule_color: Hsla,
    pub inline_code_background: Hsla,
    /// Inline code text color; `None` keeps the surrounding color.
    /// gpui can't size, pad or border part of a line, so inline code keeps
    /// the body size where Slack draws a smaller bordered chip.
    pub inline_code_color: Option<Hsla>,
    pub code_background: Hsla,
    pub code_text_size: Pixels,
    pub code_line_height: f32,
    pub code_padding: Pixels,
    pub code_border: Option<Hsla>,
    /// Wrap long code lines instead of scrolling them sideways.
    pub code_wraps: bool,
    pub radius: Pixels,
    pub quote_bar_width: Pixels,
    pub quote_padding: Pixels,
    pub quote_text_color: Hsla,
    /// Number nested ordered lists 1., a., i. by depth instead of 1. at
    /// every depth.
    pub ordered_markers_by_depth: bool,
    pub table_stripe: Hsla,
    pub checkbox_checked: Hsla,
    pub alert_colors: AlertColors,
    /// Replace blocks Slack can't show with what it receives instead
    /// ([`markup::slack::fallback`]), each with a short note.
    pub slack_fallbacks: bool,
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
    /// GitHub's light markdown body (Primer light colors).
    pub fn github_light() -> Self {
        Self {
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
            list_indent: px(28.),
            text_color: rgb(0x1f2328).into(),
            muted_color: rgb(0x59636e).into(),
            link_color: rgb(0x0969da).into(),
            border_color: rgb(0xd1d9e0).into(),
            heading_rule_color: rgba(0xd1d9e0b3).into(),
            inline_code_background: rgba(0x818b981f).into(),
            inline_code_color: None,
            code_background: rgb(0xf6f8fa).into(),
            code_text_size: px(11.9),
            code_line_height: 1.45,
            code_padding: px(16.),
            code_border: None,
            code_wraps: false,
            radius: px(6.),
            quote_bar_width: px(3.5),
            quote_padding: px(14.),
            quote_text_color: rgb(0x59636e).into(),
            ordered_markers_by_depth: false,
            table_stripe: rgb(0xf6f8fa).into(),
            checkbox_checked: rgb(0x0969da).into(),
            alert_colors: AlertColors {
                note: rgb(0x0969da).into(),
                tip: rgb(0x1a7f37).into(),
                important: rgb(0x8250df).into(),
                warning: rgb(0x9a6700).into(),
                caution: rgb(0xd1242f).into(),
            },
            slack_fallbacks: false,
        }
    }

    /// Slack's light message text. Spacing between lines comes from the
    /// newlines kept in the text, so blocks sit only a small margin apart.
    pub fn slack_light() -> Self {
        let text_color = rgb(0x1d1c1d).into();
        let border_color = rgba(0x1d1c1d21).into();
        Self {
            font_family: "Lato".into(),
            mono_family: "Monaco".into(),
            text_size: px(15.),
            line_height: 1.46668,
            // Headings are replaced by bold lines.
            heading_scale: [1.; 6],
            heading_rule_max_level: 0,
            block_gap: px(4.),
            heading_gap: px(4.),
            strong_weight: FontWeight::BOLD,
            tight_item_gap: px(0.),
            list_indent: px(26.),
            text_color,
            muted_color: rgb(0x616061).into(),
            link_color: rgb(0x1264a3).into(),
            border_color,
            heading_rule_color: border_color,
            inline_code_background: rgba(0x1d1c1d0a).into(),
            inline_code_color: Some(rgb(0xe01e5a).into()),
            code_background: rgba(0x1d1c1d0a).into(),
            code_text_size: px(12.),
            code_line_height: 1.50001,
            code_padding: px(8.),
            code_border: Some(border_color),
            code_wraps: true,
            radius: px(4.),
            quote_bar_width: px(4.),
            quote_padding: px(12.),
            quote_text_color: text_color,
            ordered_markers_by_depth: true,
            table_stripe: rgba(0x1d1c1d0a).into(),
            checkbox_checked: rgb(0x1264a3).into(),
            alert_colors: AlertColors {
                note: border_color,
                tip: border_color,
                important: border_color,
                warning: border_color,
                caution: border_color,
            },
            slack_fallbacks: true,
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
        .children(renderer.blocks(&doc.blocks, style.block_gap))
        .into_any_element()
}

/// Parses `text` and picks the style for previewing it on `target`. The one
/// place a target chooses its parser and style.
pub fn preview(target: Target, text: &str) -> (Document, MarkupStyle) {
    match target {
        Target::Github => (markup::github::parse(text), MarkupStyle::github_light()),
        Target::Slack => (markup::slack::parse(text), MarkupStyle::slack_light()),
        // An email body in the system font; nothing in it is styled.
        Target::Email => (markup::plain::parse(text), MarkupStyle::github_light()),
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
        blocks
            .iter()
            .enumerate()
            .map(|(ix, block)| {
                let fallback = self
                    .style
                    .slack_fallbacks
                    .then(|| markup::slack::fallback(block))
                    .flatten();
                let (block, note) = match &fallback {
                    Some((replacement, note)) => (replacement, Some(*note)),
                    None => (block, None),
                };
                let gap = match block {
                    _ if ix == 0 => px(0.),
                    Block::Heading { .. } => self.style.heading_gap,
                    _ => gap,
                };
                div()
                    .pt(gap)
                    .when_some(note, |this, note| this.child(conversion_note(note)))
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
                .border_l(style.quote_bar_width)
                .border_color(style.border_color)
                .px(style.quote_padding)
                .text_color(style.quote_text_color)
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
        let bullet = ["•", "◦", "▪"][self.list_depth % 3];
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
                    (None, Some(start)) => {
                        format!("{}.", ordered_marker(start + ix as u64, marker_depth))
                            .into_any_element()
                    }
                    (None, None) => bullet.into_any_element(),
                };
                div()
                    .when(ix > 0, |this| this.pt(item_gap))
                    .flex()
                    .child(
                        div()
                            .w(style.list_indent)
                            .flex_none()
                            .flex()
                            .justify_end()
                            .pr(px(6.))
                            .child(marker),
                    )
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

    /// Inline content as one wrapped text element; link ranges are clickable.
    fn text(&mut self, spans: &[Span]) -> AnyElement {
        let style = self.style;
        let mut text = String::new();
        let mut highlights = Vec::new();
        let mut mono_ranges = Vec::new();
        let mut link_ranges = Vec::new();
        let mut urls = Vec::new();
        for span in spans {
            let start = text.len();
            text.push_str(&span.text);
            let range = start..text.len();
            let mut highlight = HighlightStyle::default();
            if span.style.strong {
                highlight.font_weight = Some(style.strong_weight);
            }
            if span.style.emph {
                highlight.font_style = Some(FontStyle::Italic);
            }
            if span.style.strike {
                highlight.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                });
            }
            if span.style.code {
                highlight.background_color = Some(style.inline_code_background);
                highlight.color = style.inline_code_color;
                mono_ranges.push((range.clone(), style.mono_family.clone()));
            }
            if let Some(url) = &span.link {
                highlight.color = Some(style.link_color);
                link_ranges.push(range.clone());
                urls.push(url.clone());
            }
            highlights.push((range, highlight));
        }
        let styled = StyledText::new(text)
            .with_highlights(highlights)
            .with_font_family_overrides(mono_ranges);
        if link_ranges.is_empty() {
            return styled.into_any_element();
        }
        InteractiveText::new(self.id(), styled)
            .on_click(link_ranges, move |ix, _: &mut Window, cx: &mut App| {
                if is_openable(&urls[ix]) {
                    cx.open_url(&urls[ix]);
                }
            })
            .into_any_element()
    }
}

/// Says how a block changes on its way to the platform, above the
/// replacement block.
fn conversion_note(note: &'static str) -> impl IntoElement {
    div().flex().mb(px(4.)).child(
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(0xf4e2b0))
            .bg(rgb(0xfff8e6))
            .text_color(rgb(0x7a5a00))
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
    use super::{is_openable, ordered_marker};

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
