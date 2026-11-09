//! The Copy action: puts the draft on the pasteboard ready for its target
//! (history/docs/design.md, Output). Copying never changes the draft.

use crate::assets::COPY_ICON;
use crate::chrome::shadow;
use crate::theme::Palette;
use draftmux_storage::Target;
use gpui::{
    App, ClipboardItem, FontWeight, KeyBinding, actions, div, linear_color_stop, linear_gradient,
    prelude::*, px, svg,
};

actions!(copy, [Copy]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(editor::secondary!("shift-c"), Copy, None)]);
}

/// Copies `text`, a draft in `target`'s syntax, and returns the confirmation
/// to show.
pub fn copy(target: Target, text: &str, cx: &mut App) -> &'static str {
    match target {
        Target::Github => {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            "Copied markdown for GitHub"
        }
        Target::Rich => {
            if write_rich_text(text, cx) {
                "Copied as rich text"
            } else {
                "Rich copy unavailable; copied plain text"
            }
        }
        Target::Plain => {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            "Copied plain text"
        }
        Target::Slack => {
            if write_slack(text, cx) {
                "Copied as rich text for Slack"
            } else {
                "Rich copy unavailable; copied plain text for Slack"
            }
        }
    }
}

/// Writes `mrkdwn` to the general pasteboard as Slack's composer should
/// receive it. gpui's clipboard only writes plain text, so this goes to
/// the platform's rich clipboard directly.
fn write_slack(mrkdwn: &str, cx: &mut App) -> bool {
    crate::platform::write_flavours(&slack_flavours(&markup::slack::parse(mrkdwn)), cx)
}

/// Writes a Rich text draft's `markdown` to the general pasteboard: HTML
/// and RTF for rich-text editors, and the Markdown itself for pasting
/// without formatting.
fn write_rich_text(markdown: &str, cx: &mut App) -> bool {
    let doc = markup::github::parse(markdown);
    crate::platform::write_flavours(
        &Flavours {
            html: markup::rich_text::html(&doc),
            rtf: markup::rich_text::rtf(&doc),
            plain: markdown.to_owned(),
        },
        cx,
    )
}

/// The pasteboard contents for a Slack message: HTML, which Slack's composer
/// reads, RTF for native apps, and the visible text for pasting without
/// formatting.
pub(crate) struct Flavours {
    pub html: String,
    pub rtf: String,
    pub plain: String,
}

pub(crate) fn slack_flavours(doc: &markup::Document) -> Flavours {
    Flavours {
        html: markup::rich::html(doc),
        rtf: markup::rich::rtf(doc),
        plain: markup::rich::plain(doc),
    }
}

/// The toolbar's primary Copy button.
pub fn button(palette: &Palette, on_click: impl Fn(&mut App) + 'static) -> impl IntoElement {
    div()
        .id("copy")
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .bg(linear_gradient(
            180.,
            linear_color_stop(palette.primary_top, 0.),
            linear_color_stop(palette.primary_bottom, 1.),
        ))
        .hover(|style| {
            style.bg(linear_gradient(
                180.,
                linear_color_stop(palette.primary_hover_top, 0.),
                linear_color_stop(palette.primary_hover_bottom, 1.),
            ))
        })
        .shadow(vec![
            shadow(palette.primary_ring, 0., 0., 0.5),
            shadow(palette.primary_drop, 1., 1.5, 0.),
        ])
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(palette.on_accent)
        .on_click(move |_, _, cx| on_click(cx))
        .child(
            svg()
                .path(COPY_ICON)
                .size(px(14.))
                .text_color(palette.on_accent),
        )
        .child("Copy")
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::NORMAL)
                .text_color(palette.on_accent_muted)
                .child(editor::by_platform!("⌘⇧C", "Ctrl+Shift+C")),
        )
}
