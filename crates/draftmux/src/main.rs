mod app_settings;
mod assets;
#[cfg(feature = "capture")]
mod capture;
mod chat;
mod chat_model;
mod chrome;
mod copy;
mod drafts;
mod format;
mod inline_text;
mod ipc;
mod markup_view;
mod model_picker;
mod open;
mod process;
mod profiles;
mod resources;
mod sample;
mod scrollbar;
mod selection;
mod selection_popover;
mod settings_window;
mod show;
mod skill;
mod source;
mod theme;

use clap::{Parser, Subcommand};
use draftmux_storage::Store;

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
    Skill(skill::Args),
    #[cfg(feature = "capture")]
    #[command(hide = true)]
    Capture(capture::Args),
}

fn main() {
    if let Some(command) = Cli::parse().command {
        std::process::exit(match command {
            Command::Open(args) => open::main(args),
            Command::Show(args) => show::main(args),
            Command::Skill(args) => skill::main(args),
            #[cfg(feature = "capture")]
            Command::Capture(args) => capture::main(args),
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

    let store = match Store::default_root() {
        Ok(root) => Store::new(root),
        Err(err) => {
            eprintln!("draftmux: {err:#}");
            std::process::exit(1);
        }
    };

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            theme::init(cx);
            shell_env::init(cx);
            profiles::init(cx);
            app_settings::init(cx);
            editor::init(cx);
            markup_view::init(cx);
            format::init(cx);
            copy::init(cx);
            chat::init(cx);
            chrome::init(cx);
            selection_popover::init(cx);
            settings_window::init(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([Menu::new("Draftmux").items([
                MenuItem::action("Settings…", settings_window::OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Draftmux", Quit),
            ])]);

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
                    cx.new(|cx| chrome::Chrome::new(store, window, cx))
                })
                .expect("failed to open the main window");
            // Closing the main window ends the app; Settings is secondary.
            let main_id = window.window_id();
            cx.on_window_closed(move |cx, closed| {
                if closed == main_id {
                    cx.quit();
                }
            })
            .detach();
            server.serve(window, cx);
            cx.activate(true);
        });
}
