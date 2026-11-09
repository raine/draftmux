use gpui::App;

pub struct AppearanceObserver;
pub fn observe_appearance(_: &mut App) -> AppearanceObserver {
    AppearanceObserver
}
pub fn window_dark(window: &gpui::Window, _: &App) -> bool {
    matches!(
        window.appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    )
}
pub fn system_dark(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    )
}
pub fn reduce_transparency() -> bool {
    false
}
pub fn reduce_motion() -> bool {
    false
}

/// Retaining the clipboard keeps X11 selection ownership alive.
struct RichClipboard(arboard::Clipboard);
impl gpui::Global for RichClipboard {}

pub fn write_flavours(flavours: &crate::copy::Flavours, cx: &mut App) -> bool {
    let _ = &flavours.rtf; // Linux consumers use HTML rather than RTF.
    if !cx.has_global::<RichClipboard>() {
        match arboard::Clipboard::new() {
            Ok(clipboard) => cx.set_global(RichClipboard(clipboard)),
            Err(error) => {
                eprintln!("draftmux: rich clipboard unavailable: {error}");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(flavours.plain.clone()));
                return false;
            }
        }
    }
    match cx
        .global_mut::<RichClipboard>()
        .0
        .set_html(flavours.html.clone(), Some(flavours.plain.clone()))
    {
        Ok(()) => true,
        Err(error) => {
            eprintln!("draftmux: rich clipboard unavailable: {error}");
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(flavours.plain.clone()));
            false
        }
    }
}

/// The label of a button that reveals a path in the file manager.
pub const REVEAL_LABEL: &str = "Show in File Manager";

pub fn window_options(mut options: gpui::WindowOptions) -> gpui::WindowOptions {
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.appears_transparent = false;
        titlebar.traffic_light_position = None;
    }
    options.window_background = gpui::WindowBackgroundAppearance::Opaque;
    options.app_id = Some("io.github.raine.Draftmux".into());
    options
}
pub fn toolbar_items(palette: &crate::theme::Palette) -> Option<gpui::AnyElement> {
    use gpui::{MouseButton, div, prelude::*, px};
    Some(
        div()
            .id("open-settings")
            .mx(px(12.))
            .my(px(10.))
            .px(px(8.))
            .py(px(4.))
            .text_size(px(12.))
            .rounded(px(5.))
            .hover(|s| s.bg(palette.hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| crate::settings_window::open(cx))
            .child("Settings")
            .into_any_element(),
    )
}
pub fn menu_color(palette: &crate::theme::Palette) -> gpui::Hsla {
    palette.menu
}
pub fn popup_background() -> gpui::WindowBackgroundAppearance {
    gpui::WindowBackgroundAppearance::Opaque
}
#[path = "platform_linux/frame.rs"]
mod frame;
pub use frame::frame;
