//! Runs a one-off tool process (oxfmt, a pi selection call) under a deadline
//! and returns its stdout or a one-line message for the user.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How often a run checks its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Runs `command` (the program `name`) with `input` on stdin, or stdin closed
/// when `None`, and returns its stdout. The error is a message for the user:
/// the first line of stderr when there is one. The process and anything it
/// started are killed when `timeout` passes or `cancel` is set; the call
/// returns only after the process has exited.
pub fn run(
    name: &str,
    mut command: Command,
    input: Option<&str>,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so a timeout kills its children too.
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|err| format!("Couldn't run {name}: {err}"))?;

    // Each pipe on its own thread, so a full pipe never blocks the others.
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let input = input.as_bytes().to_vec();
        std::thread::spawn(move || {
            // The process may exit without reading it all; its exit status
            // says why.
            let _ = stdin.write_all(&input);
        });
    }
    // Readers send chunks as they arrive, so what was written before a
    // timeout is kept even if the pipe stays open. The channel disconnects
    // once both reach end of file.
    let (sender, receiver) = mpsc::channel();
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
    drop(sender);

    let cancelled = || cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed));
    // Pipes close when every process holding them exits, so reading both to
    // the end before waiting also bounds stragglers that inherited them.
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut pipes_open = true;
    let status = loop {
        if cancelled() {
            break None;
        }
        if !pipes_open {
            match child.try_wait() {
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
        match receiver.recv_timeout(remaining.min(CANCEL_POLL)) {
            Ok((true, chunk)) => stdout.extend(chunk),
            Ok((false, chunk)) => stderr.extend(chunk),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => pipes_open = false,
        }
    };
    if status.is_none() {
        // The child is unreaped, so its group id can't have been reused.
        // SAFETY: a plain syscall on our own child's process group.
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.wait();
        if cancelled() {
            return Err(format!("{name} was cancelled"));
        }
        while let Ok((is_stdout, chunk)) = receiver.try_recv() {
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
    String::from_utf8(stdout).map_err(|_| format!("{name} printed text that isn't UTF-8"))
}

/// The first non-blank line, trimmed.
fn first_line(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
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
