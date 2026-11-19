//! Secondary windows that exist at most once.

use gpui::{App, WindowHandle};

/// Brings `handle`'s window to the front if it's still open, returning
/// whether it was. A window handling an action of its own can't be updated
/// re-entrantly, so its activation waits until the action returns.
pub fn activate<V: 'static>(handle: WindowHandle<V>, cx: &mut App) -> bool {
    let open = cx
        .windows()
        .iter()
        .any(|window| window.window_id() == handle.window_id());
    if open {
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        });
    }
    open
}
