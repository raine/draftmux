//! AppKit-style pop-up and context menus, drawn in the window.

use std::rc::Rc;

use gpui::{
    App, FocusHandle, IntoElement, RenderOnce, ScrollHandle, SharedString, Window, anchored, div,
    point, prelude::*, px,
};

use crate::chrome::shadow;
use crate::theme::{Palette, hex};

#[derive(Clone)]
pub struct MenuItem<A> {
    pub label: SharedString,
    pub checked: bool,
    /// Disabled items are drawn dimmed and can't be picked.
    pub enabled: bool,
    /// `None` draws a separator.
    pub action: Option<A>,
}

gpui::actions!(menu, [Previous, Next, Confirm, Cancel]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("up", Previous, Some("Menu")),
        gpui::KeyBinding::new("down", Next, Some("Menu")),
        gpui::KeyBinding::new("enter", Confirm, Some("Menu")),
        gpui::KeyBinding::new("space", Confirm, Some("Menu")),
        gpui::KeyBinding::new("escape", Cancel, Some("Menu")),
    ]);
}

struct MenuState {
    focus: FocusHandle,
    previous: Option<FocusHandle>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    closed: bool,
}

impl MenuState {
    fn restore(&mut self, window: &mut Window, cx: &mut App) {
        if !self.closed {
            self.closed = true;
            if self.focus.is_focused(window) {
                if let Some(previous) = &self.previous {
                    previous.focus(window, cx);
                } else {
                    window.blur(cx);
                }
            }
        }
    }
}

fn next_enabled(enabled: &[bool], current: Option<usize>, backwards: bool) -> Option<usize> {
    let len = enabled.len();
    let start = current.unwrap_or(if backwards { 0 } else { len.saturating_sub(1) });
    (1..=len)
        .map(|step| {
            if backwards {
                (start + len - step) % len
            } else {
                (start + step) % len
            }
        })
        .find(|&ix| enabled[ix])
}

fn row_height<A>(item: &MenuItem<A>) -> f32 {
    if item.action.is_some() { 22. } else { 8.5 }
}

fn popup_geometry(checked_top: f32, window_height: f32) -> (f32, f32, f32) {
    let height = (window_height - 16.).clamp(30., 308.);
    let visible_top = checked_top.min(height - 30.);
    (height, visible_top, checked_top - visible_top)
}

type Pick<A> = Rc<dyn Fn(A, &mut Window, &mut App)>;
type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct MenuPanel<A: Clone + 'static> {
    palette: Palette,
    items: Vec<MenuItem<A>>,
    on_pick: Pick<A>,
    on_dismiss: Dismiss,
    popup: bool,
}

impl<A: Clone + 'static> MenuPanel<A> {
    pub fn popup(mut self) -> Self {
        self.popup = true;
        self
    }
}

/// A focus-owning menu that restores the previous action target before picking.
pub fn menu_panel<A: Clone + 'static>(
    c: &Palette,
    items: &[MenuItem<A>],
    on_pick: impl Fn(A, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> MenuPanel<A> {
    MenuPanel {
        palette: *c,
        items: items.to_vec(),
        on_pick: Rc::new(on_pick),
        on_dismiss: Rc::new(on_dismiss),
        popup: false,
    }
}

impl<A: Clone + 'static> RenderOnce for MenuPanel<A> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = self.palette;
        let enabled: Vec<_> = self
            .items
            .iter()
            .map(|item| item.enabled && item.action.is_some())
            .collect();
        let checked = self
            .items
            .iter()
            .position(|item| item.checked && item.enabled && item.action.is_some());
        let checked_top: f32 = self
            .items
            .iter()
            .take(checked.unwrap_or(0))
            .map(row_height)
            .sum();
        let (height, visible_top, scroll_offset) =
            popup_geometry(checked_top, window.viewport_size().height.into());
        let initialize = |window: &mut Window, cx: &mut gpui::Context<MenuState>| {
            let previous = window.focused(cx);
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            let scroll = ScrollHandle::new();
            scroll.set_offset(point(px(0.), px(-scroll_offset)));
            MenuState {
                focus,
                previous,
                selected: checked,
                scroll,
                closed: false,
            }
        };
        let state = window.use_keyed_state("menu-panel", cx, initialize);
        if state.read(cx).closed {
            state.update(cx, |state, cx| *state = initialize(window, cx));
        }
        let selected = state.read(cx).selected;
        let pick = {
            let state = state.clone();
            let on_pick = self.on_pick;
            move |action, window: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| state.restore(window, cx));
                on_pick(action, window, cx);
            }
        };
        let dismiss = {
            let state = state.clone();
            let on_dismiss = self.on_dismiss;
            move |window: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| state.restore(window, cx));
                on_dismiss(window, cx);
            }
        };
        let navigate = {
            let state = state.clone();
            move |backwards, cx: &mut App| {
                state.update(cx, |state, cx| {
                    state.selected = next_enabled(&enabled, state.selected, backwards);
                    if let Some(ix) = state.selected {
                        state.scroll.scroll_to_item(ix);
                    }
                    cx.notify();
                })
            }
        };
        let mut panel = div()
            .id("menu-panel")
            .track_focus(&state.read(cx).focus)
            .key_context("Menu")
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(180.))
            .max_w(px(360.))
            .max_h(px(height))
            .overflow_y_scroll()
            .track_scroll(&state.read(cx).scroll)
            .p(px(4.))
            .rounded(px(7.))
            .text_size(px(13.))
            .line_height(px(16.))
            .bg(c.menu)
            .shadow(vec![
                shadow(hex(0x00000033), 0., 0., 0.5),
                shadow(hex(0x00000029), 6., 18., 0.),
            ])
            .on_mouse_down_out({
                let dismiss = dismiss.clone();
                move |_, window, cx| dismiss(window, cx)
            })
            .on_action({
                let navigate = navigate.clone();
                move |_: &Previous, _, cx| navigate(true, cx)
            })
            .on_action(move |_: &Next, _, cx| navigate(false, cx))
            .on_action(move |_: &Cancel, window, cx| dismiss(window, cx))
            .on_action({
                let state = state.clone();
                let items = self.items.clone();
                let pick = pick.clone();
                move |_: &Confirm, window, cx| {
                    if let Some(action) = state
                        .read(cx)
                        .selected
                        .and_then(|ix| items[ix].action.clone())
                    {
                        pick(action, window, cx);
                    }
                }
            })
            .on_key_down(|_, _, cx| cx.stop_propagation());
        for (ix, item) in self.items.iter().enumerate() {
            panel = panel.child(match &item.action {
                None => div()
                    .id(("menu-separator", ix))
                    .flex_none()
                    .h(px(8.5))
                    .px(px(8.))
                    .py(px(4.))
                    .child(div().h(px(0.5)).bg(c.menu_line))
                    .on_mouse_move({
                        let state = state.clone();
                        move |_, _, cx| {
                            state.update(cx, |state, cx| {
                                if state.selected.take().is_some() {
                                    cx.notify();
                                }
                            })
                        }
                    })
                    .into_any_element(),
                Some(action) => {
                    let action = action.clone();
                    let pick = pick.clone();
                    let state = state.clone();
                    let enabled = item.enabled;
                    div()
                        .id(("menu-item", ix))
                        .flex_none()
                        .flex()
                        .items_center()
                        .h(px(22.))
                        .pr(px(10.))
                        .rounded(px(4.))
                        .text_color(if enabled { c.text } else { c.faint })
                        .when(enabled, |row| row.active(|style| style.opacity(0.8)))
                        .when(selected == Some(ix), |row| {
                            row.bg(c.accent).text_color(c.on_accent)
                        })
                        .child(
                            div()
                                .w(px(20.))
                                .flex_none()
                                .text_center()
                                .child(if item.checked { "✓" } else { "" }),
                        )
                        .child(div().truncate().child(item.label.clone()))
                        .on_mouse_move(move |_, _, cx| {
                            state.update(cx, |state, cx| {
                                let selected = enabled.then_some(ix);
                                if state.selected != selected {
                                    state.selected = selected;
                                    cx.notify();
                                }
                            })
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            if enabled {
                                pick(action.clone(), window, cx);
                            }
                        })
                        .into_any_element()
                }
            });
        }
        if self.popup {
            anchored()
                .offset(point(px(0.), px(-4. - visible_top)))
                .snap_to_window_with_margin(px(8.))
                .child(panel)
                .into_any_element()
        } else {
            panel.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{next_enabled, popup_geometry};

    #[test]
    fn popup_keeps_checked_row_visible_and_aligned() {
        for (top, window_height) in [(0., 600.), (44., 600.), (880., 600.), (880., 120.)] {
            let (height, visible_top, scroll) = popup_geometry(top, window_height);
            assert!(height <= window_height - 16.);
            assert!(visible_top + 22. <= height - 8.);
            assert_eq!(top - scroll, visible_top);
            assert!(scroll >= 0.);
        }
        assert_eq!(popup_geometry(44., 600.), (308., 44., 0.));
    }

    #[test]
    fn navigation_skips_unavailable_rows_and_wraps() {
        let enabled = [false, true, false, true, false];
        assert_eq!(next_enabled(&enabled, None, false), Some(1));
        assert_eq!(next_enabled(&enabled, None, true), Some(3));
        assert_eq!(next_enabled(&enabled, Some(1), false), Some(3));
        assert_eq!(next_enabled(&enabled, Some(3), false), Some(1));
        assert_eq!(next_enabled(&enabled, Some(1), true), Some(3));
        assert_eq!(next_enabled(&[true], Some(0), true), Some(0));
    }

    #[test]
    fn navigation_handles_no_enabled_items() {
        assert_eq!(next_enabled(&[], None, false), None);
        assert_eq!(next_enabled(&[false, false], None, true), None);
    }
}
