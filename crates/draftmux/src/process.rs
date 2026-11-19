//! Runs a one-off tool process (such as a pi selection call) under a deadline
//! and returns its stdout or a one-line message for the user.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use draftmux_process::Group;

/// How often a run checks its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Runs `command` (the program `name`) with `input` on stdin, or stdin closed
/// when `None`, and returns its stdout. The error is a message for the user:
/// the first line of stderr when there is one. The process and anything it
/// started are killed when `timeout` passes or `cancel` is set; the call
/// returns only after the process has exited.
pub fn run(
    name: &str,
    command: Command,
    input: Option<&str>,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    let mut stdout = Vec::new();
    run_streaming(name, command, input, timeout, cancel, |chunk| {
        stdout.extend_from_slice(chunk);
        Ok(())
    })?;
    String::from_utf8(stdout).map_err(|_| format!("{name} printed text that isn't UTF-8"))
}

/// Delivers stdout bytes as they arrive without retaining them. A reader error
/// stops the process group just like cancellation; success still waits for exit.
pub fn run_streaming(
    name: &str,
    command: Command,
    input: Option<&str>,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
    on_stdout: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    Spawned::spawn(name, command, input.is_some())?.finish(input, timeout, cancel, on_stdout)
}

/// A started process whose input is yet to come: it can start up while
/// waiting for it. Dropped unfinished, it is killed along with anything it
/// started.
pub struct Spawned {
    name: String,
    child: Child,
    group: Group,
    stdin: Option<ChildStdin>,
    /// Chunks of stdout (`true`) and stderr as they arrive. Disconnects once
    /// both pipes reach end of file.
    output: Receiver<(bool, Vec<u8>)>,
    finished: bool,
}

impl Spawned {
    /// Starts `command` (the program `name`) with stdin piped when `stdin`,
    /// closed otherwise. Its output is kept until [`Spawned::finish`].
    pub fn spawn(name: &str, mut command: Command, stdin: bool) -> Result<Self, String> {
        command
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own group, so a timeout kills its children too.
        let (mut child, group) = draftmux_process::spawn(&mut command)
            .map_err(|err| format!("Couldn't run {name}: {err}"))?;
        // Each pipe on its own thread, so a full pipe never blocks the
        // others. Readers send chunks as they arrive, so what was written
        // before a timeout is kept even if the pipe stays open.
        let (sender, output) = mpsc::channel();
        let read = |mut pipe: Box<dyn Read + Send>, is_stdout: bool| {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let mut buf = [0; 8192];
                while let Ok(n @ 1..) = pipe.read(&mut buf) {
                    if sender.send((is_stdout, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            });
        };
        read(
            Box::new(child.stdout.take().expect("stdout is piped")),
            true,
        );
        read(
            Box::new(child.stderr.take().expect("stderr is piped")),
            false,
        );
        Ok(Self {
            name: name.to_owned(),
            stdin: child.stdin.take(),
            child,
            group,
            output,
            finished: false,
        })
    }

    /// Whether the process has exited, such as one that failed to start.
    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    /// Writes `input` to stdin and closes it, then delivers stdout as for
    /// [`run_streaming`]. `timeout` counts from now. The error is a message
    /// for the user: the first line of stderr when there is one.
    pub fn finish(
        mut self,
        input: Option<&str>,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        mut on_stdout: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.finished = true;
        let name = self.name.clone();
        let deadline = Instant::now() + timeout;
        if let Some(mut stdin) = self.stdin.take() {
            let input = input.unwrap_or_default().as_bytes().to_vec();
            std::thread::spawn(move || {
                // The process may exit without reading it all; its exit
                // status says why.
                let _ = stdin.write_all(&input);
            });
        }

        let cancelled = || cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed));
        // Pipes close when every process holding them exits, so reading both
        // to the end before waiting also bounds stragglers that inherited
        // them.
        let mut stderr = Vec::new();
        let mut read_error = None;
        let mut pipes_open = true;
        let status = loop {
            if cancelled() {
                break None;
            }
            if !pipes_open {
                match self.child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Ok(None) => break None,
                    Err(err) => return Err(format!("Couldn't run {name}: {err}")),
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break None;
            }
            match self.output.recv_timeout(remaining.min(CANCEL_POLL)) {
                Ok((true, chunk)) => {
                    if let Err(error) = on_stdout(&chunk) {
                        read_error = Some(error);
                        break None;
                    }
                }
                Ok((false, chunk)) => stderr.extend(chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => pipes_open = false,
            }
        };
        if status.is_none() {
            self.group.kill();
            let _ = self.child.wait();
            if cancelled() {
                return Err(format!("{name} was cancelled"));
            }
            if let Some(error) = read_error {
                return Err(error);
            }
            while let Ok((is_stdout, chunk)) = self.output.try_recv() {
                if !is_stdout {
                    stderr.extend(chunk);
                }
            }
        }

        let stderr_line = first_line(&stderr);
        let Some(status) = status else {
            return Err(stderr_line.unwrap_or_else(|| {
                format!("{name} didn't finish within {} s", timeout.as_secs_f32())
            }));
        };
        if !status.success() {
            return Err(stderr_line.unwrap_or_else(|| format!("{name} failed ({status})")));
        }
        Ok(())
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if !self.finished {
            self.group.kill();
            let _ = self.child.wait();
        }
    }
}

/// The first non-blank line, trimmed.
fn first_line(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

// The tests run shell scripts.
#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// A shell script run by `/bin/sh`, because macOS vets a new executable
    /// before its first run, which can take longer than the short timeouts.
    fn script(script: &str) -> (tempfile::TempDir, Command) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        fs::write(&path, script).unwrap();
        let mut command = Command::new("/bin/sh");
        command.arg(path);
        (dir, command)
    }

    fn run_script(
        script_text: &str,
        input: Option<&str>,
        timeout: Duration,
    ) -> Result<String, String> {
        let (_dir, command) = script(script_text);
        run("tool", command, input, timeout, None)
    }

    #[test]
    fn a_spawned_process_gets_its_input_later_and_dropping_it_kills_it() {
        let (dir, command) = script("touch \"$0.started\"; tr a-z A-Z");
        let spawned = Spawned::spawn("tool", command, true).unwrap();
        while !dir.path().join("tool.started").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut output = Vec::new();
        spawned
            .finish(Some("late\n"), TIMEOUT, None, |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();
        assert_eq!(output, b"LATE\n");

        let (_dir, command) = script("sleep 30 & wait");
        let mut spawned = Spawned::spawn("tool", command, true).unwrap();
        assert!(!spawned.exited());
        let start = Instant::now();
        drop(spawned);
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn streaming_delivers_before_exit_and_reader_failure_kills_the_child() {
        let (dir, command) =
            script("printf ready; while [ ! -f \"$0.continue\" ]; do sleep 0.01; done");
        let mut output = Vec::new();
        run_streaming("tool", command, None, TIMEOUT, None, |chunk| {
            output.extend_from_slice(chunk);
            fs::write(dir.path().join("tool.continue"), "").unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(output, b"ready");

        let (_dir, command) = script("echo invalid; sleep 30 & wait");
        let start = Instant::now();
        let error = run_streaming("tool", command, None, TIMEOUT, None, |_| {
            Err("bad event".into())
        })
        .unwrap_err();
        assert_eq!(error, "bad event");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn success_returns_stdout() {
        assert_eq!(
            run_script("tr a-z A-Z", Some("* a\n"), TIMEOUT).unwrap(),
            "* A\n"
        );
    }

    #[test]
    fn without_input_stdin_is_closed() {
        assert_eq!(
            run_script("wc -c | tr -d ' '", None, TIMEOUT).unwrap(),
            "0\n"
        );
    }

    #[test]
    fn large_input_does_not_deadlock() {
        let input = "word ".repeat(100_000);
        assert_eq!(run_script("cat", Some(&input), TIMEOUT).unwrap(), input);
    }

    #[test]
    fn failure_shows_first_stderr_line() {
        let script =
            "cat >/dev/null; echo partial; printf '\\n  x boom\\nmore detail\\n' >&2; exit 2";
        assert_eq!(
            run_script(script, Some("text"), TIMEOUT).unwrap_err(),
            "x boom"
        );
    }

    #[test]
    fn failure_without_stderr_shows_status() {
        let err = run_script("exit 3", Some("text"), TIMEOUT).unwrap_err();
        assert_eq!(err, "tool failed (exit status: 3)");
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let started = Instant::now();
        let err = run_script(
            "echo stalled >&2; sleep 30 & wait",
            Some("text"),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err, "stalled");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn timeout_without_stderr_says_so() {
        let err = run_script("sleep 30", Some("text"), Duration::from_millis(200)).unwrap_err();
        assert_eq!(err, "tool didn't finish within 0.2 s");
    }

    #[test]
    fn cancel_kills_the_process_group() {
        let (dir, command) = script(r#"sleep 30 & echo $! > "$(dirname "$0")/pid"; wait"#);
        let cancel = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::Relaxed);
            }
        });
        let started = Instant::now();
        let err = run("tool", command, None, TIMEOUT, Some(&cancel)).unwrap_err();
        assert_eq!(err, "tool was cancelled");
        assert!(started.elapsed() < Duration::from_secs(3));

        let pid: libc::pid_t = fs::read_to_string(dir.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // A killed descendant can remain a zombie until its new parent
        // reaps it, especially under a container's PID 1. Check execution,
        // not whether the kernel still reserves its PID.
        let deadline = Instant::now() + Duration::from_secs(1);
        while process_running(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_running(pid),
            "the background sleep was killed with the group"
        );
    }

    fn process_running(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks whether the process exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        #[cfg(target_os = "linux")]
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some((_, state)) = stat.rsplit_once(") ")
            && state.starts_with('Z')
        {
            return false;
        }
        true
    }
}
