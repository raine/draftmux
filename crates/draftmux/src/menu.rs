//! AppKit-style pop-up and context menus, drawn in the window.

use gpui::{App, IntoElement, SharedString, Window, div, prelude::*, px};

use crate::chrome::shadow;
use crate::theme::{Palette, hex};

/// One item of a pop-up menu.
pub struct MenuItem<A> {
    pub label: SharedString,
    pub checked: bool,
    /// Disabled items are drawn dimmed and can't be picked.
    pub enabled: bool,
    /// `None` draws a separator.
    pub action: Option<A>,
}

/// The open menu's panel: a check column and one row per item.
pub fn menu_panel<A: Clone + 'static>(
    c: &Palette,
    items: &[MenuItem<A>],
    on_pick: impl Fn(A, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let mut panel = div()
        .flex()
        .flex_col()
        .min_w(px(180.))
        .max_w(px(360.))
        .p(px(4.))
        .rounded(px(7.))
        // AppKit's menu font size, wherever the menu is placed.
        .text_size(px(13.))
        .line_height(px(16.))
        .bg(c.menu)
        .shadow(vec![
            shadow(hex(0x00000033), 0., 0., 0.5),
            shadow(hex(0x00000029), 6., 18., 0.),
        ]);
    let (accent, on_accent) = (c.accent, c.on_accent);
    for (ix, item) in items.iter().enumerate() {
        panel = panel.child(match &item.action {
            None => div()
                .my(px(4.))
                .mx(px(8.))
                .h(px(0.5))
                .bg(c.menu_line)
                .into_any_element(),
            Some(_) if !item.enabled => div()
                .flex()
                .items_center()
                .h(px(22.))
                .pr(px(10.))
                .text_color(c.faint)
                .child(div().w(px(20.)).flex_none())
                .child(div().truncate().child(item.label.clone()))
                .into_any_element(),
            Some(action) => {
                let action = action.clone();
                let on_pick = on_pick.clone();
                div()
                    .id(("menu-item", ix))
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .pr(px(10.))
                    .rounded(px(4.))
                    .text_color(c.text)
                    .hover(move |item| item.bg(accent).text_color(on_accent))
                    .child(
                        div()
                            .w(px(20.))
                            .flex_none()
                            .text_center()
                            .child(if item.checked { "✓" } else { "" }),
                    )
                    .child(div().truncate().child(item.label.clone()))
                    .on_click(move |_, window, cx| on_pick(action.clone(), window, cx))
                    .into_any_element()
            }
        });
    }
    panel
}
