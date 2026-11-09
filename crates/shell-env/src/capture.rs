//! Running the login shell and reading its environment.

use std::ffi::{CStr, OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

pub(crate) type Vars = Vec<(OsString, OsString)>;

/// The value of the first variable named `name`.
pub(crate) fn get<'a>(vars: &'a Vars, name: &str) -> Option<&'a OsString> {
    vars.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// Variables that describe the capturing shell itself rather than the
/// user's configuration.
const SHELL_LOCAL_VARS: &[&str] = &["SHLVL", "_", "PWD", "OLDPWD"];

/// Directories appended to `PATH` when the login shell can't be read, so
/// common install locations still resolve.
#[cfg(target_os = "macos")]
const FALLBACK_PATH_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "~/.local/bin"];
#[cfg(not(target_os = "macos"))]
const FALLBACK_PATH_DIRS: &[&str] = &["/usr/local/bin", "~/.local/bin", "/usr/bin", "/bin"];

pub(crate) struct CaptureOptions {
    pub shell: PathBuf,
    /// The environment the shell starts from.
    pub base_env: Vars,
    pub timeout: Duration,
}

impl CaptureOptions {
    pub fn from_process() -> Self {
        let shell = std::env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .map(PathBuf::from)
            .or_else(passwd_shell)
            .unwrap_or_else(|| {
                PathBuf::from(if cfg!(target_os = "macos") {
                    "/bin/zsh"
                } else {
                    "/bin/sh"
                })
            });
        Self {
            shell,
            base_env: std::env::vars_os().collect(),
            timeout: Duration::from_secs(5),
        }
    }
}

/// Runs `shell` as an interactive login shell and returns the environment it
/// ends up with. Interactive matters: zsh reads `.zshrc`, where API keys are
/// commonly exported, only in interactive shells.
pub(crate) fn capture(options: &CaptureOptions) -> Result<Vars, String> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let start = format!("__DRAFTMUX_ENV_START_{nonce}__");
    let end = format!("__DRAFTMUX_ENV_END_{nonce}__");
    // `cd ~` runs directory hooks (direnv and similar) for the home directory.
    // The markers separate the environment from anything rc files print.
    let script = format!("cd ~; printf '%s' {start}; /usr/bin/env -0; printf '%s' {end}");

    let mut command = Command::new(&options.shell);
    // csh and tcsh have no `-l` and start a login shell when argv[0] is "-".
    let csh_family = matches!(
        options.shell.file_name().and_then(OsStr::to_str),
        Some("csh" | "tcsh")
    );
    if csh_family {
        command.arg0("-");
    } else {
        command.arg("-l");
    }
    command
        .args(["-i", "-c", &script])
        .env_clear()
        .envs(options.base_env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A new session keeps the interactive shell off any controlling terminal
    // and makes it a process group leader, so it can be killed with its
    // children.
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let deadline = Instant::now() + options.timeout;
    let mut child = command
        .spawn()
        .map_err(|err| format!("couldn't start {}: {err}", options.shell.display()))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            if sender.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut output = Vec::new();
    let result = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // `recv_timeout` still returns queued chunks after the deadline.
        let received = if remaining.is_zero() {
            Err(RecvTimeoutError::Timeout)
        } else {
            receiver.recv_timeout(remaining)
        };
        match received {
            Ok(chunk) => {
                // Only the new bytes can complete the end marker.
                let unscanned = output.len().saturating_sub(end.len());
                output.extend_from_slice(&chunk);
                if find(&output[unscanned..], end.as_bytes()).is_some()
                    && let Some(env) = between(&output, start.as_bytes(), end.as_bytes())
                {
                    break Ok(parse_env(env));
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                break Err(format!(
                    "{} didn't finish starting within {} s",
                    options.shell.display(),
                    options.timeout.as_secs_f32()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                break Err(format!(
                    "{} exited without printing its environment",
                    options.shell.display()
                ));
            }
        }
    };
    // The environment is read; nothing the shell does from here on (logout
    // scripts, exit traps, leftover jobs) matters, and it must not delay
    // startup. The unreaped child keeps its pid, so the group id is still
    // ours to kill.
    // SAFETY: plain syscalls on our own child's process group.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.wait();
    result
}

/// The bytes between the first `start` and the following `end`.
fn between<'a>(output: &'a [u8], start: &[u8], end: &[u8]) -> Option<&'a [u8]> {
    let from = find(output, start)? + start.len();
    let len = find(&output[from..], end)?;
    Some(&output[from..from + len])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parses `env -0` output: NUL-separated `NAME=value` records, where values
/// may contain newlines and `=`.
pub(crate) fn parse_env(output: &[u8]) -> Vars {
    output
        .split(|&b| b == 0)
        .filter_map(|record| {
            let eq = record.iter().position(|&b| b == b'=')?;
            let (name, value) = (&record[..eq], &record[eq + 1..]);
            let local = SHELL_LOCAL_VARS.iter().any(|v| v.as_bytes() == name);
            (!name.is_empty() && !local).then(|| {
                (
                    OsString::from_vec(name.to_vec()),
                    OsString::from_vec(value.to_vec()),
                )
            })
        })
        .collect()
}

/// `base_env` with the fallback install directories appended to `PATH`.
pub(crate) fn fallback_env(mut base_env: Vars) -> Vars {
    let home = get(&base_env, "HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = get(&base_env, "PATH")
        .map(|v| std::env::split_paths(v).collect())
        .unwrap_or_default();
    for dir in FALLBACK_PATH_DIRS {
        let dir = match (dir.strip_prefix("~/"), &home) {
            (Some(rest), Some(home)) => home.join(rest),
            (Some(_), None) => continue,
            (None, _) => PathBuf::from(dir),
        };
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let path = std::env::join_paths(dirs).unwrap_or_default();
    base_env.retain(|(k, _)| k != "PATH");
    base_env.push(("PATH".into(), path));
    base_env
}

/// The login shell from the user database, for when `SHELL` isn't set.
fn passwd_shell() -> Option<PathBuf> {
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call, and `pw_shell` points into
    // `buf`, which outlives its use below.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut passwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || passwd.pw_shell.is_null() {
        return None;
    }
    let shell = unsafe { CStr::from_ptr(passwd.pw_shell) }.to_bytes();
    (!shell.is_empty()).then(|| Path::new(OsStr::from_bytes(shell)).to_path_buf())
}
