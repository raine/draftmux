//! The app's colours and the system state that picks them: light or dark
//! appearance, and the Reduce Transparency accessibility setting.
//!
//! [`Theme`] is a gpui global read at render time, so any window can use it
//! with [`get`] or [`palette`]. Appearance changes call [`sync_window`], which
//! refreshes every window; editors made with [`editor`] restyle themselves.
//!
//! Blurred windows keep their blur when Reduce Transparency is on: the
//! surfaces that would show it paint [`Palette::sidebar`] and
//! [`Palette::menu_tint`], which turn opaque and cover it.

use std::rc::Rc;

use crate::menu::{MenuItem, menu_panel};
use crate::platform;
/// The monospaced font for code, paths and model names.
pub use editor::platform::MONO;
use editor::{ContextMenuItem, Editor, EditorStyle};
use gpui::{AnyElement, App, AppContext, Entity, Global, Hsla, IntoElement, Window, rgba};

pub struct Theme {
    pub dark: bool,
    pub reduce_transparency: bool,
    pub palette: Palette,
    /// Keeps the Reduce Transparency observer registered.
    _observer: platform::AppearanceObserver,
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
    let observer = platform::observe_appearance(cx);
    let (dark, reduce_transparency) = (platform::system_dark(cx), platform::reduce_transparency());
    cx.set_global(Theme {
        dark,
        reduce_transparency,
        palette: Palette::resolve(dark, reduce_transparency),
        _observer: observer,
    });
}

/// Re-reads the system appearance and Reduce Transparency, refreshing every
/// window if either changed. Call when a window's appearance changes.
#[cfg(target_os = "macos")]
pub fn sync(cx: &mut App) {
    update(platform::system_dark(cx), cx);
}

pub fn sync_window(window: &gpui::Window, cx: &mut App) {
    update(platform::window_dark(window, cx), cx);
}

fn update(dark: bool, cx: &mut App) {
    let reduce_transparency = platform::reduce_transparency();
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
        editor.set_context_menu(Rc::new(context_menu));
        cx.observe_global::<Theme>(move |editor: &mut Editor, cx| {
            let style = style(palette(cx));
            editor.set_style(style, cx);
        })
        .detach();
        editor
    })
}

/// The editor's right-click menu, drawn like the app's other menus.
fn context_menu(items: Vec<ContextMenuItem>, _: &mut Window, cx: &mut App) -> AnyElement {
    let (items, actions): (Vec<_>, Vec<_>) = items
        .into_iter()
        .enumerate()
        .map(|(ix, item)| {
            let menu_item = MenuItem {
                label: item.label,
                checked: false,
                enabled: item.enabled,
                action: item.action.is_some().then_some(ix),
            };
            (menu_item, item.action)
        })
        .unzip();
    let actions = Rc::new(actions);
    menu_panel(
        palette(cx),
        &items,
        move |ix, window, cx| {
            window.dispatch_action(Box::new(editor::DismissMenu), cx);
            if let Some(action) = &actions[ix] {
                window.dispatch_action(action.boxed_clone(), cx);
            }
        },
        |window, cx| window.dispatch_action(Box::new(editor::DismissMenu), cx),
    )
    .into_any_element()
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
    /// Fill behind accent text, such as the Sample badge.
    pub accent_soft: Hsla,
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
    /// Cards in the chat, such as the sample draft's.
    pub card: Hsla,
    pub card_ring: Hsla,
    pub card_drop: Hsla,
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
            text: hex(0x1d1d1fff),
            muted: hex(0x3c3c43a8),
            faint: hex(0x3c3c4373),
            hairline: hex(0x00000016),
            line_strong: hex(0x00000024),
            accent: hex(0x0a6cf5ff),
            accent_soft: hex(0xe5efffff),
            on_accent: hex(0xffffffff),
            on_accent_muted: hex(0xffffffb3),
            danger: hex(0xc9372cff),
            warning: hex(0xb25000ff),
            pane: hex(0xffffffff),
            chat: hex(0xfbfbfcff),
            toolbar: hex(0xfbfbfcff),
            sidebar: hex(0xe8e8edbd),
            sidebar_edge: hex(0x00000012),
            selected_row: hex(0x00000016),
            hover: hex(0x0000000d),
            segment_track: hex(0x7676801f),
            segment_on: hex(0xffffffff),
            segment_ring: hex(0x0000000f),
            segment_drop: hex(0x00000029),
            primary_top: hex(0x2f86ffff),
            primary_bottom: hex(0x0a6cf5ff),
            primary_hover_top: hex(0x2a80faff),
            primary_hover_bottom: hex(0x0864ebff),
            primary_ring: hex(0x003cbe73),
            primary_drop: hex(0x0032a038),
            toast: hex(0x26262cff),
            toast_text: hex(0xffffffff),
            toast_button: hex(0xffffff24),
            toast_button_hover: hex(0xffffff38),
            toast_drop: hex(0x00000040),
            scrollbar_thumb: hex(0x00000038),
            scrollbar_thumb_active: hex(0x0000005c),
            control: hex(0xffffffff),
            control_hover: hex(0xf4f4f7ff),
            card: hex(0xffffffff),
            card_ring: hex(0x0000001f),
            card_drop: hex(0x0000000d),
            send_disabled: hex(0xc9cfdfff),
            send_busy: hex(0x2b2b31ff),
            spinner_track: hex(0xcdd3e0ff),
            tool_error: hex(0xc9372ccc),
            tool_body: hex(0xf4f4f6ff),
            tool_text: hex(0x3a3a42ff),
            added: hex(0x1f7a42ff),
            added_bg: hex(0xe3f4e8ff),
            removed: hex(0xb0342aff),
            removed_bg: hex(0xfbe7e5ff),
            diff_added: hex(0x1c5e34ff),
            diff_removed: hex(0x8c2a22ff),
            menu: hex(0xf6f6f8ff),
            menu_tint: hex(0xf6f6f8db),
            menu_line: hex(0x00000016),
            menu_field: hex(0xffffffff),
            editor_text: hex(0x2a2a2eff),
            editor_read_only: hex(0x8a8a94ff),
            editor_code: hex(0xa8336aff),
            selection: hex(0xb3d4ffff),
            inactive_selection: hex(0xdcdce0ff),
        }
    }

    pub fn dark() -> Self {
        Self {
            text: hex(0xececf0ff),
            muted: hex(0xebebf599),
            faint: hex(0xebebf561),
            hairline: hex(0xffffff16),
            line_strong: hex(0xffffff24),
            accent: hex(0x0a84ffff),
            accent_soft: hex(0x0a84ff2e),
            on_accent: hex(0xffffffff),
            on_accent_muted: hex(0xffffffb3),
            danger: hex(0xff6961ff),
            warning: hex(0xffb340ff),
            pane: hex(0x1f1f22ff),
            chat: hex(0x1c1c1fff),
            toolbar: hex(0x2a2a2dff),
            sidebar: hex(0x222226e6),
            sidebar_edge: hex(0x00000073),
            selected_row: hex(0xffffff1a),
            hover: hex(0xffffff12),
            segment_track: hex(0x7676803d),
            segment_on: hex(0x636366ff),
            segment_ring: hex(0x00000000),
            segment_drop: hex(0x00000059),
            primary_top: hex(0x2d93ffff),
            primary_bottom: hex(0x0a7cf5ff),
            primary_hover_top: hex(0x46a0ffff),
            primary_hover_bottom: hex(0x1f88f7ff),
            primary_ring: hex(0x00000080),
            primary_drop: hex(0x00000040),
            toast: hex(0x3a3a3fff),
            toast_text: hex(0xffffffff),
            toast_button: hex(0xffffff24),
            toast_button_hover: hex(0xffffff38),
            toast_drop: hex(0x00000073),
            scrollbar_thumb: hex(0xffffff38),
            scrollbar_thumb_active: hex(0xffffff57),
            control: hex(0x3a3a3eff),
            control_hover: hex(0x444448ff),
            card: hex(0x2a2a2eff),
            card_ring: hex(0xffffff1a),
            card_drop: hex(0x0000000d),
            send_disabled: hex(0x3a3a40ff),
            send_busy: hex(0x636366ff),
            spinner_track: hex(0x4a4a52ff),
            tool_error: hex(0xff6961cc),
            tool_body: hex(0x2a2a2eff),
            tool_text: hex(0xd0d0d6ff),
            added: hex(0x32d74bff),
            added_bg: hex(0x32d74b26),
            removed: hex(0xff6961ff),
            removed_bg: hex(0xff696126),
            diff_added: hex(0x7ee08fff),
            diff_removed: hex(0xff8a80ff),
            menu: hex(0x2c2c30ff),
            menu_tint: hex(0x2c2c30db),
            menu_line: hex(0xffffff1f),
            menu_field: hex(0x1f1f22ff),
            editor_text: hex(0xd9d9dfff),
            editor_read_only: hex(0x6f6f78ff),
            editor_code: hex(0xff7ab2ff),
            selection: hex(0x0a84ff66),
            inactive_selection: hex(0x3a3a3eff),
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
pub(crate) fn hex(value: u32) -> Hsla {
    rgba(value).into()
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
