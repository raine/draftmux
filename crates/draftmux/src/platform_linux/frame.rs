//! Client decorations for compositors that do not provide a title bar.
use gpui::{
    AnyElement, App, CursorStyle, Decorations, Div, MouseButton, ResizeEdge, Tiling, Window,
    WindowButton, WindowButtonLayout, div, prelude::*, px,
};
const EDGE: gpui::Pixels = px(5.);
const CORNER: gpui::Pixels = px(14.);

pub fn frame(
    content: AnyElement,
    window: &mut Window,
    cx: &mut App,
    close: Box<dyn gpui::Action>,
    title: &'static str,
) -> AnyElement {
    let palette = *crate::theme::palette(cx);
    // Translucent surfaces need an opaque backing on non-blurred windows.
    let content = div()
        .size_full()
        .bg(palette.pane)
        .child(content)
        .into_any_element();
    let Decorations::Client { mut tiling } = window.window_decorations() else {
        return content;
    };
    if window.is_fullscreen() {
        return content;
    }
    if window.is_maximized() {
        tiling = Tiling::tiled();
    }
    let layout = cx
        .button_layout()
        .unwrap_or_else(WindowButtonLayout::linux_default);
    let controls = window.window_controls();
    let buttons = |side: &[Option<WindowButton>]| {
        div().flex().items_center().children(
            side.iter()
                .copied()
                .flatten()
                .filter(|button| match button {
                    WindowButton::Minimize => controls.minimize,
                    WindowButton::Maximize => controls.maximize,
                    WindowButton::Close => true,
                })
                .map(|button| {
                    let close = close.boxed_clone();
                    div()
                        .id(button.id())
                        .w(px(34.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.hover))
                        .active(|style| style.opacity(0.8))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| match button {
                            WindowButton::Minimize => window.minimize_window(),
                            WindowButton::Maximize => window.zoom_window(),
                            WindowButton::Close => window.dispatch_action(close.boxed_clone(), cx),
                        })
                        .child(match button {
                            WindowButton::Minimize => "−",
                            WindowButton::Maximize => "□",
                            WindowButton::Close => "×",
                        })
                }),
        )
    };
    div()
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(palette.toolbar)
        .border_color(palette.hairline)
        .when(!tiling.top, |frame| frame.border_t_1())
        .when(!tiling.bottom, |frame| frame.border_b_1())
        .when(!tiling.left, |frame| frame.border_l_1())
        .when(!tiling.right, |frame| frame.border_r_1())
        .text_color(palette.text)
        .font_family(editor::platform::ui_font())
        .child(
            div()
                .h(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .px(px(5.))
                .when(controls.window_menu, |bar| {
                    bar.on_mouse_down(MouseButton::Right, |event, window, _| {
                        window.show_window_menu(event.position)
                    })
                })
                .on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count == 2 {
                        window.zoom_window();
                    } else {
                        window.start_window_move();
                    }
                })
                .child(buttons(&layout.left))
                .child(div().flex_1().text_size(px(12.)).text_center().child(title))
                .child(buttons(&layout.right)),
        )
        .child(div().flex_1().min_h_0().child(content))
        .children(edges(tiling))
        .into_any_element()
}

/// The bands along the window's free edges that resize it.
fn edges(tiling: Tiling) -> Vec<Div> {
    let top = !tiling.top;
    let bottom = !tiling.bottom;
    let left = !tiling.left;
    let right = !tiling.right;
    let mut edges = Vec::new();
    let mut edge = |free: bool, resize: ResizeEdge, place: fn(Div) -> Div| {
        if free {
            edges.push(place(handle(resize)));
        }
    };
    edge(top, ResizeEdge::Top, |band| {
        band.top_0().left_0().w_full().h(EDGE)
    });
    edge(bottom, ResizeEdge::Bottom, |band| {
        band.bottom_0().left_0().w_full().h(EDGE)
    });
    edge(left, ResizeEdge::Left, |band| {
        band.top_0().left_0().h_full().w(EDGE)
    });
    edge(right, ResizeEdge::Right, |band| {
        band.top_0().right_0().h_full().w(EDGE)
    });
    edge(top && left, ResizeEdge::TopLeft, |corner| {
        corner.top_0().left_0().size(CORNER)
    });
    edge(top && right, ResizeEdge::TopRight, |corner| {
        corner.top_0().right_0().size(CORNER)
    });
    edge(bottom && left, ResizeEdge::BottomLeft, |corner| {
        corner.bottom_0().left_0().size(CORNER)
    });
    edge(bottom && right, ResizeEdge::BottomRight, |corner| {
        corner.bottom_0().right_0().size(CORNER)
    });
    edges
}

fn handle(edge: ResizeEdge) -> Div {
    let cursor = match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    };
    div().absolute().cursor(cursor).occlude().on_mouse_down(
        MouseButton::Left,
        move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(edge);
        },
    )
}
