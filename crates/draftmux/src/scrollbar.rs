//! Thin overlay scrollbars, drawn over the right edge of a scrolling pane
//! like macOS overlay scrollers: no track, a rounded thumb that strengthens while
//! hovered or dragged, fading after idle and hidden when the content fits.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use editor::Editor;
use gpui::{
    App, Bounds, DispatchPhase, Entity, HitboxBehavior, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, Window, canvas, div, point, prelude::*, px,
    quad, size,
};

use crate::theme;

const TRACK_WIDTH: Pixels = px(11.);
const THUMB_INSET: Pixels = px(3.);
const MIN_THUMB: Pixels = px(24.);
const IDLE: Duration = Duration::from_millis(1500);
const FADE: Duration = Duration::from_millis(200);

fn opacity(elapsed: Option<Duration>, active: bool, reduce_motion: bool) -> f32 {
    if active {
        return 1.;
    }
    let Some(elapsed) = elapsed else { return 0. };
    if elapsed < IDLE {
        1.
    } else if reduce_motion {
        0.
    } else {
        (1. - (elapsed - IDLE).as_secs_f32() / FADE.as_secs_f32()).clamp(0., 1.)
    }
}

/// What the scrollbar scrolls.
#[derive(Clone)]
pub enum Source {
    Handle(ScrollHandle),
    List(ListState),
    /// The editor lays out and scrolls its own text.
    Editor(Entity<Editor>),
}

/// Pointer state that outlives a frame. Keep one per scrollbar.
#[derive(Clone, Default)]
pub struct ScrollbarState(Rc<Inner>);

#[derive(Default)]
struct Inner {
    hovered: Cell<bool>,
    last_offset: Cell<Option<Pixels>>,
    last_activity: Cell<Option<Instant>>,
    timer_pending: Cell<bool>,
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
            Source::List(list) => {
                let viewport = list.viewport_bounds().size.height;
                Some(Metrics {
                    viewport,
                    content: viewport + list.max_offset_for_scrollbar().y,
                    scroll: -list.scroll_px_offset_for_scrollbar().y,
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
            Source::List(list) => {
                list.set_offset_from_scrollbar(point(px(0.), -y));
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
                        state.0.hovered.set(false);
                        if state.0.grab.take().is_some()
                            && let Source::List(list) = &source
                        {
                            list.scrollbar_drag_ended();
                        }
                        state.0.last_offset.set(Some(metrics.scroll));
                        return;
                    };
                    let now = Instant::now();
                    let previous = state.0.last_offset.replace(Some(metrics.scroll));
                    if previous.is_some_and(|offset| offset != metrics.scroll) {
                        state.0.last_activity.set(Some(now));
                    }
                    let active = state.0.hovered.get() || state.0.grab.get().is_some();
                    if active {
                        state.0.last_activity.set(Some(now));
                    }
                    let elapsed = state
                        .0
                        .last_activity
                        .get()
                        .map(|time| now.duration_since(time));
                    let alpha = opacity(elapsed, active, crate::platform::reduce_motion());
                    if !active && let Some(elapsed) = elapsed {
                        if elapsed < IDLE && !state.0.timer_pending.replace(true) {
                            let state = state.clone();
                            window
                                .spawn(cx, async move |cx| {
                                    cx.background_executor().timer(IDLE - elapsed).await;
                                    state.0.timer_pending.set(false);
                                    cx.update(|window, _| window.refresh()).ok();
                                })
                                .detach();
                        } else if elapsed >= IDLE && alpha > 0. {
                            window.request_animation_frame();
                        }
                    }
                    let palette = theme::palette(cx);
                    let width = TRACK_WIDTH - THUMB_INSET * 2.;
                    window.paint_quad(quad(
                        Bounds::new(
                            point(bounds.left() + THUMB_INSET, bounds.top() + top),
                            size(width, height),
                        ),
                        width / 2.,
                        (if active {
                            palette.scrollbar_thumb_active
                        } else {
                            palette.scrollbar_thumb
                        })
                        .opacity(alpha),
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
                            if let Source::List(list) = &source {
                                list.scrollbar_drag_started();
                            }
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
                                state.0.last_activity.set(Some(Instant::now()));
                                window.refresh();
                            }
                            if let Some(grab) = state.0.grab.get() {
                                if event.pressed_button != Some(MouseButton::Left) {
                                    state.0.grab.set(None);
                                    if let Source::List(list) = &source {
                                        list.scrollbar_drag_ended();
                                    }
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
                        let source = source.clone();
                        move |_: &MouseUpEvent, phase, window, _| {
                            if phase == DispatchPhase::Bubble && state.0.grab.take().is_some() {
                                if let Source::List(list) = &source {
                                    list.scrollbar_drag_ended();
                                }
                                state.0.last_activity.set(Some(Instant::now()));
                                window.refresh();
                            }
                        }
                    });
                },
            )
            .size_full(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_thumb_fades_then_stays_hidden() {
        assert_eq!(opacity(None, false, false), 0.);
        assert_eq!(opacity(Some(IDLE / 2), false, false), 1.);
        assert_eq!(opacity(Some(IDLE), false, false), 1.);
        assert!((opacity(Some(IDLE + FADE / 2), false, false) - 0.5).abs() < 0.001);
        assert_eq!(opacity(Some(IDLE + FADE), false, false), 0.);
        assert_eq!(opacity(Some(IDLE * 10), false, false), 0.);
    }

    #[test]
    fn hover_and_drag_keep_thumb_visible() {
        for reduced in [false, true] {
            assert_eq!(opacity(None, true, reduced), 1.);
            assert_eq!(opacity(Some(IDLE * 10), true, reduced), 1.);
        }
    }

    #[test]
    fn reduced_motion_hides_without_fading() {
        assert_eq!(opacity(Some(IDLE / 2), false, true), 1.);
        assert_eq!(opacity(Some(IDLE), false, true), 0.);
        assert_eq!(opacity(Some(IDLE + FADE / 2), false, true), 0.);
    }

    #[test]
    fn thumb_geometry_maps_scroll_range() {
        let metrics = Metrics {
            viewport: px(100.),
            content: px(1000.),
            scroll: px(450.),
        };
        let (top, height) = metrics.thumb(px(100.)).unwrap();
        assert_eq!(height, MIN_THUMB);
        assert_eq!(top, px(38.));
        assert_eq!(metrics.scroll_for(top, height, px(100.)), px(450.));
        assert_eq!(metrics.scroll_for(px(-10.), height, px(100.)), px(0.));
        assert_eq!(metrics.scroll_for(px(200.), height, px(100.)), px(900.));
        assert!(
            Metrics {
                content: px(100.),
                ..metrics
            }
            .thumb(px(100.))
            .is_none()
        );
        assert!(metrics.thumb(MIN_THUMB).is_none());
    }
}
