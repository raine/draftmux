//! Creating a draft's session by forking the source pi session, once, at
//! handoff.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use draftmux_storage::Target;
use serde_json::Value;

use crate::agent::{DraftProcess, draft_process};
use crate::connection::{Connection, Message, STOP_TIMEOUT};
use crate::protocol::{self, Incoming, State, ThinkingLevel};

/// The fork of one handoff.
pub struct ForkSpec {
    /// The source pi session file. pi only reads it, apart from adding a
    /// missing final newline.
    pub source: PathBuf,
    /// The draft's session dir; pi writes the new session file into it.
    pub session_dir: PathBuf,
    /// The bundled `pi-extension/draftmux.ts`.
    pub extension: PathBuf,
    /// The source's working directory, which pi stamps into the new session.
    pub cwd: PathBuf,
    /// The draft's pi profile, set as `PI_CODING_AGENT_DIR`.
    pub agent_dir: PathBuf,
    pub draft_path: PathBuf,
    pub target: Target,
    /// Without one, the fork continues on the source session's model.
    pub model: Option<ModelOverride>,
    /// How long pi gets to start and answer.
    pub timeout: Duration,
}

/// A model and thinking level the draft starts on instead of the source's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelOverride {
    pub provider: String,
    pub model_id: String,
    pub thinking: ThinkingLevel,
}

/// A forked session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forked {
    pub session_file: PathBuf,
    /// Why the model override wasn't applied; the session continues on the
    /// source's model.
    pub warning: Option<String>,
}

/// Runs `<command> --mode rpc --fork <source> --session-dir <dir> -e <ext>`,
/// applies the model override with `set_model` and `set_thinking_level`, and
/// returns the new session file, which pi reports in `get_state` and is
/// directly in `spec.session_dir`. pi records the override in the session, so
/// later starts with `--session` keep it. pi has written the whole fork before
/// it answers `get_state`, so a refused override, such as a model the profile
/// lacks, is a warning rather than an error. pi has exited when this returns.
/// `command` is the base pi command, as a
/// [`CommandFactory`](crate::CommandFactory) builds it. Errors are messages
/// for the user.
pub fn fork(mut command: Command, spec: &ForkSpec) -> Result<Forked, String> {
    command
        .arg("--mode")
        .arg("rpc")
        .arg("--fork")
        .arg(&spec.source)
        .arg("--session-dir")
        .arg(&spec.session_dir);
    draft_process(
        &mut command,
        &DraftProcess {
            extension: &spec.extension,
            cwd: &spec.cwd,
            agent_dir: &spec.agent_dir,
            draft_path: &spec.draft_path,
            target: spec.target,
            instructions: None,
        },
    );
    let (tx, rx) = mpsc::channel();
    let conn = Connection::spawn(command, move |message| {
        let _ = tx.send(message);
    })
    .map_err(|err| format!("couldn't start pi: {err}"))?;
    let mut rpc = Rpc {
        conn,
        rx,
        next_id: 0,
        deadline: Instant::now() + spec.timeout,
        timeout: spec.timeout,
        exited: false,
        dialog: None,
    };
    let result = rpc.run(spec);
    rpc.close();
    result
}

struct Rpc {
    conn: Connection,
    rx: mpsc::Receiver<Message>,
    next_id: u64,
    deadline: Instant,
    timeout: Duration,
    exited: bool,
    /// The last extension dialog cancelled. pi 1.5.0 reads no commands until
    /// startup finishes, so a dialog shown during startup can't be answered
    /// and pi never replies.
    dialog: Option<String>,
}

impl Rpc {
    fn run(&mut self, spec: &ForkSpec) -> Result<Forked, String> {
        let state: State = serde_json::from_value(self.request(&protocol::Command::GetState)?)
            .map_err(|err| format!("unexpected get_state answer from pi: {err}"))?;
        let file = match state.session_file {
            Some(file) if file.parent() == Some(&spec.session_dir) && file.is_file() => file,
            other => {
                return Err(format!(
                    "pi reported session file {} instead of a file in {}",
                    other.map_or("none".into(), |file| file.display().to_string()),
                    spec.session_dir.display()
                ));
            }
        };
        let warning = spec.model.as_ref().and_then(|model| {
            self.request(&protocol::Command::SetModel {
                provider: model.provider.clone(),
                model_id: model.model_id.clone(),
            })
            .and_then(|_| {
                self.request(&protocol::Command::SetThinkingLevel {
                    level: model.thinking,
                })
            })
            .err()
            .map(|err| {
                format!(
                    "couldn't switch to {}/{}, so the draft continues on the session's model: {err}",
                    model.provider, model.model_id
                )
            })
        });
        Ok(Forked {
            session_file: file,
            warning,
        })
    }

    /// Sends `command` and waits for its answer's data.
    fn request(&mut self, command: &protocol::Command) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        // A write fails when pi has exited; the exit is reported below.
        let _ = self.conn.send(&command.to_line(&id));
        loop {
            let wait = self.deadline.saturating_duration_since(Instant::now());
            let message = match self.rx.recv_timeout(wait) {
                Ok(message) => message,
                Err(_) => {
                    let mut error =
                        format!("pi didn't answer within {} s", self.timeout.as_secs_f32());
                    if let Some(title) = &self.dialog {
                        error.push_str(&format!(
                            "; an extension showed a dialog while pi started (\"{title}\"), \
                             which Draftmux can't answer"
                        ));
                    }
                    return Err(error);
                }
            };
            match message {
                Message::Line(Incoming::Response(response))
                    if response.id.as_ref() == Some(&id) =>
                {
                    return if response.success {
                        Ok(response.data.unwrap_or(Value::Null))
                    } else {
                        Err(format!(
                            "pi rejected {}: {}",
                            response.command,
                            response.error.unwrap_or_default()
                        ))
                    };
                }
                Message::Line(Incoming::Dialog { id, title, .. }) => {
                    let _ = self.conn.send(&protocol::cancel_dialog_line(&id));
                    self.dialog = Some(title);
                }
                Message::Line(_) => {}
                Message::Exited { status, stderr } => {
                    self.exited = true;
                    let mut error = format!("pi exited before answering ({status})");
                    if !stderr.is_empty() {
                        error.push_str(": ");
                        error.push_str(&stderr);
                    }
                    return Err(error);
                }
            }
        }
    }

    /// Stops pi and waits for it to exit.
    fn close(self) {
        let Rpc {
            conn,
            rx,
            mut exited,
            ..
        } = self;
        conn.stop();
        let deadline = Instant::now() + STOP_TIMEOUT + Duration::from_secs(1);
        while !exited {
            let wait = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Message::Exited { .. }) => exited = true,
                Ok(Message::Line(_)) => {}
                Err(_) => {
                    eprintln!("pi-rpc: pi didn't exit after the fork");
                    return;
                }
            }
        }
    }
}
