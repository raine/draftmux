//! The app's log: `draftmux.log` in the data directory receives the app's
//! stderr however it was launched, with a startup line and a record of any
//! panic. The log is rotated to `draftmux.log.1` once it passes `MAX_LOG`,
//! before the next app opens it.

use std::backtrace::Backtrace;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const LOG: &str = "draftmux.log";
/// The size past which the log is moved aside when the app next starts.
const MAX_LOG: u64 = 1024 * 1024;

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG)
}

/// Rotates the log if it is too large, then opens it for appending.
pub fn open_log(path: &Path) -> Result<File> {
    rotate(path, MAX_LOG).with_context(|| format!("rotating {}", path.display()))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Renames `path` to `<path>.1`, replacing an older one, when it is larger
/// than `limit` bytes. A process still writing to the old file keeps writing
/// to `<path>.1`, so no lines are lost.
fn rotate(path: &Path, limit: u64) -> io::Result<()> {
    match path.metadata() {
        Ok(meta) if meta.len() > limit => std::fs::rename(path, rotated(path)),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    name.into()
}

/// Points the app's stderr at the log in `dir`, unless it already goes there
/// (the CLI launched the app, rotating the log first) or to a terminal, then
/// records the start and installs the panic hook. Called once the process
/// owns the app lock, so a second instance doesn't rotate the running app's
/// log.
pub fn init(dir: &Path) {
    if let Err(err) = redirect_stderr(&log_path(dir)) {
        eprintln!("draftmux: {err:#}");
    }
    eprintln!(
        "{} draftmux {} started (pid {})",
        timestamp(),
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    install_panic_hook();
}

#[cfg(unix)]
fn redirect_stderr(path: &Path) -> Result<()> {
    use std::os::fd::{AsFd, AsRawFd};
    let stderr = io::stderr();
    if stderr.is_terminal() || is_same_file(stderr.as_fd(), path) {
        return Ok(());
    }
    let log = open_log(path)?;
    // SAFETY: both descriptors are open for the duration of the call; fd 2
    // keeps its own reference to the file after `log` is closed.
    if unsafe { libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO) } == -1 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("redirecting stderr to {}", path.display()));
    }
    Ok(())
}

/// Points stderr at the log unless it goes to a console. When the CLI
/// launched the app, stderr already is the log, and appending through a
/// second handle changes nothing.
#[cfg(windows)]
fn redirect_stderr(path: &Path) -> Result<()> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
    if io::stderr().is_terminal() {
        return Ok(());
    }
    // The handle stays open as the process's stderr.
    let log = open_log(path)?.into_raw_handle();
    // SAFETY: `log` is an open file handle that is never closed.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, log) } == 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("redirecting stderr to {}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn is_same_file(fd: std::os::fd::BorrowedFd<'_>, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(fd) = fd.try_clone_to_owned() else {
        return false;
    };
    match (File::from(fd).metadata(), path.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Writes the panic, where it happened and a backtrace to stderr as one
/// timestamped entry, then hands over to the previous hook. It touches no app
/// state, which may be what panicked.
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let entry = panic_entry(info, &Backtrace::force_capture());
        let _ = io::stderr().lock().write_all(entry.as_bytes());
        previous(info);
    }));
}

fn panic_entry(info: &PanicHookInfo<'_>, backtrace: &Backtrace) -> String {
    let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
    let location = info
        .location()
        .map_or_else(|| "unknown location".to_owned(), ToString::to_string);
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>");
    format!(
        "{} draftmux {} panicked on thread '{thread}' at {location}: {message}\n{backtrace}\n",
        timestamp(),
        env!("CARGO_PKG_VERSION"),
    )
}

pub fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f%:z")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_moves_only_an_oversized_log_aside() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join(LOG);
        let old = dir.path().join("draftmux.log.1");

        rotate(&log, 4).unwrap();
        assert!(!log.exists() && !old.exists());

        std::fs::write(&log, "1234").unwrap();
        rotate(&log, 4).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "1234");
        assert!(!old.exists());

        std::fs::write(&old, "older").unwrap();
        std::fs::write(&log, "12345").unwrap();
        rotate(&log, 4).unwrap();
        assert!(!log.exists());
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "12345");
    }

    #[test]
    fn open_log_appends_after_rotating() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join(LOG);
        std::fs::write(&log, "kept\n").unwrap();
        writeln!(open_log(&log).unwrap(), "added").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "kept\nadded\n");
    }

    #[cfg(unix)]
    #[test]
    fn is_same_file_compares_the_open_file() {
        use std::os::fd::AsFd;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join(LOG);
        let file = open_log(&log).unwrap();
        assert!(is_same_file(file.as_fd(), &log));
        assert!(!is_same_file(file.as_fd(), &dir.path().join("other.log")));
        std::fs::write(dir.path().join("other.log"), "").unwrap();
        assert!(!is_same_file(file.as_fd(), &dir.path().join("other.log")));
    }
}
