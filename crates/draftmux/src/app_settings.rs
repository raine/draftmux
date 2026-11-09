//! Draftmux's settings as the app holds them: changed by the Settings window,
//! which saves every change, reloaded when `settings.json` is edited outside
//! the app, and read by views that use app-wide values such as the
//! alternatives count.

use std::path::PathBuf;

use draftmux_settings::Settings;
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Task};

use crate::file_watch::{WatchedFile, every_poll};

pub struct AppSettings {
    pub settings: Settings,
    config_dir: PathBuf,
    /// Why the file couldn't be loaded or the last change saved. A file that
    /// doesn't parse isn't overwritten until the user changes something;
    /// until then the last settings that loaded stay in use.
    pub error: Option<SharedString>,
    file: WatchedFile,
    _poll: Task<()>,
}

struct GlobalSettings(Entity<AppSettings>);

impl Global for GlobalSettings {}

pub fn init(config_dir: PathBuf, cx: &mut App) {
    let entity = cx.new(|cx| {
        let mut this = AppSettings {
            settings: Settings::default(),
            file: WatchedFile::new(Settings::path(&config_dir)),
            config_dir,
            error: None,
            _poll: every_poll(cx, AppSettings::poll),
        };
        this.reload(cx);
        this
    });
    cx.set_global(GlobalSettings(entity));
}

impl AppSettings {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalSettings>().0.clone()
    }

    /// Applies `change` and saves the settings if it changed them. After a
    /// failed load the file is read again first, so a fix made outside the
    /// app isn't replaced by the defaults.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        if self.error.is_some()
            && let Ok(settings) = Settings::load(&self.config_dir)
        {
            self.settings = settings;
            self.error = None;
        }
        let before = self.settings.clone();
        change(&mut self.settings);
        if self.settings == before {
            cx.notify();
            return;
        }
        self.error = self
            .settings
            .save(&self.config_dir)
            .err()
            .map(|err| format!("Couldn't save settings: {err:#}").into());
        self.file.mark_seen();
        cx.notify();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.file.changed() {
            self.reload(cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        match Settings::load(&self.config_dir) {
            Ok(settings) => {
                self.settings = settings;
                self.error = None;
            }
            Err(err) => {
                eprintln!("draftmux: {err:#}");
                self.error = Some(format!("{err:#}").into());
            }
        }
        cx.notify();
    }
}
