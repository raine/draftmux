//! The Copy action: puts the draft on the pasteboard ready for its target
//! (docs/design.md, Output). Copying never changes the draft.

use draftmux_storage::Target;
use gpui::{
    App, ClipboardItem, FontWeight, KeyBinding, actions, div, linear_color_stop, linear_gradient,
    prelude::*, px, svg,
};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString,
};
use objc2_foundation::{NSData, NSString};

use crate::assets::COPY_ICON;
use crate::chrome::shadow;
use crate::theme::Palette;

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
        Target::Email => {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            "Copied plain text for email"
        }
        Target::Slack => {
            write_slack(text);
            "Copied as rich text for Slack"
        }
    }
}

/// Writes `mrkdwn` to the general pasteboard as Slack's composer should
/// receive it. gpui's clipboard only writes plain text, so this goes to
/// AppKit directly.
fn write_slack(mrkdwn: &str) {
    write_flavours(
        &NSPasteboard::generalPasteboard(),
        &slack_flavours(&markup::slack::parse(mrkdwn)),
    );
}

/// The pasteboard contents for a Slack message: HTML, which Slack's composer
/// reads, RTF for native apps, and the visible text for pasting without
/// formatting.
struct Flavours {
    html: String,
    rtf: String,
    plain: String,
}

fn slack_flavours(doc: &markup::Document) -> Flavours {
    Flavours {
        html: markup::rich::html(doc),
        rtf: markup::rich::rtf(doc),
        plain: markup::rich::plain(doc),
    }
}

fn write_flavours(pasteboard: &NSPasteboard, flavours: &Flavours) {
    pasteboard.clearContents();
    let html = NSString::from_str(&flavours.html);
    let rtf = NSData::with_bytes(flavours.rtf.as_bytes());
    let plain = NSString::from_str(&flavours.plain);
    // SAFETY: AppKit's pasteboard type constants are immutable statics.
    unsafe {
        pasteboard.setString_forType(&html, NSPasteboardTypeHTML);
        pasteboard.setData_forType(Some(&rtf), NSPasteboardTypeRTF);
        pasteboard.setString_forType(&plain, NSPasteboardTypeString);
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
                .child("⌘⇧C"),
        )
}

#[cfg(test)]
mod tests {
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString,
    };
    use objc2_foundation::NSString;

    use super::{slack_flavours, write_flavours};

    #[test]
    fn slack_flavours_round_trip() {
        // A private named pasteboard, so the test never touches the user's.
        let name = NSString::from_str(&format!("draftmux.test.{}", std::process::id()));
        let pasteboard = NSPasteboard::pasteboardWithName(&name);
        let doc = markup::slack::parse("*Hi* `code` :tada:\n- one\n- two\n");
        let flavours = slack_flavours(&doc);
        write_flavours(&pasteboard, &flavours);
        // SAFETY: AppKit's pasteboard type constants are immutable statics.
        let (html, rtf, plain) = unsafe {
            (
                pasteboard.stringForType(NSPasteboardTypeHTML),
                pasteboard.dataForType(NSPasteboardTypeRTF),
                pasteboard.stringForType(NSPasteboardTypeString),
            )
        };
        // SAFETY: `releaseGlobally` takes no arguments and returns nothing.
        let () = unsafe { objc2::msg_send![&*pasteboard, releaseGlobally] };
        assert_eq!(html.unwrap().to_string(), flavours.html);
        assert_eq!(rtf.unwrap().to_vec(), flavours.rtf.into_bytes());
        assert_eq!(plain.unwrap().to_string(), flavours.plain);
        assert_eq!(flavours.plain, "Hi code 🎉\n• one\n• two");
    }
}
