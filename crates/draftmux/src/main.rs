mod chrome;
mod markup_view;
mod show;

use clap::{Parser, Subcommand};

use gpui::{
    App, KeyBinding, Menu, MenuItem, SharedString, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, actions, prelude::*, px, size,
};

actions!(draftmux, [Quit]);

/// Run without a command to open the app.
#[derive(Parser)]
#[command(name = "draftmux")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Show(show::Args),
}

fn main() {
    if let Some(command) = Cli::parse().command {
        std::process::exit(match command {
            Command::Show(args) => show::main(args),
        });
    }

    gpui_platform::application().run(|cx: &mut App| {
        shell_env::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus([Menu::new("Draftmux").items([MenuItem::action("Quit Draftmux", Quit)])]);

        // Draftmux has a single window, so closing it ends the app.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            window_min_size: Some(size(px(900.), px(500.))),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::new_static("Draftmux")),
                appears_transparent: true,
                traffic_light_position: Some(chrome::TRAFFIC_LIGHT_POSITION),
            }),
            window_background: WindowBackgroundAppearance::Blurred,
            app_owns_titlebar_drag: true,
            ..Default::default()
        };
        cx.open_window(options, |_, cx| cx.new(|_| chrome::Chrome::default()))
            .expect("failed to open the main window");
        cx.activate(true);
    });
}
