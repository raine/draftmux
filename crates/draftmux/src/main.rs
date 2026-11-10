mod app_settings;
mod assets;
mod block_cache;
mod bounce;
#[cfg(feature = "capture")]
mod capture;
mod changes;
mod chat;
mod chat_model;
mod chrome;
mod copy;
mod drafts;
mod file_watch;
mod format;
mod inline_text;
mod ipc;
mod list;
mod markup_view;
mod menu;
mod model_picker;
mod open;
#[cfg(test)]
mod performance;
#[cfg_attr(target_os = "linux", path = "platform_linux.rs")]
mod platform;
mod process;
mod profiles;
mod resources;
mod sample;
mod scrollbar;
mod selectable;
mod selection;
mod selection_popover;
mod settings_window;
mod show;
mod skill;
mod source;
mod source_map;
mod theme;
mod tooltip;
mod update;

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{CommandFactory, Parser, Subcommand};
use draftmux_storage::Store;

use gpui::{
    App, KeyBinding, Menu, MenuItem, SharedString, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, actions, prelude::*, px, size,
};

actions!(draftmux, [Quit]);

/// Draftmux: polish messages handed off from coding agents.
#[derive(Parser)]
#[command(name = "draftmux", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Open(open::Args),
    Show(show::Args),
    List(list::Args),
    Skill(skill::Args),
    /// Run the app in this process.
    #[command(hide = true)]
    App,
    #[cfg(feature = "capture")]
    #[command(hide = true)]
    Capture(capture::Args),
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Some(Command::Open(args)) => open::main(args),
        Some(Command::Show(args)) => show::main(args).map(|()| 0),
        Some(Command::List(args)) => list::main(args).map(|()| 0),
        Some(Command::Skill(args)) => skill::main(args),
        #[cfg(feature = "capture")]
        Some(Command::Capture(args)) => Ok(capture::main(args)),
        Some(Command::App) => run_app().map(|()| 0),
        // Finder, the Dock and the CLI's own launch start the bundle's
        // binary with no arguments.
        None if platform::launched_as_app() => run_app().map(|()| 0),
        None => Cli::command()
            .print_help()
            .map(|()| 0)
            .context("writing to stdout"),
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
            selectable::init(cx);
            format::init(cx);
            copy::init(cx);
            chat::init(cx);
            model_picker::init(cx);
            menu::init(cx);
            chrome::init(cx);
            selection_popover::init(cx);
            settings_window::init(cx);
            update::init(cx);
            cx.bind_keys([KeyBinding::new(editor::secondary!("q"), Quit, None)]);
            cx.set_menus([Menu::new("Draftmux").items([
                MenuItem::action("Check for Updates…", update::CheckForUpdates),
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
            // Quit from another window, such as Settings, saves the draft
            // first and stays open if that fails, as the main window's own
            // Quit handler does.
            cx.on_action(move |_: &Quit, cx| {
                let saved = window
                    .update(cx, |chrome, _, cx| chrome.flush_before_exit(cx))
                    .unwrap_or(true);
                if saved {
                    cx.quit();
                }
            });
            server.serve(window, cx);
            cx.activate(true);
        });
    Ok(())
}
