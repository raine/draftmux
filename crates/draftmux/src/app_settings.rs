//! Draftmux's settings as the app holds them: loaded once at startup,
//! changed by the Settings window, which saves every change, and read by
//! views that use app-wide values such as the alternatives count.

use std::path::PathBuf;

use draftmux_settings::Settings;
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};

pub struct AppSettings {
    pub settings: Settings,
    data_dir: PathBuf,
    /// Why the file couldn't be loaded or the last change saved. A file that
    /// doesn't parse isn't overwritten until the user changes something.
    pub error: Option<SharedString>,
}

struct GlobalSettings(Entity<AppSettings>);

impl Global for GlobalSettings {}

pub fn init(cx: &mut App) {
    let data_dir = draftmux_storage::data_dir().unwrap_or_else(|_| std::env::temp_dir());
    let (settings, error) = match Settings::load(&data_dir) {
        Ok(settings) => (settings, None),
        Err(err) => {
            eprintln!("draftmux: {err:#}");
            (Settings::default(), Some(format!("{err:#}").into()))
        }
    };
    let entity = cx.new(|_| AppSettings {
        settings,
        data_dir,
        error,
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
            && let Ok(settings) = Settings::load(&self.data_dir)
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
            .save(&self.data_dir)
            .err()
            .map(|err| format!("Couldn't save settings: {err:#}").into());
        cx.notify();
    }
}
