//! Automatic updates of a released `Draftmux.app` from the GitHub releases
//! of raine/draftmux. A Developer ID signed app checks for a newer release
//! once a day, installs it in the background by swapping the bundle in place
//! and offers to restart into it. Development builds and Linux never check
//! on their own; Check for Updates still reports a newer release to them.

// Linux only reports releases, so the install states go unused there.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

mod cache;
#[cfg(target_os = "macos")]
mod install;
mod release;

use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Task};
use semver::Version;

use crate::app_settings::AppSettings;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// `CFBundleIdentifier` in macos/Info.plist.
const BUNDLE_ID: &str = "fi.zendit.Draftmux";
const RELEASES_PAGE: &str = "https://github.com/raine/draftmux/releases";
/// Disables automatic checks when set to `1`, `true` or `yes`.
const NO_CHECK_VAR: &str = "DRAFTMUX_NO_UPDATE_CHECK";
/// How long after launch the first automatic check waits, and how often a
/// running app looks again whether one is due.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);
const RECHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

gpui::actions!(draftmux, [CheckForUpdates]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release's GitHub page.
    pub page: String,
    pub archive_url: String,
    pub checksum_url: String,
}

/// Why this copy of Draftmux can't install updates itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ineligible {
    /// Not a Developer ID signed `Draftmux.app`: built from source.
    Development,
    /// The folder holding `Draftmux.app` isn't writable by this user.
    Unwritable(String),
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
}

impl Ineligible {
    fn guidance(&self, version: &Version) -> String {
        match self {
            Self::Development => format!(
                "Draftmux {version} is available. This is a development build: pull the \
                 repository and run just install-app."
            ),
            Self::Unwritable(dir) => format!(
                "Draftmux {version} is available, but Draftmux can't replace itself in {dir}. \
                 Download it from GitHub."
            ),
            Self::Unsupported => format!(
                "Draftmux {version} is available. Download it from GitHub and run its install \
                 script, or pull the repository and rebuild."
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    Installing(Version),
    /// The new version is installed and runs after a restart.
    Ready(Version),
    /// A newer release this copy can't install itself.
    Blocked {
        release: Release,
        reason: String,
    },
}

/// A message for the user about a check they asked for.
pub struct Notice {
    pub text: SharedString,
    pub error: bool,
}

pub struct Updater {
    pub status: Status,
    _poll: Task<()>,
}

struct GlobalUpdater(Entity<Updater>);

impl Global for GlobalUpdater {}

impl EventEmitter<Notice> for Updater {}

pub fn init(cx: &mut App) {
    let entity = cx.new(|cx| Updater {
        status: Status::Idle,
        _poll: cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK_DELAY).await;
            loop {
                let Ok(()) = this.update(cx, |this: &mut Updater, cx| this.check(false, cx)) else {
                    return;
                };
                cx.background_executor().timer(RECHECK_INTERVAL).await;
            }
        }),
    });
    cx.set_global(GlobalUpdater(entity));
    cx.on_action(|_: &CheckForUpdates, cx| {
        Updater::global(cx).update(cx, |updater, cx| updater.check(true, cx));
    });
}

impl Updater {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalUpdater>().0.clone()
    }

    /// Looks for a newer release and installs it when this copy can. An
    /// automatic check runs only when one is due, only for an installable
    /// app and only while checks are on; it reports nothing but its result.
    fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        match &self.status {
            Status::Idle | Status::Blocked { .. } => {}
            Status::Checking | Status::Installing(_) => return,
            Status::Ready(version) => {
                if manual {
                    cx.emit(notice(
                        format!("Draftmux {version} is installed. Restart to use it."),
                        false,
                    ));
                }
                return;
            }
        }
        if !manual && !automatic_checks(cx) {
            return;
        }
        let previous = std::mem::replace(&mut self.status, Status::Checking);
        cx.notify();
        let task = cx.background_spawn(async move {
            let eligibility = eligibility();
            let built_here = matches!(
                eligibility,
                Err(Ineligible::Development | Ineligible::Unsupported)
            );
            if !manual && (built_here || !cache::check_due()) {
                return Ok(None);
            }
            let release = cache::check()?;
            Ok::<_, anyhow::Error>(Some((release, eligibility)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.status = previous;
                match result {
                    Ok(None) => {}
                    Ok(Some((None, _))) => {
                        if manual {
                            cx.emit(notice(
                                format!("Draftmux {CURRENT_VERSION} is the latest version."),
                                false,
                            ));
                        }
                    }
                    Ok(Some((Some(release), Ok(installation)))) => {
                        this.install(release, installation, manual, cx);
                    }
                    Ok(Some((Some(release), Err(reason)))) => {
                        let guidance = reason.guidance(&release.version);
                        if manual {
                            cx.emit(notice(guidance.clone(), false));
                        }
                        if matches!(reason, Ineligible::Unwritable(_)) {
                            this.status = Status::Blocked {
                                release,
                                reason: guidance,
                            };
                        }
                    }
                    Err(err) => {
                        if manual {
                            cx.emit(notice(format!("Couldn't check for updates: {err:#}"), true));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn install(
        &mut self,
        release: Release,
        installation: Installation,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        let version = release.version.clone();
        if manual {
            cx.emit(notice(format!("Downloading Draftmux {version}…"), false));
        }
        self.status = Status::Installing(version.clone());
        let task = cx.background_spawn(async move { install::install(&release, &installation) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.status = Status::Ready(version),
                    Err(err) => {
                        this.status = Status::Idle;
                        if manual {
                            cx.emit(notice(
                                format!("Couldn't install Draftmux {version}: {err:#}"),
                                true,
                            ));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(not(target_os = "macos"))]
    fn install(&mut self, _: Release, installation: Installation, _: bool, _: &mut Context<Self>) {
        match installation {}
    }

    /// Opens the page of the release this copy can't install itself.
    pub fn open_release_page(&self, cx: &App) {
        let page = match &self.status {
            Status::Blocked { release, .. } => release.page.as_str(),
            _ => RELEASES_PAGE,
        };
        cx.open_url(page);
    }
}

#[cfg(target_os = "macos")]
use install::Installation;

#[cfg(not(target_os = "macos"))]
enum Installation {}

fn eligibility() -> Result<Installation, Ineligible> {
    #[cfg(target_os = "macos")]
    return install::installation();
    #[cfg(not(target_os = "macos"))]
    Err(Ineligible::Unsupported)
}

fn automatic_checks(cx: &App) -> bool {
    let setting = AppSettings::global(cx)
        .read(cx)
        .settings
        .check_for_updates
        .unwrap_or(true);
    setting && !env_disables(std::env::var(NO_CHECK_VAR).ok().as_deref())
}

fn env_disables(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        ["1", "true", "yes"]
            .iter()
            .any(|on| value.trim().eq_ignore_ascii_case(on))
    })
}

fn notice(text: String, error: bool) -> Notice {
    Notice {
        text: text.into(),
        error,
    }
}

fn current_version() -> Version {
    Version::parse(CURRENT_VERSION).expect("package version must be valid semver")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_disables_checks() {
        assert!(env_disables(Some("1")));
        assert!(env_disables(Some("YES")));
        assert!(!env_disables(Some("0")));
        assert!(!env_disables(Some("")));
        assert!(!env_disables(None));
    }
}
