use std::ptr::NonNull;

use block2::RcBlock;
use gpui::App;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSWorkspace};
use objc2_foundation::{
    NSArray, NSBundle, NSNotification, NSObjectProtocol, NSOperationQueue, NSString,
};

/// The label of a button that reveals a path in the file manager.
pub const REVEAL_LABEL: &str = "Show in Finder";

pub type AppearanceObserver = Retained<ProtocolObject<dyn NSObjectProtocol>>;
pub fn observe_appearance(cx: &mut App) -> AppearanceObserver {
    let (tx, rx) = async_channel::bounded::<()>(1);
    let block = RcBlock::new(move |_: NonNull<NSNotification>| {
        tx.try_send(()).ok();
    });
    // SAFETY: the notification name is an AppKit constant, the main queue
    // runs the block on the thread that registered it, and the block only
    // sends on a channel.
    let observer = unsafe {
        NSWorkspace::sharedWorkspace()
            .notificationCenter()
            .addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                None,
                Some(&NSOperationQueue::mainQueue()),
                &block,
            )
    };
    cx.spawn(async move |cx| {
        while rx.recv().await.is_ok() {
            cx.update(crate::theme::sync);
        }
    })
    .detach();
    observer
}
/// Whether the app's effective appearance is dark. Matches against Aqua and
/// Dark Aqua so the Increase Contrast variants count too; gpui's
/// `WindowAppearance` reports those as light.
pub fn window_dark(_: &gpui::Window, cx: &App) -> bool {
    system_dark(cx)
}
pub fn system_dark(_cx: &App) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    // SAFETY: the names are AppKit constants.
    let (aqua, dark_aqua) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let best = NSApplication::sharedApplication(mtm)
        .effectiveAppearance()
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[aqua, dark_aqua]));
    best.is_some_and(|name| &*name == dark_aqua)
}

/// Whether this app is the active one. Focus moving between its own
/// windows, such as into a popup panel, leaves it active.
pub fn app_active(_cx: &mut App) -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}

pub fn reduce_transparency() -> bool {
    // objc2-app-kit declares this under its NSAccessibility feature, which
    // gpui's dependency graph doesn't enable.
    // SAFETY: an NSWorkspace property returning BOOL, available since 10.10.
    unsafe {
        msg_send![
            &*NSWorkspace::sharedWorkspace(),
            accessibilityDisplayShouldReduceTransparency
        ]
    }
}

/// Whether LaunchServices started this process as its app (Finder, the
/// Dock, `open -a`). LaunchServices sets `__CFBundleIdentifier` to the
/// launched app's identifier; a shell passes on its terminal's.
pub fn launched_as_app() -> bool {
    let Some(launched) = std::env::var_os("__CFBundleIdentifier") else {
        return false;
    };
    NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some_and(|own| launched.to_str() == Some(&own.to_string()))
}

pub fn reduce_motion() -> bool {
    // SAFETY: an NSWorkspace property returning BOOL, available since 10.12.
    unsafe {
        msg_send![
            &*NSWorkspace::sharedWorkspace(),
            accessibilityDisplayShouldReduceMotion
        ]
    }
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification: &'static NSString;
}

use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString,
};
use objc2_foundation::NSData;

use crate::copy::Flavours;
pub fn write_flavours(flavours: &Flavours, _: &mut App) -> bool {
    write_pasteboard(&NSPasteboard::generalPasteboard(), flavours);
    true
}
fn write_pasteboard(pasteboard: &NSPasteboard, flavours: &Flavours) {
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
    if let Some(chromium) = &flavours.chromium {
        let kind = NSString::from_str("org.chromium.web-custom-data");
        pasteboard.setData_forType(Some(&NSData::with_bytes(chromium)), &kind);
    }
}

pub fn window_options(options: gpui::WindowOptions) -> gpui::WindowOptions {
    options
}
pub fn toolbar_items(_: &crate::theme::Palette) -> Option<gpui::AnyElement> {
    None
}
pub fn menu_color(palette: &crate::theme::Palette) -> gpui::Hsla {
    palette.menu_tint
}
/// The corner radius the window server gives pop-up windows on macOS 26.
pub const POPUP_RADIUS: f32 = 14.;

pub fn popup_background() -> gpui::WindowBackgroundAppearance {
    gpui::WindowBackgroundAppearance::Blurred
}
pub fn frame(
    content: gpui::AnyElement,
    _: &mut gpui::Window,
    _: &mut App,
    _: Box<dyn gpui::Action>,
    _: &'static str,
) -> gpui::AnyElement {
    content
}

#[cfg(test)]
mod tests {
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString,
    };
    use objc2_foundation::NSString;

    use super::write_pasteboard;
    use crate::copy::slack_flavours;

    #[test]
    fn slack_flavours_round_trip() {
        // A private named pasteboard, so the test never touches the user's.
        let name = NSString::from_str(&format!("draftmux.test.{}", std::process::id()));
        let pasteboard = NSPasteboard::pasteboardWithName(&name);
        let doc = markup::slack::parse("*Hi* `code` :tada:\n- one\n- two\n");
        let flavours = slack_flavours(&doc);
        write_pasteboard(&pasteboard, &flavours);
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
