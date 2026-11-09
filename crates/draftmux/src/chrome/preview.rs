//! A cached rendering boundary for the preview's fixed-size viewport.

use draftmux_storage::Target;
use gpui::{
    Context, Entity, IntoElement, Render, ScrollHandle, Window, canvas, div, prelude::*, px,
};
use markup::Document;

use crate::markup_view::{self, SelectBody};
use crate::selectable::{self, TextSelection};
use crate::theme;

pub(super) struct Preview {
    pub document: Option<Document>,
    pub target: Option<Target>,
    pub selection: Entity<TextSelection>,
    pub scroll: ScrollHandle,
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timing = editor::timing::span("preview-elements");
        let frame = editor::timing::span("preview-frame");
        let theme = theme::get(cx);
        let (body, background) = match (&self.document, self.target) {
            (Some(doc), Some(target)) if !doc.blocks.is_empty() => {
                let style = markup_view::style(target, theme.dark);
                (
                    markup_view::render_preview(
                        doc,
                        &style,
                        "preview-body",
                        &self.scroll,
                        Some(SelectBody {
                            state: self.selection.clone(),
                            body: 0,
                            color: theme.palette.selection,
                        }),
                        window,
                        cx,
                    ),
                    Some(style.background),
                )
            }
            _ => (
                div()
                    .text_size(px(14.))
                    .text_color(theme.palette.faint)
                    .child("Nothing to preview")
                    .into_any_element(),
                None,
            ),
        };
        selectable::area(&self.selection, cx)
            .id("preview")
            .size_full()
            .when_some(background, |this, background| this.bg(background))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_6()
            .pt_5()
            .pb_10()
            .child(body)
            .child(canvas(|_, _, _| (), move |_, _, _, _| drop(frame)).absolute())
    }
}
