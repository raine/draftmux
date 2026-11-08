//! The Format action: runs the editor text through oxfmt on demand
//! (docs/design.md, Formatting).
//!
//! oxfmt runs from the draft's own directory with an explicit empty config,
//! so no `.oxfmtrc` from the source repo or the home directory changes how
//! the message wraps. An `.editorconfig` above the drafts directory still
//! applies; it has no prose-wrap setting.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use editor::{Editor, EditorEvent};
use gpui::{
    App, Context, Entity, FontWeight, KeyBinding, SharedString, Subscription, Task, Window,
    actions, div, prelude::*, px, rgb, rgba,
};
use shell_env::ShellEnv;

actions!(format, [Format]);

/// oxfmt is a Node script; a cold start takes about a second.
const TIMEOUT: Duration = Duration::from_secs(5);

const TEXT: u32 = 0x1d1d1f;
const MUTED: u32 = 0x3c3c43a8;
const BUTTON_HOVER: u32 = 0x0000000d;

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-f", Format, None)]);
}

/// Runs Format for one editor and renders its button and last message.
pub struct Formatter {
    editor: Entity<Editor>,
    /// The focused draft's directory, oxfmt's working directory.
    draft_dir: Option<PathBuf>,
    agent_running: bool,
    running: Option<Task<()>>,
    message: Option<SharedString>,
    _editor_events: Subscription,
}

impl Formatter {
    pub fn new(editor: Entity<Editor>, cx: &mut Context<Self>) -> Self {
        let _editor_events = cx.subscribe(&editor, |this, _, event, cx| {
            if *event == EditorEvent::Edited && this.message.take().is_some() {
                cx.notify();
            }
        });
        Self {
            editor,
            draft_dir: None,
            agent_running: false,
            running: None,
            message: None,
            _editor_events,
        }
    }

    pub fn set_draft_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.draft_dir = Some(dir);
        self.message = None;
        cx.notify();
    }

    /// Format is unavailable while the agent runs, and a format that
    /// finishes after the agent started is discarded.
    pub fn set_agent_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.agent_running = running;
        cx.notify();
    }

    /// The oxfmt executable, `None` while the shell environment is being
    /// captured or when oxfmt isn't installed.
    fn oxfmt(cx: &App) -> Option<(std::sync::Arc<ShellEnv>, PathBuf)> {
        let env = ShellEnv::global(cx)?;
        let path = env.oxfmt()?.to_path_buf();
        Some((env, path))
    }

    fn available(&self) -> bool {
        self.draft_dir.is_some() && !self.agent_running && self.running.is_none()
    }

    pub fn format(&mut self, cx: &mut Context<Self>) {
        if !self.available() {
            return;
        }
        let (Some((env, oxfmt)), Some(dir)) = (Self::oxfmt(cx), self.draft_dir.clone()) else {
            return;
        };
        let input = self.editor.read(cx).text().to_string();
        self.message = None;
        cx.notify();

        let run = cx.background_executor().spawn({
            let input = input.clone();
            let dir = dir.clone();
            async move {
                let config = empty_config()?;
                run(
                    oxfmt_command(env.command(&oxfmt), &dir, config.path()),
                    &input,
                    TIMEOUT,
                )
            }
        });
        self.running = Some(cx.spawn(async move |this, cx| {
            let result = run.await;
            this.update(cx, |this, cx| {
                this.running = None;
                cx.notify();
                // The text, the draft or the editor's owner changed while
                // oxfmt ran; its output and errors are about something else.
                if this.agent_running
                    || this.draft_dir.as_ref() != Some(&dir)
                    || this.editor.read(cx).text() != input
                {
                    return;
                }
                match result {
                    Ok(output) if output == input => {
                        this.message = Some("Already formatted".into());
                    }
                    Ok(output) => {
                        this.editor
                            .update(cx, |editor, cx| editor.replace_all(&output, cx));
                    }
                    Err(err) => this.message = Some(err.into()),
                }
            })
            .ok();
        }));
    }
}

impl Render for Formatter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = Self::oxfmt(cx).is_some();
        let available = self.available();
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .children(self.message.clone().map(|message| {
                div()
                    .max_w(px(360.))
                    .truncate()
                    .text_color(rgba(MUTED))
                    .child(message)
            }))
            .when(visible, |this| {
                this.child(
                    div()
                        .id("format")
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_color(rgb(TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Format")
                        .child(div().text_color(rgba(MUTED)).child("⌘⇧F"))
                        .map(|button| {
                            if available {
                                button
                                    .hover(|style| style.bg(rgba(BUTTON_HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| this.format(cx)))
                            } else {
                                button.opacity(0.4)
                            }
                        }),
                )
            })
    }
}

/// An empty oxfmt config. Passing it explicitly stops oxfmt from searching
/// the working directory's ancestors for one.
fn empty_config() -> Result<tempfile::NamedTempFile, String> {
    tempfile::Builder::new()
        .prefix("draftmux-oxfmtrc")
        .suffix(".json")
        .tempfile()
        .and_then(|mut file| file.write_all(b"{}").map(|()| file))
        .map_err(|err| format!("Couldn't write the oxfmt config: {err}"))
}

/// `command` (an oxfmt executable) set up to format markdown on stdin from
/// `dir` with the config at `config`.
fn oxfmt_command(mut command: Command, dir: &Path, config: &Path) -> Command {
    command
        .arg("--stdin-filepath")
        .arg("draft.md")
        .arg("--config")
        .arg(config)
        .current_dir(dir)
        // Diagnostics without ANSI escapes, for the one-line message.
        .env("NO_COLOR", "1");
    command
}

/// Runs `command` with `input` on stdin and returns its stdout. The error is
/// a message for the user: the first line of stderr when there is one. The
/// process and anything it started are killed when `timeout` passes.
fn run(mut command: Command, input: &str, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so a timeout kills its children too.
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|err| format!("Couldn't run oxfmt: {err}"))?;

    // Each pipe on its own thread, so a full pipe never blocks the others.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = input.as_bytes().to_vec();
    std::thread::spawn(move || {
        // oxfmt may exit without reading it all; its exit status says why.
        let _ = stdin.write_all(&input);
    });
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

    // Pipes close when every process holding them exits, so reading both to
    // the end before waiting also bounds stragglers that inherited them.
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut pipes_open = true;
    let status = loop {
        if !pipes_open {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Ok(None) => break None,
                Err(err) => return Err(format!("Couldn't run oxfmt: {err}")),
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((true, chunk)) => stdout.extend(chunk),
            Ok((false, chunk)) => stderr.extend(chunk),
            Err(RecvTimeoutError::Timeout) => break None,
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
        while let Ok((is_stdout, chunk)) = receiver.try_recv() {
            if !is_stdout {
                stderr.extend(chunk);
            }
        }
    }

    let stderr_line = first_line(&stderr);
    let Some(status) = status else {
        return Err(stderr_line
            .unwrap_or_else(|| format!("oxfmt didn't finish within {} s", timeout.as_secs_f32())));
    };
    if !status.success() {
        return Err(stderr_line.unwrap_or_else(|| format!("oxfmt failed ({status})")));
    }
    String::from_utf8(stdout).map_err(|_| "oxfmt printed text that isn't UTF-8".to_string())
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

    use super::*;

    /// A fake oxfmt: a shell script in its own temp directory, run by
    /// `/bin/sh` because macOS vets a new executable before its first run,
    /// which can take longer than the short test timeouts.
    fn fake(script: &str) -> (tempfile::TempDir, Command) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxfmt");
        fs::write(&path, script).unwrap();
        let mut command = Command::new("/bin/sh");
        command.arg(path);
        (dir, command)
    }

    fn run_fake(script: &str, input: &str, timeout: Duration) -> Result<String, String> {
        let (dir, command) = fake(script);
        let config = empty_config().unwrap();
        run(
            oxfmt_command(command, dir.path(), config.path()),
            input,
            timeout,
        )
    }

    #[test]
    fn success_returns_stdout() {
        let (bin, command) = fake(
            r#"pwd -P > "$(dirname "$0")/cwd"; printf '%s\n' "$@" > "$(dirname "$0")/args"; tr a-z A-Z"#,
        );
        let draft = tempfile::tempdir().unwrap();
        let config = empty_config().unwrap();
        let output = run(
            oxfmt_command(command, draft.path(), config.path()),
            "* a\n",
            TIMEOUT,
        );
        assert_eq!(output.unwrap(), "* A\n");

        let args = fs::read_to_string(bin.path().join("args")).unwrap();
        let config_path = config.path().display().to_string();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "--stdin-filepath",
                "draft.md",
                "--config",
                config_path.as_str()
            ]
        );
        let cwd = fs::read_to_string(bin.path().join("cwd")).unwrap();
        assert_eq!(Path::new(cwd.trim()), draft.path().canonicalize().unwrap());
        assert_eq!(fs::read_to_string(config.path()).unwrap(), "{}");
    }

    #[test]
    fn large_input_does_not_deadlock() {
        let input = "word ".repeat(100_000);
        assert_eq!(run_fake("cat", &input, TIMEOUT).unwrap(), input);
    }

    #[test]
    fn failure_shows_first_stderr_line() {
        let script =
            "cat >/dev/null; echo partial; printf '\\n  x boom\\nmore detail\\n' >&2; exit 2";
        assert_eq!(run_fake(script, "text", TIMEOUT).unwrap_err(), "x boom");
    }

    #[test]
    fn failure_without_stderr_shows_status() {
        let err = run_fake("exit 3", "text", TIMEOUT).unwrap_err();
        assert_eq!(err, "oxfmt failed (exit status: 3)");
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let started = Instant::now();
        let err = run_fake(
            "echo stalled >&2; sleep 30 & wait",
            "text",
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err, "stalled");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn timeout_without_stderr_says_so() {
        let err = run_fake("sleep 30", "text", Duration::from_millis(200)).unwrap_err();
        assert_eq!(err, "oxfmt didn't finish within 0.2 s");
    }

    /// Needs oxfmt on `PATH`: `cargo test -p draftmux -- --ignored`.
    #[test]
    #[ignore]
    fn real_oxfmt_formats_markdown_with_default_options() {
        // A broken config above the draft directory must not apply.
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".oxfmtrc.json"), "").unwrap();
        let draft = root.path().join("draft");
        fs::create_dir(&draft).unwrap();

        let config = empty_config().unwrap();
        let input = "* a\n* b\n\nA long line that stays\nbroken where it is.\n\n```js\nconst  x =  1\n```\n";
        let output = run(
            oxfmt_command(Command::new("oxfmt"), &draft, config.path()),
            input,
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            output,
            "- a\n- b\n\nA long line that stays\nbroken where it is.\n\n```js\nconst x = 1;\n```\n"
        );
    }
}
