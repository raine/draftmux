//! The Format action: runs the editor text through oxfmt on demand.
//!
//! oxfmt runs from the draft's own directory with an explicit empty config,
//! so no `.oxfmtrc` from the source repo or the home directory changes how
//! the message wraps. An `.editorconfig` above the drafts directory still
//! applies; it has no prose-wrap setting.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use editor::{Editor, EditorEvent};
use gpui::{
    Animation, AnimationExt, App, Context, ElementId, Entity, FontWeight, KeyBinding, SharedString,
    Subscription, Task, Window, actions, div, ease_out_quint, prelude::*, px,
};
use shell_env::ShellEnv;

use crate::process;
use crate::theme;

actions!(format, [Format]);

/// oxfmt is a Node script; a cold start takes about a second.
const TIMEOUT: Duration = Duration::from_secs(5);
const MESSAGE_FADE: Duration = Duration::from_millis(200);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(editor::secondary!("shift-f"), Format, None)]);
}

/// Runs Format for one editor and renders its button and last message.
pub struct Formatter {
    editor: Entity<Editor>,
    /// The focused draft's directory, oxfmt's working directory.
    draft_dir: Option<PathBuf>,
    agent_running: bool,
    /// The draft's target reads text differently from CommonMark, so Format
    /// is hidden.
    hidden: bool,
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
            hidden: false,
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

    /// oxfmt formats CommonMark, so Format is hidden for targets that read
    /// text differently and a format that finishes after the switch is
    /// discarded. It would escape and rewrite plain text, and in a Slack
    /// message, where every line break shows, it merges a line after a `>`
    /// quote into the quote, adds and collapses blank lines and renumbers
    /// lists.
    pub fn set_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.hidden = hidden;
        self.message = None;
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
        self.draft_dir.is_some() && !self.agent_running && !self.hidden && self.running.is_none()
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
                process::run(
                    "oxfmt",
                    oxfmt_command(env.command(&oxfmt), &dir, config.path()),
                    Some(&input),
                    TIMEOUT,
                    None,
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
                    || this.hidden
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
                        this.editor.update(cx, |editor, cx| {
                            crate::changes::replace_all(editor, &output, cx)
                        });
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
        let visible = Self::oxfmt(cx).is_some() && !self.hidden;
        let available = self.available();
        let palette = *theme::palette(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .children(self.message.clone().map(|message| {
                div()
                    .max_w(px(360.))
                    .truncate()
                    .text_color(palette.muted)
                    .child(message.clone())
                    // Keyed by the text, so a different message fades in too.
                    .with_animation(
                        ElementId::Name(message),
                        Animation::new(MESSAGE_FADE).with_easing(ease_out_quint()),
                        |message, delta| message.opacity(delta),
                    )
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
                        .text_color(palette.text)
                        .font_weight(FontWeight::MEDIUM)
                        .child("Format")
                        .child(
                            div()
                                .text_color(palette.muted)
                                .child(editor::by_platform!("⌘⇧F", "Ctrl+Shift+F")),
                        )
                        .map(|button| {
                            if available {
                                button
                                    .hover(|style| style.bg(palette.hover))
                                    .active(|style| style.opacity(0.8))
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

    #[test]
    fn success_returns_stdout() {
        let (bin, command) = fake(
            r#"pwd -P > "$(dirname "$0")/cwd"; printf '%s\n' "$@" > "$(dirname "$0")/args"; tr a-z A-Z"#,
        );
        let draft = tempfile::tempdir().unwrap();
        let config = empty_config().unwrap();
        let output = process::run(
            "oxfmt",
            oxfmt_command(command, draft.path(), config.path()),
            Some("* a\n"),
            TIMEOUT,
            None,
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

    /// Needs oxfmt on `PATH`: `cargo test -j 2 -p draftmux --bin draftmux real_oxfmt -- --ignored`.
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
        let output = process::run(
            "oxfmt",
            oxfmt_command(Command::new("oxfmt"), &draft, config.path()),
            Some(input),
            TIMEOUT,
            None,
        )
        .unwrap();
        assert_eq!(
            output,
            "- a\n- b\n\nA long line that stays\nbroken where it is.\n\n```js\nconst x = 1;\n```\n"
        );
    }
}
