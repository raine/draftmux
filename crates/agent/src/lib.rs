//! What the chat needs from a draft's agent, whatever runs it: the commands
//! it sends, the conversation events and history it shows, and the model it
//! reports. A backend adapter turns its own protocol into these types, so
//! the chat never reads a backend's wire format.
//!
//! [`transcript`] reduces the events and history to the items the chat
//! shows, and [`quote`] is the selection format Draftmux puts in prompts.
//!
//! The crate doesn't depend on gpui: requests return futures and events
//! arrive as a stream, both usable from gpui's executors.

pub mod quote;
pub mod transcript;

use std::fmt;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;

/// One draft's agent. Its process starts on demand and stops once idle or
/// when the agent is dropped.
///
/// Requests are sent when the method is called, not when its future is
/// first polled, so a history request made before a prompt is answered
/// without that prompt.
pub trait Agent: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    /// The draft was selected: start the agent if it isn't running.
    fn focus(&self) -> Result<(), Error>;

    /// The app switched to another draft: stop the agent now if it is idle,
    /// otherwise once the current run settles.
    fn blur(&self);

    /// Sends a user message. Resolves once the agent has accepted or
    /// rejected it; the run's progress arrives as [`Event`]s. Save the
    /// draft before calling this, so the agent reads the editor's text.
    fn prompt(&self, text: String) -> BoxFuture<'static, Result<Prompted, Error>>;

    /// Stops the current run. Its end arrives as [`Event::RunSettled`].
    fn abort(&self) -> BoxFuture<'static, Result<(), Error>>;

    /// The conversation so far.
    fn history(&self) -> BoxFuture<'static, Result<History, Error>>;

    /// The models [`Agent::switch_model`] accepts.
    fn available_models(&self) -> BoxFuture<'static, Result<Vec<Model>, Error>>;

    /// Switches the conversation's model and returns the state the agent
    /// then reports, which may differ from the request. Fails with
    /// [`ErrorKind::Stopped`] when the agent stopped in between; the next
    /// [`Event::Ready`] reports the model then.
    fn switch_model(&self, model: Model) -> BoxFuture<'static, Result<State, Error>>;

    /// The thinking settings the conversation's model takes, lowest first,
    /// as the backend names them; empty when the model has none.
    fn thinking_levels(&self) -> BoxFuture<'static, Result<Vec<String>, Error>>;

    /// Sets the conversation's thinking setting, one of
    /// [`Agent::thinking_levels`], or `None` for the backend's own when
    /// [`Capabilities::default_thinking`] says it has one. Returns the
    /// state the agent then reports, which may differ from the request.
    /// Fails with [`ErrorKind::Stopped`] like [`Agent::switch_model`].
    fn set_thinking(&self, level: Option<String>) -> BoxFuture<'static, Result<State, Error>>;

    /// Replaces the conversation's earlier messages with a summary that
    /// keeps what `instructions` asks for. Only while no run is under way.
    /// Resolves once compaction ended, which also arrives as
    /// [`Event::CompactionEnded`], or fails when the agent didn't take the
    /// request.
    fn compact(&self, instructions: String) -> BoxFuture<'static, Result<(), Error>>;
}

/// Everything an agent reports, across its process restarts.
pub type Events = BoxStream<'static, Event>;

/// What an agent supports beyond prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub model_switching: bool,
    /// The thinking setting can be left to the backend, which picks one
    /// for the model; [`State::thinking`] is then `None`.
    pub default_thinking: bool,
}

/// What the agent did with a prompt it accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompted {
    /// A run started, or will once the current one ends.
    Running,
    /// The agent handled the prompt itself, such as a command, and no run
    /// follows.
    Handled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// What went wrong, for the user.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The agent was stopped before answering.
    Stopped,
    Other,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// The model and thinking setting the agent resolved for the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The model the conversation is set to; none when the backend picks
    /// it.
    pub model: Option<Model>,
    /// The thinking setting as the backend names it; none when the backend
    /// picks it.
    pub thinking: Option<String>,
    /// The model id the backend reported a run used, when that is a
    /// different name than `model`, such as the id an alias resolved to.
    pub running: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub provider: String,
    pub id: String,
}

/// What the agent reports, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A new agent process is ready and resolved this state, or the
    /// running process reported a change to it.
    Ready(State),
    /// The agent started a run, for a prompt or on its own.
    RunStarted,
    /// The run is over, including retries and compaction. `aborted` says
    /// whether it was stopped.
    RunSettled {
        aborted: bool,
    },
    /// A model request failed and is tried again.
    Retrying {
        attempt: u32,
        max_attempts: u32,
        error: String,
    },
    /// The agent took a user message into the conversation: the echo of a
    /// prompt, or one the agent started a run with.
    UserMessage(String),
    /// An assistant message starts streaming.
    AssistantStarted,
    /// The streaming message starts another text block.
    AssistantBlock,
    /// More text of the streaming message's current block.
    AssistantText(String),
    /// The assistant message is complete: `text` is its final text, which
    /// replaces what streamed.
    AssistantEnded {
        text: String,
        stop: Stop,
    },
    ToolStarted(ToolCall),
    /// The running tool's output so far.
    ToolOutput {
        id: String,
        output: String,
    },
    /// `draft_write` says the tool changed the draft's file, so the editor
    /// should reload it.
    ToolFinished {
        id: String,
        result: ToolResult,
        draft_write: bool,
    },
    /// The conversation as the model sees it reached `tokens` tokens: the
    /// latest model request's input and output.
    Context(u64),
    /// The agent started replacing earlier messages with a summary, on
    /// request or on its own.
    CompactionStarted,
    /// Compaction ended. `compacted` says whether earlier messages were
    /// replaced; `tokens` is the conversation's size after it, when known,
    /// and `error` why it failed, when the agent said.
    CompactionEnded {
        compacted: bool,
        tokens: Option<u64>,
        error: Option<String>,
    },
    /// The agent's process exited without being asked to. `failed_prompt`
    /// is the prompt whose run it interrupted, offered again by a retry;
    /// the next request starts a new process.
    Crashed {
        error: String,
        failed_prompt: Option<String>,
    },
}

/// Why an assistant message ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Completed,
    Aborted,
    /// The model failed, with its error message when it gave one.
    Error(Option<String>),
}

/// The conversation as the chat shows it: the Draftmux part, and how many
/// messages of the source conversation came before it.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub earlier: usize,
    pub messages: Vec<Message>,
    /// The whole conversation's size in tokens as the model last saw it,
    /// source part included; `None` when no model request reported it
    /// since the last compaction.
    pub context: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
        stop: Stop,
    },
    ToolResult {
        id: String,
        result: ToolResult,
    },
    /// Earlier messages were replaced by a summary.
    Compacted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub kind: ToolKind,
}

/// The tools the chat shows a row for, with the arguments it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolKind {
    Bash { command: String },
    Read { path: String },
    Edit { path: String },
    Write { path: String, content: String },
    Grep { pattern: String },
    Find { pattern: String },
    Ls { path: String },
    Other { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The result's text.
    pub output: String,
    pub is_error: bool,
    pub duration: Option<Duration>,
    /// The unified patch an edit made.
    pub patch: Option<String>,
}

/// What a draft started from a summary tells its agent about where it came
/// from: the `summary` the handing-off agent wrote, and the `transcript` of
/// the session it summarizes, to read for details the summary leaves out.
pub fn summary_message(summary: &str, transcript: &std::path::Path) -> String {
    format!(
        "This session starts from a summary of the conversation the draft was handed off \
         from, not from that conversation's history. Its full transcript is {}; read it only \
         when the summary leaves out a detail you need.\n\n<summary>\n{}\n</summary>",
        transcript.display(),
        summary.trim()
    )
}
