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
    /// Why the file couldn't be loaded. A file that doesn't parse isn't
    /// overwritten until the user changes something; until then the last
    /// settings that loaded stay in use.
    load_error: Option<SharedString>,
    /// Why the last change couldn't be saved. The change stays in
    /// `settings` and is saved with the next one.
    save_error: Option<SharedString>,
    file: WatchedFile,
    _poll: Task<()>,
}

struct GlobalSettings(Entity<AppSettings>);

impl Global for GlobalSettings {}

pub fn init(config_dir: PathBuf, cx: &mut App) {
    let entity = cx.new(|cx| AppSettings {
        _poll: every_poll(cx, |this: &mut AppSettings, cx| {
            if this.poll() {
                cx.notify();
            }
        }),
        ..AppSettings::load(config_dir)
    });
    cx.set_global(GlobalSettings(entity));
}

impl AppSettings {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalSettings>().0.clone()
    }

    fn load(config_dir: PathBuf) -> Self {
        let mut this = Self {
            settings: Settings::default(),
            file: WatchedFile::new(Settings::path(&config_dir)),
            config_dir,
            load_error: None,
            save_error: None,
            _poll: Task::ready(()),
        };
        this.reload();
        this
    }

    /// The error to show: a failed save, else a failed load.
    pub fn error(&self) -> Option<SharedString> {
        self.save_error.clone().or_else(|| self.load_error.clone())
    }

    /// Applies `change` and saves the settings if it changed them.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        self.apply(change);
        cx.notify();
    }

    /// Applies `change` to the settings on disk, read again first if they
    /// were edited outside the app since the last poll or failed to load,
    /// so the save doesn't replace those edits.
    fn apply(&mut self, change: impl FnOnce(&mut Settings)) {
        if self.file.changed() || self.load_error.is_some() {
            self.reload();
        }
        let before = self.settings.clone();
        change(&mut self.settings);
        if self.settings == before && self.save_error.is_none() {
            return;
        }
        self.save_error = self
            .settings
            .save(&self.config_dir)
            .err()
            .map(|err| format!("Couldn't save settings: {err:#}").into());
        self.file.mark_seen();
    }

    /// Reloads the settings if the file changed; returns whether it did.
    fn poll(&mut self) -> bool {
        if !self.file.changed() {
            return false;
        }
        self.reload();
        true
    }

    /// Loads the file, keeping the current settings if it doesn't parse.
    /// An edit made outside the app replaces any change that failed to
    /// save.
    fn reload(&mut self) {
        match Settings::load(&self.config_dir) {
            Ok(settings) => {
                self.settings = settings;
                self.load_error = None;
                self.save_error = None;
            }
            Err(err) => {
                eprintln!("draftmux: {err:#}");
                self.load_error = Some(format!("{err:#}").into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alternatives(n: usize) -> impl FnOnce(&mut Settings) {
        move |settings| settings.alternatives = Some(n)
    }

    #[test]
    fn a_change_keeps_edits_made_since_the_last_poll() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = AppSettings::load(dir.path().to_owned());
        app.apply(alternatives(2));

        let mut edited = Settings::load(dir.path()).unwrap();
        edited.selection_actions = Some(Vec::new());
        edited.save(dir.path()).unwrap();
        app.apply(alternatives(4));

        let saved = Settings::load(dir.path()).unwrap();
        assert_eq!(saved.alternatives, Some(4));
        assert_eq!(saved.selection_actions, Some(Vec::new()));
        assert!(app.error().is_none());
    }

    #[test]
    fn a_failed_save_is_kept_for_the_next_change() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("config");
        let mut app = AppSettings::load(dir.clone());
        // A file where the directory should be makes saving fail.
        std::fs::write(&dir, "").unwrap();
        app.apply(alternatives(2));
        assert!(app.error().is_some());

        std::fs::remove_file(&dir).unwrap();
        app.apply(|settings| settings.selection_actions = Some(Vec::new()));
        let saved = Settings::load(&dir).unwrap();
        assert_eq!(saved.alternatives, Some(2));
        assert_eq!(saved.selection_actions, Some(Vec::new()));
        assert!(app.error().is_none());
    }
}
