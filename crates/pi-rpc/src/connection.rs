//! One running pi process: stdio pipes, reader threads and shutdown.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{self, Incoming};

/// How long pi gets to exit after stdin closes before SIGTERM, then before
/// SIGKILL. pi treats stdin EOF as shutdown and disposes its runtime.
const EOF_GRACE: Duration = Duration::from_secs(3);
const TERM_GRACE: Duration = Duration::from_secs(2);
/// How long to wait for stdout and stderr to reach EOF after pi exits. A
/// process pi started can keep the pipes open.
const DRAIN_GRACE: Duration = Duration::from_secs(1);
const STDERR_TAIL: usize = 16 * 1024;

pub(crate) enum Message {
    Line(Incoming),
    /// The process exited and its output was read. Always the last message.
    Exited {
        status: String,
        stderr: String,
    },
}

pub(crate) struct Connection {
    stdin: ChildStdin,
    pid: libc::pid_t,
    exit: Arc<ExitState>,
}

#[derive(Default)]
struct ExitState {
    inner: Mutex<ExitInner>,
    changed: Condvar,
}

#[derive(Default)]
struct ExitInner {
    exited: bool,
    stdout_done: bool,
    stderr_done: bool,
}

impl Connection {
    /// Spawns `command` in its own process group with piped stdio. Every
    /// record pi prints, then its exit, is passed to `on_message` from a
    /// background thread.
    pub(crate) fn spawn(
        mut command: Command,
        on_message: impl Fn(Message) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let pid = child.id() as libc::pid_t;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let exit = Arc::new(ExitState::default());
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let on_message = Arc::new(on_message);

        let (exit_out, on_line) = (exit.clone(), on_message.clone());
        thread::Builder::new()
            .name("pi-stdout".into())
            .spawn(move || {
                read_records(stdout, |incoming| on_line(Message::Line(incoming)));
                exit_out.update(|inner| inner.stdout_done = true);
            })?;

        let (exit_err, tail_err) = (exit.clone(), tail.clone());
        thread::Builder::new()
            .name("pi-stderr".into())
            .spawn(move || {
                read_tail(stderr, &tail_err);
                exit_err.update(|inner| inner.stderr_done = true);
            })?;

        let exit_wait = exit.clone();
        thread::Builder::new()
            .name("pi-wait".into())
            .spawn(move || {
                let status = child.wait();
                exit_wait.update(|inner| inner.exited = true);
                // Deliver everything pi wrote before reporting the exit.
                exit_wait.wait_until(Instant::now() + DRAIN_GRACE, |inner| {
                    inner.stdout_done && inner.stderr_done
                });
                let stderr = String::from_utf8_lossy(tail.lock().unwrap().make_contiguous())
                    .trim()
                    .to_string();
                let status = match status {
                    Ok(status) => status.to_string(),
                    Err(err) => format!("wait failed: {err}"),
                };
                on_message(Message::Exited { status, stderr });
            })?;

        Ok(Self { stdin, pid, exit })
    }

    pub(crate) fn send(&mut self, line: &str) -> io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()
    }

    /// Closes stdin so pi shuts down, then escalates to SIGTERM and SIGKILL
    /// of the process group if it doesn't exit. Returns at once.
    pub(crate) fn stop(self) {
        let Self { stdin, pid, exit } = self;
        drop(stdin);
        let escalate = move || {
            for (grace, signal) in [(EOF_GRACE, libc::SIGTERM), (TERM_GRACE, libc::SIGKILL)] {
                if exit.wait_until(Instant::now() + grace, |inner| inner.exited) {
                    return;
                }
                // SAFETY: plain syscalls. pid was running a moment ago; the
                // window for its reaping and reuse in between is negligible.
                unsafe {
                    if signal == libc::SIGTERM {
                        libc::kill(pid, signal);
                    } else {
                        libc::killpg(pid, signal);
                    }
                }
            }
        };
        if let Err(err) = thread::Builder::new()
            .name("pi-stop".into())
            .spawn(escalate)
        {
            eprintln!("pi-rpc: couldn't start the stop thread: {err}");
        }
    }
}

impl ExitState {
    fn update(&self, f: impl FnOnce(&mut ExitInner)) {
        f(&mut self.inner.lock().unwrap());
        self.changed.notify_all();
    }

    /// Waits until `done` holds or `deadline` passes. Returns whether it holds.
    fn wait_until(&self, deadline: Instant, done: impl Fn(&ExitInner) -> bool) -> bool {
        let mut inner = self.inner.lock().unwrap();
        while !done(&inner) {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            inner = self.changed.wait_timeout(inner, deadline - now).unwrap().0;
        }
        true
    }
}

/// Reads LF-terminated JSON records. Only `\n` frames a record; U+2028 and
/// other separators can appear inside JSON strings. Lines that aren't valid
/// records are logged and skipped.
fn read_records(stdout: impl Read, mut on_record: impl FnMut(Incoming)) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(err) => {
                eprintln!("pi-rpc: reading pi's stdout failed: {err}");
                return;
            }
        }
        let mut line = buf.as_slice();
        line = line.strip_suffix(b"\n").unwrap_or(line);
        line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let parsed = std::str::from_utf8(line)
            .map_err(|err| err.to_string())
            .and_then(|line| protocol::parse_line(line).map_err(|err| err.to_string()));
        match parsed {
            Ok(incoming) => on_record(incoming),
            Err(err) => eprintln!(
                "pi-rpc: skipping a malformed record from pi ({err}): {}",
                String::from_utf8_lossy(&line[..line.len().min(200)])
            ),
        }
    }
}

/// Keeps the last [`STDERR_TAIL`] bytes of stderr for error reports.
fn read_tail(stderr: impl Read, tail: &Mutex<VecDeque<u8>>) {
    let mut stderr = stderr;
    let mut chunk = [0; 4096];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut tail = tail.lock().unwrap();
                tail.extend(&chunk[..n]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                tail.drain(..excess);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;

    #[test]
    fn framing() {
        let input = "{\"type\":\"turn_start\"}\r\n\n{\"type\":\"compaction_start\",\"reason\":\"a\u{2028}b\"}\nnot json\n{\"type\":\"turn_end\"}";
        let mut events = Vec::new();
        read_records(input.as_bytes(), |incoming| match incoming {
            Incoming::Event(event) => events.push(event),
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(
            events,
            [
                Event::TurnStart,
                Event::CompactionStart {
                    reason: "a\u{2028}b".into()
                },
                Event::TurnEnd,
            ]
        );
    }
}
