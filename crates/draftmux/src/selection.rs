//! Selection actions as one-off agent calls that leave the draft's own
//! conversation alone, on the agent the draft runs on.
//!
//! pi: each call forks the draft's session into a temporary session
//! directory: `pi --mode json -p --fork <draft session> --session-dir <temp>
//! -e <ext>`, with the draft's profile and cwd, the prompt on stdin and
//! `DRAFTMUX_MODE=selection`, so the extension blocks every tool call.
//!
//! Claude Code: each call continues a copy of the draft's conversation in the
//! temporary directory as a new session that isn't saved, with no tools
//! ([`draftmux_claude_rpc::Oneshot`]), and gets the prompt on stdin.
//!
//! Both agents load the session before reading stdin, so the agent can start
//! ahead of the call ([`Warm`]) and wait for its prompt. A call takes it only
//! if it would start the same way, from an unchanged conversation file.
//!
//! The temporary directory is deleted once the agent has exited, so nothing
//! reaches the draft's own session or chat. The prompt carries the editor
//! text and the selection, and the agent answers with replacement options as
//! JSON. Continuing the sentence at the cursor is the same call about an
//! empty selection, answered with options or with plain text. Each call logs
//! how long the agent took to start replying, to start its text and to
//! finish.

use std::ffi::OsString;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use draftmux_claude_rpc::{DraftSession, Oneshot};
use draftmux_pi_rpc::ThinkingLevel;
use draftmux_storage::{Backend as DraftBackend, Meta, Target};
use serde::Deserialize;

use crate::process;

mod stream;

pub(crate) use stream::Agent;
use stream::{Format, Stream};

/// A selection call waits for a full model turn over the whole session.
const TIMEOUT: Duration = Duration::from_secs(120);

/// pi gets the prompt on the command line, and macOS limits arguments plus
/// environment to 1 MiB.
const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// Selection-call settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionConfig {
    /// `None` keeps the model the agent restores from the forked session,
    /// which reuses the session's prompt cache. pi takes `provider/model`,
    /// Claude Code a model such as `haiku`.
    pub model: Option<String>,
    /// pi's, sent with `model` only; pi restores the session's otherwise.
    pub thinking: Option<ThinkingLevel>,
    /// Claude Code's `--effort`. Claude Code doesn't restore the session's
    /// effort, so a call on the draft's model passes the draft's.
    pub effort: Option<String>,
    /// How many options each action asks for.
    pub alternatives: usize,
    /// Off runs pi with `--no-context-files`, as the draft's own pi.
    pub context_files: bool,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            model: None,
            thinking: None,
            effort: None,
            alternatives: draftmux_settings::DEFAULT_ALTERNATIVES,
            context_files: true,
        }
    }
}

impl SelectionConfig {
    /// For a draft, from the settings it kept at creation, asking for
    /// `alternatives` options. A Claude draft without a selection model of
    /// its own uses the model and effort chosen for the draft; a selection
    /// model runs at its default effort, which the draft's may not fit.
    pub fn for_draft(meta: &Meta, alternatives: usize) -> Self {
        let settings = &meta.settings;
        if let DraftBackend::Claude(claude) = &meta.backend {
            return match &claude.selection_model {
                Some(model) => Self {
                    model: Some(model.clone()),
                    alternatives,
                    ..Self::default()
                },
                None => Self {
                    model: claude.model.clone(),
                    effort: claude.effort.clone(),
                    alternatives,
                    ..Self::default()
                },
            };
        }
        let model = settings.selection_model.as_ref();
        Self {
            model: model.map(|m| format!("{}/{}", m.provider, m.model_id)),
            thinking: model.and_then(|m| ThinkingLevel::parse(&m.thinking)),
            effort: None,
            alternatives,
            context_files: settings.context_files,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Alternatives,
    /// The prompt of one of the user's selection actions.
    Instruction(String),
    /// Finish the sentence at the cursor, an empty selection: as options, or
    /// as plain text when `text`.
    Continue {
        text: bool,
    },
}

impl Action {
    fn instruction(&self) -> &str {
        match self {
            Action::Alternatives => {
                "Suggest alternative phrasings that preserve the meaning and fit the surrounding text."
            }
            Action::Instruction(text) => text,
            Action::Continue { .. } => "",
        }
    }
}

/// The agent a call runs on: the draft's, with what starting it takes.
pub enum Backend {
    Pi {
        /// A command for the pi executable.
        pi: Command,
        extension: PathBuf,
        /// The draft's pi session file (`PiSession::session_file`).
        session_file: PathBuf,
        agent_dir: PathBuf,
    },
    Claude {
        /// The `claude` executable; its version is checked before the call.
        program: PathBuf,
        /// The login shell's environment, which the call's is taken from.
        env: Vec<(OsString, OsString)>,
        session: DraftSession,
    },
}

/// What a call is about: the selection in the editor's current text.
#[derive(Debug, Clone)]
pub struct Request {
    pub cwd: PathBuf,
    pub target: Target,
    pub text: String,
    pub range: Range<usize>,
    pub action: Action,
}

impl Request {
    pub fn new(meta: &Meta, text: String, range: Range<usize>, action: Action) -> Self {
        Self {
            cwd: meta.cwd.clone(),
            target: meta.target,
            text,
            range,
            action,
        }
    }

    fn selected(&self) -> &str {
        &self.text[self.range.clone()]
    }

    fn format(&self, count: usize) -> Format {
        match self.action {
            Action::Continue { text: true } => Format::Text,
            _ => Format::Options(count),
        }
    }

    /// Fits an option or the streamed text into the text around the
    /// selection: keeps the selection's own whitespace around a
    /// replacement, and spaces a continuation from its neighbours.
    fn fit(&self, option: &str) -> String {
        if let Action::Continue { .. } = self.action {
            let before = self.text[..self.range.start].chars().next_back();
            let after = self.text[self.range.end..].chars().next();
            return join(before, option, after);
        }
        let (leading, trailing) = surrounding_whitespace(self.selected());
        format!("{leading}{option}{trailing}")
    }
}

/// A call's agent started before its request: it loads the session while
/// the user picks an action, then waits for the prompt on stdin. Dropping it
/// kills the agent and removes its temporary directory.
pub struct Warm {
    /// Which calls it fits: the agent, settings and conversation it started
    /// with.
    key: String,
    agent: Agent,
    // Dropped before `temp`, so the agent has exited when it goes.
    process: process::Spawned,
    temp: tempfile::TempDir,
    started: Instant,
}

impl Warm {
    /// Starts the agent for calls with `config` on `backend` in `cwd`.
    pub fn start(
        backend: Backend,
        cwd: &Path,
        config: &SelectionConfig,
        temp_parent: &Path,
    ) -> Result<Self, String> {
        let key = key(&backend, cwd, config)?;
        let temp = tempfile::Builder::new()
            .prefix("draftmux-selection-")
            .tempdir_in(temp_parent)
            .map_err(|err| format!("Couldn't create a temporary session directory: {err}"))?;
        let (agent, command) = match backend {
            Backend::Pi {
                pi,
                extension,
                session_file,
                agent_dir,
            } => {
                let command = command(
                    pi,
                    cwd,
                    &session_file,
                    &agent_dir,
                    &extension,
                    temp.path(),
                    config,
                );
                (Agent::Pi, command)
            }
            Backend::Claude {
                program,
                env,
                session,
            } => {
                crate::chat::check_claude(&program, &env)?;
                let command = oneshot(program, cwd, env, session, config).command(temp.path())?;
                (Agent::Claude, command)
            }
        };
        Ok(Self {
            key,
            agent,
            process: process::Spawned::spawn(agent.name(), command, true)?,
            temp,
            started: Instant::now(),
        })
    }

    /// Whether it can take a call that would start with `key`: an agent
    /// that has exited, or that loaded an older conversation, can't.
    fn fits(&mut self, key: &str) -> bool {
        self.key == key && !self.process.exited()
    }
}

/// Leaves `slot` holding an agent started for calls with `config` on
/// `backend` in `cwd`: the one it holds if that still fits, else a new one.
pub fn prestart(
    slot: &Mutex<Option<Warm>>,
    backend: Backend,
    cwd: &Path,
    config: &SelectionConfig,
    temp_parent: &Path,
) -> Result<(), String> {
    let key = key(&backend, cwd, config)?;
    let unfit = {
        let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if held.as_mut().is_some_and(|warm| warm.fits(&key)) {
            return Ok(());
        }
        held.take()
    };
    // Killed outside the lock, which calls wait on.
    drop(unfit);
    let warm = Warm::start(backend, cwd, config, temp_parent)?;
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(warm);
    Ok(())
}

fn oneshot(
    program: PathBuf,
    cwd: &Path,
    env: Vec<(OsString, OsString)>,
    session: DraftSession,
    config: &SelectionConfig,
) -> Oneshot {
    Oneshot {
        program,
        cwd: cwd.to_path_buf(),
        env,
        session,
        model: config.model.clone(),
        effort: config.effort.clone(),
    }
}

/// What a call on `backend` would start with, down to the conversation
/// file's size and modification time.
fn key(backend: &Backend, cwd: &Path, config: &SelectionConfig) -> Result<String, String> {
    let (agent, conversation) = match backend {
        Backend::Pi { session_file, .. } => (Agent::Pi, Some(session_file.clone())),
        Backend::Claude {
            program,
            env,
            session,
        } => {
            let oneshot = oneshot(program.clone(), cwd, env.clone(), session.clone(), config);
            (Agent::Claude, oneshot.conversation()?)
        }
    };
    let state = conversation.as_ref().map(|path| {
        let meta = std::fs::metadata(path).ok();
        (
            meta.as_ref().map(|meta| meta.len()),
            meta.and_then(|meta| meta.modified().ok()),
        )
    });
    Ok(format!(
        "{agent:?} {cwd:?} {:?} {:?} {:?} {} {conversation:?} {state:?}",
        config.model, config.thinking, config.effort, config.context_files
    ))
}

/// Runs the call on `backend` and returns up to `config.alternatives`
/// replacements for the selection, with the selection's own leading and
/// trailing whitespace kept; or, continuing as text, the one continuation,
/// spaced to fit at the cursor. Runs on `warm` when it fits the call, else
/// starts the agent, with its temporary directory in `temp_parent`; the
/// directory is gone when this returns. `preview` receives provisional
/// complete options (or an empty list to retract them); only the returned
/// options may be applied to the editor. Logs how long each part took.
pub fn run(
    backend: Backend,
    request: &Request,
    config: &SelectionConfig,
    temp_parent: &Path,
    warm: Option<Warm>,
    cancel: &AtomicBool,
    mut preview: impl FnMut(Vec<String>),
) -> Result<Vec<String>, String> {
    let prompt = prompt(request, config.alternatives)?;
    let key = key(&backend, &request.cwd, config)?;
    let warm = warm.and_then(|mut warm| warm.fits(&key).then_some(warm));
    let reused = warm.as_ref().map(|warm| warm.started.elapsed());
    let warm = match warm {
        Some(warm) => warm,
        None => Warm::start(backend, &request.cwd, config, temp_parent)?,
    };
    let Warm {
        agent,
        process,
        temp,
        ..
    } = warm;
    let input = match agent {
        Agent::Pi => prompt,
        Agent::Claude => Oneshot::input(&prompt),
    };
    let with_whitespace = |options: Vec<String>| {
        options
            .into_iter()
            .map(|option| request.fit(&option))
            .collect()
    };
    let mut stream = Stream::new(agent, request.format(config.alternatives));
    let sent = Instant::now();
    // The directory lives until the child has exited, including cancellation.
    let output = process.finish(Some(input.as_str()), TIMEOUT, Some(cancel), |chunk| {
        stream.feed(chunk, &mut |options| preview(with_whitespace(options)))
    });
    drop(temp);
    eprintln!(
        "{} draftmux: selection call on {} {}{}",
        crate::diagnostics::timestamp(),
        agent.name(),
        timings(sent, reused, stream.timings(), &output),
        stream
            .usage()
            .map(|usage| format!(
                "; tokens: {} input, {} cache read, {} cache write, {} output",
                usage.input, usage.cache_read, usage.cache_write, usage.output
            ))
            .unwrap_or_default(),
    );
    let result = match output {
        Ok(()) => stream.finish(),
        Err(err) if cancel.load(Ordering::Relaxed) => Err(err),
        Err(err) => Err(stream.reported_error().unwrap_or(err)),
    }
    .map(with_whitespace);
    if result.is_err() {
        preview(Vec::new());
    }
    result
}

/// How a call went, from sending the prompt at `sent`: when the reply
/// started, when its text started and when the agent exited.
fn timings(
    sent: Instant,
    reused: Option<Duration>,
    (reply, text): (Option<Instant>, Option<Instant>),
    output: &Result<(), String>,
) -> String {
    let since = |at: Option<Instant>| {
        at.map_or("-".to_owned(), |at| {
            format!("{:.2}s", at.duration_since(sent).as_secs_f32())
        })
    };
    let started = match reused {
        Some(waited) => format!("pre-started {:.1}s before", waited.as_secs_f32()),
        None => "started cold".to_owned(),
    };
    let outcome = match output {
        Ok(()) => "done",
        Err(_) => "failed",
    };
    format!(
        "({started}): reply {}, text {}, {outcome} {}",
        since(reply),
        since(text),
        since(Some(Instant::now())),
    )
}

#[allow(clippy::too_many_arguments)]
fn command(
    mut pi: Command,
    cwd: &Path,
    session_file: &Path,
    agent_dir: &Path,
    extension: &Path,
    session_dir: &Path,
    config: &SelectionConfig,
) -> Command {
    // The prompt goes to stdin, which pi reads as the prompt: Windows
    // limits a command line to 32K characters, and a `.cmd` shim's arguments
    // can't span lines.
    pi.args(["--mode", "json"])
        .arg("-p")
        .arg("--fork")
        .arg(session_file)
        .arg("--session-dir")
        .arg(session_dir)
        .arg("-e")
        .arg(extension);
    if !config.context_files {
        pi.arg("--no-context-files");
    }
    if let Some(model) = &config.model {
        pi.arg("--model").arg(model);
        if let Some(thinking) = config.thinking {
            pi.arg("--thinking").arg(thinking.as_str());
        }
    }
    pi.current_dir(cwd)
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .env("DRAFTMUX_MODE", "selection")
        // The captured environment can carry values from a pi session the
        // app was launched from; selection mode reads none of them.
        .env_remove("DRAFTMUX_DRAFT_PATH")
        .env_remove("DRAFTMUX_INSTRUCTIONS");
    pi
}

const START: &str = "<<<SELECTION>>>";
const END: &str = "<<<END SELECTION>>>";
const CURSOR: &str = "<<<CURSOR>>>";

/// What the draft is written in. `pi-extension/draftmux.ts` describes the
/// targets with the same text, and Claude drafts get it as their target
/// platform.
pub(crate) fn syntax(target: Target) -> &'static str {
    match target {
        Target::Github => {
            "GitHub-flavored Markdown, rendered as in a GitHub comment: every line break inside a paragraph shows, so keep each paragraph on one line rather than wrapping it"
        }
        Target::GithubFile => {
            "GitHub-flavored Markdown for a file in a repository, such as README.md, rendered as GitHub shows a repository file: a single line break inside a paragraph shows as a space, so wrap paragraphs and list items to the width the file already uses; frontmatter is part of the file"
        }
        Target::Slack => {
            "Markdown for a Slack message: every line break shows as written; **bold**, *italic*, ~~strike~~, `code`, [text](url) links; :shortcode: emoji and <@U…> mentions work; headings paste as a bold line and tables as a code block; keep list items to one line"
        }
        Target::Rich => {
            "Markdown, pasted as rich text into an email or document (headings, bold, italic, links, lists, quotes, code and tables carry over; no raw HTML, alerts or @mentions; no subject line)"
        }
        Target::Plain => {
            "plain text, such as an email body (no Markdown syntax such as **bold**, # headings, backticks, [text](url) links or tables; write URLs in full; no subject line)"
        }
    }
}

/// Starts with a fixed line, so pi never reads it as a flag (`-…`) or a
/// file reference (`@…`), and Claude Code never as a slash command.
fn prompt(request: &Request, count: usize) -> Result<String, String> {
    if let Action::Continue { text } = request.action {
        return continue_prompt(request, text, count);
    }
    let selected = request.selected().trim();
    if selected.is_empty() {
        return Err("Select some text first".into());
    }
    let text = &request.text;
    let marked = format!(
        "{}{START}{}{END}{}",
        &text[..request.range.start],
        request.selected(),
        &text[request.range.end..]
    );
    let syntax = syntax(request.target);
    let prompt = format!(
        "[Draftmux selection request]\n\
         This is a one-off request about the draft, separate from editing the draft file. \
         Don't edit files or use tools; answer with JSON only.\n\
         \n\
         The current draft, written in {syntax}, with the selected passage between {START} and {END}:\n\
         \n\
         {marked}\n\
         \n\
         Selected passage:\n\
         \n\
         {selected}\n\
         \n\
         Task for the selected passage: {instruction}\n\
         \n\
         Reply with exactly this JSON shape and nothing else: {{\"alternatives\": [\"...\"]}}\n\
         - Give {count} distinct options, each a complete replacement for the selected passage only, \
         fitting the text around it.\n\
         - Write them in {syntax}, keeping the passage's formatting and line breaks where they still fit.\n\
         - No commentary, numbering or code fences.",
        instruction = request.action.instruction(),
    );
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("The draft is too long for a selection action".into());
    }
    Ok(prompt)
}

/// The prompt for continuing at the cursor, `request.range.start`.
fn continue_prompt(request: &Request, text: bool, count: usize) -> Result<String, String> {
    let at = request.range.start;
    let marked = format!("{}{CURSOR}{}", &request.text[..at], &request.text[at..]);
    let syntax = syntax(request.target);
    let reply = if text {
        "Reply with only the text to insert, on one line: no quotes, commentary, \
         explanation or code fences."
            .to_owned()
    } else {
        format!(
            "Reply with exactly this JSON shape and nothing else: {{\"alternatives\": [\"...\"]}}\n\
             - Give {count} distinct options, each only the text to insert at the cursor, on one line.\n\
             - No commentary, numbering or code fences."
        )
    };
    let prompt = format!(
        "[Draftmux continue request]\n\
         This is a one-off request about the draft, separate from editing the draft file. \
         Don't edit files or use tools.\n\
         \n\
         The current draft, written in {syntax}, with the cursor marked by {CURSOR}:\n\
         \n\
         {marked}\n\
         \n\
         Task: write the text to insert at the cursor that finishes the sentence the cursor is in, \
         in the draft's voice and using what this conversation knows. If the cursor is at the end \
         of a sentence, write the next sentence instead. Write only that one sentence or the rest \
         of it, in {syntax}. Your text goes in at the cursor as is: start where the text before \
         the cursor leaves off and don't repeat any of it.\n\
         \n\
         {reply}"
    );
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("The draft is too long to continue".into());
    }
    Ok(prompt)
}

/// The continuation in a plain-text reply, or the part of it streamed so
/// far (`done` false): the first line, without a code fence, a wrapping
/// quote or a preamble line ending in a colon. `None` when there's nothing
/// to show yet, or, when `done`, nothing usable.
fn continuation(reply: &str, done: bool) -> Option<String> {
    let mut text = reply.trim_start();
    if let Some(fenced) = text.strip_prefix("```") {
        // The fence's language tag runs to the end of its line.
        text = fenced.split_once('\n')?.1.trim_start();
    }
    let (first, rest) = match text.split_once('\n') {
        Some((first, rest)) => (first, Some(rest)),
        None => (text, None),
    };
    let preamble = first.trim_end().ends_with(':');
    let line = match rest {
        Some(rest) if preamble && !rest.trim().is_empty() => {
            rest.trim_start().split('\n').next().unwrap_or_default()
        }
        // A colon may yet turn out to end a preamble.
        None if preamble && !done => return None,
        _ => first,
    };
    let mut line = line.trim_end_matches("```");
    if done {
        line = line.trim();
        for (open, close) in [('"', '"'), ('“', '”'), ('`', '`')] {
            if let Some(inner) = line
                .strip_prefix(open)
                .and_then(|inner| inner.strip_suffix(close))
                .filter(|inner| !inner.contains(close))
            {
                line = inner.trim();
            }
        }
    } else {
        line = line.trim_start_matches(['"', '“', '`']);
    }
    (!line.trim().is_empty()).then(|| line.to_owned())
}

/// `continuation` to insert between `before` and `after`, the characters
/// around the cursor: one space between it and the text before it, unless
/// that ends in whitespace, the continuation starts with punctuation that
/// attaches to it, or the cursor is inside a word; and one space after it
/// when a word follows.
fn join(before: Option<char>, continuation: &str, after: Option<char>) -> String {
    let text = continuation.trim();
    if text.is_empty() {
        return String::new();
    }
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let inside_word = word(before) && word(after);
    let attaches = text.starts_with([',', '.', ';', ':', '!', '?', ')', ']', '}', '…', '\'', '’']);
    let space_before = before.is_some_and(|c| !c.is_whitespace()) && !attaches && !inside_word;
    let space_after = !inside_word && (word(after) || matches!(after, Some('(' | '[' | '"' | '“')));
    format!(
        "{}{text}{}",
        if space_before { " " } else { "" },
        if space_after { " " } else { "" }
    )
}

#[derive(Deserialize)]
struct Reply {
    alternatives: Vec<String>,
}

/// The options in `agent`'s reply: the JSON object it contains, even when
/// the model wrapped it in a code fence or text. Empty and repeated options
/// are dropped.
fn parse(agent: &str, output: &str, count: usize) -> Result<Vec<String>, String> {
    let none = || format!("{agent}'s reply had no alternatives");
    let json = output
        .find('{')
        .zip(output.rfind('}'))
        .filter(|(start, end)| start < end)
        .map(|(start, end)| &output[start..=end]);
    let reply: Reply = json
        .and_then(|json| serde_json::from_str(json).ok())
        .ok_or_else(none)?;
    let mut options: Vec<String> = Vec::new();
    for option in reply.alternatives {
        let option = option.trim();
        if !option.is_empty() && !options.iter().any(|o| o == option) {
            options.push(option.to_string());
        }
    }
    options.truncate(count);
    if options.is_empty() {
        return Err(none());
    }
    Ok(options)
}

/// The whitespace before and after the selection's text, which options
/// replace without it.
fn surrounding_whitespace(selected: &str) -> (&str, &str) {
    let start = selected.len() - selected.trim_start().len();
    let end = selected.trim_end().len().max(start);
    (&selected[..start], &selected[end..])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicBool;

    use draftmux_storage::{ClaudeSession, DraftSettings};

    use super::*;

    fn request(text: &str, selected: &str, action: Action) -> Request {
        let start = text.find(selected).unwrap();
        Request {
            cwd: "/repo".into(),
            target: Target::Github,
            text: text.to_string(),
            range: start..start + selected.len(),
            action,
        }
    }

    fn meta(backend: DraftBackend) -> Meta {
        Meta {
            target: Target::Github,
            syntax: None,
            status: draftmux_storage::Status::Active,
            title: String::new(),
            source: None,
            cwd: "/repo".into(),
            backend,
            note: None,
            first_draft_pending: false,
            settings: DraftSettings::default(),
            file: None,
            created_at: Default::default(),
            updated_at: Default::default(),
        }
    }

    #[cfg(unix)]
    fn pi(pi: Command) -> Backend {
        Backend::Pi {
            pi,
            extension: "/ext".into(),
            session_file: "/drafts/x/session/s.jsonl".into(),
            agent_dir: "/profile".into(),
        }
    }

    #[test]
    fn prompt_marks_the_selection_in_the_draft() {
        let request = request(
            "Hello there.\n\n-- a dash line\n",
            "-- a dash line\n",
            Action::Instruction("make it formal".into()),
        );
        let prompt = prompt(&request, 3).unwrap();
        assert!(prompt.starts_with("[Draftmux selection request]\n"));
        assert!(
            prompt.contains("Hello there.\n\n<<<SELECTION>>>-- a dash line\n<<<END SELECTION>>>\n")
        );
        assert!(prompt.contains("Selected passage:\n\n-- a dash line\n\nTask"));
        assert!(prompt.contains("Task for the selected passage: make it formal"));
        assert!(prompt.contains("Give 3 distinct options"));
        assert!(prompt.contains("GitHub-flavored Markdown"));
    }

    #[test]
    fn custom_actions_pass_the_prompt_unchanged() {
        let mut actions = draftmux_settings::default_selection_actions();
        actions.push(draftmux_settings::SelectionAction {
            name: "Custom".into(),
            prompt: "Keep the details.\n  Use a warmer tone.  ".into(),
        });
        for action in actions {
            let request = request(
                "Hi there",
                "there",
                Action::Instruction(action.prompt.clone()),
            );
            assert_eq!(request.action.instruction(), action.prompt);
            assert!(prompt(&request, 3).unwrap().contains(&format!(
                "Task for the selected passage: {}\n\n",
                action.prompt
            )));
        }
    }

    #[test]
    fn syntax_matches_the_draft_prompt() {
        let extension = include_str!("../../../pi-extension/draftmux.ts");
        for &target in Target::ALL {
            let literal = format!("\"{}\"", syntax(target));
            assert!(extension.contains(&literal), "{literal}");
        }
    }

    #[test]
    fn prompt_names_slack_syntax_and_refuses_blank_or_huge_selections() {
        let mut slack = request(
            "Ship it today",
            "today",
            Action::Instruction("Shorten it".into()),
        );
        slack.target = Target::Slack;
        assert!(
            prompt(&slack, 3)
                .unwrap()
                .contains("Markdown for a Slack message")
        );
        slack.target = Target::Plain;
        assert!(
            prompt(&slack, 3)
                .unwrap()
                .contains("plain text, such as an email")
        );

        let blank = request("a \n b", " \n ", Action::Alternatives);
        assert!(prompt(&blank, 3).is_err());

        let long = "word ".repeat(60_000);
        let huge = request(&long, "word", Action::Alternatives);
        assert_eq!(
            prompt(&huge, 3).unwrap_err(),
            "The draft is too long for a selection action"
        );
    }

    #[test]
    fn parse_reads_json_inside_fences_and_text() {
        let output = "Sure!\n```json\n{\"alternatives\": [\"one\", \"two\\nlines\", \" one \", \"\", \"four\"]}\n```\n";
        assert_eq!(
            parse("pi", output, 3).unwrap(),
            ["one", "two\nlines", "four"]
        );
        assert_eq!(parse("pi", output, 1).unwrap(), ["one"]);
    }

    #[test]
    fn parse_rejects_replies_without_options() {
        for output in [
            "No JSON here",
            "{\"alternatives\": []}",
            "{\"other\": 1}",
            "} {",
        ] {
            assert_eq!(
                parse("pi", output, 3).unwrap_err(),
                "pi's reply had no alternatives"
            );
        }
    }

    #[test]
    fn whitespace_around_the_selection_is_kept() {
        assert_eq!(surrounding_whitespace("  word \n"), ("  ", " \n"));
        assert_eq!(surrounding_whitespace("word"), ("", ""));
        assert_eq!(surrounding_whitespace("   "), ("   ", ""));
    }

    #[test]
    fn command_forks_into_the_temp_dir_with_selection_mode() {
        let mut config = SelectionConfig::default();
        let request = request("Hi there", "there", Action::Alternatives);
        let build = |config: &SelectionConfig| {
            let command = command(
                Command::new("pi"),
                &request.cwd,
                Path::new("/drafts/x/session/s.jsonl"),
                Path::new("/profile"),
                Path::new("/ext/draftmux.ts"),
                Path::new("/tmp/s"),
                config,
            );
            let args: Vec<String> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            (command, args)
        };

        let (command, args) = build(&config);
        assert_eq!(
            args,
            [
                "--mode",
                "json",
                "-p",
                "--fork",
                "/drafts/x/session/s.jsonl",
                "--session-dir",
                "/tmp/s",
                "-e",
                "/ext/draftmux.ts"
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/repo")));
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("PI_CODING_AGENT_DIR".as_ref(), Some("/profile".as_ref()))));
        assert!(envs.contains(&("DRAFTMUX_MODE".as_ref(), Some("selection".as_ref()))));
        assert!(envs.contains(&("DRAFTMUX_INSTRUCTIONS".as_ref(), None)));

        config.thinking = Some(ThinkingLevel::High);
        assert_eq!(build(&config).1.len(), 9, "thinking only with a model");
        config.model = Some("anthropic/claude-haiku-5-5".into());
        assert_eq!(
            build(&config).1[9..],
            [
                "--model",
                "anthropic/claude-haiku-5-5",
                "--thinking",
                "high"
            ]
        );

        config.context_files = false;
        assert_eq!(build(&config).1[9], "--no-context-files");
    }

    #[test]
    fn config_for_a_draft_comes_from_its_settings() {
        let mut meta = meta(DraftBackend::pi("/profile"));
        assert_eq!(
            SelectionConfig::for_draft(&meta, 4),
            SelectionConfig {
                alternatives: 4,
                ..SelectionConfig::default()
            }
        );
        meta.settings = DraftSettings {
            instructions: None,
            context_files: false,
            selection_model: Some(draftmux_storage::ModelChoice {
                provider: "anthropic".into(),
                model_id: "claude-haiku-5-5".into(),
                thinking: "xhigh".into(),
            }),
        };
        assert_eq!(
            SelectionConfig::for_draft(&meta, 2),
            SelectionConfig {
                model: Some("anthropic/claude-haiku-5-5".into()),
                thinking: Some(ThinkingLevel::Xhigh),
                effort: None,
                alternatives: 2,
                context_files: false,
            }
        );
    }

    #[test]
    fn a_claude_draft_s_config_takes_its_own_models() {
        let mut claude = ClaudeSession {
            session_id: "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60".parse().unwrap(),
            transcript: None,
            config_dir: None,
            fork: None,
            model: None,
            effort: None,
            selection_model: None,
        };
        let config = |claude: &ClaudeSession| {
            SelectionConfig::for_draft(&meta(DraftBackend::Claude(claude.clone())), 3)
        };
        assert_eq!(config(&claude), SelectionConfig::default());
        claude.model = Some("opus".into());
        claude.effort = Some("low".into());
        assert_eq!(
            config(&claude),
            SelectionConfig {
                model: Some("opus".into()),
                effort: Some("low".into()),
                ..SelectionConfig::default()
            }
        );
        claude.selection_model = Some("haiku".into());
        assert_eq!(
            config(&claude),
            SelectionConfig {
                model: Some("haiku".into()),
                ..SelectionConfig::default()
            }
        );
    }

    /// A fake pi that records its arguments and checks the temp session dir
    /// exists while it runs.
    #[cfg(unix)]
    #[test]
    fn run_returns_options_and_removes_the_temp_dir() {
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("pi");
        fs::write(
            &script,
            r#"cat > "$0.stdin"
while [ "$1" ]; do [ "$1" = --session-dir ] && test -d "$2" && touch "$2/s.jsonl"; shift; done
echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"{\"alternatives\":[\"over here\","}}'
echo '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"alternatives\":[\"over here\",\"here\"]}"}]}}'
echo '{"type":"agent_settled","aborted":false}'"#,
        )
        .unwrap();
        let mut pi = Command::new("/bin/sh");
        pi.arg(&script);
        let parent = tempfile::tempdir().unwrap();
        let mut request = request("Hi there\n", "there\n", Action::Alternatives);
        request.cwd = parent.path().to_path_buf();

        let mut previews = Vec::new();
        let options = run(
            self::pi(pi),
            &request,
            &SelectionConfig::default(),
            parent.path(),
            None,
            &AtomicBool::new(false),
            |options| previews.push(options),
        )
        .unwrap();
        assert_eq!(options, ["over here\n", "here\n"]);
        let prompt = fs::read_to_string(bin.path().join("pi.stdin")).unwrap();
        assert!(
            prompt.contains("<<<SELECTION>>>there\n<<<END SELECTION>>>"),
            "{prompt}"
        );
        assert_eq!(previews[0], ["over here\n"]);
        assert_eq!(previews.last().unwrap(), &options);
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    /// A call takes the agent started ahead while the session is as it
    /// loaded it, and starts another once the session has changed.
    #[cfg(unix)]
    #[test]
    fn a_call_runs_on_the_agent_started_ahead_unless_the_session_changed() {
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("pi");
        fs::write(
            &script,
            r#"echo start >> "$0.starts"
cat > /dev/null
echo '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"{\"alternatives\":[\"here\"]}"}]}}'
echo '{"type":"agent_settled","aborted":false}'"#,
        )
        .unwrap();
        let starts = || {
            fs::read_to_string(bin.path().join("pi.starts"))
                .unwrap()
                .lines()
                .count()
        };
        let session_file = bin.path().join("s.jsonl");
        fs::write(&session_file, "{}\n").unwrap();
        let backend = || {
            let mut pi = Command::new("/bin/sh");
            pi.arg(&script);
            Backend::Pi {
                pi,
                extension: "/ext".into(),
                session_file: session_file.clone(),
                agent_dir: "/profile".into(),
            }
        };
        let parent = tempfile::tempdir().unwrap();
        let mut request = request("Hi there", "there", Action::Alternatives);
        request.cwd = parent.path().to_path_buf();
        let config = SelectionConfig::default();
        let call = |warm| {
            run(
                backend(),
                &request,
                &config,
                parent.path(),
                warm,
                &AtomicBool::new(false),
                |_| {},
            )
        };

        let slot = Mutex::new(None);
        prestart(&slot, backend(), &request.cwd, &config, parent.path()).unwrap();
        // Still fits: kept rather than started again.
        prestart(&slot, backend(), &request.cwd, &config, parent.path()).unwrap();
        assert_eq!(call(slot.lock().unwrap().take()).unwrap(), ["here"]);
        assert_eq!(starts(), 1);

        prestart(&slot, backend(), &request.cwd, &config, parent.path()).unwrap();
        while starts() < 2 {
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::write(&session_file, "{}\n{}\n").unwrap();
        assert_eq!(call(slot.lock().unwrap().take()).unwrap(), ["here"]);
        assert_eq!(starts(), 3, "the stale agent was replaced");
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn failed_process_retracts_complete_previews() {
        let mut pi = Command::new("/bin/sh");
        pi.args(["-c", r#"echo '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"{\"alternatives\":[\"preview\","}}'; exit 1"#]);
        let parent = tempfile::tempdir().unwrap();
        let mut request = request("word", "word", Action::Alternatives);
        request.cwd = parent.path().to_owned();
        let mut previews = Vec::new();
        assert!(
            run(
                self::pi(pi),
                &request,
                &SelectionConfig::default(),
                parent.path(),
                None,
                &AtomicBool::new(false),
                |options| previews.push(options)
            )
            .is_err()
        );
        assert_eq!(previews, [vec!["preview".to_string()], Vec::new()]);
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn run_reports_pi_errors() {
        let mut pi = Command::new("/bin/sh");
        pi.args(["-c", "echo 'No API key found for anthropic' >&2; exit 1"]);
        let parent = tempfile::tempdir().unwrap();
        let mut request = request(
            "Hi there",
            "there",
            Action::Instruction("Shorten it".into()),
        );
        request.cwd = parent.path().to_path_buf();
        let err = run(
            self::pi(pi),
            &request,
            &SelectionConfig::default(),
            parent.path(),
            None,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, "No API key found for anthropic");
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    fn cursor(text: &str, at: &str, text_reply: bool) -> Request {
        let at = text.find(at).unwrap();
        Request {
            cwd: "/repo".into(),
            target: Target::Github,
            text: text.to_string(),
            range: at..at,
            action: Action::Continue { text: text_reply },
        }
    }

    #[test]
    fn continue_prompt_marks_the_cursor_and_asks_for_text_or_options() {
        let request = cursor("The fox jumps. It was", " jumps", true);
        let text = prompt(&request, 3).unwrap();
        assert!(text.starts_with("[Draftmux continue request]\n"), "{text}");
        assert!(text.contains("The fox<<<CURSOR>>> jumps. It was\n"));
        assert!(text.contains("finishes the sentence the cursor is in"));
        assert!(text.contains("Reply with only the text to insert"));
        assert!(!text.contains("alternatives"));

        let options = prompt(&cursor("Ship it", "", false), 4).unwrap();
        assert!(options.contains("<<<CURSOR>>>Ship it"));
        assert!(options.contains("{\"alternatives\": [\"...\"]}"));
        assert!(options.contains("Give 4 distinct options"));

        let long = "word ".repeat(60_000);
        assert_eq!(
            prompt(&cursor(&long, "word", true), 3).unwrap_err(),
            "The draft is too long to continue"
        );
    }

    #[test]
    fn continuation_takes_the_first_line_without_wrappers_or_preambles() {
        let done = |reply| continuation(reply, true);
        assert_eq!(
            done(" jumps over the dog.  ").unwrap(),
            "jumps over the dog."
        );
        assert_eq!(done("\"jumps over.\"").unwrap(), "jumps over.");
        assert_eq!(done("“jumps over.”").unwrap(), "jumps over.");
        assert_eq!(done("```text\njumps over.\n```").unwrap(), "jumps over.");
        assert_eq!(done("jumps over.\n\nMore text.").unwrap(), "jumps over.");
        assert_eq!(
            done("Here's a continuation:\n\njumps over.").unwrap(),
            "jumps over."
        );
        // A quote that only starts the text belongs to it.
        assert_eq!(done("\"Hi,\" she said.").unwrap(), "\"Hi,\" she said.");
        assert_eq!(done("as follows:").unwrap(), "as follows:");
        for empty in ["", "  \n ", "```\n```", "\"\""] {
            assert_eq!(done(empty), None, "{empty:?}");
        }

        let partial = |reply| continuation(reply, false);
        assert_eq!(partial(" jumps ").unwrap(), "jumps ");
        assert_eq!(partial("\"jumps").unwrap(), "jumps");
        assert_eq!(partial("```"), None);
        assert_eq!(partial("Here's a continuation:"), None);
        assert_eq!(partial("Here's one:\njumps").unwrap(), "jumps");
    }

    #[test]
    fn join_spaces_a_continuation_from_its_neighbours() {
        let cases = [
            (Some('x'), "jumps", None, " jumps"),
            (Some('.'), "Next one.", None, " Next one."),
            (Some('"'), "Next.", Some('\n'), " Next."),
            (Some(' '), " jumps ", None, "jumps"),
            (None, "Start.", None, "Start."),
            (Some('\n'), "Start.", Some('\n'), "Start."),
            (Some('x'), ", and more", None, ", and more"),
            (Some('x'), "'s done.", None, "'s done."),
            // Inside a word, the rest of it.
            (Some('u'), "mps", Some('s'), "mps"),
            (Some(','), "and then", Some('t'), " and then "),
            (Some('x'), "jumps over", Some(' '), " jumps over"),
            (Some('x'), "jumps over", Some('.'), " jumps over"),
            (Some('x'), "  ", Some('t'), ""),
        ];
        for (before, text, after, joined) in cases {
            assert_eq!(
                join(before, text, after),
                joined,
                "{before:?} {text:?} {after:?}"
            );
        }
    }

    /// A fake pi that streams `reply` as one text delta per character.
    #[cfg(unix)]
    fn streaming_pi(dir: &Path, reply: &str) -> Command {
        let mut events = String::new();
        for c in reply.chars() {
            let delta = serde_json::json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":c.to_string()}});
            events.push_str(&format!("{delta}\n"));
        }
        let end = serde_json::json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":reply}]}});
        events.push_str(&format!(
            "{end}\n{{\"type\":\"agent_settled\",\"aborted\":false}}\n"
        ));
        fs::write(dir.join("events"), events).unwrap();
        let script = dir.join("pi");
        fs::write(
            &script,
            format!(
                "cat > \"$0.stdin\"\ncat '{}'\n",
                dir.join("events").display()
            ),
        )
        .unwrap();
        let mut pi = Command::new("/bin/sh");
        pi.arg(&script);
        pi
    }

    #[cfg(unix)]
    #[test]
    fn continuing_as_text_streams_the_continuation_spaced_to_fit() {
        let bin = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mut request = cursor("The quick fox\n", "\n", true);
        request.cwd = parent.path().to_path_buf();
        let mut previews = Vec::new();
        let result = run(
            self::pi(streaming_pi(bin.path(), "jumps over.")),
            &request,
            &SelectionConfig::default(),
            parent.path(),
            None,
            &AtomicBool::new(false),
            |options| previews.push(options),
        )
        .unwrap();
        assert_eq!(result, [" jumps over."]);
        assert_eq!(previews.first().unwrap(), &[" j"]);
        assert_eq!(previews.last().unwrap(), &result);
        // Every preview extends the one before.
        for pair in previews.windows(2) {
            assert!(pair[1][0].starts_with(&pair[0][0]), "{pair:?}");
        }
        let prompt = fs::read_to_string(bin.path().join("pi.stdin")).unwrap();
        assert!(prompt.contains("The quick fox<<<CURSOR>>>\n"), "{prompt}");
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn continuing_with_options_spaces_each() {
        let bin = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mut request = cursor("Done.", "", false);
        request.range = 5..5;
        request.cwd = parent.path().to_path_buf();
        let reply = r#"{"alternatives":["Next one.","Then this."]}"#;
        let options = run(
            self::pi(streaming_pi(bin.path(), reply)),
            &request,
            &SelectionConfig::default(),
            parent.path(),
            None,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(options, [" Next one.", " Then this."]);
    }

    #[cfg(unix)]
    #[test]
    fn claude_continues_as_text_from_a_copy_of_the_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let session = claude_session(&tmp.path().join("draft"));
        let before = fs::read(session.owned()).unwrap();
        let parent = tmp.path().join("temp");
        fs::create_dir_all(&parent).unwrap();
        let frames = r#"{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"jumps "}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"over."}}}
{"type":"result","subtype":"success","is_error":false,"result":"jumps over.","stop_reason":"end_turn","terminal_reason":"completed"}"#;
        let mut request = cursor("The fox", "", true);
        request.range = 7..7;
        request.cwd = tmp.path().to_path_buf();
        let mut previews = Vec::new();
        let result = run(
            Backend::Claude {
                program: fake_claude(tmp.path(), frames, 0),
                env: vec![("PATH".into(), "/usr/bin:/bin".into())],
                session: session.clone(),
            },
            &request,
            &SelectionConfig::default(),
            &parent,
            None,
            &AtomicBool::new(false),
            |options| previews.push(options),
        )
        .unwrap();
        assert_eq!(result, [" jumps over."]);
        assert_eq!(
            previews,
            [vec![" jumps".to_string()], vec![" jumps over.".to_string()]]
        );
        let stdin = claude_prompt(tmp.path());
        assert!(
            stdin.starts_with("[Draftmux continue request]\n"),
            "{stdin}"
        );
        assert_eq!(fs::read(session.owned()).unwrap(), before);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);

        // An empty reply is a failure, with the reason.
        let tmp = tempfile::tempdir().unwrap();
        let session = claude_session(&tmp.path().join("draft"));
        let frames = r#"{"type":"result","subtype":"success","is_error":false,"result":"```\n```","stop_reason":"end_turn","terminal_reason":"completed"}"#;
        let mut request = cursor("The fox", "", true);
        request.cwd = tmp.path().to_path_buf();
        let err = run(
            Backend::Claude {
                program: fake_claude(tmp.path(), frames, 0),
                env: vec![("PATH".into(), "/usr/bin:/bin".into())],
                session,
            },
            &request,
            &SelectionConfig::default(),
            tmp.path(),
            None,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, "Claude Code's reply had no continuation");
    }

    /// Needs pi on `PATH` with a working default model in `~/.pi/agent`:
    /// `cargo test -j 2 -p draftmux --bin draftmux real_pi_selection -- --ignored`.
    #[test]
    #[ignore]
    fn real_pi_selection_call_leaves_the_draft_session_alone() {
        let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".pi/agent"));
        let cwd = tempfile::tempdir().unwrap();
        let draft_session = tempfile::tempdir().unwrap();
        let status = Command::new("pi")
            .args(["-p", "Reply with OK and nothing else.", "--session-dir"])
            .arg(draft_session.path())
            .current_dir(cwd.path())
            .env("PI_CODING_AGENT_DIR", &agent_dir)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "creating the source session");
        let session_file = fs::read_dir(draft_session.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let before = fs::read(&session_file).unwrap();

        let text = "Thanks for the detailed report! I'll take a look tomorrow.\n";
        let start = text.find("I'll").unwrap();
        let request = Request {
            cwd: cwd.path().to_path_buf(),
            target: Target::Github,
            text: text.to_string(),
            range: start..text.len(),
            // Tool calls are blocked; the model still answers in text.
            action: Action::Instruction(
                "First run `ls` with bash, then make it more casual.".into(),
            ),
        };
        let parent = tempfile::tempdir().unwrap();
        let extension = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../pi-extension/draftmux.ts"
        ));
        let started = std::time::Instant::now();
        let mut first_preview = None;
        let backend = Backend::Pi {
            pi: Command::new("pi"),
            extension: extension.to_path_buf(),
            session_file: session_file.clone(),
            agent_dir: agent_dir.clone(),
        };
        let options = run(
            backend,
            &request,
            &SelectionConfig::default(),
            parent.path(),
            None,
            &AtomicBool::new(false),
            |options| {
                if !options.is_empty() {
                    first_preview.get_or_insert(started.elapsed());
                }
                eprintln!("preview at {:?}: {options:?}", started.elapsed());
            },
        )
        .unwrap();
        eprintln!(
            "first preview: {first_preview:?}; finished at {:?}; options: {options:?}",
            started.elapsed()
        );
        assert!(first_preview.is_some());
        assert!(!options.is_empty() && options.len() <= 3);
        assert!(options.iter().all(|o| o.ends_with('\n')));

        assert_eq!(
            fs::read(&session_file).unwrap(),
            before,
            "draft session unchanged"
        );
        assert_eq!(fs::read_dir(draft_session.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(parent.path()).unwrap().count(),
            0,
            "temp dir removed"
        );

        // Continuing the sentence at the cursor streams one line of text.
        let continuation = run(
            Backend::Pi {
                pi: Command::new("pi"),
                extension: extension.to_path_buf(),
                session_file: session_file.clone(),
                agent_dir,
            },
            &continue_request(cwd.path()),
            &SelectionConfig::default(),
            parent.path(),
            None,
            &AtomicBool::new(false),
            |options| eprintln!("continuation preview: {options:?}"),
        )
        .unwrap();
        check_continuation(&continuation);
        assert_eq!(fs::read(&session_file).unwrap(), before);
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    /// A request to continue "I'll take a look" after its last word.
    fn continue_request(cwd: &Path) -> Request {
        let text = "Thanks for the detailed report! I'll take a look\n";
        let at = text.len() - 1;
        Request {
            cwd: cwd.to_path_buf(),
            target: Target::Github,
            text: text.to_string(),
            range: at..at,
            action: Action::Continue { text: true },
        }
    }

    fn check_continuation(options: &[String]) {
        eprintln!("continuation: {options:?}");
        let [continuation] = options else {
            panic!("one continuation: {options:?}")
        };
        assert!(continuation.starts_with(' '), "spaced from the word before");
        assert!(continuation.trim().len() > 1);
        assert!(!continuation.contains('\n'));
        assert!(!continuation.contains(CURSOR));
    }

    /// A fake `claude` (`claude.sh`) that answers `--version`, records its
    /// arguments and stdin in `args` and `stdin`, checks the conversation's
    /// copy exists while it runs, and prints `frames`.
    #[cfg(unix)]
    fn fake_claude(dir: &Path, frames: &str, status: i32) -> PathBuf {
        let script = dir.join("claude.sh");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
[ "$1" = --version ] && echo '2.1.293 (Claude Code)' && exit 0
printf '%s\n' "$@" > "{dir}/args"
cat > "{dir}/stdin"
while [ $# -gt 0 ]; do [ "$1" = --resume ] && test -f "$2" && touch "{dir}/copy-existed"; shift; done
cat <<'FRAMES'
{frames}
FRAMES
exit {status}
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// The prompt the fake claude in `dir` got: the text of the one user
    /// message on its stdin.
    fn claude_prompt(dir: &Path) -> String {
        let stdin = fs::read_to_string(dir.join("stdin")).unwrap();
        let mut lines = stdin.lines();
        let message: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(lines.next(), None);
        assert_eq!(message["type"], "user");
        message["message"]["content"].as_str().unwrap().to_owned()
    }

    /// A Claude draft whose conversation is the claude-rpc fixture, adopted
    /// into `dir`.
    fn claude_session(dir: &Path) -> DraftSession {
        let session = DraftSession {
            id: "11111111-1111-4111-8111-111111111111".into(),
            dir: dir.to_path_buf(),
            config_dir: Some(dir.join("config")),
            fork_boundary: None,
            transcript: None,
        };
        fs::create_dir_all(dir).unwrap();
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../claude-rpc/tests/fixtures/transcripts/fork-source.jsonl"
            ),
            session.owned(),
        )
        .unwrap();
        session
    }

    #[cfg(unix)]
    fn run_claude(
        tmp: &Path,
        frames: &str,
        status: i32,
        previews: &mut Vec<Vec<String>>,
    ) -> Result<Vec<String>, String> {
        let session = claude_session(&tmp.join("draft"));
        let parent = tmp.join("temp");
        fs::create_dir_all(&parent).unwrap();
        let mut request = request("Hi there\n", "there\n", Action::Alternatives);
        request.cwd = tmp.to_path_buf();
        let config = SelectionConfig {
            model: Some("haiku".into()),
            ..SelectionConfig::default()
        };
        let before = fs::read(session.owned()).unwrap();
        let backend = Backend::Claude {
            program: fake_claude(tmp, frames, status),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            session: session.clone(),
        };
        let result = run(
            backend,
            &request,
            &config,
            &parent,
            None,
            &AtomicBool::new(false),
            |options| previews.push(options),
        );
        assert_eq!(fs::read(session.owned()).unwrap(), before);
        assert_eq!(fs::read_dir(&session.dir).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(&parent).unwrap().count(),
            0,
            "temp dir removed"
        );
        result
    }

    #[cfg(unix)]
    #[test]
    fn claude_answers_from_a_copy_of_the_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let frames = r#"{"type":"system","subtype":"init"}
{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}
{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text","text":""}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"{\"alternatives\":[\"over here\","}}}
{"type":"assistant","message":{"content":[{"type":"text","text":"{\"alternatives\":[\"over here\",\"here\"]}"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"{\"alternatives\":[\"over here\",\"here\"]}","stop_reason":"end_turn","terminal_reason":"completed"}"#;
        let mut previews = Vec::new();
        let options = run_claude(tmp.path(), frames, 0, &mut previews).unwrap();
        assert_eq!(options, ["over here\n", "here\n"]);
        assert_eq!(previews[0], ["over here\n"]);
        assert_eq!(previews.last().unwrap(), &options);

        let args = fs::read_to_string(tmp.path().join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert!(args.starts_with(&[
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json"
        ]));
        for flag in [
            "--no-session-persistence",
            "--fork-session",
            "--strict-mcp-config",
        ] {
            assert!(args.contains(&flag), "{flag}");
        }
        let tools = args.iter().position(|a| *a == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        let model = args.iter().position(|a| *a == "--model").unwrap();
        assert_eq!(args[model + 1], "haiku");
        assert!(tmp.path().join("copy-existed").exists());
        let stdin = claude_prompt(tmp.path());
        assert!(
            stdin.starts_with("[Draftmux selection request]\n"),
            "{stdin}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_failures_retract_previews_and_say_why() {
        // A missing login: claude reports it, then exits 1.
        let tmp = tempfile::tempdir().unwrap();
        let frames = r#"{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"{\"alternatives\":[\"early\","}}}
{"type":"assistant","error":"authentication_failed","message":{"content":[{"type":"text","text":"Not logged in · Please run /login"}]}}
{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","terminal_reason":"api_error"}"#;
        let mut previews = Vec::new();
        let err = run_claude(tmp.path(), frames, 1, &mut previews).unwrap_err();
        assert!(err.contains("isn't logged in"), "{err}");
        assert_eq!(previews.first().unwrap(), &["early\n"]);
        assert!(previews.last().unwrap().is_empty());

        // A reply with nothing but thinking.
        let tmp = tempfile::tempdir().unwrap();
        let frames = r#"{"type":"user","isSynthetic":true,"message":{"content":[{"type":"text","text":"[Your previous response had no visible output.]"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"","stop_reason":"end_turn","terminal_reason":"completed"}"#;
        let err = run_claude(tmp.path(), frames, 0, &mut Vec::new()).unwrap_err();
        assert_eq!(err, "Claude Code's reply had no alternatives");

        // No result before exiting.
        let tmp = tempfile::tempdir().unwrap();
        let frames = r#"{"type":"system","subtype":"init"}"#;
        let err = run_claude(tmp.path(), frames, 0, &mut Vec::new()).unwrap_err();
        assert_eq!(err, "Claude Code didn't finish the selection request");
    }

    #[cfg(unix)]
    #[test]
    fn a_claude_draft_whose_transcript_is_lost_runs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut session = claude_session(&tmp.path().join("draft"));
        fs::remove_file(session.owned()).unwrap();
        session.transcript = Some(session.owned());
        let mut request = request("Hi there", "there", Action::Alternatives);
        request.cwd = tmp.path().to_path_buf();
        let backend = Backend::Claude {
            program: fake_claude(tmp.path(), "", 0),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            session,
        };
        let err = run(
            backend,
            &request,
            &SelectionConfig::default(),
            tmp.path(),
            None,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("transcript"), "{err}");
        assert!(!tmp.path().join("args").exists(), "claude never ran");
    }

    /// Needs a logged-in Claude Code 2.1.293 or later as `claude` on `PATH`:
    /// `cargo test -j 2 -p draftmux --bin draftmux real_claude_selection -- --ignored`.
    /// Leaves no transcript behind.
    #[test]
    #[ignore]
    fn real_claude_selection_call_leaves_the_draft_session_alone() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let program = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("claude"))
            .find(|path| path.is_file())
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().canonicalize().unwrap().join("cwd");
        fs::create_dir(&cwd).unwrap();
        let mut session = claude_session(&tmp.path().join("draft"));
        // Keychain logins don't follow another config dir.
        session.config_dir = None;
        let before = fs::read(session.owned()).unwrap();

        let text = "Thanks for the detailed report! I'll take a look tomorrow.\n";
        let start = text.find("I'll").unwrap();
        let request = Request {
            cwd: cwd.clone(),
            target: Target::Github,
            text: text.to_string(),
            range: start..text.len(),
            action: Action::Instruction("Make it more casual.".into()),
        };
        let parent = tmp.path().join("temp");
        fs::create_dir(&parent).unwrap();
        let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let mut first_preview = false;
        let options = run(
            Backend::Claude {
                program: program.clone(),
                env,
                session: session.clone(),
            },
            &request,
            &SelectionConfig {
                model: Some("haiku".into()),
                ..SelectionConfig::default()
            },
            &parent,
            None,
            &AtomicBool::new(false),
            |options| first_preview |= !options.is_empty(),
        )
        .unwrap();
        eprintln!("options: {options:?}");
        assert!(first_preview);
        assert!(!options.is_empty() && options.len() <= 3);
        assert!(options.iter().all(|o| o.ends_with('\n')));
        assert_eq!(fs::read(session.owned()).unwrap(), before);
        assert_eq!(fs::read_dir(&session.dir).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(&parent).unwrap().count(),
            0,
            "temp dir removed"
        );

        // Continuing the sentence at the cursor streams one line of text.
        let continuation = run(
            Backend::Claude {
                program,
                env: std::env::vars_os().collect(),
                session: session.clone(),
            },
            &continue_request(&cwd),
            &SelectionConfig {
                model: Some("haiku".into()),
                ..SelectionConfig::default()
            },
            &parent,
            None,
            &AtomicBool::new(false),
            |options| eprintln!("continuation preview: {options:?}"),
        )
        .unwrap();
        check_continuation(&continuation);
        assert_eq!(fs::read(session.owned()).unwrap(), before);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);

        // Claude Code names the project directory after the cwd; whatever it
        // made for this temporary one must hold no transcript.
        let name = cwd.to_str().unwrap().replace(['/', '.', '_'], "-");
        let project = home.join(".claude/projects").join(&name);
        if project.exists() {
            let files: Vec<_> = walk(&project);
            assert!(files.is_empty(), "left behind: {files:?}");
            let memory = project.join("memory");
            if memory.is_dir() {
                fs::remove_dir(memory).unwrap();
            }
            fs::remove_dir(&project).unwrap();
        }
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
        files
    }
}
