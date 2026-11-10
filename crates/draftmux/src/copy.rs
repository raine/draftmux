//! The Copy action: puts the draft on the pasteboard ready for its target
//! (history/docs/design.md, Output). Copying never changes the draft.

use std::time::Duration;

use crate::assets::{CHECK_ICON, COPY_ICON};
use crate::chrome::shadow;
use crate::theme::Palette;
use draftmux_storage::Target;
use gpui::{
    Animation, AnimationExt, App, ClipboardItem, ElementId, FontWeight, KeyBinding, actions, div,
    ease_out_quint, linear_color_stop, linear_gradient, prelude::*, px, svg,
};

actions!(copy, [Copy]);

/// How long the Copy button shows its checkmark after a copy.
pub const CONFIRMATION: Duration = Duration::from_millis(1200);
/// How long the Copy button's icons take to cross-fade.
const ICON_FADE: Duration = Duration::from_millis(120);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(editor::secondary!("shift-c"), Copy, None)]);
}

/// Copies `text`, a draft in `target`'s syntax, and returns the confirmation
/// to show.
pub fn copy(target: Target, text: &str, cx: &mut App) -> &'static str {
    let text = copy_body(text);
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

/// Body shared by every clipboard format. Malformed frontmatter stays intact.
fn copy_body(text: &str) -> &str {
    markup::frontmatter::parse(text).body
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
            chromium: None,
        },
        cx,
    )
}

/// The pasteboard contents for a Slack message: HTML, which Slack's composer
/// reads, RTF for native apps, and the visible text for pasting without
/// formatting. With user mentions, also Slack's own format, which keeps
/// them mentions, as Chromium web custom data.
pub(crate) struct Flavours {
    pub html: String,
    pub rtf: String,
    pub plain: String,
    pub chromium: Option<Vec<u8>>,
}

pub(crate) fn slack_flavours(doc: &markup::Document) -> Flavours {
    Flavours {
        html: markup::rich::html(doc),
        rtf: markup::rich::rtf(doc),
        plain: markup::rich::plain(doc),
        chromium: markup::texty::texty(doc).map(|texty| {
            web_custom_data(&[
                ("public.utf8-plain-text", &markup::rich::plain(doc)),
                ("slack/texty", &texty),
            ])
        }),
    }
}

/// Chromium's pickled web custom data, the clipboard's
/// `org.chromium.web-custom-data`: a payload length, then the entry count
/// and each entry's type and value as UTF-16 strings, all little-endian and
/// each string padded to four bytes.
fn web_custom_data(entries: &[(&str, &str)]) -> Vec<u8> {
    fn string(out: &mut Vec<u8>, text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        out.extend((units.len() as u32).to_le_bytes());
        out.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        out.resize(out.len().next_multiple_of(4), 0);
    }
    let mut payload = (entries.len() as u32).to_le_bytes().to_vec();
    for (kind, value) in entries {
        string(&mut payload, kind);
        string(&mut payload, value);
    }
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.extend(payload);
    out
}

/// The toolbar's primary Copy button. `copied` is the serial of the copy
/// it is confirming, if any.
pub fn button(
    palette: &Palette,
    copied: Option<usize>,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    let (from, to) = match copied {
        Some(_) => (COPY_ICON, CHECK_ICON),
        None => (CHECK_ICON, COPY_ICON),
    };
    let key: ElementId = match copied {
        Some(serial) => ("copy-done", serial).into(),
        None => "copy-idle".into(),
    };
    let color = palette.on_accent;
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
        .active(|style| style.opacity(0.8))
        .on_click(move |_, _, cx| on_click(cx))
        .child(div().relative().size(px(14.)).with_animation(
            key,
            Animation::new(ICON_FADE).with_easing(ease_out_quint()),
            move |icon, delta| {
                let layer = |path, opacity| {
                    svg()
                        .absolute()
                        .size_full()
                        .path(path)
                        .text_color(color)
                        .opacity(opacity)
                };
                icon.child(layer(from, 1. - delta)).child(layer(to, delta))
            },
        ))
        .child("Copy")
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::NORMAL)
                .text_color(palette.on_accent_muted)
                .child(editor::by_platform!("⌘⇧C", "Ctrl+Shift+C")),
        )
}

#[cfg(test)]
mod tests {
    use super::{copy_body, web_custom_data};

    #[test]
    fn copy_excludes_fields_and_preserves_body() {
        assert_eq!(
            copy_body("---\ntitle: PR\nto: a@example.com\n---\n**Body**\n"),
            "**Body**\n"
        );
        for text in [
            "Body",
            "---\nHorizontal rule",
            "---\ntitle: [broken\n---\nBody",
        ] {
            assert_eq!(copy_body(text), text);
        }
    }

    #[test]
    fn pickles_web_custom_data() {
        assert_eq!(
            web_custom_data(&[("ab", "é")]),
            [
                20, 0, 0, 0, // payload length
                1, 0, 0, 0, // entries
                2, 0, 0, 0, b'a', 0, b'b', 0, // "ab"
                1, 0, 0, 0, 0xe9, 0, 0, 0, // "é", padded
            ]
        );
    }
}
