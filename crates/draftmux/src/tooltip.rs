//! Hover labels for icon-only buttons, shown after gpui's hover delay.

use gpui::{AnyView, App, Context, SharedString, Window, div, prelude::*, px};

use crate::chrome::shadow;
use crate::theme::{self, hex};

pub struct Tooltip {
    text: SharedString,
}

/// A tooltip builder for `.tooltip(...)` showing `text`.
pub fn text(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip { text: text.clone() }).into()
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        // gpui places the tooltip at the pointer; the margin clears the cursor.
        div().pl(px(8.)).pt(px(16.)).child(
            div()
                .px(px(7.))
                .py(px(3.))
                .rounded(px(5.))
                .bg(palette.menu)
                .text_color(palette.text)
                .text_size(px(12.))
                .line_height(px(15.))
                .whitespace_nowrap()
                .shadow(vec![
                    shadow(hex(0x00000033), 0., 0., 0.5),
                    shadow(hex(0x00000026), 2., 8., 0.),
                ])
                .child(self.text.clone()),
        )
    }
}
