//! Where Draftmux's configuration lives: plain JSON files the Settings window
//! writes and users can edit by hand, which the app reloads when they change.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::store::{DATA_DIR_VAR, data_dir};

/// Overrides [`config_dir`].
pub const CONFIG_DIR_VAR: &str = "DRAFTMUX_CONFIG_DIR";

/// The configuration files, which older versions kept in [`data_dir`].
pub const CONFIG_FILES: [&str; 2] = ["settings.json", "profiles.json"];

/// The configuration directory: `$DRAFTMUX_CONFIG_DIR` when set, else
/// `$DRAFTMUX_DATA_DIR` when set, so a development build keeps its own
/// settings beside its own drafts, else `$XDG_CONFIG_HOME/draftmux`
/// (normally `~/.config/draftmux`) on every platform.
pub fn config_dir() -> Result<PathBuf> {
    config_dir_from(
        std::env::var_os(CONFIG_DIR_VAR),
        std::env::var_os(DATA_DIR_VAR),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::home_dir(),
    )
}

/// [`config_dir`], after moving configuration files still in the data
/// directory into it. Entry points that read configuration call this once.
pub fn prepare_config_dir() -> Result<PathBuf> {
    let dir = config_dir()?;
    if let Ok(data) = data_dir() {
        adopt(&data, &dir);
    }
    Ok(dir)
}

fn config_dir_from(
    config: Option<OsString>,
    data: Option<OsString>,
    xdg: Option<OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    let set = |var: Option<OsString>| var.filter(|dir| !dir.is_empty()).map(PathBuf::from);
    if let Some(dir) = set(config) {
        if !dir.is_absolute() {
            bail!(
                "{CONFIG_DIR_VAR} must be an absolute path: {}",
                dir.display()
            );
        }
        return Ok(dir);
    }
    if let Some(dir) = set(data) {
        if !dir.is_absolute() {
            bail!("{DATA_DIR_VAR} must be an absolute path: {}", dir.display());
        }
        return Ok(dir);
    }
    // The XDG spec ignores a relative XDG_CONFIG_HOME.
    if let Some(dir) = set(xdg).filter(|dir| dir.is_absolute()) {
        return Ok(dir.join("draftmux"));
    }
    let home = home.context("can't find the home directory")?;
    Ok(home.join(".config").join("draftmux"))
}

/// Moves each configuration file in `data` that `config` lacks into
/// `config`. Best effort: a file that can't be moved stays where it is and
/// the defaults apply.
fn adopt(data: &Path, config: &Path) {
    if data == config {
        return;
    }
    for name in CONFIG_FILES {
        let (from, to) = (data.join(name), config.join(name));
        if !from.is_file() || to.exists() {
            continue;
        }
        let moved = std::fs::create_dir_all(config)
            .and_then(|()| std::fs::rename(&from, &to))
            // A rename can't cross file systems.
            .or_else(|_| std::fs::copy(&from, &to).and_then(|_| std::fs::remove_file(&from)));
        if let Err(err) = moved {
            eprintln!(
                "draftmux: couldn't move {} to {}: {err}",
                from.display(),
                to.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(config: Option<&str>, data: Option<&str>, xdg: Option<&str>) -> Result<PathBuf> {
        config_dir_from(
            config.map(Into::into),
            data.map(Into::into),
            xdg.map(Into::into),
            Some("/home/u".into()),
        )
    }

    #[test]
    fn config_dir_resolution() {
        let path = |p: &str| Some(PathBuf::from(p));
        assert_eq!(resolve(Some("/c"), Some("/d"), Some("/x")).ok(), path("/c"));
        assert_eq!(resolve(None, Some("/d"), Some("/x")).ok(), path("/d"));
        assert_eq!(
            resolve(Some(""), None, Some("/x")).ok(),
            path("/x/draftmux")
        );
        assert_eq!(
            resolve(None, None, Some("rel")).ok(),
            path("/home/u/.config/draftmux")
        );
        assert_eq!(
            resolve(None, None, None).ok(),
            path("/home/u/.config/draftmux")
        );
        assert!(resolve(Some("c"), None, None).is_err());
        assert!(resolve(None, Some("d"), None).is_err());
    }

    #[test]
    fn adopt_moves_missing_files_only() {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("draftmux");
        std::fs::write(data.path().join("settings.json"), "old").unwrap();
        std::fs::write(data.path().join("profiles.json"), "old").unwrap();
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("profiles.json"), "new").unwrap();

        adopt(data.path(), &config);

        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(config.join("settings.json")), "old");
        assert!(!data.path().join("settings.json").exists());
        assert_eq!(read(config.join("profiles.json")), "new");
        assert_eq!(read(data.path().join("profiles.json")), "old");
    }
}
