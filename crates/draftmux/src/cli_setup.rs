//! Links the `draftmux` command into `/usr/local/bin` for a Draftmux.app
//! installed from the disk image, which doesn't put the command on `PATH`.
//! The Pi and Claude Code skills run it. The app menu's Install Command Line
//! Tool… does this, and the first launch from Applications offers it once.
//!
//! macOS lists `/usr/local/bin` in `/etc/paths`, so every shell finds the
//! command there without profile changes. The link is made as the user when
//! the directory allows it, else with administrator privileges through the
//! system password prompt. It is only created in place of nothing or a link
//! into another `Draftmux.app`, never over an unrelated file or link, and
//! only for an app that stays where it is, in Applications.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::{AnyWindowHandle, App, AppContext as _, AsyncApp, PromptButton, PromptLevel};
use shell_env::ShellEnv;

use crate::resources;

gpui::actions!(draftmux, [InstallCommandLineTool]);

const LINK: &str = "/usr/local/bin/draftmux";
/// Records in the data directory that the first launch offered setup.
const OFFERED_MARKER: &str = "cli-setup-offered";

/// Handles the menu action, prompting over `window`, and offers setup on the
/// first launch from Applications where `draftmux` doesn't already run this
/// app.
pub fn init(window: AnyWindowHandle, data_dir: &Path, cx: &mut App) {
    cx.on_action(move |_: &InstallCommandLineTool, cx| {
        cx.spawn(async move |cx| install(window, cx).await).detach();
    });
    let marker = data_dir.join(OFFERED_MARKER);
    cx.spawn(async move |cx| offer(window, marker, cx).await)
        .detach();
}

/// The installed app's command, or why the app must move first.
fn installed_exe() -> Result<PathBuf, String> {
    let home = std::env::home_dir().ok_or("Draftmux can't find your home directory.")?;
    let exe = resources::exe().map_err(|err| format!("{err:#}"))?;
    Ok(installed_app(&exe, &home)?.join("Contents/MacOS/draftmux"))
}

async fn offer(window: AnyWindowHandle, marker: PathBuf, cx: &mut AsyncApp) {
    let env = cx.update(|cx| ShellEnv::ready(cx)).await;
    let Ok(exe) = installed_exe() else { return };
    if marker.exists() || readiness(&exe, &env) == Readiness::Ready {
        return;
    }
    let answer = prompt(
        window,
        PromptLevel::Info,
        "Install the draftmux command?",
        "Pi and Claude Code use it to hand drafts to Draftmux. Installing it may ask \
         for your password."
            .into(),
        &[PromptButton::ok("Install"), PromptButton::cancel("Not Now")],
        cx,
    )
    .await;
    if let Err(err) = fs::write(&marker, "") {
        eprintln!("cli setup: couldn't record the offer: {err}");
    }
    if answer == Some(0) {
        install(window, cx).await;
    }
}

async fn install(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let exe = match installed_exe() {
        Ok(exe) => exe,
        Err(reason) => {
            let title = "Move Draftmux to Applications first";
            prompt(
                window,
                PromptLevel::Warning,
                title,
                reason,
                &[PromptButton::ok("OK")],
                cx,
            )
            .await;
            return;
        }
    };
    let link_path = Path::new(LINK);
    let linked = match link(&exe, link_path) {
        Err(LinkError::NeedsAdmin) => {
            let target = exe.clone();
            match cx
                .background_spawn(async move { link_as_admin(&target, link_path) })
                .await
            {
                Ok(true) => Ok(Linked::Created),
                // The user dismissed the password prompt.
                Ok(false) => return,
                Err(err) => Err(err),
            }
        }
        Err(LinkError::Conflict(detail)) => {
            let title = "The command line tool wasn't installed";
            prompt(
                window,
                PromptLevel::Warning,
                title,
                detail,
                &[PromptButton::ok("OK")],
                cx,
            )
            .await;
            return;
        }
        Err(LinkError::Failed(err)) => Err(err.to_string()),
        Ok(linked) => Ok(linked),
    };
    let (level, title, detail) = match linked {
        Err(err) => (
            PromptLevel::Critical,
            "The command line tool couldn't be installed",
            format!(
                "Linking {LINK} failed: {err}. To link it yourself, run this in a \
                 terminal:\n\n{}",
                manual_command(&exe)
            ),
        ),
        Ok(linked) => {
            let env = cx.update(|cx| ShellEnv::ready(cx)).await;
            let done = match linked {
                Linked::Created | Linked::Updated => format!("Linked {LINK} to this app."),
                Linked::Unchanged => format!("{LINK} already links to this app."),
            };
            match readiness(&exe, &env) {
                Readiness::Ready => (
                    PromptLevel::Info,
                    "The draftmux command is installed",
                    format!(
                        "{done} Pi and Claude Code can now run it; restart any agent \
                         sessions that were already open."
                    ),
                ),
                readiness => (
                    PromptLevel::Warning,
                    "The draftmux command needs shell setup",
                    format!("{done} {}", readiness.guidance()),
                ),
            }
        }
    };
    prompt(window, level, title, detail, &[PromptButton::ok("OK")], cx).await;
}

/// Shows a native alert over `window` and resolves to the button picked.
async fn prompt(
    window: AnyWindowHandle,
    level: PromptLevel,
    title: &str,
    detail: String,
    answers: &[PromptButton],
    cx: &mut AsyncApp,
) -> Option<usize> {
    let answer = cx
        .update_window(window, |_, window, cx| {
            window.activate_window();
            window.prompt(level, title, Some(&detail), answers, cx)
        })
        .ok()?;
    answer.await.ok()
}

/// The app bundle holding `exe` if it sits directly in `/Applications` or
/// `~/Applications`, else why the link can't be made yet. A link into a
/// mounted disk image or the temporary copy macOS runs a quarantined app
/// from (App Translocation) would break once that goes away.
fn installed_app<'a>(exe: &'a Path, home: &Path) -> Result<&'a Path, String> {
    let Some(app) = resources::bundle(exe) else {
        return Err(
            "This copy of Draftmux isn't an app bundle. The command line tool \
                    comes with Draftmux.app."
                .into(),
        );
    };
    let move_it = "Drag Draftmux into the Applications folder, open it from there, and \
                   install the command line tool again.";
    if exe.starts_with("/Volumes") {
        return Err(format!(
            "Draftmux is running from the disk image. {move_it}"
        ));
    }
    if exe
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Err(format!(
            "macOS is running Draftmux from a temporary copy. {move_it}"
        ));
    }
    let parent = app.parent();
    if parent != Some(Path::new("/Applications")) && parent != Some(&home.join("Applications")) {
        return Err(format!(
            "Draftmux is in {}, outside Applications. {move_it}",
            parent.unwrap_or(app).display()
        ));
    }
    Ok(app)
}

#[derive(Debug, PartialEq, Eq)]
enum Linked {
    Created,
    /// Replaced a link into another `Draftmux.app`.
    Updated,
    Unchanged,
}

#[derive(Debug)]
enum LinkError {
    /// Something that isn't Draftmux's command is in the way; nothing was
    /// changed. Holds a message for the user.
    Conflict(String),
    /// The directory is missing or not writable by this user.
    NeedsAdmin,
    Failed(io::Error),
}

/// Makes `link` a symlink to `target` as this user, replacing only a link
/// into another `Draftmux.app`.
fn link(target: &Path, link: &Path) -> Result<Linked, LinkError> {
    let needs_admin = |err: io::Error| match err.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound => LinkError::NeedsAdmin,
        _ => LinkError::Failed(err),
    };
    let existing = match fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Some(fs::read_link(link).map_err(LinkError::Failed)?)
        }
        Ok(_) => {
            return Err(LinkError::Conflict(format!(
                "{} already exists and isn't a link to Draftmux, so it was left alone. \
                 Remove it and try again.",
                link.display()
            )));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(LinkError::Failed(err)),
    };
    let Some(existing) = existing else {
        std::os::unix::fs::symlink(target, link).map_err(needs_admin)?;
        return Ok(Linked::Created);
    };
    if existing == target {
        return Ok(Linked::Unchanged);
    }
    if !is_draftmux_command(&existing) {
        return Err(LinkError::Conflict(format!(
            "{} links to {}, which isn't Draftmux, so it was left alone. Remove it and \
             try again.",
            link.display(),
            existing.display()
        )));
    }
    // A new link renamed over the old one, so the command never goes
    // missing.
    let staged = link.with_file_name(format!(".draftmux-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(target, &staged).map_err(needs_admin)?;
    if let Err(err) = fs::rename(&staged, link) {
        fs::remove_file(&staged).ok();
        return Err(LinkError::Failed(err));
    }
    Ok(Linked::Updated)
}

/// Whether `path` is the command inside some `Draftmux.app`, as the install
/// scripts check before replacing a link.
fn is_draftmux_command(path: &Path) -> bool {
    path.ends_with("Draftmux.app/Contents/MacOS/draftmux")
}

/// Creates the link's directory and the link as an administrator, after
/// [`link`] has checked that only nothing or a Draftmux link is replaced.
/// `Ok(false)` when the user dismissed the password prompt.
fn link_as_admin(target: &Path, link: &Path) -> Result<bool, String> {
    let script = format!(
        "do shell script {} with prompt {} with administrator privileges",
        applescript_string(&link_command(target, link)),
        applescript_string("Draftmux wants to install the draftmux command in /usr/local/bin."),
    );
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|err| format!("running osascript: {err}"))?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // "User canceled. (-128)"
    if stderr.contains("(-128)") {
        return Ok(false);
    }
    Err(stderr.trim().to_owned())
}

/// The shell command that makes `link` point at `target`, creating its
/// directory.
fn link_command(target: &Path, link: &Path) -> String {
    let dir = link.parent().unwrap_or(Path::new("/"));
    format!(
        "mkdir -p {} && ln -sfn {} {}",
        shell_quote(dir),
        shell_quote(target),
        shell_quote(link)
    )
}

/// The commands for the user to link the command themselves.
fn manual_command(target: &Path) -> String {
    format!(
        "sudo sh -c {}",
        shell_quote(Path::new(&link_command(target, Path::new(LINK))))
    )
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

#[derive(Debug, PartialEq, Eq)]
enum Readiness {
    /// `draftmux` on the login shell's `PATH` runs this app.
    Ready,
    /// No `draftmux` on the login shell's `PATH`.
    NotOnPath,
    /// Another `draftmux` comes first on `PATH`.
    Shadowed(PathBuf),
    /// The login shell couldn't be read, so `PATH` is unknown.
    Unknown(String),
}

fn readiness(exe: &Path, env: &ShellEnv) -> Readiness {
    readiness_of(exe, env.find("draftmux").as_deref(), env.capture_error())
}

/// How `draftmux` resolves on the login shell's `PATH`, given the first
/// one `found` there and why the shell couldn't be read, if it couldn't.
fn readiness_of(exe: &Path, found: Option<&Path>, capture_error: Option<&str>) -> Readiness {
    let runs_exe = |path: &Path| dunce::canonicalize(path).is_ok_and(|p| p == exe);
    match (found, capture_error) {
        (_, Some(err)) => Readiness::Unknown(err.to_owned()),
        (Some(path), None) if runs_exe(path) => Readiness::Ready,
        (Some(path), None) => Readiness::Shadowed(path.to_owned()),
        (None, None) => Readiness::NotOnPath,
    }
}

impl Readiness {
    /// What the user should do so that `draftmux` runs this app.
    fn guidance(&self) -> String {
        let restart = "Then restart Draftmux and any agent sessions.";
        match self {
            Self::Ready => String::new(),
            Self::NotOnPath => format!(
                "Your login shell's PATH doesn't include /usr/local/bin, so Pi and Claude \
                 Code can't find the command. Add it to PATH in your shell profile. {restart}"
            ),
            Self::Shadowed(other) => format!(
                "{} comes first on your login shell's PATH and runs a different Draftmux. \
                 Remove it, or put /usr/local/bin before its directory on PATH. {restart}",
                other.display()
            ),
            Self::Unknown(err) => format!(
                "Draftmux couldn't read your login shell's PATH ({err}), so it can't tell \
                 whether Pi and Claude Code will find the command. Running `which draftmux` \
                 in a terminal shows whether they will."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    /// A fake installed app whose command prints its arguments.
    fn fake_app(dir: &Path) -> PathBuf {
        let exe = dir.join("Draftmux.app/Contents/MacOS/draftmux");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "#!/bin/sh\necho \"ran $*\"\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        dunce::canonicalize(exe).unwrap()
    }

    fn run_with_path(command: &str, path: &Path) -> std::process::Output {
        Command::new("/bin/sh")
            .args(["-c", command])
            .env_clear()
            .env("PATH", path)
            .output()
            .unwrap()
    }

    #[test]
    fn the_command_runs_after_setup_and_not_before() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake_app(&tmp.path().join("Applications"));
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .unwrap();
        let path = Path::new(&path);

        let before = run_with_path("draftmux skill", path);
        assert_eq!(before.status.code(), Some(127));

        assert_eq!(link(&exe, &bin.join("draftmux")).unwrap(), Linked::Created);
        let after = run_with_path("draftmux skill", path);
        assert!(after.status.success());
        assert_eq!(String::from_utf8_lossy(&after.stdout), "ran skill\n");
    }

    #[test]
    fn repeated_setup_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake_app(tmp.path());
        let link_path = tmp.path().join("draftmux");
        assert_eq!(link(&exe, &link_path).unwrap(), Linked::Created);
        assert_eq!(link(&exe, &link_path).unwrap(), Linked::Unchanged);
        assert_eq!(fs::read_link(&link_path).unwrap(), exe);
    }

    #[test]
    fn a_link_into_another_draftmux_app_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake_app(tmp.path());
        let link_path = tmp.path().join("draftmux");
        symlink("/Old/Draftmux.app/Contents/MacOS/draftmux", &link_path).unwrap();
        assert_eq!(link(&exe, &link_path).unwrap(), Linked::Updated);
        assert_eq!(fs::read_link(&link_path).unwrap(), exe);
        let leftovers = fs::read_dir(tmp.path())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name() != "Draftmux.app")
            .count();
        assert_eq!(leftovers, 1, "only the link remains beside the app");
    }

    #[test]
    fn unrelated_files_and_links_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake_app(tmp.path());

        let file = tmp.path().join("file");
        fs::write(&file, "mine").unwrap();
        assert!(matches!(link(&exe, &file), Err(LinkError::Conflict(_))));
        assert_eq!(fs::read_to_string(&file).unwrap(), "mine");

        let other = tmp.path().join("other");
        symlink("/usr/local/bin/something", &other).unwrap();
        assert!(matches!(link(&exe, &other), Err(LinkError::Conflict(_))));
        assert_eq!(
            fs::read_link(&other).unwrap(),
            Path::new("/usr/local/bin/something")
        );

        let dangling = tmp.path().join("dangling");
        symlink(tmp.path().join("gone"), &dangling).unwrap();
        assert!(matches!(link(&exe, &dangling), Err(LinkError::Conflict(_))));
    }

    #[test]
    fn a_missing_or_read_only_directory_needs_an_administrator() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake_app(tmp.path());
        let missing = tmp.path().join("missing/draftmux");
        assert!(matches!(link(&exe, &missing), Err(LinkError::NeedsAdmin)));
        assert!(!tmp.path().join("missing").exists());

        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        symlink(
            "/Old/Draftmux.app/Contents/MacOS/draftmux",
            locked.join("old"),
        )
        .unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let new = link(&exe, &locked.join("draftmux"));
        let update = link(&exe, &locked.join("old"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(new, Err(LinkError::NeedsAdmin)));
        assert!(matches!(update, Err(LinkError::NeedsAdmin)));
    }

    #[test]
    fn the_privileged_command_survives_both_quotings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(r#"it's "quoted" \ odd"#);
        let exe = fake_app(&root);
        let link_path = root.join("new bin/draftmux");
        // The privileged script without the privileges.
        let script = format!(
            "do shell script {}",
            applescript_string(&link_command(&exe, &link_path))
        );
        let status = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read_link(&link_path).unwrap(), exe);
    }

    #[test]
    fn the_manual_command_links_through_sh() {
        let command = manual_command(Path::new(
            "/Applications/Draftmux.app/Contents/MacOS/draftmux",
        ));
        assert_eq!(
            command,
            r#"sudo sh -c 'mkdir -p '\''/usr/local/bin'\'' && ln -sfn '\''/Applications/Draftmux.app/Contents/MacOS/draftmux'\'' '\''/usr/local/bin/draftmux'\'''"#
        );
    }

    #[test]
    fn only_an_app_in_applications_is_linked() {
        let home = Path::new("/Users/me");
        let exe = |app: &str| PathBuf::from(format!("{app}/Contents/MacOS/draftmux"));
        for app in [
            "/Applications/Draftmux.app",
            "/Users/me/Applications/Draftmux.app",
        ] {
            assert_eq!(installed_app(&exe(app), home), Ok(Path::new(app)));
        }
        for (app, reason) in [
            ("/Volumes/Draftmux/Draftmux.app", "disk image"),
            (
                "/private/var/folders/x/T/AppTranslocation/ABC/d/Draftmux.app",
                "temporary copy",
            ),
            ("/Users/me/Downloads/Draftmux.app", "outside Applications"),
            ("/Applications/Tools/Draftmux.app", "outside Applications"),
        ] {
            let err = installed_app(&exe(app), home).unwrap_err();
            assert!(err.contains(reason), "{app}: {err}");
        }
        assert!(installed_app(Path::new("/repo/target/debug/draftmux"), home).is_err());
    }

    #[test]
    fn readiness_follows_the_login_shell_path() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake_app(tmp.path());
        let link_path = tmp.path().join("draftmux");
        symlink(&exe, &link_path).unwrap();
        let other = tmp.path().join("other-draftmux");
        fs::write(&other, "").unwrap();

        assert_eq!(readiness_of(&exe, Some(&link_path), None), Readiness::Ready);
        assert_eq!(readiness_of(&exe, None, None), Readiness::NotOnPath);
        assert_eq!(
            readiness_of(&exe, Some(&other), None),
            Readiness::Shadowed(other.clone())
        );
        assert_eq!(
            readiness_of(&exe, Some(&link_path), Some("timed out")),
            Readiness::Unknown("timed out".into())
        );
    }
}
