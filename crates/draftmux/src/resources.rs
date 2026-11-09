//! Files the app ships beside its binary: in `Draftmux.app/Contents/Resources`
//! on macOS or `share/draftmux` beside the Linux install's `bin` directory.
//! Development binaries use the repository. Installed binaries never fall
//! back to build-machine resources.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The running binary with symlinks resolved, so a `draftmux` on `PATH` that
/// links into a bundle is seen as bundled.
pub fn exe() -> Result<PathBuf> {
    std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .context("locating the draftmux binary")
}

/// The `.app` directory holding `exe`, if it is `<name>.app/Contents/MacOS/<exe>`.
pub fn bundle(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then_some(app)
}

/// The pi extension passed with `-e` to every pi process the app starts.
pub fn pi_extension() -> Result<PathBuf> {
    pi_extension_for(&exe()?)
}

fn pi_extension_for(exe: &Path) -> Result<PathBuf> {
    let path = match (bundle(exe), linux_layout(exe)) {
        (Some(app), _) => app.join("Contents/Resources/pi-extension/draftmux.ts"),
        (None, Some(resources)) => resources.join("pi-extension/draftmux.ts"),
        (None, None) => Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../pi-extension/draftmux.ts"
        ))
        .to_path_buf(),
    };
    if !path.is_file() {
        bail!("the pi extension is missing at {}", path.display());
    }
    Ok(path)
}

/// The marker distinguishes an installed tree from a development binary.
fn linux_layout(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let root = bin.parent()?;
    (bin.file_name()? == "bin" && root.join(".draftmux-bundle").is_file())
        .then(|| root.join("share/draftmux"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn bundle_requires_app_contents_macos() {
        let app = Path::new("/Applications/Draftmux.app");
        assert_eq!(bundle(&app.join("Contents/MacOS/draftmux")), Some(app));
        assert_eq!(bundle(Path::new("/repo/target/debug/draftmux")), None);
        assert_eq!(bundle(Path::new("/x/Contents/MacOS/draftmux")), None);
        assert_eq!(bundle(Path::new("/x/Draftmux.app/MacOS/draftmux")), None);
    }

    #[test]
    fn bundled_extension_comes_from_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Draftmux.app");
        let exe = app.join("Contents/MacOS/draftmux");
        let ext = app.join("Contents/Resources/pi-extension/draftmux.ts");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert!(pi_extension_for(&exe).is_err(), "no fallback to the repo");

        fs::create_dir_all(ext.parent().unwrap()).unwrap();
        fs::write(&ext, "").unwrap();
        assert_eq!(pi_extension_for(&exe).unwrap(), ext);
    }

    #[test]
    fn linux_extension_is_relocatable_and_never_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app with spaces");
        fs::create_dir_all(app.join("bin")).unwrap();
        fs::write(app.join(".draftmux-bundle"), "draftmux\n").unwrap();
        let exe = app.join("bin/draftmux");
        assert!(pi_extension_for(&exe).is_err());
        let ext = app.join("share/draftmux/pi-extension/draftmux.ts");
        fs::create_dir_all(ext.parent().unwrap()).unwrap();
        fs::write(&ext, "").unwrap();
        assert_eq!(pi_extension_for(&exe).unwrap(), ext);
    }

    #[test]
    fn unbundled_extension_comes_from_the_repo() {
        let path = pi_extension_for(Path::new("/repo/target/debug/draftmux")).unwrap();
        assert!(path.ends_with("pi-extension/draftmux.ts"));
    }
}
