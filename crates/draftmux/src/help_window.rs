//! A compact reference to the app's features and their shortcuts, in a
//! single reusable secondary window.

use gpui::{
    App, Context, FocusHandle, FontWeight, Global, KeyBinding, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, div, prelude::*, px, size,
};

use crate::theme::{self, Palette};

actions!(help, [OpenHelp, CloseHelp]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(editor::secondary!("/"), OpenHelp, None),
        KeyBinding::new("escape", CloseHelp, Some("Help")),
        KeyBinding::new(editor::secondary!("w"), CloseHelp, Some("Help")),
    ]);
    cx.on_action(|_: &OpenHelp, cx| open(cx));
}

/// What a key does, by section.
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Writing",
        &[
            (
                editor::by_platform!("⌘↩", "Ctrl+Enter"),
                "Continue the sentence at the cursor",
            ),
            ("⇥", "Take the continuation"),
            (
                editor::by_platform!("⌥→", "Ctrl+Right"),
                "Take its next word",
            ),
            ("esc", "Drop it"),
            (
                editor::by_platform!("⌘⇧↩", "Ctrl+Shift+Enter"),
                "Pick from several continuations",
            ),
            ("Select text", "Rewrite it with an action or a comment"),
            (
                "esc",
                "Leave the options for later; its badge brings them back",
            ),
            (
                editor::by_platform!("⌥↓  ⌥↑", "Alt+Down  Alt+Up"),
                "Next or previous options left for later",
            ),
            (
                editor::by_platform!("⌘⌫", "Ctrl+Backspace"),
                "Discard the options shown",
            ),
            (
                editor::by_platform!("⌘-drag", "Ctrl-drag"),
                "Add another selection",
            ),
        ],
    ),
    (
        "Draft",
        &[
            (
                editor::by_platform!("⌘⇧C", "Ctrl+Shift+C"),
                "Copy formatted for the target",
            ),
            (
                editor::by_platform!("⌘⇧F", "Ctrl+Shift+F"),
                "Format the Markdown",
            ),
            (
                editor::by_platform!("⌘S", "Ctrl+S"),
                "Save to the file it came from",
            ),
            (
                editor::by_platform!("⌘⇧E", "Ctrl+Shift+E"),
                "Review the agent's changes",
            ),
            ("[  ]", "Previous or next change in review"),
            ("k  r", "Keep or revert the change"),
        ],
    ),
    (
        "Window",
        &[
            (
                editor::by_platform!("⌘\\", "Ctrl+\\"),
                "Show or hide the sidebar",
            ),
            (
                editor::by_platform!("⌥⌘E", "Ctrl+Alt+E"),
                "Show or hide the Markdown",
            ),
            (
                editor::by_platform!("⌥⌘P", "Ctrl+Alt+P"),
                "Show or hide the preview",
            ),
            (
                editor::by_platform!("⌘+  ⌘−  ⌘0", "Ctrl+=  Ctrl+−  Ctrl+0"),
                "Zoom in, out or back",
            ),
            (editor::by_platform!("⌘,", "Ctrl+,"), "Settings"),
            (editor::by_platform!("⌘/", "Ctrl+/"), "This help"),
        ],
    ),
];

struct OpenWindow(WindowHandle<HelpWindow>);
impl Global for OpenWindow {}

pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<OpenWindow>().map(|open| open.0)
        && crate::single_window::activate(handle, cx)
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(440.), px(612.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some("Draftmux Help".into()),
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
            HelpWindow { focus }
        })
    }) {
        Ok(handle) => cx.set_global(OpenWindow(handle)),
        Err(err) => eprintln!("draftmux: couldn't open the Help window: {err:#}"),
    }
}

struct HelpWindow {
    focus: FocusHandle,
}

impl Render for HelpWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::sync_window(window, cx);
        let c = theme::palette(cx);
        let content = div()
            .id("help")
            .size_full()
            .overflow_y_scroll()
            .pt(px(16.))
            .px(px(24.))
            .pb(px(20.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .bg(c.pane)
            .text_color(c.text)
            .font_family(editor::platform::ui_font())
            .text_size(px(13.))
            .key_context("Help")
            .track_focus(&self.focus)
            .on_action(|_: &CloseHelp, window, _| window.remove_window())
            .children(SECTIONS.iter().map(|(title, rows)| section(title, rows, c)));
        crate::platform::frame(
            content.into_any_element(),
            window,
            cx,
            Box::new(CloseHelp),
            "Draftmux Help",
        )
    }
}

fn section(title: &str, rows: &[(&str, &str)], c: &Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .pb(px(4.))
                .mb(px(2.))
                .border_b_1()
                .border_color(c.hairline)
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.muted)
                .child(title.to_owned()),
        )
        .children(rows.iter().map(|(keys, what)| {
            div()
                .flex()
                .items_baseline()
                .py(px(2.))
                .child(
                    div()
                        .flex_none()
                        .w(px(if cfg!(target_os = "macos") { 96. } else { 150. }))
                        .text_color(c.muted)
                        .child(keys.to_string()),
                )
                .child(div().flex_1().child(what.to_string()))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn shortcut_inside_help_reuses_window(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            theme::init(cx);
            init(cx);
            open(cx);
        });
        let handle = cx.update(|cx| cx.global::<OpenWindow>().0);
        let mut window = gpui::VisualTestContext::from_window(handle.into(), cx);
        window.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-/"
        } else {
            "ctrl-/"
        });
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(cx.windows().len(), 1));
    }

    #[gpui::test]
    fn reuses_open_window_and_closes_on_escape(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            theme::init(cx);
            init(cx);
            open(cx);
            let first = cx.global::<OpenWindow>().0;
            open(cx);
            assert_eq!(cx.windows().len(), 1);
            assert_eq!(cx.global::<OpenWindow>().0.window_id(), first.window_id());
        });
        let handle = cx.update(|cx| cx.global::<OpenWindow>().0);
        let mut window = gpui::VisualTestContext::from_window(handle.into(), cx);
        window.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|cx| assert!(cx.windows().is_empty()));
    }
}
