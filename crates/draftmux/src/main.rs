mod chrome;
mod ipc;
mod markup_view;
mod open;
mod resources;
mod show;
mod source;

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
    Open(open::Args),
    Show(show::Args),
}

fn main() {
    if let Some(command) = Cli::parse().command {
        std::process::exit(match command {
            Command::Open(args) => open::main(args),
            Command::Show(args) => show::main(args),
        });
    }

    let server = match ipc::claim() {
        Ok(ipc::Claim::Owner(server)) => server,
        Ok(ipc::Claim::Taken) => {
            if let Err(err) = ipc::activate_running() {
                eprintln!("draftmux: {err:#}");
                std::process::exit(1);
            }
            return;
        }
        Err(err) => {
            eprintln!("draftmux: {err:#}");
            std::process::exit(1);
        }
    };

    gpui_platform::application().run(move |cx: &mut App| {
        shell_env::init(cx);
        editor::init(cx);
        markup_view::init(cx);
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
        let window = cx
            .open_window(options, |window, cx| {
                cx.new(|cx| chrome::Chrome::new(window, cx))
            })
            .expect("failed to open the main window");
        server.serve(window, cx);
        cx.activate(true);
    });
}
