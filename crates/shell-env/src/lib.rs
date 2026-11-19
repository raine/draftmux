//! The user's login-shell environment and the tools Draftmux runs with it.
//!
//! Apps launched from Finder or the Dock inherit launchd's minimal `PATH` and
//! none of the variables exported in shell profiles, such as provider API
//! keys. [`init`] captures the login shell's environment once at startup and
//! resolves `pi` and `claude` against it. Commands are spawned through
//! [`ShellEnv::command`] so they see that environment.
//!
//! Windows apps inherit the user's environment from the process that starts
//! them, so there the app's own environment is the snapshot, with `PATH`
//! completed from the registry.

#[cfg(unix)]
mod capture;
#[cfg(windows)]
mod windows;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;
use futures::future::Shared;
use gpui::{App, Global, Task};

#[cfg(unix)]
use capture::CaptureOptions;

pub(crate) type Vars = Vec<(OsString, OsString)>;

/// The value of the first variable named `name`, ignoring case on Windows,
/// where variable names are case-insensitive.
pub(crate) fn get<'a>(vars: &'a Vars, name: &str) -> Option<&'a OsString> {
    vars.iter()
        .find(|(k, _)| {
            if cfg!(windows) {
                k.eq_ignore_ascii_case(name)
            } else {
                k == name
            }
        })
        .map(|(_, v)| v)
}

/// The file names `pi` and `claude` are found under. On Windows, pi's
/// installers provide `pi.exe` or npm's `pi.cmd`; Claude Code needs its
/// native `claude.exe`, since multi-line arguments can't be passed to a
/// `.cmd` script.
#[cfg(unix)]
const PI: &[&str] = &["pi"];
#[cfg(unix)]
const CLAUDE: &[&str] = &["claude"];
#[cfg(windows)]
const PI: &[&str] = &["pi.exe", "pi.cmd"];
#[cfg(windows)]
const CLAUDE: &[&str] = &["claude.exe"];

/// A snapshot of the login shell's environment and the tools found in its
/// `PATH`. It doesn't change while the app runs.
pub struct ShellEnv {
    vars: Vars,
    /// Why the login shell couldn't be read, when the app's own environment
    /// (with common install directories added to `PATH`) is used instead.
    capture_error: Option<String>,
    pi: Option<PathBuf>,
    claude: Option<PathBuf>,
}

/// A tool wasn't found. Its `Display` is a message for the user.
#[derive(Debug, Clone)]
pub struct Missing {
    tool: &'static str,
    capture_error: Option<String>,
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tool = self.tool;
        if cfg!(windows) {
            return write!(
                f,
                "{tool} wasn't found on your PATH. Install {tool} or add its directory to \
                 PATH, then restart Draftmux."
            );
        }
        write!(
            f,
            "{tool} wasn't found on your login shell's PATH. Install {tool} or add its \
             directory to PATH in your shell profile, then restart Draftmux."
        )?;
        if let Some(err) = &self.capture_error {
            write!(f, " (Couldn't read the login shell environment: {err}.)")?;
        }
        Ok(())
    }
}

impl std::error::Error for Missing {}

impl ShellEnv {
    /// Captures the login shell's environment now, blocking for up to the
    /// capture timeout. The app uses [`init`] instead.
    pub fn capture() -> Self {
        #[cfg(unix)]
        return Self::capture_with(&CaptureOptions::from_process());
        #[cfg(windows)]
        return Self::new(windows::env(), None);
    }

    #[cfg(unix)]
    fn capture_with(options: &CaptureOptions) -> Self {
        let (vars, capture_error) = match capture::capture(options) {
            Ok(vars) => (vars, None),
            Err(err) => (capture::fallback_env(options.base_env.clone()), Some(err)),
        };
        Self::new(vars, capture_error)
    }

    fn new(vars: Vars, capture_error: Option<String>) -> Self {
        let path = get(&vars, "PATH").map(OsString::as_os_str);
        Self {
            pi: find_executable(PI, path),
            claude: find_executable(CLAUDE, path),
            vars,
            capture_error,
        }
    }

    /// The pi executable, or an error to show in place of the agent.
    pub fn pi(&self) -> Result<&Path, Missing> {
        self.found(self.pi.as_deref(), "pi")
    }

    /// The first `claude` on the PATH, which may be a script that runs
    /// Claude Code, never a shell alias or function. An error to show in
    /// place of the agent when there is none.
    pub fn claude(&self) -> Result<&Path, Missing> {
        self.found(self.claude.as_deref(), "Claude Code")
    }

    fn found<'a>(&self, path: Option<&'a Path>, tool: &'static str) -> Result<&'a Path, Missing> {
        path.ok_or_else(|| Missing {
            tool,
            capture_error: self.capture_error.clone(),
        })
    }

    /// The first executable file called `name` on the captured `PATH`,
    /// with symlinks unresolved.
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        find_executable(&[name], get(&self.vars, "PATH").map(OsString::as_os_str))
    }

    /// Why the login shell couldn't be read, when the app's own environment
    /// with common install directories added to `PATH` stands in for it.
    pub fn capture_error(&self) -> Option<&str> {
        self.capture_error.as_deref()
    }

    /// The captured variables, for a process that builds its own
    /// environment from them.
    pub fn vars(&self) -> &[(OsString, OsString)] {
        &self.vars
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

    /// The captured environment, or `None` until capture has finished.
    pub fn global(cx: &App) -> Option<Arc<ShellEnv>> {
        cx.try_global::<GlobalShellEnv>()?.0.peek().cloned()
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
            .field("claude", &self.claude)
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
            let env = ShellEnv::capture();
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
        "shell-env: {source} in {} ms; pi: {}; claude: {}",
        started.elapsed().as_millis(),
        tool(&env.pi),
        tool(&env.claude),
    );
}

/// The first executable file with one of `names` in the absolute directories
/// of `path`, trying every name in a directory before the next. Symlinks are
/// kept unresolved so tool upgrades are followed.
fn find_executable(names: &[&str], path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.metadata().is_ok_and(|m| is_executable(&m)))
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// Windows runs a file by its extension, which the names carry.
#[cfg(windows)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn variables_are_found_whatever_their_case() {
        let vars: Vars = vec![("Path".into(), "C:\\bin".into())];
        assert_eq!(get(&vars, "PATH"), Some(&"C:\\bin".into()));
    }

    #[test]
    fn tools_need_a_runnable_extension() {
        let dir = tempfile::tempdir().unwrap();
        let (npm, native) = (dir.path().join("npm"), dir.path().join("native"));
        std::fs::create_dir(&npm).unwrap();
        std::fs::create_dir(&native).unwrap();
        // npm installs a shell script beside the cmd shim.
        std::fs::write(npm.join("pi"), "").unwrap();
        std::fs::write(npm.join("pi.cmd"), "").unwrap();
        std::fs::write(npm.join("claude.cmd"), "").unwrap();
        std::fs::write(native.join("claude.exe"), "").unwrap();
        let path = std::env::join_paths([&npm, &native]).unwrap();
        assert_eq!(find_executable(PI, Some(&path)), Some(npm.join("pi.cmd")));
        assert_eq!(
            find_executable(CLAUDE, Some(&path)),
            Some(native.join("claude.exe"))
        );
    }
}
