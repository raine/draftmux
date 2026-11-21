//! A draft's Claude Code agent behind the shared agent contract
//! ([`draftmux_agent`]): [`ClaudeAgent`] wraps [`Agent`], gives the model
//! Draftmux's context, keeps its writes to the draft, and reports which of
//! its tool calls changed the draft.
//!
//! - **Context.** Ahead of a process's first prompt, a
//!   `<draftmux-instructions>` block carries the drafting rules, the draft
//!   path and the user's instructions. They can't go into the system
//!   prompt: Claude Code ignores `--append-system-prompt` and
//!   `--system-prompt` when it resumes a session, and a draft's session is
//!   always resumed or forked. A `<draftmux-context>` block carries the
//!   target platform's syntax when it changed, and a `<draftmux-draft>`
//!   block the saved draft when it differs from what the model last saw.
//!   All three are sent again to a new process and after compaction, and
//!   history leaves them out.
//! - **Policy.** A `PreToolUse` hook for every tool decides each call with
//!   [`DraftPolicy`]; unlike permission prompts, it also runs for tools
//!   that settings allow. claude must confirm it registered the hook, or
//!   no prompt is sent. Permission prompts allow an Edit or Write only
//!   when the hook allowed that call, so a call that bypassed the hook
//!   isn't granted. Every failure is answered with a denial: an error
//!   answer would let the tool run.
//! - **Writes.** A call the hook allowed changed the draft when it reports
//!   success. When it fails or never reports, as when interrupted or when
//!   claude crashes, it counts only if the draft now holds exactly what the
//!   call would have written; any other change is left to the app's check
//!   for outside edits. A successful Edit or Write the hook never saw
//!   means the hook isn't enforced: the process is stopped and later ones
//!   start without Edit and Write.
//!
//! - **Model.** The model the user picks is switched live with
//!   `set_model` and passed to every later process. The state reports the
//!   chosen model and, once a run starts, the model claude says it used.
//!   The effort is the state's thinking setting: switched live with
//!   `apply_flag_settings` and passed to later processes as `--effort`.
//!
//! What the draft's descriptor must keep, the transcript's path and the
//! chosen model and effort, arrives as [`Record`]s.
//!
//! Callbacks are answered on a thread of the agent's own, in order with
//! the frames, not on the app's executor.

use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use draftmux_agent as contract;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, StreamExt, future};
use serde_json::Value;

use crate::agent::PromptContext;
use crate::history;
use crate::policy::{self, Decision, DraftPolicy};
use crate::{
    Agent, AgentEvent, Config, Error, HookRequest, LaunchSpec, ModelInfo, PermissionRequest,
    PreToolUseHook, Tool,
};

/// The tools of an agent that may edit.
const EDITING_TOOLS: &[Tool] = &[Tool::Read, Tool::Glob, Tool::Grep, Tool::Edit, Tool::Write];
/// The tools left once editing was found unenforced.
const READ_ONLY_TOOLS: &[Tool] = &[Tool::Read, Tool::Glob, Tool::Grep];
/// Seconds claude waits for the hook's answer before refusing the call.
const HOOK_TIMEOUT: u32 = 30;
/// Drafts larger than this are left for the model to read.
const MAX_SENT_DRAFT_BYTES: usize = 50 * 1024;

const INSTRUCTIONS_TAG: &str = "<draftmux-instructions>";
const CONTEXT_TAG: &str = "<draftmux-context>";
const DRAFT_TAG: &str = "<draftmux-draft>";

/// Whether a user message's text block is context Draftmux added.
pub(crate) fn is_context_block(text: &str) -> bool {
    text.starts_with(INSTRUCTIONS_TAG)
        || text.starts_with(CONTEXT_TAG)
        || text.starts_with(DRAFT_TAG)
}

/// What the model is told about the draft.
pub struct DraftContext {
    /// The draft's `draft.md`.
    pub draft_path: PathBuf,
    /// The user's drafting instructions; may be empty.
    pub instructions: String,
    /// The target platform's syntax, such as "plain text, such as an email
    /// body", read before every prompt; `None` when it can't be told.
    pub target: Box<dyn Fn() -> Option<String> + Send + Sync>,
    /// For a draft started from a summary, the message that carries it
    /// ([`draftmux_agent::summary_message`]), sent with the instructions.
    pub summary: Option<String>,
}

/// What changed that the draft's descriptor keeps, so a later start
/// continues the same session on the same model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// The session's transcript is now at this path
    /// ([`AgentEvent::Transcript`]).
    Transcript(PathBuf),
    /// claude accepted this model for the session.
    Model(String),
    /// claude accepted this effort for the session; `None` is the model's
    /// default.
    Effort(Option<String>),
}

/// A draft's Claude [`Agent`] as a [`contract::Agent`].
pub struct ClaudeAgent {
    agent: Arc<Agent>,
    context: Arc<DraftContext>,
    ledger: Arc<Mutex<Ledger>>,
    /// The models the last process listed; none before one was ready.
    models: Arc<Mutex<Option<Vec<ModelInfo>>>>,
    records: mpsc::UnboundedSender<Record>,
    /// Resolves once the last history requested was read. A prompt waits
    /// for it, so that history never includes the prompt.
    history_read: Mutex<Option<future::Shared<oneshot::Receiver<()>>>>,
    /// Told when the compaction the user asked for ends.
    compaction: Compaction,
}

/// The waiter of the compaction the user asked for, until it ends.
type Compaction = Arc<Mutex<Option<oneshot::Sender<()>>>>;

/// What the model was last sent or saw, so a prompt carries only what
/// changed.
#[derive(Default)]
struct Ledger {
    /// The instructions were sent to this process since it started or
    /// last compacted.
    instructed: bool,
    /// The target syntax last sent.
    target: Option<String>,
    /// The draft text the model last saw in full: through a draft block,
    /// its own Read, or its own successful write.
    seen: Option<String>,
}

impl ClaudeAgent {
    /// Like [`Agent::new`], for a draft the agent may edit: the spec's
    /// tools, hooks and permission prompts are set here.
    /// Returns the agent, its events and its [`Record`]s. Fails when the
    /// spec loads settings, which can add hooks and allow tools, or when
    /// the draft's directory can't be resolved.
    pub fn new(
        mut spec: LaunchSpec,
        config: Config,
        context: DraftContext,
    ) -> Result<(Self, contract::Events, mpsc::UnboundedReceiver<Record>), String> {
        if !spec.setting_sources.is_empty() {
            return Err("Claude Code settings can't be loaded for a draft yet".into());
        }
        let policy = DraftPolicy::new(&context.draft_path).map_err(|err| {
            format!(
                "couldn't resolve the draft {}: {err}",
                context.draft_path.display()
            )
        })?;
        spec.tools = EDITING_TOOLS.to_vec();
        spec.hooks = vec![PreToolUseHook {
            matcher: None,
            timeout: Some(HOOK_TIMEOUT),
        }];
        spec.permission_prompt = true;
        let (agent, mut raw) = Agent::new(spec, config);
        let agent = Arc::new(agent);
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let models = Arc::new(Mutex::new(None));
        let compaction = Compaction::default();
        let (tx, events) = mpsc::unbounded();
        let (records_tx, records) = mpsc::unbounded();
        let mut observer = Observer {
            agent: Arc::downgrade(&agent),
            policy,
            ledger: ledger.clone(),
            models: models.clone(),
            running: None,
            events: tx,
            records: records_tx.clone(),
            calls: HashMap::new(),
            approved: Vec::new(),
            message: None,
            error_reported: false,
            editing: true,
            compaction: compaction.clone(),
            compacting: false,
            input_tokens: 0,
        };
        thread::Builder::new()
            .name("claude-draft".into())
            .spawn(move || {
                while let Some(event) = futures::executor::block_on(raw.next()) {
                    observer.on_event(event);
                }
            })
            .map_err(|err| err.to_string())?;
        let agent = Self {
            agent,
            context: Arc::new(context),
            ledger,
            models,
            records: records_tx,
            history_read: Mutex::new(None),
            compaction,
        };
        Ok((agent, events.boxed(), records))
    }
}

impl contract::Agent for ClaudeAgent {
    fn capabilities(&self) -> contract::Capabilities {
        contract::Capabilities {
            model_switching: true,
            default_thinking: true,
        }
    }

    fn focus(&self) -> Result<(), contract::Error> {
        self.agent.focus().map_err(error)
    }

    fn blur(&self) {
        self.agent.blur();
    }

    fn prompt(
        &self,
        text: String,
    ) -> future::BoxFuture<'static, Result<contract::Prompted, contract::Error>> {
        // Claude Code reads slash commands only from plain text.
        let context = (!text.trim_start().starts_with('/')).then(|| {
            let context = self.context.clone();
            let ledger = self.ledger.clone();
            Box::new(move |first| blocks(&context, &mut ledger.lock().unwrap(), first))
                as PromptContext
        });
        let history_read = self.history_read.lock().unwrap().clone();
        let agent = self.agent.clone();
        async move {
            if let Some(read) = history_read {
                let _ = read.await;
            }
            agent
                .prompt_with(text, context)
                .await
                .map(|()| contract::Prompted::Running)
                .map_err(error)
        }
        .boxed()
    }

    fn abort(&self) -> future::BoxFuture<'static, Result<(), contract::Error>> {
        self.agent
            .interrupt()
            .map(|reply| reply.map_err(error))
            .boxed()
    }

    /// Read on a thread of its own: the transcript can be large.
    fn history(&self) -> future::BoxFuture<'static, Result<contract::History, contract::Error>> {
        let (tx, rx) = oneshot::channel();
        let (read_tx, read_rx) = oneshot::channel();
        *self.history_read.lock().unwrap() = Some(read_rx.shared());
        let agent = self.agent.clone();
        // Dropping `read_tx` once read, or if the thread can't start,
        // releases the prompts waiting on it.
        let read = thread::Builder::new()
            .name("claude-history".into())
            .spawn(move || {
                let _ = tx.send(agent.history());
                drop(read_tx);
            });
        async move {
            let history = match read {
                Ok(_) => rx
                    .await
                    .unwrap_or_else(|_| Err("reading the history failed".into())),
                Err(err) => Err(err.to_string()),
            };
            history.map_err(|message| contract::Error {
                kind: contract::ErrorKind::Other,
                message,
            })
        }
        .boxed()
    }

    fn available_models(
        &self,
    ) -> future::BoxFuture<'static, Result<Vec<contract::Model>, contract::Error>> {
        let models = self.models.lock().unwrap();
        future::ready(match &*models {
            Some(models) => Ok(models.iter().map(|m| model(&m.value)).collect()),
            None => Err(contract::Error {
                kind: contract::ErrorKind::Other,
                message: "Claude Code lists its models once it has started.".into(),
            }),
        })
        .boxed()
    }

    fn switch_model(
        &self,
        choice: contract::Model,
    ) -> future::BoxFuture<'static, Result<contract::State, contract::Error>> {
        let id = choice.id;
        let reply = self.agent.set_model(id.clone());
        let records = self.records.clone();
        let agent = self.agent.clone();
        async move {
            reply.await.map_err(error)?;
            let _ = records.unbounded_send(Record::Model(id.clone()));
            Ok(contract::State {
                model: Some(model(&id)),
                thinking: agent.effort(),
                // Known again once a run reports it.
                running: None,
            })
        }
        .boxed()
    }

    /// The efforts of the chosen model, or of claude's `default` when the
    /// session continues on its own model.
    fn thinking_levels(&self) -> future::BoxFuture<'static, Result<Vec<String>, contract::Error>> {
        let chosen = self.agent.model();
        let chosen = chosen.as_deref().unwrap_or("default");
        let models = self.models.lock().unwrap();
        future::ready(match &*models {
            Some(models) => Ok(models
                .iter()
                .find(|m| m.value == chosen)
                .or_else(|| {
                    models
                        .iter()
                        .find(|m| m.resolved_model.as_deref() == Some(chosen))
                })
                .map(|m| m.effort_levels.clone())
                .unwrap_or_default()),
            None => Err(contract::Error {
                kind: contract::ErrorKind::Other,
                message: "Claude Code lists its efforts once it has started.".into(),
            }),
        })
        .boxed()
    }

    fn set_thinking(
        &self,
        level: Option<String>,
    ) -> future::BoxFuture<'static, Result<contract::State, contract::Error>> {
        let reply = self.agent.set_effort(level.clone());
        let records = self.records.clone();
        let agent = self.agent.clone();
        async move {
            reply.await.map_err(error)?;
            let _ = records.unbounded_send(Record::Effort(level.clone()));
            Ok(contract::State {
                model: agent.model().as_deref().map(model),
                thinking: level,
                running: None,
            })
        }
        .boxed()
    }

    /// Runs Claude Code's `/compact`, which ends the run it starts once
    /// compacted.
    fn compact(
        &self,
        instructions: String,
    ) -> future::BoxFuture<'static, Result<(), contract::Error>> {
        let (tx, rx) = oneshot::channel();
        *self.compaction.lock().unwrap() = Some(tx);
        let prompt = self.prompt(format!("/compact {}", instructions.trim()));
        let compaction = self.compaction.clone();
        async move {
            if let Err(err) = prompt.await {
                compaction.lock().unwrap().take();
                return Err(err);
            }
            rx.await.map_err(|_| contract::Error {
                kind: contract::ErrorKind::Stopped,
                message: "Claude Code stopped before it compacted the conversation".into(),
            })
        }
        .boxed()
    }
}

fn error(error: Error) -> contract::Error {
    contract::Error {
        kind: match error {
            Error::Stopped => contract::ErrorKind::Stopped,
            _ => contract::ErrorKind::Other,
        },
        message: error.to_string(),
    }
}

fn model(id: &str) -> contract::Model {
    contract::Model {
        provider: "claude".into(),
        id: id.to_owned(),
    }
}

/// The block of drafting rules for the draft at `draft_path`, followed by
/// the user's `instructions` when there are any and the message carrying
/// the draft's `summary` when it started from one.
pub fn instructions_block(draft_path: &Path, instructions: &str, summary: Option<&str>) -> String {
    let draft = draft_path.display();
    let mut prompt = format!(
        "{INSTRUCTIONS_TAG}
You are helping the user polish a message in Draftmux. These instructions come from Draftmux, not the user, and are sent again when Draftmux restarts you.

Draft file: {draft}

- Make every change to the draft by editing that file with the Edit or Write tool; the user sees it in the Draftmux editor. Don't paste drafts or rewritten versions in chat; use chat for discussion.
- Edit and Write only work on the draft file; give its absolute path. Read, Glob and Grep work as usual for checking facts.
- Draftmux puts a {DRAFT_TAG} block holding the draft file's exact text before the user's message when it differs from what you last read, edited or wrote, for example after the user edits it, and a {CONTEXT_TAG} block with the target platform when it changes. The user doesn't see these blocks.
- Read the draft once before your first Edit or Write after these instructions: the tools refuse a file you haven't read since they were sent, even when a {DRAFT_TAG} block shows its text.
- Put fields in YAML frontmatter at the very top of the draft, between --- lines: title: for PRs and issues; subject:, to: and cc: for emails (only known recipients). Any other key is allowed. Use string values, quoting YAML punctuation as needed; use comma-separated strings rather than lists. Keep these fields separate from the message body. Draftmux displays them separately and copies only the body.
- Change only what the user asks for; keep the rest of the draft, including its formatting, as it is."
    );
    let instructions = instructions.trim();
    if !instructions.is_empty() {
        prompt.push_str("\n\nDrafting instructions from the user:\n");
        prompt.push_str(instructions);
    }
    if let Some(summary) = summary {
        prompt.push_str("\n\n");
        prompt.push_str(summary);
    }
    prompt.push_str("\n</draftmux-instructions>");
    prompt
}

/// The context blocks of a prompt, given what the model saw. `first` says
/// the prompt is a new process's first, which starts the ledger over.
fn blocks(context: &DraftContext, ledger: &mut Ledger, first: bool) -> Vec<String> {
    if first {
        *ledger = Ledger::default();
    }
    let mut blocks = Vec::new();
    if !ledger.instructed {
        blocks.push(instructions_block(
            &context.draft_path,
            &context.instructions,
            context.summary.as_deref(),
        ));
        ledger.instructed = true;
    }
    if let Some(target) = (context.target)()
        && ledger.target.as_ref() != Some(&target)
    {
        blocks.push(format!(
            "{CONTEXT_TAG}\nTarget platform: {target}\n</draftmux-context>"
        ));
        ledger.target = Some(target);
    }
    // Saved before the prompt, so this is what the editor showed.
    if let Ok(draft) = fs::read_to_string(&context.draft_path)
        && ledger.seen.as_ref() != Some(&draft)
    {
        if draft.len() > MAX_SENT_DRAFT_BYTES {
            blocks.push(format!(
                "{CONTEXT_TAG}\nThe draft file changed since you last read, edited or wrote it, \
                 and is too long to include here. Read it before editing.\n</draftmux-context>"
            ));
            ledger.seen = None;
        } else {
            let newline = if draft.is_empty() || draft.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            blocks.push(format!("{DRAFT_TAG}\n{draft}{newline}</draftmux-draft>"));
            ledger.seen = Some(draft);
        }
    }
    blocks
}

/// Follows one agent's events on its thread: answers callbacks, maps
/// frames to contract events, and works out the draft writes.
struct Observer {
    agent: Weak<Agent>,
    policy: DraftPolicy,
    ledger: Arc<Mutex<Ledger>>,
    models: Arc<Mutex<Option<Vec<ModelInfo>>>>,
    /// The model the process's last run said it used.
    running: Option<String>,
    events: mpsc::UnboundedSender<contract::Event>,
    records: mpsc::UnboundedSender<Record>,
    /// The tool calls of the run so far, by `tool_use_id`: name and input.
    calls: HashMap<String, (String, Value)>,
    /// Draft writes the hook allowed whose result hasn't arrived, in the
    /// order they were allowed.
    approved: Vec<Approved>,
    /// The assistant message streaming: its id and text blocks.
    message: Option<(String, Vec<String>)>,
    /// The run's error was shown, so its settlement doesn't show it again.
    error_reported: bool,
    /// Edit and Write are still trusted.
    editing: bool,
    compaction: Compaction,
    /// Claude Code said it is compacting, and the boundary hasn't arrived.
    compacting: bool,
    /// The input tokens of the request streaming, cached ones included.
    input_tokens: u64,
}

impl Observer {
    fn emit(&self, event: contract::Event) {
        let _ = self.events.unbounded_send(event);
    }

    fn state(&self) -> contract::State {
        contract::State {
            model: self
                .agent
                .upgrade()
                .and_then(|agent| agent.model())
                .as_deref()
                .map(model),
            thinking: self.agent.upgrade().and_then(|agent| agent.effort()),
            running: self.running.clone(),
        }
    }

    fn on_event(&mut self, event: AgentEvent) {
        use contract::Event as E;
        match event {
            AgentEvent::Ready(init) => {
                *self.models.lock().unwrap() = Some(init.models);
                self.running = None;
                self.emit(E::Ready(self.state()));
            }
            AgentEvent::RunStarted => {
                self.error_reported = false;
                self.emit(E::RunStarted);
            }
            AgentEvent::Frame(frame) => self.on_frame(&frame),
            AgentEvent::Hook(request) => self.on_hook(request),
            AgentEvent::Permission(request) => self.on_permission(request),
            AgentEvent::Transcript(path) => {
                let _ = self.records.unbounded_send(Record::Transcript(path));
            }
            AgentEvent::CallbackCancelled { .. } => {}
            AgentEvent::RunSettled { aborted, error } => {
                self.end_run(aborted);
                let requested = self.compaction.lock().unwrap().take();
                if requested.is_some() || std::mem::take(&mut self.compacting) {
                    // The run ended without a compact boundary.
                    self.emit(E::CompactionEnded {
                        compacted: false,
                        tokens: None,
                        error: error.filter(|_| !aborted),
                    });
                    if let Some(tx) = requested {
                        let _ = tx.send(());
                    }
                    self.emit(E::RunSettled { aborted });
                    return;
                }
                if let Some(error) = error.filter(|_| !self.error_reported) {
                    self.emit(E::AssistantEnded {
                        text: String::new(),
                        stop: contract::Stop::Error(Some(error)),
                    });
                }
                self.emit(E::RunSettled { aborted });
            }
            AgentEvent::Crashed {
                error,
                failed_prompt,
            } => {
                self.end_run(true);
                // Dropping the waiter fails the compaction request.
                self.compaction.lock().unwrap().take();
                self.compacting = false;
                self.emit(E::Crashed {
                    error,
                    failed_prompt,
                });
            }
        }
    }

    fn on_frame(&mut self, frame: &Value) {
        use contract::Event as E;
        match frame["type"].as_str() {
            Some("stream_event") => {
                let event = &frame["event"];
                match event["type"].as_str() {
                    Some("message_start") => {
                        let usage = &event["message"]["usage"];
                        self.input_tokens = [
                            "input_tokens",
                            "cache_creation_input_tokens",
                            "cache_read_input_tokens",
                        ]
                        .iter()
                        .filter_map(|key| usage[key].as_u64())
                        .sum();
                        if self.input_tokens > 0 {
                            self.emit(E::Context(self.input_tokens));
                        }
                        let id = event["message"]["id"].as_str().unwrap_or_default();
                        self.message = Some((id.to_owned(), Vec::new()));
                        self.emit(E::AssistantStarted);
                    }
                    Some("content_block_start")
                        if event["content_block"]["type"] == "text"
                            && event["index"].as_u64().is_some_and(|index| index > 0) =>
                    {
                        self.emit(E::AssistantBlock);
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                        let text = event["delta"]["text"].as_str().unwrap_or_default();
                        self.emit(E::AssistantText(text.to_owned()));
                    }
                    Some("message_delta") if self.input_tokens > 0 => {
                        let output = event["usage"]["output_tokens"].as_u64().unwrap_or(0);
                        self.emit(E::Context(self.input_tokens + output));
                    }
                    Some("message_stop") => self.end_message(contract::Stop::Completed),
                    _ => {}
                }
            }
            Some("assistant") => self.on_assistant(frame),
            Some("user") if frame["isReplay"] != true => self.on_user(frame),
            Some("system") if frame["subtype"] == "status" => {
                if frame["status"] == "compacting" && !self.compacting {
                    self.compacting = true;
                    self.emit(E::CompactionStarted);
                }
            }
            Some("system") if frame["subtype"] == "compact_boundary" => {
                // The summary may have lost what the model was sent.
                *self.ledger.lock().unwrap() = Ledger::default();
                self.compacting = false;
                self.emit(E::CompactionEnded {
                    compacted: true,
                    tokens: frame["compact_metadata"]["post_tokens"].as_u64(),
                    error: None,
                });
                if let Some(tx) = self.compaction.lock().unwrap().take() {
                    let _ = tx.send(());
                }
            }
            Some("system") if frame["subtype"] == "init" => {
                let running = frame["model"].as_str().map(str::to_owned);
                if running.is_some() && running != self.running {
                    self.running = running;
                    self.emit(E::Ready(self.state()));
                }
            }
            _ => {}
        }
    }

    /// One content block of an assistant message, or a whole message
    /// that wasn't streamed, such as a failed login's.
    fn on_assistant(&mut self, frame: &Value) {
        let message = &frame["message"];
        let id = message["id"].as_str().unwrap_or_default();
        let streamed = self.message.as_ref().is_some_and(|(open, _)| open == id);
        if !streamed {
            self.end_message(contract::Stop::Completed);
            self.message = Some((id.to_owned(), Vec::new()));
            self.emit(contract::Event::AssistantStarted);
        }
        let blocks = message["content"].as_array().map(Vec::as_slice);
        for block in blocks.unwrap_or_default() {
            if block["type"] == "text"
                && let Some(text) = block["text"].as_str().filter(|t| !t.trim().is_empty())
            {
                if let Some((_, texts)) = &mut self.message {
                    texts.push(text.to_owned());
                }
            } else if block["type"] == "tool_use" {
                let call = history::tool_call(block);
                let name = block["name"].as_str().unwrap_or_default().to_owned();
                self.calls
                    .insert(call.id.clone(), (name, block["input"].clone()));
                self.emit(contract::Event::ToolStarted(call));
            }
        }
        if let Some(kind) = frame["error"].as_str() {
            let mut text = self.message.take().map(|(_, texts)| texts.join("\n\n"));
            if kind == "authentication_failed" {
                let hint = "Claude Code isn't logged in. Run `claude` in a terminal and log in, \
                            then send again.";
                text = Some(match text.filter(|text| !text.is_empty()) {
                    Some(text) => format!("{text}\n\n{hint}"),
                    None => hint.into(),
                });
            }
            self.error_reported = true;
            self.emit(contract::Event::AssistantEnded {
                text: String::new(),
                stop: contract::Stop::Error(text.filter(|text| !text.is_empty())),
            });
        } else if !streamed {
            self.end_message(contract::Stop::Completed);
        }
    }

    fn end_message(&mut self, stop: contract::Stop) {
        if let Some((_, texts)) = self.message.take() {
            self.emit(contract::Event::AssistantEnded {
                text: texts.join("\n\n"),
                stop,
            });
        }
    }

    fn on_user(&mut self, frame: &Value) {
        let blocks = frame["message"]["content"].as_array().map(Vec::as_slice);
        let results: Vec<&Value> = blocks
            .unwrap_or_default()
            .iter()
            .filter(|block| block["type"] == "tool_result")
            .collect();
        for block in &results {
            let Some(id) = block["tool_use_id"].as_str() else {
                continue;
            };
            let is_error = block["is_error"] == true;
            // `tool_use_result` belongs to the frame's only result.
            let detail = (results.len() == 1).then(|| &frame["tool_use_result"]);
            let result = contract::ToolResult {
                output: history::content_text(&block["content"]),
                is_error,
                duration: None,
                patch: detail.and_then(history::patch_of),
            };
            let (name, input) = self.calls.remove(id).unwrap_or_default();
            let (draft_write, breach) = self.finished(id, &name, &input, is_error, detail);
            self.emit(contract::Event::ToolFinished {
                id: id.to_owned(),
                result,
                draft_write,
            });
            if let Some(error) = breach {
                self.unenforced(error);
            }
        }
    }

    /// Works out whether call `id` of `name` with `input`, which just
    /// ended, changed the draft, and what the model saw of it; with the
    /// error to stop on when it was a write the hook didn't allow.
    fn finished(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        is_error: bool,
        detail: Option<&Value>,
    ) -> (bool, Option<String>) {
        if let Some(at) = self.approved.iter().position(|approved| approved.id == id) {
            let approved = self.approved.remove(at);
            let now = fs::read_to_string(self.policy.draft()).ok();
            let landed = approved.wrote(now.as_deref());
            // The model knows the draft only if it holds just what the call
            // wrote, not a failed edit or a change made since.
            self.ledger.lock().unwrap().seen = if landed && !is_error { now } else { None };
            return (!is_error || landed, None);
        }
        let path = input["file_path"].as_str().unwrap_or_default();
        match name {
            "Edit" | "Write" if !is_error => (
                true,
                Some(format!(
                    "Claude Code ran {name} on {path} without asking Draftmux, so Draftmux \
                     can't keep its writes to the draft. Editing is off for this draft until \
                     Draftmux restarts."
                )),
            ),
            "Read" if !is_error && self.policy.is_draft(path) => {
                if let Some(shown) = full_read(detail) {
                    // Unless the draft changed after it was read.
                    let now = fs::read_to_string(self.policy.draft()).ok();
                    self.ledger.lock().unwrap().seen = now.filter(|now| now == shown);
                }
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn on_hook(&mut self, request: HookRequest) {
        let decision = catch_unwind(AssertUnwindSafe(|| {
            let tool = request.input["tool_name"].as_str().unwrap_or_default();
            self.policy.decide(tool, &request.input["tool_input"])
        }))
        .unwrap_or_else(|_| Decision::Deny("Draftmux couldn't check this tool call.".into()));
        let Some(id) = request.tool_use_id.clone() else {
            request.deny("Draftmux: the tool call has no id.");
            return;
        };
        match decision {
            Decision::Allow => {
                request.proceed();
            }
            Decision::DraftWrite if !self.editing => {
                request.deny("Draftmux: editing is off for this draft.");
            }
            Decision::DraftWrite => {
                self.approved.push(Approved {
                    id,
                    tool: request.input["tool_name"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    input: request.input["tool_input"].clone(),
                    before: fs::read_to_string(self.policy.draft()).ok(),
                });
                if !request.proceed() {
                    // claude withdrew the call, or the process is gone.
                    self.approved.pop();
                }
            }
            Decision::Deny(reason) => {
                request.deny(&reason);
            }
        }
    }

    fn on_permission(&mut self, request: PermissionRequest) {
        let approved = request
            .tool_use_id
            .as_ref()
            .is_some_and(|id| self.approved.iter().any(|approved| &approved.id == id));
        match request.tool_name.as_str() {
            "Read" | "Glob" | "Grep" => {
                request.allow();
            }
            "Edit" | "Write" if approved => {
                request.allow();
            }
            "Edit" | "Write" => {
                request.deny("Draftmux: this write wasn't checked, so it isn't allowed.");
                self.unenforced(
                    "Claude Code asked to write without running Draftmux's check first, so \
                     Draftmux can't keep its writes to the draft. Editing is off for this \
                     draft until Draftmux restarts."
                        .into(),
                );
            }
            name => {
                let reason = format!("Draftmux: {name} isn't available in this draft.");
                request.deny(&reason);
            }
        }
    }

    /// The hook isn't enforced: stops claude and starts later processes
    /// without Edit and Write.
    fn unenforced(&mut self, error: String) {
        if !self.editing {
            return;
        }
        self.editing = false;
        eprintln!("claude-rpc: {error}");
        if let Some(agent) = self.agent.upgrade() {
            agent.set_tools(READ_ONLY_TOOLS.to_vec());
            agent.stop();
        }
        self.end_run(true);
        self.emit(contract::Event::Crashed {
            error,
            failed_prompt: None,
        });
    }

    /// Ends what the run left open: the streaming message, and the allowed
    /// writes that never reported, which count by what the draft holds.
    fn end_run(&mut self, aborted: bool) {
        self.end_message(if aborted {
            contract::Stop::Aborted
        } else {
            contract::Stop::Completed
        });
        let now = fs::read_to_string(self.policy.draft()).ok();
        let approved = std::mem::take(&mut self.approved);
        if !approved.is_empty() {
            self.ledger.lock().unwrap().seen = None;
        }
        // Edit and Write run one at a time, so a draft holding what one of
        // them would leave means it and every write allowed before it ran.
        let landed = approved
            .iter()
            .rposition(|approved| approved.wrote(now.as_deref()))
            .map_or(0, |last| last + 1);
        for (n, approved) in approved.into_iter().enumerate() {
            let draft_write = n < landed;
            self.emit(contract::Event::ToolFinished {
                id: approved.id,
                result: contract::ToolResult {
                    output: "The tool didn't report a result.".into(),
                    is_error: true,
                    duration: None,
                    patch: None,
                },
                draft_write,
            });
        }
        self.calls.clear();
    }
}

/// A draft write the hook allowed: the call, and the draft's text then.
struct Approved {
    id: String,
    tool: String,
    input: Value,
    before: Option<String>,
}

impl Approved {
    /// Whether the draft, now holding `now`, holds what the call writes.
    fn wrote(&self, now: Option<&str>) -> bool {
        now.is_some_and(|now| {
            self.before.as_deref() != Some(now)
                && policy::expected(&self.tool, &self.input, self.before.as_deref()).as_deref()
                    == Some(now)
        })
    }
}

/// The text a Read's result showed, when that was the whole file.
fn full_read(detail: Option<&Value>) -> Option<&str> {
    let file = &detail?["file"];
    let whole = file["startLine"].as_u64() == Some(1)
        && file["numLines"]
            .as_u64()
            .zip(file["totalLines"].as_u64())
            .is_some_and(|(shown, total)| shown >= total);
    whole.then(|| file["content"].as_str()).flatten()
}
