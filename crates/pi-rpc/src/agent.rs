//! The pi process of one draft: started on demand, stopped when idle or
//! switched away from, and replaced after a crash.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use draftmux_storage::Target;
use futures::channel::{mpsc, oneshot};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::connection::{Connection, Message};
use crate::protocol::{
    self, Disposition, Event, Incoming, Model, ModelsData, PromptData, Response, State,
    ThinkingLevel,
};

/// Builds the base command for the pi executable. The app passes
/// `move || shell_env.command(pi)` so pi runs with the login-shell
/// environment; the agent adds arguments, cwd and the variables below.
pub type CommandFactory = Arc<dyn Fn() -> Command + Send + Sync>;

/// Everything needed to start a draft's pi process.
pub struct LaunchSpec {
    pub command: CommandFactory,
    /// The draft's pi session file (`Meta::pi_session_file`).
    pub session_file: PathBuf,
    /// The bundled `pi-extension/draftmux.ts`.
    pub extension: PathBuf,
    /// The draft's working directory.
    pub cwd: PathBuf,
    /// The draft's pi profile, set as `PI_CODING_AGENT_DIR`.
    pub agent_dir: PathBuf,
    pub draft_path: PathBuf,
    pub target: Target,
    pub instructions: Option<String>,
}

impl LaunchSpec {
    /// `pi --mode rpc --session <file> -e <ext>` with the environment the
    /// bundled extension reads (see `pi-extension/README.md`).
    fn command(&self) -> Command {
        let mut command = (self.command)();
        command
            .arg("--mode")
            .arg("rpc")
            .arg("--session")
            .arg(&self.session_file)
            .arg("-e")
            .arg(&self.extension)
            .current_dir(&self.cwd)
            .env("PI_CODING_AGENT_DIR", &self.agent_dir)
            .env("DRAFTMUX_MODE", "draft")
            .env("DRAFTMUX_DRAFT_PATH", &self.draft_path)
            .env(
                "DRAFTMUX_TARGET",
                match self.target {
                    Target::Github => "github",
                    Target::Slack => "slack",
                },
            );
        // The captured environment can carry values from a pi session the
        // app was launched from.
        match &self.instructions {
            Some(instructions) => command.env("DRAFTMUX_INSTRUCTIONS", instructions),
            None => command.env_remove("DRAFTMUX_INSTRUCTIONS"),
        };
        command
    }
}

/// What the agent reports to the chat, in order, across process restarts.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Pi(Event),
    /// A new pi process answered its first `get_state`: what it resolved
    /// for the session. From here the idle deadline can apply.
    Ready(State),
    /// pi exited without being asked to. `failed_turn` is the prompt whose
    /// run it interrupted; show it as an error with a retry that sends it
    /// again with [`Agent::prompt`]. The next call starts a new process.
    /// pi may already have saved the interrupted prompt, so a retry can
    /// leave it twice in the session.
    Crashed {
        error: String,
        failed_turn: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A prompt is already in flight or the agent is running.
    Busy,
    /// pi answered with an error, for example a missing API key.
    Rejected(String),
    /// pi exited before answering.
    Exited,
    /// The process was stopped before answering.
    Stopped,
    /// pi couldn't be started.
    Spawn(String),
    /// Writing to pi failed.
    Write(String),
    /// pi's answer didn't have the expected shape.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Busy => write!(f, "the agent is busy"),
            Error::Rejected(err) => write!(f, "{err}"),
            Error::Exited => write!(f, "pi exited before answering"),
            Error::Stopped => write!(f, "pi was stopped before answering"),
            Error::Spawn(err) => write!(f, "couldn't start pi: {err}"),
            Error::Write(err) => write!(f, "couldn't write to pi: {err}"),
            Error::Protocol(err) => write!(f, "unexpected answer from pi: {err}"),
        }
    }
}

impl std::error::Error for Error {}

type Reply = Result<Option<Value>, Error>;

/// One draft's pi process.
///
/// Holds no timer. Contract for the app:
///
/// - Call [`Agent::focus`] when the draft is selected and [`Agent::blur`]
///   when switching away. Requests start pi when it isn't running.
/// - After each call and each event, schedule a wake-up for
///   [`Agent::deadline`] and call [`Agent::poll`] then; that stops an idle
///   process.
/// - Flush the autosave before [`Agent::prompt`] so pi reads the editor
///   text.
/// - Dropping the agent stops pi.
pub struct Agent {
    shared: Arc<Shared>,
}

struct Shared {
    spec: LaunchSpec,
    idle_timeout: Duration,
    events: mpsc::UnboundedSender<AgentEvent>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Increments per process, so a replaced process's late output is
    /// ignored.
    generation: u64,
    live: Option<Live>,
}

struct Live {
    conn: Connection,
    next_id: u64,
    pending: HashMap<String, Pending>,
    /// Between `agent_start` and `agent_settled`, including runs that
    /// extensions started.
    running: bool,
    turn: Option<Turn>,
    stop_when_quiescent: bool,
    last_activity: Instant,
}

enum Pending {
    Caller(oneshot::Sender<Reply>),
    /// The `get_state` sent at start, which tells whether pi is already
    /// running (an extension can start a run before events are subscribed).
    Startup,
}

/// A prompt the app sent. It is in flight until pi rejects it, reports it
/// handled, or the run it started settles.
struct Turn {
    id: String,
    text: String,
    started: bool,
    /// pi ignores an abort that arrives before its run exists, so it is sent
    /// again on `agent_start`.
    abort_requested: bool,
}

impl Live {
    fn quiescent(&self) -> bool {
        !self.running && self.turn.is_none() && self.pending.is_empty()
    }

    fn send(
        &mut self,
        command: &protocol::Command,
        pending: Option<Pending>,
    ) -> Result<String, Error> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.conn
            .send(&command.to_line(&id))
            .map_err(|err| Error::Write(err.to_string()))?;
        if let Some(pending) = pending {
            self.pending.insert(id.clone(), pending);
        }
        self.last_activity = Instant::now();
        Ok(id)
    }

    fn fail_pending(&mut self, error: Error) {
        for (_, pending) in self.pending.drain() {
            if let Pending::Caller(tx) = pending {
                let _ = tx.send(Err(error.clone()));
            }
        }
    }
}

impl Agent {
    /// Nothing starts until [`Agent::focus`] or a request. Events from every
    /// process the agent starts arrive on the returned receiver.
    pub fn new(
        spec: LaunchSpec,
        idle_timeout: Duration,
    ) -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        let (events, rx) = mpsc::unbounded();
        let shared = Arc::new(Shared {
            spec,
            idle_timeout,
            events,
            inner: Mutex::new(Inner::default()),
        });
        (Self { shared }, rx)
    }

    /// The draft was selected: start pi if it isn't running.
    pub fn focus(&self) -> Result<(), Error> {
        let mut inner = self.shared.lock();
        let live = self.shared.ensure_live(&mut inner)?;
        live.stop_when_quiescent = false;
        Ok(())
    }

    /// The app switched to another draft: stop pi now if it is idle,
    /// otherwise once the current turn settles.
    pub fn blur(&self) {
        let mut inner = self.shared.lock();
        if let Some(live) = &mut inner.live {
            live.stop_when_quiescent = true;
            if live.quiescent() {
                stop(&mut inner);
            }
        }
    }

    /// Stops pi now, abandoning any turn in flight.
    pub fn stop(&self) {
        stop(&mut self.shared.lock());
    }

    /// When pi should be stopped for inactivity; `None` when it isn't
    /// running or is busy.
    pub fn deadline(&self) -> Option<Instant> {
        self.shared.deadline(&self.shared.lock())
    }

    /// Stops pi if the idle deadline has passed. Returns whether it stopped.
    pub fn poll(&self, now: Instant) -> bool {
        let mut inner = self.shared.lock();
        let due = self.shared.deadline(&inner).is_some_and(|due| due <= now);
        if due {
            stop(&mut inner);
        }
        due
    }

    /// Sends a user message. Resolves once pi has accepted or rejected it;
    /// the run's progress arrives as events.
    pub fn prompt(
        &self,
        text: String,
    ) -> impl Future<Output = Result<Disposition, Error>> + Send + use<> {
        let rx = (|| {
            let mut inner = self.shared.lock();
            let live = self.shared.ensure_live(&mut inner)?;
            if live.running || live.turn.is_some() {
                return Err(Error::Busy);
            }
            let (tx, rx) = oneshot::channel();
            let command = protocol::Command::Prompt {
                message: text.clone(),
            };
            let id = live.send(&command, Some(Pending::Caller(tx)))?;
            live.turn = Some(Turn {
                id,
                text,
                started: false,
                abort_requested: false,
            });
            Ok(rx)
        })();
        async move { decode::<PromptData>(rx).await.map(|data| data.disposition) }
    }

    /// Aborts the current run. Does nothing when pi isn't running.
    pub fn abort(&self) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let rx = {
            let mut inner = self.shared.lock();
            match &mut inner.live {
                None => None,
                Some(live) => {
                    if let Some(turn) = &mut live.turn {
                        turn.abort_requested = true;
                    }
                    let (tx, rx) = oneshot::channel();
                    Some(
                        live.send(&protocol::Command::Abort, Some(Pending::Caller(tx)))
                            .map(|_| rx),
                    )
                }
            }
        };
        async move {
            match rx {
                None => Ok(()),
                Some(rx) => reply(rx?).await.map(drop),
            }
        }
    }

    /// The model, thinking level and session pi resolved.
    pub fn get_state(&self) -> impl Future<Output = Result<State, Error>> + Send + use<> {
        let rx = self.request(protocol::Command::GetState);
        decode(rx)
    }

    /// Switches the session's model. pi records it in the session, not in
    /// its settings.
    pub fn set_model(
        &self,
        provider: String,
        model_id: String,
    ) -> impl Future<Output = Result<Model, Error>> + Send + use<> {
        let rx = self.request(protocol::Command::SetModel { provider, model_id });
        decode(rx)
    }

    /// Sets the session's thinking level. pi clamps it to what the model
    /// supports; read the result from [`Agent::get_state`].
    pub fn set_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let rx = self.request(protocol::Command::SetThinkingLevel { level });
        async move { reply(rx?).await.map(drop) }
    }

    /// The models of the draft's profile that have credentials.
    pub fn get_available_models(
        &self,
    ) -> impl Future<Output = Result<Vec<Model>, Error>> + Send + use<> {
        let rx = self.request(protocol::Command::GetAvailableModels);
        async move { decode::<ModelsData>(rx).await.map(|data| data.models) }
    }

    fn request(&self, command: protocol::Command) -> Result<oneshot::Receiver<Reply>, Error> {
        let mut inner = self.shared.lock();
        let live = self.shared.ensure_live(&mut inner)?;
        let (tx, rx) = oneshot::channel();
        live.send(&command, Some(Pending::Caller(tx)))?;
        Ok(rx)
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn reply(rx: oneshot::Receiver<Reply>) -> Reply {
    rx.await.unwrap_or(Err(Error::Stopped))
}

async fn decode<T: DeserializeOwned>(
    rx: Result<oneshot::Receiver<Reply>, Error>,
) -> Result<T, Error> {
    let data = reply(rx?).await?.unwrap_or(Value::Null);
    serde_json::from_value(data).map_err(|err| Error::Protocol(err.to_string()))
}

fn stop(inner: &mut Inner) {
    if let Some(mut live) = inner.live.take() {
        live.fail_pending(Error::Stopped);
        live.conn.stop();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn deadline(&self, inner: &Inner) -> Option<Instant> {
        let live = inner.live.as_ref()?;
        live.quiescent()
            .then(|| live.last_activity + self.idle_timeout)
    }

    fn ensure_live<'a>(self: &Arc<Self>, inner: &'a mut Inner) -> Result<&'a mut Live, Error> {
        if inner.live.is_none() {
            inner.generation += 1;
            let generation = inner.generation;
            let weak = Arc::downgrade(self);
            let conn = Connection::spawn(self.spec.command(), move |message| {
                on_message(&weak, generation, message)
            })
            .map_err(|err| Error::Spawn(err.to_string()))?;
            let mut live = Live {
                conn,
                next_id: 0,
                pending: HashMap::new(),
                running: false,
                turn: None,
                stop_when_quiescent: false,
                last_activity: Instant::now(),
            };
            if let Err(err) = live.send(&protocol::Command::GetState, Some(Pending::Startup)) {
                live.conn.stop();
                return Err(err);
            }
            inner.live = Some(live);
        }
        Ok(inner.live.as_mut().expect("just started"))
    }

    fn emit(&self, event: AgentEvent) {
        // The receiver may be gone while the app tears down.
        let _ = self.events.unbounded_send(event);
    }
}

fn on_message(shared: &Weak<Shared>, generation: u64, message: Message) {
    let Some(shared) = shared.upgrade() else {
        return;
    };
    let mut inner = shared.lock();
    if inner.generation != generation {
        return;
    }
    let Some(live) = &mut inner.live else {
        return;
    };
    live.last_activity = Instant::now();
    match message {
        Message::Line(Incoming::Response(response)) => {
            if let Some(state) = on_response(live, response) {
                shared.emit(AgentEvent::Ready(state));
            }
        }
        Message::Line(Incoming::Dialog { id, method, title }) => {
            if let Err(err) = live.conn.send(&protocol::cancel_dialog_line(&id)) {
                eprintln!("pi-rpc: couldn't cancel an extension dialog: {err}");
            }
            shared.emit(AgentEvent::Pi(Event::DialogCancelled { method, title }));
        }
        Message::Line(Incoming::Event(event)) => {
            match &event {
                Event::AgentStart => {
                    live.running = true;
                    if live.turn.as_ref().is_some_and(|turn| turn.abort_requested)
                        && let Err(err) = live.send(&protocol::Command::Abort, None)
                    {
                        eprintln!("pi-rpc: couldn't resend abort: {err}");
                    }
                }
                Event::AgentSettled { .. } => {
                    live.running = false;
                    if live.turn.as_ref().is_some_and(|turn| turn.started) {
                        live.turn = None;
                    }
                }
                _ => {}
            }
            shared.emit(AgentEvent::Pi(event));
        }
        Message::Exited { status, stderr } => {
            let mut live = inner.live.take().expect("checked above");
            live.fail_pending(Error::Exited);
            let mut error = format!("pi exited unexpectedly ({status})");
            if !stderr.is_empty() {
                error.push_str(": ");
                error.push_str(&stderr);
            }
            shared.emit(AgentEvent::Crashed {
                error,
                failed_turn: live.turn.map(|turn| turn.text),
            });
            return;
        }
    }
    if live.stop_when_quiescent && live.quiescent() {
        stop(&mut inner);
    }
}

/// Returns the state from the startup `get_state`.
fn on_response(live: &mut Live, response: Response) -> Option<State> {
    let Some(id) = response.id else {
        eprintln!(
            "pi-rpc: pi answered `{}` without an id: {}",
            response.command,
            response.error.unwrap_or_default()
        );
        return None;
    };
    let reply = if response.success {
        Ok(response.data)
    } else {
        Err(Error::Rejected(response.error.unwrap_or_default()))
    };
    if let Some(turn) = &mut live.turn
        && turn.id == id
    {
        let disposition = reply.as_ref().ok().and_then(|data| {
            serde_json::from_value::<PromptData>(data.clone()?)
                .ok()
                .map(|data| data.disposition)
        });
        match disposition {
            Some(Disposition::Started | Disposition::Queued) => turn.started = true,
            Some(Disposition::Handled) | None => live.turn = None,
        }
    }
    match live.pending.remove(&id) {
        Some(Pending::Caller(tx)) => {
            let _ = tx.send(reply);
        }
        Some(Pending::Startup) => {
            match reply.and_then(|data| {
                serde_json::from_value::<State>(data.unwrap_or_default())
                    .map_err(|err| Error::Protocol(err.to_string()))
            }) {
                Ok(state) => {
                    live.running = state.is_streaming;
                    return Some(state);
                }
                Err(err) => eprintln!("pi-rpc: the startup get_state failed: {err}"),
            }
        }
        None => {}
    }
    None
}
