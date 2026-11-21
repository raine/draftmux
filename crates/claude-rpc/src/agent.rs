//! The `claude` process of one draft: started on demand, stopped when idle
//! or switched away from, and replaced after a crash. Control requests
//! claude doesn't answer in time fail, so a stuck process still goes idle.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::{mpsc, oneshot};
use serde_json::{Value, json};

use draftmux_agent::History;

use crate::connection::{Closed, Connection, EXIT_BOUND, Exit, Message, Ticket};
use crate::launch::{LaunchSpec, Session, Tool, home};
use crate::protocol::{self, Init};
use crate::session::{self, DraftSession, Missing, Start};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// How long a process with nothing in flight lives on.
    pub idle_timeout: Duration,
    /// How long claude gets to answer a control request, and how long
    /// `idle` may lag a run's `result` before the run is taken as settled.
    pub request_timeout: Duration,
    /// The longest stdout record accepted. A longer one ends the process.
    pub max_record: usize,
}

/// What the agent reports, in order, across process restarts.
#[derive(Debug)]
pub enum AgentEvent {
    /// A new process answered `initialize`.
    Ready(Init),
    /// A run started: a prompt was sent, or claude started one itself.
    RunStarted,
    /// A conversation frame, unchanged: `assistant`, `user`,
    /// `stream_event`, `system`, `result` and whatever else claude prints,
    /// including a `result` that arrives outside a run, such as the error
    /// for a missing session before the process exits.
    Frame(Value),
    /// claude asks whether a tool may run. Unanswered requests are denied
    /// when dropped.
    Permission(PermissionRequest),
    /// A registered `PreToolUse` hook was called. Unanswered calls are
    /// denied when dropped.
    Hook(HookRequest),
    /// claude withdrew the permission or hook request with this id, for
    /// example on an interrupt. An answer to it isn't sent.
    CallbackCancelled { request_id: String },
    /// The run is over: claude reported `idle`. `aborted` says whether it
    /// was interrupted; `error` is what its failed `result` reported, such
    /// as a missing login, which only shows here, not at startup.
    RunSettled {
        aborted: bool,
        error: Option<String>,
    },
    /// claude exited without being asked to, or its output couldn't be
    /// read. `failed_prompt` is the prompt whose run it interrupted. The
    /// next request starts a new process.
    Crashed {
        error: String,
        failed_prompt: Option<String>,
    },
    /// A [`Session::Draft`]'s transcript is now at this path: in Claude's
    /// storage after the session's first run, then in the draft once
    /// adopted. Record it as the draft's
    /// [`DraftSession::transcript`], so a later start reports a lost
    /// transcript instead of starting over.
    Transcript(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A prompt is already in flight or a run is going.
    Busy,
    /// claude answered a control request with an error.
    Rejected(String),
    /// claude exited before answering.
    Exited,
    /// The process was stopped, or the prompt interrupted, before
    /// answering.
    Stopped,
    /// claude couldn't be started.
    Spawn(String),
    /// Writing to claude failed.
    Write(String),
    /// claude didn't answer within the request timeout. It may still act on
    /// the request.
    Timeout(Duration),
    /// The draft's session can't continue without this file.
    Missing(Missing),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Busy => write!(f, "the agent is busy"),
            Error::Rejected(err) => write!(f, "{err}"),
            Error::Exited => write!(f, "claude exited before answering"),
            Error::Stopped => write!(f, "claude was stopped before answering"),
            Error::Spawn(err) => write!(f, "couldn't start claude: {err}"),
            Error::Write(err) => write!(f, "couldn't write to claude: {err}"),
            Error::Timeout(timeout) => {
                write!(f, "claude didn't answer within {} s", timeout.as_secs())
            }
            Error::Missing(missing) => write!(f, "{missing}"),
        }
    }
}

impl std::error::Error for Error {}

type Reply = Result<(), Error>;

/// One draft's `claude` process.
///
/// No call blocks on claude: lines are queued to a writer thread, and a
/// timer thread per process fails control requests claude doesn't answer
/// in time and stops it once idle. The one wait is for a stopped process
/// to exit before a request starts another on the same session, usually
/// half a second and at most the stop escalation; [`Agent::focus`] waits
/// for it on a background thread. Contract for the app:
///
/// - Call [`Agent::focus`] when the draft is selected and [`Agent::blur`]
///   when switching away. Requests start claude when it isn't running.
/// - Flush the autosave before [`Agent::prompt`] so claude reads the editor
///   text.
/// - Dropping the agent stops claude, interrupting a run first.
pub struct Agent {
    shared: Arc<Shared>,
}

struct Shared {
    spec: LaunchSpec,
    config: Config,
    /// Where Claude keeps a [`Session::Draft`]'s transcript until adopted.
    storage: Option<PathBuf>,
    /// The draft session's recorded transcript, as last reported.
    recorded: Mutex<Option<PathBuf>>,
    events: mpsc::UnboundedSender<AgentEvent>,
    inner: Mutex<Inner>,
    /// Wakes the timer thread after `inner` changed.
    changed: Condvar,
}

struct Inner {
    /// Increments per process, so a replaced process's late output is
    /// ignored.
    generation: u64,
    /// The model and effort the next process starts with.
    chosen: Chosen,
    /// The tools the next process starts with.
    tools: Vec<Tool>,
    live: Option<Live>,
    /// The exit of the last process stopped. The next one waits for it, so
    /// two processes never write one session.
    previous: Option<Exit>,
    /// The draft is selected, so a process starts once `previous` exits.
    focused: bool,
}

/// What processes start with, as `--model` and `--effort` take it; `None`
/// leaves it to claude.
struct Chosen {
    model: Option<String>,
    effort: Option<String>,
}

struct Live {
    conn: Connection,
    generation: u64,
    next_id: u64,
    pending: HashMap<String, Pending>,
    initialized: bool,
    /// A prompt was written to this process.
    prompted: bool,
    /// Why the process can't be used anymore. It is being stopped, and its
    /// exit is reported as a crash.
    failed: Option<String>,
    turn: Option<Turn>,
    run: Option<Run>,
    /// Whether claude reported `session_state_changed`. Without it, a run
    /// ends at its `result`.
    reports_state: bool,
    /// Permission and hook requests claude is waiting on, by request id.
    callbacks: HashMap<String, Callback>,
    stop_when_quiescent: bool,
    /// The process created the draft session's transcript in Claude's
    /// storage, which isn't recorded until a run of it settles.
    unrecorded: bool,
    last_activity: Instant,
    /// When a run whose `result` arrived is taken as settled without
    /// `idle`.
    settle_due: Option<Instant>,
}

struct Pending {
    waiter: Waiter,
    /// When the request fails with [`Error::Timeout`].
    due: Instant,
}

enum Waiter {
    /// The `initialize` sent at start. Prompts wait for it.
    Initialize,
    Caller(oneshot::Sender<Reply>),
    /// `set_model`; the model applies to later processes once accepted.
    SetModel(oneshot::Sender<Reply>, String),
    /// `apply_flag_settings` with `effortLevel`; the effort applies to
    /// later processes once accepted.
    SetEffort(oneshot::Sender<Reply>, Option<String>),
    /// An interrupt the agent sent on its own.
    Ignore,
}

/// A prompt the app sent, until the run it started settles.
struct Turn {
    text: String,
    /// Answers [`Agent::prompt`] while the prompt waits for `initialize`;
    /// `None` once it was written.
    held: Option<oneshot::Sender<Result<(), Error>>>,
    /// Builds the prompt's context blocks when it is written.
    context: Option<PromptContext>,
}

/// Builds the text blocks sent ahead of a prompt, when it is written.
/// Called with whether the prompt is the process's first, under the
/// agent's lock, so it must not call into the [`Agent`].
pub(crate) type PromptContext = Box<dyn FnOnce(bool) -> Vec<String> + Send>;

#[derive(Default)]
struct Run {
    /// claude reported it running. claude reads stdin in order, so the
    /// `idle` of a run settled without it arrives before the next run's
    /// first `running`, and isn't this run's.
    observed: bool,
    interrupt_requested: bool,
    /// What the run's `result` reported.
    outcome: Option<Outcome>,
}

struct Outcome {
    aborted: bool,
    error: Option<String>,
}

struct Callback {
    ticket: Arc<Ticket>,
    answered: bool,
}

impl Live {
    fn quiescent(&self) -> bool {
        self.turn.is_none()
            && self.run.is_none()
            && self.pending.is_empty()
            && self.callbacks.values().all(|callback| callback.answered)
    }

    fn send(&mut self, line: String) -> Result<(), Error> {
        self.conn
            .send(line, None)
            .map_err(|err| Error::Write(err.to_string()))?;
        self.last_activity = Instant::now();
        Ok(())
    }

    /// Sends a control request whose answer goes to `waiter`.
    fn request(&mut self, request: Value, waiter: Waiter, timeout: Duration) -> Result<(), Error> {
        self.next_id += 1;
        let id = format!("draftmux_{}_{}", self.generation, self.next_id);
        self.send(protocol::control_request(&id, request))?;
        let due = self.last_activity + timeout;
        self.pending.insert(id, Pending { waiter, due });
        Ok(())
    }

    fn interrupt(&mut self, waiter: Waiter, timeout: Duration) -> Result<(), Error> {
        self.request(json!({"subtype": "interrupt"}), waiter, timeout)
    }

    /// Fails every request and a prompt still waiting for `initialize`.
    /// Answers already queued are still written before stdin closes.
    fn abandon(&mut self, error: Error) {
        for (_, pending) in self.pending.drain() {
            match pending.waiter {
                Waiter::Caller(tx) | Waiter::SetModel(tx, _) | Waiter::SetEffort(tx, _) => {
                    let _ = tx.send(Err(error.clone()));
                }
                Waiter::Initialize | Waiter::Ignore => {}
            }
        }
        self.fail_held(error);
        self.callbacks.clear();
    }

    fn fail_held(&mut self, error: Error) {
        if let Some(turn) = self.turn.take_if(|turn| turn.held.is_some()) {
            let _ = turn.held.expect("checked").send(Err(error));
        }
    }

    /// When the timer has something to do.
    fn wake_at(&self, idle_timeout: Duration) -> Option<Instant> {
        if self.failed.is_some() {
            return None;
        }
        let due = self.pending.values().map(|pending| pending.due).min();
        let idle = self.quiescent().then(|| self.last_activity + idle_timeout);
        due.into_iter().chain(idle).chain(self.settle_due).min()
    }
}

impl Agent {
    /// Nothing starts until [`Agent::focus`] or a request. Events from every
    /// process the agent starts arrive on the returned receiver.
    pub fn new(spec: LaunchSpec, config: Config) -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        let (events, rx) = mpsc::unbounded();
        let chosen = Chosen {
            model: spec.model.clone(),
            effort: spec.effort.clone(),
        };
        let tools = spec.tools.clone();
        let (storage, recorded) = match &spec.session {
            Some(Session::Draft(draft)) => {
                let home = home(spec.extra_env.iter().rev().chain(&spec.env));
                (Some(draft.storage(&home)), draft.transcript.clone())
            }
            _ => (None, None),
        };
        let shared = Arc::new(Shared {
            spec,
            config,
            storage,
            recorded: Mutex::new(recorded),
            events,
            inner: Mutex::new(Inner {
                generation: 0,
                chosen,
                tools,
                live: None,
                previous: None,
                focused: false,
            }),
            changed: Condvar::new(),
        });
        (Self { shared }, rx)
    }

    /// The draft was selected: start claude if it isn't running. While the
    /// previous process is still exiting, it starts on a background thread
    /// once that has exited, and a failed start shows on the next request.
    pub fn focus(&self) -> Result<(), Error> {
        let mut inner = self.shared.lock();
        inner.focused = true;
        if inner.live.is_none()
            && let Some(previous) = inner.previous.clone()
            && !previous.wait(Instant::now())
        {
            let shared = Arc::downgrade(&self.shared);
            thread::Builder::new()
                .name("claude-focus".into())
                .spawn(move || {
                    if !previous.wait(Instant::now() + EXIT_BOUND) {
                        return;
                    }
                    let Some(shared) = shared.upgrade() else {
                        return;
                    };
                    let mut inner = shared.lock();
                    if inner.focused
                        && let Err(err) = shared.focus(&mut inner)
                    {
                        eprintln!("claude-rpc: couldn't start claude: {err}");
                    }
                })
                .map_err(|err| Error::Spawn(err.to_string()))?;
            return Ok(());
        }
        self.shared.focus(&mut inner)
    }

    /// The app switched to another draft: stop claude now if nothing is in
    /// flight, otherwise once the run settles.
    pub fn blur(&self) {
        let mut inner = self.shared.lock();
        inner.focused = false;
        if let Some(live) = &mut inner.live {
            live.stop_when_quiescent = true;
        }
        self.shared.settled(&mut inner);
    }

    /// Stops claude now. A run in progress is interrupted first, since
    /// claude finishes a run after stdin closes.
    pub fn stop(&self) {
        let mut inner = self.shared.lock();
        inner.focused = false;
        self.shared.stop(&mut inner);
    }

    /// Sends a user message. Resolves once it is written to claude, which
    /// for a new process is after `initialize`; the run's progress arrives
    /// as events. Claude doesn't acknowledge prompts, so a missing login
    /// only shows in [`AgentEvent::RunSettled`].
    pub fn prompt(&self, text: String) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        self.prompt_with(text, None)
    }

    /// Like [`Agent::prompt`], with text blocks from `context` ahead of
    /// `text` in the user message. Crash reports and the conversation's
    /// retry offer `text` alone.
    pub(crate) fn prompt_with(
        &self,
        text: String,
        context: Option<PromptContext>,
    ) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let held = (|| {
            let mut inner = self.shared.lock();
            let live = self.shared.ensure_live(&mut inner)?;
            if live.turn.is_some() || live.run.is_some() {
                return Err(Error::Busy);
            }
            if live.initialized {
                self.shared.write_prompt(live, text, context)?;
                Ok(None)
            } else {
                let (tx, rx) = oneshot::channel();
                live.turn = Some(Turn {
                    text,
                    held: Some(tx),
                    context,
                });
                Ok(Some(rx))
            }
        })();
        async move {
            match held? {
                None => Ok(()),
                Some(rx) => rx.await.unwrap_or(Err(Error::Stopped)),
            }
        }
    }

    /// Interrupts the current run and resolves when claude acknowledges
    /// that. The acknowledgement comes before the run ends, which arrives
    /// as [`AgentEvent::RunSettled`]. A prompt still waiting for
    /// `initialize` is dropped instead and fails with [`Error::Stopped`].
    /// Does nothing when no run is going.
    pub fn interrupt(&self) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let rx = (|| {
            let mut inner = self.shared.lock();
            let Some(live) = inner.live.as_mut().filter(|live| live.failed.is_none()) else {
                return Ok(None);
            };
            if live.turn.as_ref().is_some_and(|turn| turn.held.is_some()) {
                live.fail_held(Error::Stopped);
                self.shared.settled(&mut inner);
                return Ok(None);
            }
            let Some(run) = &mut live.run else {
                return Ok(None);
            };
            run.interrupt_requested = true;
            let (tx, rx) = oneshot::channel();
            live.interrupt(Waiter::Caller(tx), self.shared.config.request_timeout)?;
            self.shared.changed.notify_all();
            Ok(Some(rx))
        })();
        async move {
            match rx? {
                None => Ok(()),
                Some(rx) => rx.await.unwrap_or(Err(Error::Stopped)),
            }
        }
    }

    /// The model processes start with, as `--model` takes it; `None` for
    /// the session's own.
    pub(crate) fn model(&self) -> Option<String> {
        self.shared.lock().chosen.model.clone()
    }

    /// The effort processes start with, as `--effort` takes it; `None` for
    /// the model's default.
    pub(crate) fn effort(&self) -> Option<String> {
        self.shared.lock().chosen.effort.clone()
    }

    /// The conversation of a [`Session::Draft`] so far, read from its
    /// transcript; empty for other sessions. Reads files, but doesn't wait
    /// for claude.
    pub fn history(&self) -> Result<History, String> {
        match self.shared.draft() {
            Some((draft, storage)) => draft.history(&storage),
            None => Ok(History {
                earlier: 0,
                messages: Vec::new(),
                context: None,
            }),
        }
    }

    /// Switches the session's model from the next turn, by a value from
    /// [`Init::models`]. Once accepted, later processes start with it too.
    pub fn set_model(
        &self,
        model: String,
    ) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let rx = (|| {
            let mut inner = self.shared.lock();
            let live = self.shared.ensure_live(&mut inner)?;
            let (tx, rx) = oneshot::channel();
            let request = json!({"subtype": "set_model", "model": model});
            live.request(
                request,
                Waiter::SetModel(tx, model),
                self.shared.config.request_timeout,
            )?;
            self.shared.changed.notify_all();
            Ok(rx)
        })();
        async move { rx?.await.unwrap_or(Err(Error::Stopped)) }
    }

    /// Sets the session's effort from the next turn, one of the model's
    /// [`ModelInfo::effort_levels`](crate::ModelInfo::effort_levels), or
    /// `None` for the model's default. Once accepted, later processes
    /// start with it too.
    pub fn set_effort(
        &self,
        effort: Option<String>,
    ) -> impl Future<Output = Result<(), Error>> + Send + use<> {
        let rx = (|| {
            let mut inner = self.shared.lock();
            let live = self.shared.ensure_live(&mut inner)?;
            let (tx, rx) = oneshot::channel();
            let request = json!({
                "subtype": "apply_flag_settings",
                "settings": {"effortLevel": effort},
            });
            live.request(
                request,
                Waiter::SetEffort(tx, effort),
                self.shared.config.request_timeout,
            )?;
            self.shared.changed.notify_all();
            Ok(rx)
        })();
        async move { rx?.await.unwrap_or(Err(Error::Stopped)) }
    }

    /// The tools of processes started from now on. The running process
    /// keeps its own.
    pub(crate) fn set_tools(&self, tools: Vec<Tool>) {
        self.shared.lock().tools = tools;
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    fn emit(&self, event: AgentEvent) {
        // The receiver may be gone while the app tears down. Callback
        // events are sent through `emit_unlocked`: dropping one answers it,
        // which takes the lock.
        debug_assert!(!matches!(
            event,
            AgentEvent::Permission(_) | AgentEvent::Hook(_)
        ));
        let _ = self.events.unbounded_send(event);
    }

    /// The draft session with its latest recorded transcript, and where
    /// Claude keeps it.
    fn draft(&self) -> Option<(DraftSession, PathBuf)> {
        let Some(Session::Draft(draft)) = &self.spec.session else {
            return None;
        };
        let mut draft = draft.clone();
        draft.transcript = self.recorded.lock().unwrap().clone();
        Some((draft, self.storage.clone()?))
    }

    fn adopt_on_exit(&self) {
        if let Err(err) = self.adopt() {
            eprintln!("claude-rpc: {err}");
        }
    }

    fn record(&self, path: PathBuf) {
        *self.recorded.lock().unwrap() = Some(path.clone());
        self.emit(AgentEvent::Transcript(path));
    }

    /// Moves the draft session's transcript into the draft. Only while no
    /// process has the session.
    fn adopt(&self) -> Result<(), Error> {
        let Some((draft, storage)) = self.draft() else {
            return Ok(());
        };
        let adopted = draft.adopt(&storage).map_err(|err| {
            Error::Spawn(format!(
                "couldn't move the transcript into {}: {err}",
                draft.dir.display()
            ))
        })?;
        if let Some(path) = adopted {
            self.record(path);
        }
        Ok(())
    }

    /// How the next process continues the session, once nothing else has
    /// it.
    fn start(&self) -> Result<Option<Start>, Error> {
        Ok(match &self.spec.session {
            None => None,
            Some(Session::New(id)) => Some(Start::New(id.clone())),
            Some(Session::Resume(target)) => Some(Start::Resume(target.clone())),
            Some(Session::Draft(_)) => {
                self.adopt()?;
                let (draft, _) = self.draft().expect("a draft session");
                Some(draft.start().map_err(Error::Missing)?)
            }
        })
    }

    fn stop(&self, inner: &mut Inner) {
        if let Some(mut live) = inner.live.take() {
            inner.previous = Some(live.conn.exit());
            if live.failed.is_none()
                && live.run.is_some()
                && let Err(err) = live.interrupt(Waiter::Ignore, self.config.request_timeout)
            {
                eprintln!("claude-rpc: couldn't interrupt the run before stopping: {err}");
            }
            live.abandon(Error::Stopped);
            // Dropping the connection closes stdin after the interrupt.
            drop(live);
            // Lets the timer thread exit.
            self.changed.notify_all();
        }
    }

    /// Stops a blurred process once nothing is in flight, and wakes the
    /// timer to recompute its deadlines.
    fn settled(&self, inner: &mut Inner) {
        self.changed.notify_all();
        // A failed process stays until its exit is reported as a crash.
        if inner.live.as_ref().is_some_and(|live| {
            live.failed.is_none() && live.stop_when_quiescent && live.quiescent()
        }) {
            self.stop(inner);
        }
    }

    fn focus(self: &Arc<Self>, inner: &mut Inner) -> Result<(), Error> {
        let live = self.ensure_live(inner)?;
        live.stop_when_quiescent = false;
        Ok(())
    }

    fn ensure_live<'a>(self: &Arc<Self>, inner: &'a mut Inner) -> Result<&'a mut Live, Error> {
        if inner.live.is_none() {
            if let Some(previous) = &inner.previous {
                if !previous.wait(Instant::now() + EXIT_BOUND) {
                    return Err(Error::Spawn(
                        "the previous claude process of this session hasn't exited".into(),
                    ));
                }
                inner.previous = None;
            }
            let start = self.start()?;
            inner.generation += 1;
            let generation = inner.generation;
            let weak = Arc::downgrade(self);
            let command = self.spec.command(
                inner.chosen.model.as_deref(),
                inner.chosen.effort.as_deref(),
                &inner.tools,
                start.as_ref(),
            );
            let conn = Connection::spawn(command, self.config.max_record, move |message| {
                on_message(&weak, generation, message)
            })
            .map_err(|err| Error::Spawn(err.to_string()))?;
            let mut live = Live {
                conn,
                generation,
                next_id: 0,
                pending: HashMap::new(),
                initialized: false,
                prompted: false,
                failed: None,
                turn: None,
                run: None,
                reports_state: false,
                callbacks: HashMap::new(),
                stop_when_quiescent: false,
                unrecorded: matches!(
                    (&self.spec.session, &start),
                    (
                        Some(Session::Draft(_)),
                        Some(Start::New(_) | Start::Fork { .. })
                    )
                ),
                last_activity: Instant::now(),
                settle_due: None,
            };
            let mut initialize = json!({"subtype": "initialize"});
            if !self.spec.hooks.is_empty() {
                initialize["hooks"] = protocol::hooks(&self.spec.hooks);
            }
            live.request(initialize, Waiter::Initialize, self.config.request_timeout)?;
            let shared = self.clone();
            thread::Builder::new()
                .name("claude-timer".into())
                .spawn(move || shared.run_timer(generation))
                .map_err(|err| Error::Spawn(err.to_string()))?;
            inner.live = Some(live);
        }
        let live = inner.live.as_mut().expect("just started");
        if live.failed.is_some() {
            return Err(Error::Exited);
        }
        Ok(live)
    }

    /// Writes the prompt of a new turn and starts its run.
    fn write_prompt(
        &self,
        live: &mut Live,
        text: String,
        context: Option<PromptContext>,
    ) -> Result<(), Error> {
        let blocks = context.map_or_else(Vec::new, |context| context(!live.prompted));
        live.send(protocol::user_message(&blocks, &text))?;
        live.prompted = true;
        live.turn = Some(Turn {
            text,
            held: None,
            context: None,
        });
        live.run = Some(Run::default());
        self.emit(AgentEvent::RunStarted);
        Ok(())
    }

    fn settle(&self, live: &mut Live) {
        let Some(run) = live.run.take() else {
            return;
        };
        live.settle_due = None;
        live.turn = None;
        let Outcome { aborted, error } = run.outcome.unwrap_or(Outcome {
            aborted: run.interrupt_requested,
            error: None,
        });
        self.emit(AgentEvent::RunSettled { aborted, error });
        if live.unrecorded
            && let Some((draft, storage)) = self.draft()
        {
            match session::native(&storage, &draft.id) {
                Ok(Some(path)) => {
                    live.unrecorded = false;
                    self.record(path);
                }
                Ok(None) => {}
                Err(err) => eprintln!("claude-rpc: couldn't look for the transcript: {err}"),
            }
        }
    }

    /// Expires requests, settles a run whose `idle` is overdue and stops
    /// claude when it goes idle, until process `generation` stops.
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
            if live.failed.is_none() {
                let mut stop = false;
                let timeout = self.config.request_timeout;
                for (_, pending) in live.pending.extract_if(|_, pending| pending.due <= now) {
                    match pending.waiter {
                        Waiter::Caller(tx) | Waiter::SetModel(tx, _) | Waiter::SetEffort(tx, _) => {
                            let _ = tx.send(Err(Error::Timeout(timeout)));
                        }
                        Waiter::Initialize => {
                            eprintln!(
                                "claude-rpc: claude didn't answer initialize within {} s",
                                timeout.as_secs()
                            );
                            stop = true;
                        }
                        Waiter::Ignore => {}
                    }
                }
                if stop {
                    live.fail_held(Error::Timeout(timeout));
                    self.stop(&mut inner);
                    return;
                }
                if live.settle_due.is_some_and(|due| due <= now) {
                    eprintln!("claude-rpc: claude didn't report idle after the run's result");
                    self.settle(live);
                }
                let idle = live.quiescent()
                    && (live.stop_when_quiescent
                        || live.last_activity + self.config.idle_timeout <= now);
                if idle {
                    self.stop(&mut inner);
                    return;
                }
            }
            inner = match live.wake_at(self.config.idle_timeout) {
                Some(at) => {
                    let wait = at.saturating_duration_since(now);
                    self.changed.wait_timeout(inner, wait).unwrap().0
                }
                None => self.changed.wait(inner).unwrap(),
            };
        }
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
    if matches!(message, Message::Exited { .. }) && inner.live.is_none() {
        // The last process stopped is gone, so nothing has the session.
        shared.adopt_on_exit();
        return;
    }
    let Inner { chosen, live, .. } = &mut *inner;
    let Some(live) = live else {
        return;
    };
    live.last_activity = Instant::now();
    let mut callback = None;
    match message {
        Message::Line(_) if live.failed.is_some() => {}
        Message::Line(frame) => match on_frame(&shared, live, chosen, frame) {
            Ok(event) => callback = event,
            Err(()) => {
                // initialize was rejected.
                shared.stop(&mut inner);
                return;
            }
        },
        Message::Closed(closed) => {
            let reason = match &closed {
                Closed::Eof => "claude exited unexpectedly".to_string(),
                Closed::Error(err) => format!("reading claude's output failed: {err}"),
                Closed::Oversized(max) => format!("claude sent a record over {max} bytes"),
            };
            fail(live, reason, closed != Closed::Eof);
        }
        Message::WriteFailed(err) => fail(live, format!("writing to claude failed: {err}"), true),
        Message::Exited { status, stderr } => {
            let mut live = inner.live.take().expect("checked above");
            live.abandon(Error::Exited);
            let reason = live
                .failed
                .take()
                .unwrap_or_else(|| "claude exited unexpectedly".into());
            let mut error = format!("{reason} ({status})");
            if !stderr.is_empty() {
                error.push_str(": ");
                error.push_str(&stderr);
            }
            shared.emit(AgentEvent::Crashed {
                error,
                failed_prompt: live.turn.map(|turn| turn.text),
            });
            shared.adopt_on_exit();
            shared.changed.notify_all();
            return;
        }
    }
    shared.settled(&mut inner);
    drop(inner);
    if let Some(event) = callback {
        // Sent unlocked: if the receiver is gone, dropping the event
        // answers the callback, which takes the lock.
        let _ = shared.events.unbounded_send(event);
    }
}

/// Fails everything in flight and ends the process; its exit is reported
/// as a crash. `kill` is for a process that may not exit on its own.
fn fail(live: &mut Live, reason: String, kill: bool) {
    if live.failed.is_some() {
        return;
    }
    eprintln!("claude-rpc: {reason}");
    live.failed = Some(reason);
    live.abandon(Error::Exited);
    if kill {
        live.conn.kill();
    } else {
        live.conn.stop();
    }
}

/// Handles one stdout frame. Returns a callback event to send once the lock
/// is released, or `Err` when `initialize` failed and claude should stop.
fn on_frame(
    shared: &Arc<Shared>,
    live: &mut Live,
    chosen: &mut Chosen,
    frame: Value,
) -> Result<Option<AgentEvent>, ()> {
    // Answers that were written no longer need tracking.
    live.callbacks
        .retain(|_, callback| !callback.answered || callback.ticket.queued());
    match frame["type"].as_str() {
        Some("control_response") => return on_response(shared, live, chosen, &frame["response"]),
        Some("control_request") => return Ok(on_request(shared, live, frame)),
        Some("control_cancel_request") => {
            let id = frame["request_id"].as_str().unwrap_or_default();
            if let Some(callback) = live.callbacks.remove(id) {
                callback.ticket.cancel();
                shared.emit(AgentEvent::CallbackCancelled {
                    request_id: id.to_string(),
                });
            }
        }
        Some("system") if frame["subtype"] == "session_state_changed" => {
            live.reports_state = true;
            match frame["state"].as_str() {
                Some("running" | "requires_action") => {
                    let run = live.run.get_or_insert_with(|| {
                        shared.emit(AgentEvent::RunStarted);
                        Run::default()
                    });
                    run.observed = true;
                }
                Some("idle") if live.run.as_ref().is_some_and(|run| run.observed) => {
                    shared.settle(live);
                }
                _ => {}
            }
        }
        Some("result") => {
            let aborted = protocol::result_aborted(&frame);
            let error = (!aborted).then(|| protocol::result_error(&frame)).flatten();
            shared.emit(AgentEvent::Frame(frame));
            if let Some(run) = &mut live.run {
                run.outcome = Some(Outcome { aborted, error });
                if live.reports_state {
                    live.settle_due = Some(Instant::now() + shared.config.request_timeout);
                } else {
                    shared.settle(live);
                }
            }
        }
        _ => shared.emit(AgentEvent::Frame(frame)),
    }
    Ok(None)
}

fn on_response(
    shared: &Arc<Shared>,
    live: &mut Live,
    chosen: &mut Chosen,
    response: &Value,
) -> Result<Option<AgentEvent>, ()> {
    let id = response["request_id"].as_str().unwrap_or_default();
    let Some(pending) = live.pending.remove(id) else {
        eprintln!("claude-rpc: ignoring a late or unknown control response `{id}`");
        return Ok(None);
    };
    let reply = if response["subtype"] == "error" {
        let error = response["error"].as_str().unwrap_or("unknown error");
        Err(Error::Rejected(error.to_string()))
    } else {
        Ok(())
    };
    match pending.waiter {
        Waiter::Caller(tx) => {
            let _ = tx.send(reply);
        }
        Waiter::SetModel(tx, requested) => {
            if reply.is_ok() {
                chosen.model = Some(requested);
            }
            let _ = tx.send(reply);
        }
        Waiter::SetEffort(tx, requested) => {
            if reply.is_ok() {
                chosen.effort = requested;
            }
            let _ = tx.send(reply);
        }
        Waiter::Ignore => {}
        Waiter::Initialize => match reply {
            Ok(()) => {
                let init = Init::parse(&response["response"]);
                if !shared.spec.hooks.is_empty() && !init.hooks_applied {
                    let err = Error::Rejected(
                        "claude didn't confirm it applied Draftmux's tool hooks, so it can't \
                         run safely; this version of Claude Code may be unsupported"
                            .into(),
                    );
                    eprintln!("claude-rpc: initialize failed: {err}");
                    live.fail_held(err);
                    return Err(());
                }
                live.initialized = true;
                shared.emit(AgentEvent::Ready(init));
                if let Some(turn) = live.turn.take() {
                    let tx = turn.held.expect("only a held turn precedes initialize");
                    let _ = tx.send(shared.write_prompt(live, turn.text, turn.context));
                }
            }
            Err(err) => {
                eprintln!("claude-rpc: initialize failed: {err}");
                live.fail_held(err);
                return Err(());
            }
        },
    }
    Ok(None)
}

/// Registers a permission or hook request and returns its event, or
/// answers a request that can't be handled.
fn on_request(shared: &Arc<Shared>, live: &mut Live, frame: Value) -> Option<AgentEvent> {
    let Some(id) = frame["request_id"].as_str().map(str::to_owned) else {
        eprintln!("claude-rpc: ignoring a control request without an id");
        return None;
    };
    let request = &frame["request"];
    let subtype = request["subtype"].as_str().unwrap_or_default();
    let text = |key| request[key].as_str().map(str::to_owned);
    let hook = (subtype == "hook_callback")
        .then(|| {
            let callback_id = request["callback_id"].as_str()?;
            (0..shared.spec.hooks.len()).find(|&i| protocol::hook_id(i) == callback_id)
        })
        .flatten();
    let reply = match subtype {
        "can_use_tool" => None,
        // An error response would let the tool run.
        "hook_callback" if hook.is_none() => Some(protocol::control_success(
            &id,
            protocol::hook_deny("Draftmux has no hook with this id"),
        )),
        "hook_callback" => None,
        _ => Some(protocol::control_error(
            &id,
            &format!("Unsupported control request subtype: {subtype}"),
        )),
    };
    if let Some(reply) = reply {
        if let Err(err) = live.send(reply) {
            eprintln!("claude-rpc: couldn't answer a control request: {err}");
        }
        return None;
    }
    let ticket = Arc::new(Ticket::default());
    live.callbacks.insert(
        id.clone(),
        Callback {
            ticket,
            answered: false,
        },
    );
    let responder = |fallback| Responder {
        shared: Arc::downgrade(shared),
        generation: live.generation,
        request_id: id.clone(),
        fallback: Some(fallback),
    };
    Some(match hook {
        None => AgentEvent::Permission(PermissionRequest {
            tool_name: text("tool_name").unwrap_or_default(),
            input: request["input"].clone(),
            tool_use_id: text("tool_use_id"),
            responder: responder(protocol::permission_deny(
                "Draftmux didn't answer the permission request",
            )),
        }),
        Some(hook) => AgentEvent::Hook(HookRequest {
            hook,
            input: request["input"].clone(),
            tool_use_id: text("tool_use_id"),
            responder: responder(protocol::hook_deny("Draftmux didn't answer the hook")),
        }),
    })
}

/// Answers one callback of one process, at most once; an unanswered one is
/// denied when dropped.
struct Responder {
    shared: Weak<Shared>,
    generation: u64,
    request_id: String,
    /// Sent on drop when no answer was given.
    fallback: Option<Value>,
}

impl Responder {
    /// Queues the answer unless the process ended or claude withdrew the
    /// request. Returns whether it was queued.
    fn answer(&mut self, response: Value) -> bool {
        self.fallback = None;
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let mut inner = shared.lock();
        if inner.generation != self.generation {
            return false;
        }
        let Some(live) = inner.live.as_mut().filter(|live| live.failed.is_none()) else {
            return false;
        };
        let Some(callback) = live.callbacks.get_mut(&self.request_id) else {
            return false;
        };
        if callback.answered {
            return false;
        }
        callback.answered = true;
        let ticket = callback.ticket.clone();
        let line = protocol::control_success(&self.request_id, response);
        let queued = live.conn.send(line, Some(ticket)).is_ok();
        live.last_activity = Instant::now();
        shared.settled(&mut inner);
        queued
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if let Some(fallback) = self.fallback.take() {
            self.answer(fallback);
        }
    }
}

impl fmt::Debug for Responder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Responder")
            .field("generation", &self.generation)
            .field("request_id", &self.request_id)
            .finish()
    }
}

/// claude asks whether `tool_name` may run with `input`. Dropping the
/// request denies it.
#[derive(Debug)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: Option<String>,
    responder: Responder,
}

impl PermissionRequest {
    pub fn request_id(&self) -> &str {
        &self.responder.request_id
    }

    /// Lets the tool run with its input. Returns whether the answer was
    /// queued: not when claude withdrew the request or the process ended.
    pub fn allow(self) -> bool {
        let Self {
            input,
            mut responder,
            ..
        } = self;
        responder.answer(protocol::permission_allow(input))
    }

    /// Refuses the tool call; claude shows the model `message`.
    pub fn deny(mut self, message: &str) -> bool {
        self.responder.answer(protocol::permission_deny(message))
    }
}

/// A call of [`LaunchSpec::hooks`]`[hook]`, with the hook input (tool name
/// and input among it). Dropping the call denies the tool.
#[derive(Debug)]
pub struct HookRequest {
    pub hook: usize,
    pub input: Value,
    pub tool_use_id: Option<String>,
    responder: Responder,
}

impl HookRequest {
    pub fn request_id(&self) -> &str {
        &self.responder.request_id
    }

    /// No objection: the call goes on to the usual permission checks.
    pub fn proceed(mut self) -> bool {
        self.responder.answer(protocol::hook_continue())
    }

    /// Blocks the tool call.
    pub fn deny(mut self, reason: &str) -> bool {
        self.responder.answer(protocol::hook_deny(reason))
    }
}
