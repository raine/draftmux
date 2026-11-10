//! The pi process of one draft: started on demand, stopped when idle or
//! switched away from, and replaced after a crash. Requests pi doesn't
//! answer in time fail, so a stuck pi still goes idle.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::{mpsc, oneshot};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::connection::{Connection, Message};
use crate::protocol::{
    self, Disposition, Event, Incoming, MessagesData, Model, ModelsData, PromptData, Response,
    State, ThinkingLevel,
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
    pub instructions: Option<String>,
    /// Off runs pi with `--no-context-files`.
    pub context_files: bool,
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
            .arg(&self.session_file);
        if !self.context_files {
            command.arg("--no-context-files");
        }
        draft_process(
            &mut command,
            &DraftProcess {
                extension: &self.extension,
                cwd: &self.cwd,
                agent_dir: &self.agent_dir,
                draft_path: &self.draft_path,
                instructions: self.instructions.as_deref(),
            },
        );
        command
    }
}

/// What a pi process running in draft mode is told about its draft.
pub(crate) struct DraftProcess<'a> {
    pub extension: &'a Path,
    pub cwd: &'a Path,
    pub agent_dir: &'a Path,
    pub draft_path: &'a Path,
    pub instructions: Option<&'a str>,
}

/// Adds `-e <ext>`, the cwd, the profile and the extension's environment.
pub(crate) fn draft_process(command: &mut Command, draft: &DraftProcess) {
    command
        .arg("-e")
        .arg(draft.extension)
        .current_dir(draft.cwd)
        .env("PI_CODING_AGENT_DIR", draft.agent_dir)
        .env("DRAFTMUX_MODE", "draft")
        .env("DRAFTMUX_DRAFT_PATH", draft.draft_path);
    // The captured environment can carry values from a pi session the
    // app was launched from.
    match draft.instructions {
        Some(instructions) => command.env("DRAFTMUX_INSTRUCTIONS", instructions),
        None => command.env_remove("DRAFTMUX_INSTRUCTIONS"),
    };
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
    /// pi didn't answer within the request timeout. It may still act on the
    /// request.
    Timeout(Duration),
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
            Error::Timeout(timeout) => {
                write!(f, "pi didn't answer within {} s", timeout.as_secs())
            }
            Error::Protocol(err) => write!(f, "unexpected answer from pi: {err}"),
        }
    }
}

impl std::error::Error for Error {}

type Reply = Result<Option<Value>, Error>;

/// One draft's pi process.
///
/// No call blocks on pi: requests are queued to a writer thread, and a
/// timer thread per process fails requests pi doesn't answer in time and
/// stops pi once it has been idle for the idle timeout. Contract for the
/// app:
///
/// - Call [`Agent::focus`] when the draft is selected and [`Agent::blur`]
///   when switching away. Requests start pi when it isn't running.
/// - Flush the autosave before [`Agent::prompt`] so pi reads the editor
///   text.
/// - Dropping the agent stops pi.
pub struct Agent {
    shared: Arc<Shared>,
}

struct Shared {
    spec: LaunchSpec,
    idle_timeout: Duration,
    request_timeout: Duration,
    events: mpsc::UnboundedSender<AgentEvent>,
    inner: Mutex<Inner>,
    /// Wakes the timer thread after `inner` changed.
    changed: Condvar,
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
    /// The startup `get_state` after its deadline passed. It no longer keeps
    /// pi from going idle, but a late answer is still reported.
    late_startup: Option<String>,
    /// Between `agent_start` and `agent_settled`, including runs that
    /// extensions started.
    running: bool,
    /// Between `compaction_start` and `compaction_end`.
    compacting: bool,
    turn: Option<Turn>,
    stop_when_quiescent: bool,
    last_activity: Instant,
}

struct Pending {
    waiter: Waiter,
    /// When the request fails with [`Error::Timeout`].
    due: Instant,
}

enum Waiter {
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

    /// Queues `command`; with a `waiter`, its answer is awaited until
    /// `timeout` passes.
    fn send(
        &mut self,
        command: &protocol::Command,
        waiter: Option<(Waiter, Duration)>,
    ) -> Result<String, Error> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.conn
            .send(command.to_line(&id))
            .map_err(|err| Error::Write(err.to_string()))?;
        self.last_activity = Instant::now();
        if let Some((waiter, timeout)) = waiter {
            let due = self.last_activity + timeout;
            self.pending.insert(id.clone(), Pending { waiter, due });
        }
        Ok(id)
    }

    fn fail_pending(&mut self, error: Error) {
        for (_, pending) in self.pending.drain() {
            if let Waiter::Caller(tx) = pending.waiter {
                let _ = tx.send(Err(error.clone()));
            }
        }
    }

    /// Fails the requests whose deadline has passed. A prompt that fails
    /// ends its turn, so pi can go idle. Returns whether to stop pi: the user
    /// asked to abort that prompt, and pi ignores an abort until the run
    /// starts, which the ended turn no longer waits for.
    fn expire(&mut self, now: Instant, timeout: Duration) -> bool {
        if self.working() {
            return false;
        }
        let mut stop = false;
        for (id, pending) in self.pending.extract_if(|_, pending| pending.due <= now) {
            match pending.waiter {
                Waiter::Caller(tx) => {
                    let _ = tx.send(Err(Error::Timeout(timeout)));
                }
                Waiter::Startup => {
                    eprintln!(
                        "pi-rpc: pi didn't answer the startup get_state within {} s",
                        timeout.as_secs()
                    );
                    self.late_startup = Some(id.clone());
                }
            }
            if let Some(turn) = self.turn.take_if(|turn| turn.id == id) {
                stop |= turn.abort_requested;
            }
        }
        stop
    }

    /// Whether pi shows it is working: deadlines are suspended meanwhile.
    /// pi compacts before it answers a prompt, and a `/command` can run the
    /// agent before it answers; pi can't go idle during either anyway.
    fn working(&self) -> bool {
        self.running || self.compacting
    }

    /// Restarts the deadlines once pi stops working.
    fn rearm(&mut self, timeout: Duration) {
        if self.working() {
            return;
        }
        let due = Instant::now() + timeout;
        for pending in self.pending.values_mut() {
            pending.due = pending.due.max(due);
        }
    }

    /// When the timer has something to do: the earliest request deadline,
    /// or going idle.
    fn wake_at(&self, idle_timeout: Duration) -> Option<Instant> {
        let due = (!self.working())
            .then(|| self.pending.values().map(|pending| pending.due).min())
            .flatten();
        let idle = self.quiescent().then(|| self.last_activity + idle_timeout);
        due.into_iter().chain(idle).min()
    }
}

impl Agent {
    /// Nothing starts until [`Agent::focus`] or a request. Events from every
    /// process the agent starts arrive on the returned receiver.
    /// Requests pi doesn't answer within `request_timeout` fail with
    /// [`Error::Timeout`].
    pub fn new(
        spec: LaunchSpec,
        idle_timeout: Duration,
        request_timeout: Duration,
    ) -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        let (events, rx) = mpsc::unbounded();
        let shared = Arc::new(Shared {
            spec,
            idle_timeout,
            request_timeout,
            events,
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
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
                self.shared.stop(&mut inner);
            }
        }
    }

    /// Stops pi now, abandoning any turn in flight.
    pub fn stop(&self) {
        self.shared.stop(&mut self.shared.lock());
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
            let command = protocol::Command::Prompt {
                message: text.clone(),
            };
            let (id, rx) = self.shared.call(live, &command)?;
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
                    Some(
                        self.shared
                            .call(live, &protocol::Command::Abort)
                            .map(|(_, rx)| rx),
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
        let rx = self.shared.request(protocol::Command::GetState);
        decode(rx)
    }

    /// Switches the session's model. pi records it in the session, not in
    /// its settings.
    pub fn set_model(
        &self,
        provider: String,
        model_id: String,
    ) -> impl Future<Output = Result<Model, Error>> + Send + use<> {
        let rx = self
            .shared
            .request(protocol::Command::SetModel { provider, model_id });
        decode(rx)
    }

    /// Switches the session's model, then reads back what pi resolved. pi
    /// records the switch in the session, so it survives respawns, and may
    /// change the thinking level with it; show the returned state rather
    /// than the requested model. pi handles requests concurrently, so the
    /// state is only asked for once the switch is done.
    ///
    /// The state is read from the process that made the switch. If that
    /// process stopped in between (a blur, a crash, the agent dropped), this
    /// is [`Error::Stopped`], though the switch was recorded; the next
    /// process reports the model in [`AgentEvent::Ready`].
    pub fn switch_model(
        &self,
        provider: String,
        model_id: String,
    ) -> impl Future<Output = Result<State, Error>> + Send + use<> {
        let set = self.set_model(provider, model_id);
        let generation = self.shared.lock().generation;
        let shared = Arc::downgrade(&self.shared);
        async move {
            set.await?;
            let rx = match shared.upgrade() {
                Some(shared) => shared.request_from(generation, protocol::Command::GetState),
                None => Err(Error::Stopped),
            };
            decode(rx).await
        }
    }

    /// Sets the session's thinking level. pi clamps it to what the model
    /// supports; read the result from [`Agent::get_state`].
    pub fn set_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let rx = self
            .shared
            .request(protocol::Command::SetThinkingLevel { level });
        async move { reply(rx?).await.map(drop) }
    }

    /// The models of the draft's profile that have credentials.
    pub fn get_available_models(
        &self,
    ) -> impl Future<Output = Result<Vec<Model>, Error>> + Send + use<> {
        let rx = self.shared.request(protocol::Command::GetAvailableModels);
        async move { decode::<ModelsData>(rx).await.map(|data| data.models) }
    }

    /// The session's messages as pi's `AgentMessage` JSON, oldest first:
    /// what the model sees, so compacted history is a summary.
    pub fn get_messages(&self) -> impl Future<Output = Result<Vec<Value>, Error>> + Send + use<> {
        let rx = self.shared.request(protocol::Command::GetMessages);
        async move { decode::<MessagesData>(rx).await.map(|data| data.messages) }
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

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn stop(&self, inner: &mut Inner) {
        if let Some(mut live) = inner.live.take() {
            live.fail_pending(Error::Stopped);
            live.conn.stop();
            // Lets the timer thread exit.
            self.changed.notify_all();
        }
    }

    /// Sends `command` and awaits its answer until the request timeout.
    fn call(
        &self,
        live: &mut Live,
        command: &protocol::Command,
    ) -> Result<(String, oneshot::Receiver<Reply>), Error> {
        let (tx, rx) = oneshot::channel();
        let id = live.send(command, Some((Waiter::Caller(tx), self.request_timeout)))?;
        // The new deadline may be earlier than the one the timer waits for.
        self.changed.notify_all();
        Ok((id, rx))
    }

    fn request(
        self: &Arc<Self>,
        command: protocol::Command,
    ) -> Result<oneshot::Receiver<Reply>, Error> {
        let mut inner = self.lock();
        let live = self.ensure_live(&mut inner)?;
        self.call(live, &command).map(|(_, rx)| rx)
    }

    /// Sends `command` to process `generation` only, without starting one.
    fn request_from(
        &self,
        generation: u64,
        command: protocol::Command,
    ) -> Result<oneshot::Receiver<Reply>, Error> {
        let mut inner = self.lock();
        if inner.generation != generation {
            return Err(Error::Stopped);
        }
        let live = inner.live.as_mut().ok_or(Error::Stopped)?;
        self.call(live, &command).map(|(_, rx)| rx)
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
                late_startup: None,
                running: false,
                compacting: false,
                turn: None,
                stop_when_quiescent: false,
                last_activity: Instant::now(),
            };
            let startup = (Waiter::Startup, self.request_timeout);
            if let Err(err) = live.send(&protocol::Command::GetState, Some(startup)) {
                live.conn.stop();
                return Err(err);
            }
            let shared = self.clone();
            if let Err(err) = thread::Builder::new()
                .name("pi-timer".into())
                .spawn(move || shared.run_timer(generation))
            {
                live.conn.stop();
                return Err(Error::Spawn(err.to_string()));
            }
            inner.live = Some(live);
        }
        Ok(inner.live.as_mut().expect("just started"))
    }

    /// Expires requests and stops pi when it goes idle, until process
    /// `generation` stops.
    fn run_timer(&self, generation: u64) {
        let mut inner = self.lock();
        loop {
            if inner.generation != generation {
                return;
            }
            let Some(live) = &mut inner.live else {
                return;
            };
            let now = Instant::now();
            let abandoned = live.expire(now, self.request_timeout);
            let idle = live.quiescent()
                && (live.stop_when_quiescent || live.last_activity + self.idle_timeout <= now);
            let wake = live.wake_at(self.idle_timeout);
            if abandoned || idle {
                self.stop(&mut inner);
                return;
            }
            inner = match wake {
                Some(at) => {
                    let wait = at.saturating_duration_since(now);
                    self.changed.wait_timeout(inner, wait).unwrap().0
                }
                None => self.changed.wait(inner).unwrap(),
            };
        }
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
    // Takes effect once the lock is released.
    shared.changed.notify_all();
    live.last_activity = Instant::now();
    match message {
        Message::Line(Incoming::Response(response)) => {
            if let Some(state) = on_response(live, response) {
                shared.emit(AgentEvent::Ready(state));
            }
        }
        Message::Line(Incoming::Dialog { id, method, title }) => {
            if let Err(err) = live.conn.send(protocol::cancel_dialog_line(&id)) {
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
                    live.rearm(shared.request_timeout);
                }
                Event::CompactionStart { .. } => live.compacting = true,
                Event::CompactionEnd { .. } => {
                    live.compacting = false;
                    live.rearm(shared.request_timeout);
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
        shared.stop(&mut inner);
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
    let waiter = live.pending.remove(&id).map(|pending| pending.waiter);
    let waiter = waiter.or_else(|| {
        live.late_startup
            .take_if(|startup| *startup == id)
            .map(|_| Waiter::Startup)
    });
    match waiter {
        Some(Waiter::Caller(tx)) => {
            let _ = tx.send(reply);
        }
        Some(Waiter::Startup) => {
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
