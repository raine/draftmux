//! The user's login-shell environment and the tools Draftmux runs with it.
//!
//! Apps launched from Finder or the Dock inherit launchd's minimal `PATH` and
//! none of the variables exported in shell profiles, such as provider API
//! keys. [`init`] captures the login shell's environment once at startup and
//! resolves `pi` and `oxfmt` against it. pi and oxfmt are spawned through
//! [`ShellEnv::command`] so they see that environment.

mod capture;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;
use futures::future::Shared;
use gpui::{App, Global, Task};

use capture::{CaptureOptions, Vars};

/// A snapshot of the login shell's environment and the tools found in its
/// `PATH`. It doesn't change while the app runs.
pub struct ShellEnv {
    vars: Vars,
    /// Why the login shell couldn't be read, when the app's own environment
    /// (with common install directories added to `PATH`) is used instead.
    capture_error: Option<String>,
    pi: Option<PathBuf>,
    oxfmt: Option<PathBuf>,
}

/// pi wasn't found. Its `Display` is a message for the user.
#[derive(Debug, Clone)]
pub struct PiMissing {
    capture_error: Option<String>,
}

impl fmt::Display for PiMissing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pi wasn't found on your login shell's PATH. Install pi or add its \
             directory to PATH in your shell profile, then restart Draftmux."
        )?;
        if let Some(err) = &self.capture_error {
            write!(f, " (Couldn't read the login shell environment: {err}.)")?;
        }
        Ok(())
    }
}

impl std::error::Error for PiMissing {}

impl ShellEnv {
    fn capture_with(options: &CaptureOptions) -> Self {
        let (vars, capture_error) = match capture::capture(options) {
            Ok(vars) => (vars, None),
            Err(err) => (capture::fallback_env(options.base_env.clone()), Some(err)),
        };
        Self::new(vars, capture_error)
    }

    fn new(vars: Vars, capture_error: Option<String>) -> Self {
        let path = capture::get(&vars, "PATH").map(OsString::as_os_str);
        Self {
            pi: find_executable("pi", path),
            oxfmt: find_executable("oxfmt", path),
            vars,
            capture_error,
        }
    }

    /// The pi executable, or an error to show in place of the agent.
    pub fn pi(&self) -> Result<&Path, PiMissing> {
        self.pi.as_deref().ok_or_else(|| PiMissing {
            capture_error: self.capture_error.clone(),
        })
    }

    /// The oxfmt executable. Format is hidden when this is `None`.
    pub fn oxfmt(&self) -> Option<&Path> {
        self.oxfmt.as_deref()
    }

    /// A command for `program` with exactly the captured environment. Callers
    /// add arguments, the working directory and any overrides.
    pub fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(self.vars.iter().map(|(k, v)| (k, v)));
        command
    }

    /// The captured environment, or `None` while capture is still running.
    pub fn global(cx: &App) -> Option<Arc<ShellEnv>> {
        cx.global::<GlobalShellEnv>().0.peek().cloned()
    }

    /// Resolves to the captured environment once capture finishes.
    pub fn ready(cx: &App) -> impl Future<Output = Arc<ShellEnv>> + use<> {
        cx.global::<GlobalShellEnv>().0.clone()
    }
}

/// Values are omitted; the environment holds credentials.
impl fmt::Debug for ShellEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellEnv")
            .field(
                "vars",
                &self.vars.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("capture_error", &self.capture_error)
            .field("pi", &self.pi)
            .field("oxfmt", &self.oxfmt)
            .finish()
    }
}

struct GlobalShellEnv(Shared<Task<Arc<ShellEnv>>>);

impl Global for GlobalShellEnv {}

/// Starts capturing the login-shell environment in the background. Views
/// that read [`ShellEnv::global`] while rendering are redrawn when it's ready.
pub fn init(cx: &mut App) {
    let capture = cx
        .background_executor()
        .spawn(async {
            let started = Instant::now();
            let env = ShellEnv::capture_with(&CaptureOptions::from_process());
            log_summary(&env, started);
            Arc::new(env)
        })
        .shared();
    cx.set_global(GlobalShellEnv(capture.clone()));
    cx.spawn(async move |cx| {
        capture.await;
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

fn log_summary(env: &ShellEnv, started: Instant) {
    let source = match &env.capture_error {
        None => "login shell".to_string(),
        Some(err) => format!("fallback ({err})"),
    };
    let tool = |path: &Option<PathBuf>| {
        path.as_ref()
            .map_or("missing".to_string(), |p| p.display().to_string())
    };
    eprintln!(
        "shell-env: {source} in {} ms; pi: {}; oxfmt: {}",
        started.elapsed().as_millis(),
        tool(&env.pi),
        tool(&env.oxfmt),
    );
}

/// The first executable file named `name` in the absolute directories of
/// `path`. Symlinks are kept unresolved so tool upgrades are followed.
fn find_executable(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

#[cfg(test)]
mod tests;
