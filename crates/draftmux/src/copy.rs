//! The Copy action: puts the draft on the pasteboard ready for its target
//! (docs/design.md, Output). Copying never changes the draft.

use draftmux_storage::Target;
use gpui::{
    App, ClipboardItem, FontWeight, KeyBinding, actions, div, linear_color_stop, linear_gradient,
    prelude::*, px, rgb, rgba, svg,
};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString,
};
use objc2_foundation::{NSData, NSString};

use crate::assets::COPY_ICON;
use crate::chrome::shadow;

actions!(copy, [Copy]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-c", Copy, None)]);
}

/// Copies `text`, a draft in `target`'s syntax, and returns the confirmation
/// to show.
pub fn copy(target: Target, text: &str, cx: &mut App) -> &'static str {
    match target {
        Target::Github => {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            "Copied markdown for GitHub"
        }
        Target::Slack => {
            write_slack(text);
            "Copied as rich text for Slack"
        }
    }
}

/// Writes `mrkdwn` as HTML, which Slack's composer reads, RTF for native
/// apps, and the mrkdwn itself as plain text. gpui's clipboard only writes
/// plain text, so this goes to AppKit directly.
fn write_slack(mrkdwn: &str) {
    let doc = markup::slack::parse(mrkdwn);
    let html = NSString::from_str(&markup::rich::html(&doc));
    let rtf = NSData::with_bytes(markup::rich::rtf(&doc).as_bytes());
    let plain = NSString::from_str(mrkdwn);
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: AppKit's pasteboard type constants are immutable statics.
    unsafe {
        pasteboard.setString_forType(&html, NSPasteboardTypeHTML);
        pasteboard.setData_forType(Some(&rtf), NSPasteboardTypeRTF);
        pasteboard.setString_forType(&plain, NSPasteboardTypeString);
    }
}

/// The toolbar's primary Copy button.
pub fn button(on_click: impl Fn(&mut App) + 'static) -> impl IntoElement {
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
            linear_color_stop(rgb(0x2f86ff), 0.),
            linear_color_stop(rgb(0x0a6cf5), 1.),
        ))
        .hover(|style| {
            style.bg(linear_gradient(
                180.,
                linear_color_stop(rgb(0x2a80fa), 0.),
                linear_color_stop(rgb(0x0864eb), 1.),
            ))
        })
        .shadow(vec![
            shadow(0x003cbe73, 0., 0., 0.5),
            shadow(0x0032a038, 1., 1.5, 0.),
        ])
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(0xffffff))
        .on_click(move |_, _, cx| on_click(cx))
        .child(
            svg()
                .path(COPY_ICON)
                .size(px(14.))
                .text_color(rgb(0xffffff)),
        )
        .child("Copy")
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::NORMAL)
                .text_color(rgba(0xffffffb3))
                .child("⌘⇧C"),
        )
}
