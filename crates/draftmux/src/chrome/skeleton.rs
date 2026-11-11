//! The preview's placeholder while the agent writes into an empty draft: grey
//! bars in the shape of a typical message for the target, set in the
//! target's own type so each bar is one line of text where the draft lands.

use std::time::Duration;

use draftmux_storage::Target;
use gpui::{
    Animation, AnimationExt, AnyElement, Div, Hsla, IntoElement, Pixels, div, prelude::*, px,
    relative,
};

use crate::chat::{WAVE_PERIOD, wave};
use crate::markup_view::MarkupStyle;

/// How far behind its predecessor each line brightens, so the chat's
/// shimmer wave runs down the page.
const LINE_STAGGER: f32 = 0.09;
/// A bar's height, relative to the text size: about a lowercase line.
const BAR: f32 = 0.62;
/// Bar and bullet alpha at rest and at the wave's crest.
const REST: f32 = 0.08;
const CREST: f32 = 0.14;

/// One block of the outline; widths are fractions of the body's width.
enum Block {
    /// A section heading with the rule GitHub draws under it.
    Heading(f32),
    /// A first line in bold, the way Slack messages often open.
    Lead(f32),
    Paragraph(&'static [f32]),
    List(&'static [f32]),
}

/// The outline that fits most drafts for `target`.
fn outline(target: Target) -> &'static [Block] {
    use Block::*;
    match target {
        // Markdown documents: PR descriptions, issues, release notes.
        Target::Github => &[
            Heading(0.26),
            Paragraph(&[0.97, 0.91, 0.42]),
            Paragraph(&[0.88, 0.64]),
            List(&[0.71, 0.58, 0.76]),
        ],
        // Short, flat messages: no headings, often a bold first line.
        Target::Slack => &[
            Lead(0.58),
            Paragraph(&[0.94, 0.86, 0.38]),
            List(&[0.66, 0.72, 0.51]),
        ],
        // Mostly email: a greeting, paragraphs, a sign-off.
        Target::Rich => &[
            Paragraph(&[0.16]),
            Paragraph(&[0.96, 0.93, 0.61]),
            Paragraph(&[0.92, 0.97, 0.89, 0.34]),
            Paragraph(&[0.22, 0.18]),
        ],
        // No structure to suggest.
        Target::Plain => &[Paragraph(&[0.95, 0.89, 0.47]), Paragraph(&[0.93, 0.71])],
    }
}

/// Space between blocks, as the target's preview spaces paragraphs.
fn block_gap(target: Target, style: &MarkupStyle) -> Pixels {
    match target {
        Target::Github | Target::Plain => style.text_gap,
        // A blank line between Slack paragraphs is a short gap.
        Target::Slack => px(8.),
        // Mail's paragraph margins collapse to one line.
        Target::Rich => style.text_size * style.line_height,
    }
}

pub(super) fn render(target: Target, style: &MarkupStyle) -> AnyElement {
    let text = style.text_color;
    let size = style.text_size;
    let line = size * style.line_height;
    let heading = size * style.heading_scale[1];
    let heading_line = heading * style.heading_line_height;
    let gap = block_gap(target, style);
    let indent = style.list_indent;
    let (bullet_left, bullet_size) = (style.bullet.left, style.bullet.size);
    let rule = style.heading_rule_color;
    // GPUI freezes repeating animations at phase zero for Reduce Motion.
    div()
        .flex()
        .flex_col()
        .gap(gap)
        .with_animation(
            "preview-skeleton",
            Animation::new(Duration::from_secs_f32(WAVE_PERIOD))
                .repeat_synced()
                .with_max_fps(30.),
            move |body, phase| {
                let mut ix = 0;
                let mut alpha = |strong: bool| {
                    let lit = wave(phase * WAVE_PERIOD - ix as f32 * LINE_STAGGER);
                    ix += 1;
                    let base = if strong { REST * 1.6 } else { REST };
                    text.opacity(base + (CREST - REST) * lit)
                };
                body.children(outline(target).iter().map(|block| {
                    match block {
                        Block::Heading(w) => div()
                            .pb(heading * 0.3)
                            .border_b_1()
                            .border_color(rule)
                            .child(bar(heading_line, heading, *w, alpha(false)))
                            .into_any_element(),
                        Block::Lead(w) => bar(line, size, *w, alpha(true)).into_any_element(),
                        Block::Paragraph(widths) => div()
                            .children(widths.iter().map(|w| bar(line, size, *w, alpha(false))))
                            .into_any_element(),
                        Block::List(widths) => div()
                            .children(widths.iter().map(|w| {
                                let color = alpha(false);
                                div()
                                    .relative()
                                    .pl(indent)
                                    .child(
                                        div()
                                            .absolute()
                                            .left(bullet_left)
                                            .top((line - bullet_size) / 2.)
                                            .size(bullet_size)
                                            .rounded_full()
                                            .bg(color.opacity(1.5)),
                                    )
                                    .child(bar(line, size, *w, color))
                            }))
                            .into_any_element(),
                    }
                }))
            },
        )
        .into_any_element()
}

/// One line of text `line` tall, drawn as a rounded bar `width` of the body
/// wide.
fn bar(line: Pixels, size: Pixels, width: f32, color: Hsla) -> Div {
    div().h(line).flex().items_center().child(
        div()
            .w(relative(width))
            .h(size * BAR)
            .rounded_full()
            .bg(color),
    )
}
