//! How the CLI reaches the running app: a Unix socket in the data directory.
//!
//! The app process holding `draftmux.lock` owns `draftmux.sock`; the lock is
//! taken before gpui starts and held for the process lifetime, so only one app
//! runs and a stale socket left by a crash is safe to replace. Each connection
//! carries one JSON request line and gets one JSON reply line, sent after the
//! window has handled the request, or an error if it hasn't within
//! `REPLY_TIMEOUT`. Requests name drafts by ID only; the app reads the draft
//! from disk.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use draftmux_storage::{CONFIG_DIR_VAR, DATA_DIR_VAR, data_dir};
use gpui::{App, AsyncApp, WindowHandle};
use serde::{Deserialize, Serialize};

use crate::chrome::Chrome;
use crate::resources;

const SOCKET: &str = "draftmux.sock";
const LOCK: &str = "draftmux.lock";
const LOG: &str = "draftmux.log";
/// How long the CLI waits for a launched app to start listening, and for a
/// reply.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How long the app waits for the window to handle a request before replying
/// with an error. Shorter than `TIMEOUT`, so a CLI whose request is read
/// promptly gets that error rather than timing out itself.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);
const MAX_REQUEST: u64 = 4096;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "lowercase")]
pub enum Request {
    /// Bring the window to the front.
    Activate,
    /// Select the draft and bring the window to the front.
    Focus { id: String },
}

/// `{}` on success.
#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub enum Claim {
    /// This process is the app.
    Owner(Server),
    /// Another app process holds the lock.
    Taken,
}

pub struct Server {
    lock: File,
    listener: UnixListener,
}

/// Takes the app lock and binds the socket, before the app opens a window.
pub fn claim() -> Result<Claim> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let lock_path = dir.join(LOCK);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Claim::Taken),
        Err(TryLockError::Error(err)) => {
            return Err(err).with_context(|| format!("locking {}", lock_path.display()));
        }
    }
    let socket = dir.join(SOCKET);
    match std::fs::remove_file(&socket) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => {
            return Err(err).with_context(|| format!("removing {}", socket.display()));
        }
        _ => {}
    }
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    Ok(Claim::Owner(Server { lock, listener }))
}

/// A request on its way to the main thread, with where to send its reply.
type Requests = async_channel::Sender<(Request, SyncSender<Reply>)>;

impl Server {
    /// Answers requests for the app's lifetime. Each connection is read on its
    /// own thread, so one waiting on the main thread doesn't hold up the rest;
    /// the main thread handles requests one at a time.
    pub fn serve(self, window: WindowHandle<Chrome>, cx: &mut App) {
        let (requests, received) = async_channel::unbounded();
        thread::spawn(move || {
            let _lock = self.lock;
            accept(&self.listener, &requests, REPLY_TIMEOUT);
        });
        cx.spawn(async move |cx| {
            while let Ok((request, reply)) = received.recv().await {
                let error = handle(request, window, cx)
                    .err()
                    .map(|err| format!("{err:#}"));
                // The connection's thread may have stopped waiting.
                reply.try_send(Reply { error }).ok();
            }
        })
        .detach();
    }
}

fn accept(listener: &UnixListener, requests: &Requests, reply_timeout: Duration) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let requests = requests.clone();
                thread::spawn(move || {
                    if let Err(err) = answer(stream, &requests, reply_timeout) {
                        eprintln!("draftmux: request failed: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("draftmux: request failed: {err:#}"),
        }
    }
}

fn answer(stream: UnixStream, requests: &Requests, reply_timeout: Duration) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line)?;
    let reply = match serde_json::from_str::<Request>(&line) {
        Ok(request) => {
            let (send, recv) = mpsc::sync_channel(1);
            // If the main thread is gone, the failed send drops `send` and
            // `recv` disconnects.
            requests.send_blocking((request, send)).ok();
            match recv.recv_timeout(reply_timeout) {
                Ok(reply) => reply,
                Err(RecvTimeoutError::Timeout) => Reply {
                    error: Some(format!(
                        "the app didn't respond within {}s",
                        reply_timeout.as_secs_f32()
                    )),
                },
                Err(RecvTimeoutError::Disconnected) => Reply {
                    error: Some("the app is shutting down".into()),
                },
            }
        }
        Err(err) => Reply {
            error: Some(format!("invalid request: {err}")),
        },
    };
    write_line(&stream, &reply)
}

fn handle(request: Request, window: WindowHandle<Chrome>, cx: &mut AsyncApp) -> Result<()> {
    window.update(cx, |chrome, window, cx| {
        if let Request::Focus { id } = &request {
            chrome.focus_draft(id, window, cx)?;
        }
        window.activate_window();
        anyhow::Ok(())
    })??;
    cx.update(|cx| cx.activate(true));
    Ok(())
}

/// Sends a request to the app, launching it first if none is running.
pub fn send(request: &Request) -> Result<()> {
    let dir = data_dir()?;
    let socket = dir.join(SOCKET);
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(err) if not_listening(&err) => wait_for_app(&socket, Some(launch(&dir)?))?,
        Err(err) => return Err(err).with_context(|| format!("connecting to {}", socket.display())),
    };
    exchange(stream, request)
}

/// Asks the app that holds the lock to come to the front, waiting for it to
/// start listening if it is still starting.
pub fn activate_running() -> Result<()> {
    let socket = data_dir()?.join(SOCKET);
    exchange(wait_for_app(&socket, None)?, &Request::Activate)
}

/// Starts the app detached from the caller, with no stdin or stdout, so a
/// caller reading the CLI's output doesn't wait for the app to exit; stderr
/// goes to `draftmux.log`. From inside `Draftmux.app` the bundle is launched
/// through LaunchServices (`open -n`, leaving the lock to decide which
/// instance runs), which doesn't pass on the caller's environment, so `HOME`
/// and the directory overrides are forwarded to keep the app on the CLI's
/// data and configuration directories; the returned child is then `open`,
/// which exits once the launch is submitted. Otherwise this binary is spawned
/// as `draftmux app` in its own process group, so the agent's tool timeout or
/// Ctrl-C doesn't kill it.
fn launch(dir: &Path) -> Result<Child> {
    let log_path = dir.join(LOG);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let exe = resources::exe()?;
    let mut command = match resources::bundle(&exe) {
        Some(app) => {
            let mut command = Command::new("/usr/bin/open");
            command
                .arg("-n")
                .arg("-a")
                .arg(app)
                .arg("--stderr")
                .arg(&log_path);
            for name in ["HOME", DATA_DIR_VAR, CONFIG_DIR_VAR, "XDG_CONFIG_HOME"] {
                if let Some(value) = std::env::var_os(name) {
                    let mut env = std::ffi::OsString::from(format!("{name}="));
                    env.push(value);
                    command.arg("--env").arg(env);
                }
            }
            command
        }
        None => {
            let mut command = Command::new(&exe);
            command.arg("app");
            if let Some(home) = std::env::home_dir() {
                command.current_dir(home);
            }
            command.process_group(0);
            command
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .with_context(|| format!("launching {}", exe.display()))
}

/// Polls until the app accepts connections. A launched app that exits
/// successfully lost the lock to another app process that is still starting,
/// so only a failed exit ends the wait early.
fn wait_for_app(socket: &Path, mut child: Option<Child>) -> Result<UnixStream> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if not_listening(&err) => {}
            Err(err) => {
                return Err(err).with_context(|| format!("connecting to {}", socket.display()));
            }
        }
        if let Some(status) = child.as_mut().map(Child::try_wait).transpose()?.flatten() {
            if !status.success() {
                bail!(
                    "the app exited with {status}; see {}",
                    socket.with_file_name(LOG).display()
                );
            }
            child = None;
        }
        if Instant::now() > deadline {
            bail!(
                "the app didn't start listening within {}s; see {}",
                TIMEOUT.as_secs(),
                socket.with_file_name(LOG).display()
            );
        }
        thread::sleep(POLL);
    }
}

fn exchange(stream: UnixStream, request: &Request) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    write_line(&stream, request)?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("waiting for the app's reply")?;
    let reply: Reply = serde_json::from_str(&line)
        .with_context(|| format!("unexpected reply from the app: {line:?}"))?;
    match reply.error {
        Some(error) => bail!("{error}"),
        None => Ok(()),
    }
}

fn write_line(mut stream: &UnixStream, value: &impl Serialize) -> Result<()> {
    let mut json = serde_json::to_vec(value)?;
    json.push(b'\n');
    stream.write_all(&json)?;
    Ok(())
}

/// No app is listening: the socket was never created, or its owner died.
fn not_listening(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    type Received = async_channel::Receiver<(Request, SyncSender<Reply>)>;

    /// Serves a socket in a temporary directory, with requests left to the
    /// returned receiver.
    fn serve(reply_timeout: Duration) -> (tempfile::TempDir, std::path::PathBuf, Received) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join(SOCKET);
        let listener = UnixListener::bind(&socket).unwrap();
        let (requests, received) = async_channel::unbounded();
        thread::spawn(move || accept(&listener, &requests, reply_timeout));
        (dir, socket, received)
    }

    fn request(socket: &Path) -> thread::JoinHandle<Result<()>> {
        let stream = UnixStream::connect(socket).unwrap();
        thread::spawn(move || exchange(stream, &Request::Activate))
    }

    #[test]
    fn unanswered_requests_get_an_error_and_later_ones_are_served() {
        let (_dir, socket, received) = serve(Duration::from_millis(100));
        let err = request(&socket).join().unwrap().unwrap_err();
        assert_eq!(err.to_string(), "the app didn't respond within 0.1s");

        let answered = request(&socket);
        // The timed-out request is still queued; the reply to it is dropped.
        for _ in 0..2 {
            let (_, reply) = received.recv_blocking().unwrap();
            reply.try_send(Reply { error: None }).ok();
        }
        answered.join().unwrap().unwrap();
    }

    #[test]
    fn requests_reach_the_app_while_another_is_waiting() {
        let (_dir, socket, received) = serve(Duration::from_secs(5));
        let first = request(&socket);
        let (_, first_reply) = received.recv_blocking().unwrap();
        let second = request(&socket);
        let (_, second_reply) = received.recv_blocking().unwrap();
        first_reply.try_send(Reply { error: None }).unwrap();
        second_reply.try_send(Reply { error: None }).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
    }

    #[test]
    fn requests_after_the_app_stops_get_an_error() {
        let (_dir, socket, received) = serve(Duration::from_secs(5));
        drop(received);
        let err = request(&socket).join().unwrap().unwrap_err();
        assert_eq!(err.to_string(), "the app is shutting down");
    }
}
