//! Thin overlay scrollbars, drawn over the right edge of a scrolling pane
//! like macOS overlay scrollers: no track, a rounded thumb that strengthens while
//! hovered or dragged, hidden when the content fits.

use std::cell::Cell;
use std::rc::Rc;

use editor::Editor;
use gpui::{
    App, Bounds, DispatchPhase, Entity, HitboxBehavior, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, Window, canvas, div, point, prelude::*, px,
    quad, size,
};

use crate::theme;

const TRACK_WIDTH: Pixels = px(11.);
const THUMB_INSET: Pixels = px(3.);
const MIN_THUMB: Pixels = px(24.);

/// What the scrollbar scrolls.
#[derive(Clone)]
pub enum Source {
    Handle(ScrollHandle),
    /// The editor lays out and scrolls its own text.
    Editor(Entity<Editor>),
}

/// Pointer state that outlives a frame. Keep one per scrollbar.
#[derive(Clone, Default)]
pub struct ScrollbarState(Rc<Inner>);

#[derive(Default)]
struct Inner {
    hovered: Cell<bool>,
    /// While dragging, where the thumb was grabbed, from its top.
    grab: Cell<Option<Pixels>>,
}

#[derive(Clone, Copy)]
struct Metrics {
    viewport: Pixels,
    content: Pixels,
    scroll: Pixels,
}

impl Source {
    fn metrics(&self, cx: &App) -> Option<Metrics> {
        match self {
            Source::Handle(handle) => {
                let viewport = handle.bounds().size.height;
                Some(Metrics {
                    viewport,
                    content: viewport + handle.max_offset().y,
                    scroll: -handle.offset().y,
                })
            }
            Source::Editor(editor) => {
                let (viewport, content, scroll) = editor.read(cx).scroll_metrics()?;
                Some(Metrics {
                    viewport,
                    content,
                    scroll,
                })
            }
        }
    }

    fn scroll_to(&self, y: Pixels, window: &mut Window, cx: &mut App) {
        match self {
            Source::Handle(handle) => {
                handle.set_offset(point(handle.offset().x, -y));
                window.refresh();
            }
            Source::Editor(editor) => editor.update(cx, |editor, cx| editor.set_scroll_y(y, cx)),
        }
    }
}

impl Metrics {
    /// The thumb's top and height within a track of `track` height.
    fn thumb(&self, track: Pixels) -> Option<(Pixels, Pixels)> {
        let max_scroll = self.content - self.viewport;
        if max_scroll <= px(0.5) || track <= MIN_THUMB {
            return None;
        }
        let height = (track * (self.viewport / self.content)).max(MIN_THUMB);
        let top = (track - height) * (self.scroll / max_scroll).clamp(0., 1.);
        Some((top, height))
    }

    /// The scroll offset that puts the thumb's top at `top`, for a thumb from
    /// `thumb()`, which is always shorter than the track.
    fn scroll_for(&self, top: Pixels, height: Pixels, track: Pixels) -> Pixels {
        (self.content - self.viewport) * (top / (track - height)).clamp(0., 1.)
    }
}

/// An overlay for the right edge of the parent, which must be `relative()`
/// and the same height as the scrolled viewport.
pub fn scrollbar(source: Source, state: &ScrollbarState) -> impl IntoElement {
    let state = state.clone();
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(TRACK_WIDTH)
        .child(
            canvas(
                |bounds, window, _| {
                    window.insert_hitbox(bounds, HitboxBehavior::BlockMouseExceptScroll)
                },
                move |bounds: Bounds<Pixels>, hitbox, window, cx| {
                    let track = bounds.size.height;
                    let Some(metrics) = source.metrics(cx) else {
                        return;
                    };
                    let Some((top, height)) = metrics.thumb(track) else {
                        return;
                    };
                    let active = state.0.hovered.get() || state.0.grab.get().is_some();
                    let palette = theme::palette(cx);
                    let width = TRACK_WIDTH - THUMB_INSET * 2.;
                    window.paint_quad(quad(
                        Bounds::new(
                            point(bounds.left() + THUMB_INSET, bounds.top() + top),
                            size(width, height),
                        ),
                        width / 2.,
                        if active {
                            palette.scrollbar_thumb_active
                        } else {
                            palette.scrollbar_thumb
                        },
                        px(0.),
                        gpui::transparent_black(),
                        Default::default(),
                    ));

                    window.on_mouse_event({
                        let (state, source, hitbox) =
                            (state.clone(), source.clone(), hitbox.clone());
                        move |event: &MouseDownEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble
                                || event.button != MouseButton::Left
                                || !hitbox.is_hovered(window)
                            {
                                return;
                            }
                            cx.stop_propagation();
                            let y = event.position.y - bounds.top();
                            let grab = if (top..top + height).contains(&y) {
                                y - top
                            } else {
                                // Clicking the track jumps there, as with macOS's
                                // "jump to the spot that's clicked".
                                let grab = height / 2.;
                                source.scroll_to(
                                    metrics.scroll_for(y - grab, height, track),
                                    window,
                                    cx,
                                );
                                grab
                            };
                            state.0.grab.set(Some(grab));
                            window.refresh();
                        }
                    });
                    window.on_mouse_event({
                        let (state, source, hitbox) =
                            (state.clone(), source.clone(), hitbox.clone());
                        move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let hovered = hitbox.is_hovered(window);
                            if state.0.hovered.replace(hovered) != hovered {
                                window.refresh();
                            }
                            if let Some(grab) = state.0.grab.get() {
                                if event.pressed_button != Some(MouseButton::Left) {
                                    state.0.grab.set(None);
                                    window.refresh();
                                    return;
                                }
                                let top = event.position.y - bounds.top() - grab;
                                source.scroll_to(
                                    metrics.scroll_for(top, height, track),
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }
                        }
                    });
                    window.on_mouse_event({
                        let state = state.clone();
                        move |_: &MouseUpEvent, phase, window, _| {
                            if phase == DispatchPhase::Bubble && state.0.grab.take().is_some() {
                                window.refresh();
                            }
                        }
                    });
                },
            )
            .size_full(),
        )
}
