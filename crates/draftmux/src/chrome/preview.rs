//! A cached rendering boundary for the preview's fixed-size viewport.

use draftmux_storage::Target;
use gpui::{
    AnyElement, Bounds, Context, Entity, IntoElement, Pixels, Render, ScrollHandle, Window, canvas,
    div, prelude::*, px,
};
use markup::Document;

use crate::block_cache::BlockCache;
use crate::markup_view::{self, SelectBody};
use crate::selectable::{self, TextSelection};
use crate::theme;

pub(super) struct Preview {
    pub document: Option<Document>,
    pub target: Option<Target>,
    pub selection: Entity<TextSelection>,
    pub scroll: ScrollHandle,
    pub cache: BlockCache,
    pub appearance: Option<(Target, bool, f32)>,
    pub _selection: gpui::Subscription,
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        canvas(
            move |bounds, window, cx| {
                let mut body = entity.update(cx, |this, cx| this.body(bounds, window, cx));
                body.layout_as_root(bounds.size.into(), window, cx);
                body.prepaint_at(bounds.origin, window, cx);
                entity.update(cx, |this, cx| {
                    let texts = this.cache.texts();
                    this.selection
                        .update(cx, |selection, _| selection.set_texts(texts));
                });
                body
            },
            |_, mut body: AnyElement, window, cx| body.paint(window, cx),
        )
        .size_full()
    }
}

impl Preview {
    fn body(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _timing = editor::timing::span("preview-elements");
        let frame = editor::timing::span("preview-frame");
        let theme = theme::get(cx);
        let appearance = self
            .target
            .map(|target| (target, theme.dark, window.scale_factor()));
        if self.appearance != appearance {
            self.cache.clear();
            self.appearance = appearance;
        }
        if self
            .document
            .as_ref()
            .is_none_or(|doc| doc.blocks.is_empty())
        {
            self.cache.reset();
        }
        // px_6 is 1.5 rem on each side of the scrolling viewport.
        let width = (bounds.size.width - window.rem_size() * 3.).max(px(0.));
        let (body, background) = match (&self.document, self.target) {
            (Some(doc), Some(target)) if !doc.blocks.is_empty() => {
                let style = std::rc::Rc::new(markup_view::style(target, theme.dark));
                (
                    markup_view::render_preview(
                        doc,
                        style.clone(),
                        &self.scroll,
                        Some(SelectBody {
                            state: self.selection.clone(),
                            body: 0,
                            color: theme.palette.selection,
                            registry: None,
                        }),
                        (&mut self.cache, width),
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
            .into_any_element()
    }
}
