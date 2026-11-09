mod app_settings;
mod assets;
#[cfg(feature = "capture")]
mod capture;
mod chat;
mod chat_model;
mod chrome;
mod copy;
mod drafts;
mod file_watch;
mod format;
mod inline_text;
mod ipc;
mod markup_view;
mod model_picker;
mod open;
#[cfg_attr(target_os = "linux", path = "platform_linux.rs")]
mod platform;
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

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context as _, Result};
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

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Some(Command::Open(args)) => open::main(args),
        Some(Command::Show(args)) => show::main(args).map(|()| 0),
        Some(Command::Skill(args)) => skill::main(args),
        #[cfg(feature = "capture")]
        Some(Command::Capture(args)) => Ok(capture::main(args)),
        None => run_app().map(|()| 0),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("draftmux: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Writes `bytes` to `out`. A reader that stops early, as with `| head`,
/// isn't a failure.
fn write_output(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result.context("writing to stdout"),
    }
}

/// Runs the app, or brings the already running one forward.
fn run_app() -> Result<()> {
    let server = match ipc::claim()? {
        ipc::Claim::Owner(server) => server,
        ipc::Claim::Taken => return ipc::activate_running(),
    };
    let data_dir = draftmux_storage::data_dir()?;
    let config_dir = draftmux_storage::config_dir()?;
    let store = Store::new(Store::default_root()?);

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            theme::init(cx);
            shell_env::init(cx);
            profiles::init(data_dir, &config_dir, cx);
            app_settings::init(config_dir, cx);
            editor::init(cx);
            markup_view::init(cx);
            format::init(cx);
            copy::init(cx);
            chat::init(cx);
            model_picker::init(cx);
            chrome::init(cx);
            selection_popover::init(cx);
            settings_window::init(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new(editor::secondary!("q"), Quit, None)]);
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
                .open_window(platform::window_options(options), |window, cx| {
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
    Ok(())
}
