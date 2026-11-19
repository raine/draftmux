//! App identity and project links in a single reusable secondary window.

use gpui::{
    App, Context, FocusHandle, FontWeight, Global, KeyBinding, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, div, img, prelude::*, px, size,
};

use crate::{assets::APP_ICON, theme, update::CURRENT_VERSION};

actions!(about, [OpenAbout, CloseAbout]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", CloseAbout, Some("About")),
        KeyBinding::new(editor::secondary!("w"), CloseAbout, Some("About")),
    ]);
    cx.on_action(|_: &OpenAbout, cx| open(cx));
}

struct OpenWindow(WindowHandle<AboutWindow>);
impl Global for OpenWindow {}

pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<OpenWindow>().map(|open| open.0)
        && crate::single_window::activate(handle, cx)
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(360.), px(340.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some("About Draftmux".into()),
            ..Default::default()
        }),
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    match cx.open_window(crate::platform::window_options(options), |window, cx| {
        cx.new(|cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            AboutWindow { focus }
        })
    }) {
        Ok(handle) => cx.set_global(OpenWindow(handle)),
        Err(err) => eprintln!("draftmux: couldn't open the About window: {err:#}"),
    }
}

struct AboutWindow {
    focus: FocusHandle,
}

impl Render for AboutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::sync_window(window, cx);
        let c = theme::palette(cx);
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .bg(c.pane)
            .text_color(c.text)
            .font_family(editor::platform::ui_font())
            .text_size(px(13.))
            .key_context("About")
            .track_focus(&self.focus)
            .on_action(|_: &CloseAbout, window, _| window.remove_window())
            .child(img(APP_ICON).size(px(88.)))
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::BOLD)
                    .child("Draftmux"),
            )
            .child(
                div()
                    .text_color(c.muted)
                    .child(format!("Version {CURRENT_VERSION}")),
            )
            .child(
                div()
                    .text_color(c.muted)
                    .child("Polish the text your coding agent writes."),
            )
            .child(
                div().flex().gap(px(16.)).mt(px(6.)).children(
                    [
                        ("website", "Website", "https://draftmux.raine.dev/"),
                        ("github", "GitHub", "https://github.com/raine/draftmux"),
                        (
                            "license",
                            "MIT License",
                            "https://github.com/raine/draftmux/blob/main/LICENSE",
                        ),
                    ]
                    .map(|(id, label, url)| {
                        div()
                            .id(id)
                            .text_color(c.accent)
                            .cursor_pointer()
                            .hover(|style| style.underline())
                            .on_click(move |_, _, cx| cx.open_url(url))
                            .child(label)
                    }),
                ),
            )
            .child(
                div()
                    .mt(px(6.))
                    .text_size(px(11.))
                    .text_color(c.muted)
                    .child("© 2026 Raine Virta"),
            );
        crate::platform::frame(
            content.into_any_element(),
            window,
            cx,
            Box::new(CloseAbout),
            "About Draftmux",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn reuses_open_window_and_reopens_after_close(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            theme::init(cx);
            init(cx);
            open(cx);
            let first = cx.global::<OpenWindow>().0;
            open(cx);
            assert_eq!(cx.windows().len(), 1);
            assert_eq!(cx.global::<OpenWindow>().0.window_id(), first.window_id());
            first
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(cx.windows().is_empty());
            open(cx);
            assert_eq!(cx.windows().len(), 1);
            let reopened = cx.global::<OpenWindow>().0;
            assert!(reopened.update(cx, |_, _, _| ()).is_ok());
        });
    }
}
