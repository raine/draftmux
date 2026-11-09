//! The app's colours and the system state that picks them: light or dark
//! appearance, and the Reduce Transparency accessibility setting.
//!
//! [`Theme`] is a gpui global read at render time, so any window can use it
//! with [`get`] or [`palette`]. It changes only through [`sync`], which
//! refreshes every window; editors made with [`editor`] restyle themselves.
//!
//! Blurred windows keep their blur when Reduce Transparency is on: the
//! surfaces that would show it paint [`Palette::sidebar`] and
//! [`Palette::menu_tint`], which turn opaque and cover it.

use std::ptr::NonNull;

use block2::RcBlock;
use editor::{Editor, EditorStyle};
use gpui::{App, AppContext, Entity, Global, Hsla, rgba};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSWorkspace};
use objc2_foundation::{NSArray, NSNotification, NSObjectProtocol, NSOperationQueue, NSString};

pub struct Theme {
    pub dark: bool,
    pub reduce_transparency: bool,
    pub palette: Palette,
    /// Keeps the Reduce Transparency observer registered.
    _observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Global for Theme {}

pub fn get(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

pub fn palette(cx: &App) -> &Palette {
    &get(cx).palette
}

/// Reads the system state and starts following Reduce Transparency. Call
/// before opening windows so their first frame has the right colours.
pub fn init(cx: &mut App) {
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
    let (dark, reduce_transparency) = (system_dark(), reduce_transparency());
    cx.set_global(Theme {
        dark,
        reduce_transparency,
        palette: Palette::resolve(dark, reduce_transparency),
        _observer: observer,
    });
    cx.spawn(async move |cx| {
        while rx.recv().await.is_ok() {
            cx.update(sync);
        }
    })
    .detach();
}

/// Re-reads the system appearance and Reduce Transparency, refreshing every
/// window if either changed. Call when a window's appearance changes.
pub fn sync(cx: &mut App) {
    let (dark, reduce_transparency) = (system_dark(), reduce_transparency());
    let theme = get(cx);
    if theme.dark == dark && theme.reduce_transparency == reduce_transparency {
        return;
    }
    let theme = cx.global_mut::<Theme>();
    theme.dark = dark;
    theme.reduce_transparency = reduce_transparency;
    theme.palette = Palette::resolve(dark, reduce_transparency);
    cx.refresh_windows();
}

/// An empty editor styled by `style` from the palette, restyled whenever the
/// theme changes.
pub fn editor(style: fn(&Palette) -> EditorStyle, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut editor = Editor::new("", cx);
        editor.set_style(style(palette(cx)), cx);
        cx.observe_global::<Theme>(move |editor: &mut Editor, cx| {
            let style = style(palette(cx));
            editor.set_style(style, cx);
        })
        .detach();
        editor
    })
}

/// Whether the app's effective appearance is dark. Matches against Aqua and
/// Dark Aqua so the Increase Contrast variants count too; gpui's
/// `WindowAppearance` reports those as light.
fn system_dark() -> bool {
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

fn reduce_transparency() -> bool {
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

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification: &'static NSString;
}

/// Colours of the "Native" prototype variant (history/docs/prototype/index.html):
/// `.t-native` for light, `.t-dark` and `body.t-dark-pop` for dark.
#[derive(Clone, Copy)]
pub struct Palette {
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub hairline: Hsla,
    pub line_strong: Hsla,
    pub accent: Hsla,
    /// Text and icons on accent fills.
    pub on_accent: Hsla,
    pub on_accent_muted: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    /// Editor and preview panes.
    pub pane: Hsla,
    pub chat: Hsla,
    /// The toolbar above the content.
    pub toolbar: Hsla,
    /// Tint over the window blur; opaque with Reduce Transparency.
    pub sidebar: Hsla,
    pub sidebar_edge: Hsla,
    pub selected_row: Hsla,
    /// Background of ghost buttons and rows under the pointer.
    pub hover: Hsla,
    pub archive_button: Hsla,
    pub archive_button_selected: Hsla,
    pub archive_button_hover: Hsla,
    pub segment_track: Hsla,
    pub segment_on: Hsla,
    pub segment_ring: Hsla,
    pub segment_drop: Hsla,
    pub primary_top: Hsla,
    pub primary_bottom: Hsla,
    pub primary_hover_top: Hsla,
    pub primary_hover_bottom: Hsla,
    pub primary_ring: Hsla,
    pub primary_drop: Hsla,
    pub toast: Hsla,
    pub toast_text: Hsla,
    pub toast_button: Hsla,
    pub toast_button_hover: Hsla,
    pub toast_drop: Hsla,
    pub scrollbar_thumb: Hsla,
    pub scrollbar_thumb_active: Hsla,
    /// Bordered controls on a pane, such as Retry and the composer box.
    pub control: Hsla,
    pub control_hover: Hsla,
    pub send_disabled: Hsla,
    pub send_busy: Hsla,
    pub spinner_track: Hsla,
    pub tool_error: Hsla,
    pub tool_body: Hsla,
    pub tool_text: Hsla,
    pub added: Hsla,
    pub added_bg: Hsla,
    pub removed: Hsla,
    pub removed_bg: Hsla,
    /// Text of added and removed lines in a diff.
    pub diff_added: Hsla,
    pub diff_removed: Hsla,
    /// Menus drawn inside the window.
    pub menu: Hsla,
    /// Tint of menus in their own blurred window; opaque with Reduce
    /// Transparency.
    pub menu_tint: Hsla,
    pub menu_line: Hsla,
    pub menu_field: Hsla,
    pub editor_text: Hsla,
    pub editor_read_only: Hsla,
    pub editor_code: Hsla,
    pub selection: Hsla,
    pub inactive_selection: Hsla,
}

impl Palette {
    /// The palette for an appearance and Reduce Transparency setting.
    pub fn resolve(dark: bool, reduce_transparency: bool) -> Self {
        let mut palette = if dark { Self::dark() } else { Self::light() };
        if reduce_transparency {
            palette.sidebar.a = 1.;
            palette.menu_tint = palette.menu;
        }
        palette
    }

    pub fn light() -> Self {
        Self {
            text: c(0x1d1d1fff),
            muted: c(0x3c3c43a8),
            faint: c(0x3c3c4373),
            hairline: c(0x00000016),
            line_strong: c(0x00000024),
            accent: c(0x0a6cf5ff),
            on_accent: c(0xffffffff),
            on_accent_muted: c(0xffffffb3),
            danger: c(0xc9372cff),
            warning: c(0xb25000ff),
            pane: c(0xffffffff),
            chat: c(0xfbfbfcff),
            toolbar: c(0xfbfbfcff),
            sidebar: c(0xe8e8edbd),
            sidebar_edge: c(0x00000012),
            selected_row: c(0x00000016),
            hover: c(0x0000000d),
            archive_button: c(0xe4e4eaff),
            archive_button_selected: c(0xd9d9dfff),
            archive_button_hover: c(0xd6d6deff),
            segment_track: c(0x7676801f),
            segment_on: c(0xffffffff),
            segment_ring: c(0x0000000f),
            segment_drop: c(0x00000029),
            primary_top: c(0x2f86ffff),
            primary_bottom: c(0x0a6cf5ff),
            primary_hover_top: c(0x2a80faff),
            primary_hover_bottom: c(0x0864ebff),
            primary_ring: c(0x003cbe73),
            primary_drop: c(0x0032a038),
            toast: c(0x26262cff),
            toast_text: c(0xffffffff),
            toast_button: c(0xffffff24),
            toast_button_hover: c(0xffffff38),
            toast_drop: c(0x00000040),
            scrollbar_thumb: c(0x00000038),
            scrollbar_thumb_active: c(0x0000005c),
            control: c(0xffffffff),
            control_hover: c(0xf4f4f7ff),
            send_disabled: c(0xc9cfdfff),
            send_busy: c(0x2b2b31ff),
            spinner_track: c(0xcdd3e0ff),
            tool_error: c(0xc9372ccc),
            tool_body: c(0xf4f4f6ff),
            tool_text: c(0x3a3a42ff),
            added: c(0x1f7a42ff),
            added_bg: c(0xe3f4e8ff),
            removed: c(0xb0342aff),
            removed_bg: c(0xfbe7e5ff),
            diff_added: c(0x1c5e34ff),
            diff_removed: c(0x8c2a22ff),
            menu: c(0xf6f6f8ff),
            menu_tint: c(0xf6f6f8db),
            menu_line: c(0x00000016),
            menu_field: c(0xffffffff),
            editor_text: c(0x2a2a2eff),
            editor_read_only: c(0x8a8a94ff),
            editor_code: c(0xa8336aff),
            selection: c(0xb3d4ffff),
            inactive_selection: c(0xdcdce0ff),
        }
    }

    pub fn dark() -> Self {
        Self {
            text: c(0xececf0ff),
            muted: c(0xebebf599),
            faint: c(0xebebf561),
            hairline: c(0xffffff16),
            line_strong: c(0xffffff24),
            accent: c(0x0a84ffff),
            on_accent: c(0xffffffff),
            on_accent_muted: c(0xffffffb3),
            danger: c(0xff6961ff),
            warning: c(0xffb340ff),
            pane: c(0x1f1f22ff),
            chat: c(0x1c1c1fff),
            toolbar: c(0x2a2a2dff),
            sidebar: c(0x222226e6),
            sidebar_edge: c(0x00000073),
            selected_row: c(0xffffff1a),
            hover: c(0xffffff12),
            archive_button: c(0x3a3a3eff),
            archive_button_selected: c(0x444448ff),
            archive_button_hover: c(0x4a4a4fff),
            segment_track: c(0x7676803d),
            segment_on: c(0x636366ff),
            segment_ring: c(0x00000000),
            segment_drop: c(0x00000059),
            primary_top: c(0x2d93ffff),
            primary_bottom: c(0x0a7cf5ff),
            primary_hover_top: c(0x46a0ffff),
            primary_hover_bottom: c(0x1f88f7ff),
            primary_ring: c(0x00000080),
            primary_drop: c(0x00000040),
            toast: c(0x3a3a3fff),
            toast_text: c(0xffffffff),
            toast_button: c(0xffffff24),
            toast_button_hover: c(0xffffff38),
            toast_drop: c(0x00000073),
            scrollbar_thumb: c(0xffffff38),
            scrollbar_thumb_active: c(0xffffff57),
            control: c(0x3a3a3eff),
            control_hover: c(0x444448ff),
            send_disabled: c(0x3a3a40ff),
            send_busy: c(0x636366ff),
            spinner_track: c(0x4a4a52ff),
            tool_error: c(0xff6961cc),
            tool_body: c(0x2a2a2eff),
            tool_text: c(0xd0d0d6ff),
            added: c(0x32d74bff),
            added_bg: c(0x32d74b26),
            removed: c(0xff6961ff),
            removed_bg: c(0xff696126),
            diff_added: c(0x7ee08fff),
            diff_removed: c(0xff8a80ff),
            menu: c(0x2c2c30ff),
            menu_tint: c(0x2c2c30db),
            menu_line: c(0xffffff1f),
            menu_field: c(0x1f1f22ff),
            editor_text: c(0xd9d9dfff),
            editor_read_only: c(0x6f6f78ff),
            editor_code: c(0xff7ab2ff),
            selection: c(0x0a84ff66),
            inactive_selection: c(0x3a3a3eff),
        }
    }

    /// The draft editor's style: [`EditorStyle::default`]'s metrics with
    /// these colours. Other editors override the metrics.
    pub fn editor_style(&self) -> EditorStyle {
        EditorStyle {
            text: self.editor_text,
            read_only_text: self.editor_read_only,
            heading: self.text,
            code: self.editor_code,
            link: self.accent,
            muted: self.muted,
            caret: self.accent,
            selection: self.selection,
            inactive_selection: self.inactive_selection,
            ..EditorStyle::default()
        }
    }
}

/// A colour from `0xRRGGBBAA`.
fn c(hex: u32) -> Hsla {
    rgba(hex).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduce_transparency_makes_translucent_surfaces_opaque() {
        for dark in [false, true] {
            let tinted = Palette::resolve(dark, false);
            assert!(tinted.sidebar.a < 1. && tinted.menu_tint.a < 1.);
            let opaque = Palette::resolve(dark, true);
            assert_eq!((opaque.sidebar.a, opaque.menu_tint.a), (1., 1.));
        }
    }
}
