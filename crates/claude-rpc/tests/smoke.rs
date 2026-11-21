//! Live checks against the installed `claude` (`CLAUDE_BIN`, by default
//! `~/.local/bin/claude`), with synthetic prompts in a temporary directory.
//! They use the account's login and spend a few `haiku` turns, so they are
//! ignored by default:
//!
//! ```sh
//! cargo test -j 2 -p draftmux-claude-rpc --test smoke -- --ignored
//! ```
//!
//! The transcripts the CLI writes under `~/.claude/projects/` for the
//! temporary directory are removed afterwards; a draft's are in another
//! temporary directory.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use draftmux_agent::{Agent as _, Event, Message};
use draftmux_claude_rpc::{
    Agent, AgentEvent, ClaudeAgent, Config, DraftContext, DraftSession, Error, LaunchSpec, Session,
    SettingSource, Snapshot, transcript,
};
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use futures::executor::block_on;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(90);

fn program() -> PathBuf {
    std::env::var_os("CLAUDE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/bin/claude"))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

/// A temporary working directory whose native transcripts are removed on
/// drop.
struct Scratch {
    dir: tempfile::TempDir,
    cwd: PathBuf,
    /// The session whose process must be gone first: claude appends to the
    /// transcript as it exits.
    session: String,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        Self {
            dir,
            cwd,
            session: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// `~/.claude/projects/<cwd with / and . as ->`.
    fn projects(&self) -> PathBuf {
        let encoded: String = self
            .cwd
            .to_str()
            .unwrap()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        home().join(".claude/projects").join(encoded)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while running(&self.session) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let projects = self.projects();
        // Only a directory named after this temporary directory.
        let name = self.dir.path().file_name().unwrap().to_str().unwrap();
        assert!(
            projects
                .to_str()
                .unwrap()
                .ends_with(&name.replace('.', "-"))
        );
        let _ = std::fs::remove_dir_all(projects);
    }
}

fn spec(cwd: &Path, session: Session) -> LaunchSpec {
    LaunchSpec {
        program: program(),
        cwd: cwd.to_path_buf(),
        env: std::env::vars_os().collect(),
        extra_env: vec![],
        model: Some("haiku".into()),
        effort: None,
        tools: vec![],
        setting_sources: vec![],
        session: Some(session),
        hooks: vec![],
        permission_prompt: true,
        system_prompt: None,
    }
}

fn config() -> Config {
    Config {
        idle_timeout: Duration::from_secs(120),
        request_timeout: Duration::from_secs(30),
        max_record: 16 << 20,
    }
}

fn next(events: &mut UnboundedReceiver<AgentEvent>) -> AgentEvent {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(event) => return event,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Err(err) => panic!("no event within {TIMEOUT:?}: {err:?}"),
        }
    }
}

/// Frames until the run settles, and how it settled.
fn run(events: &mut UnboundedReceiver<AgentEvent>) -> (Vec<Value>, bool, Option<String>) {
    let mut frames = Vec::new();
    loop {
        match next(events) {
            AgentEvent::Frame(frame) => frames.push(frame),
            AgentEvent::RunSettled { aborted, error } => return (frames, aborted, error),
            AgentEvent::Crashed { error, .. } => panic!("crashed: {error}"),
            _ => {}
        }
    }
}

fn text(frames: &[Value]) -> (String, String) {
    let deltas = frames
        .iter()
        .filter(|f| f["type"] == "stream_event" && f["event"]["delta"]["type"] == "text_delta")
        .filter_map(|f| f["event"]["delta"]["text"].as_str())
        .collect();
    let blocks = frames
        .iter()
        .filter(|f| f["type"] == "assistant")
        .flat_map(|f| {
            f["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .collect();
    (deltas, blocks)
}

/// Whether a process with `needle` in its arguments is running.
fn running(needle: &str) -> bool {
    let out = Command::new("pgrep")
        .arg("-f")
        .arg(needle)
        .output()
        .unwrap();
    !out.stdout.is_empty()
}

#[test]
#[ignore = "runs the real claude"]
fn live_turns_interrupt_and_models() {
    let scratch = Scratch::new();
    let id = scratch.session.clone();
    let (agent, mut events) = Agent::new(spec(&scratch.cwd, Session::New(id.clone())), config());

    block_on(agent.prompt("Reply with exactly the word OK.".into())).unwrap();
    let AgentEvent::Ready(init) = next(&mut events) else {
        panic!("expected Ready first");
    };
    assert!(init.models.iter().any(|m| m.value == "haiku"), "{init:?}");
    assert!(matches!(next(&mut events), AgentEvent::RunStarted));
    let (frames, aborted, error) = run(&mut events);
    assert_eq!((aborted, error), (false, None));
    let (deltas, blocks) = text(&frames);
    assert_eq!(deltas, blocks, "streamed text equals the final text");
    assert!(blocks.contains("OK"), "{blocks}");
    assert!(running(&id));

    block_on(agent.prompt("Write the numbers from 1 to 400 as words, one per line.".into()))
        .unwrap();
    assert!(matches!(next(&mut events), AgentEvent::RunStarted));
    loop {
        if let AgentEvent::Frame(frame) = next(&mut events)
            && frame["type"] == "stream_event"
            && frame["event"]["delta"]["type"] == "text_delta"
        {
            break;
        }
    }
    let started = Instant::now();
    block_on(agent.interrupt()).unwrap();
    let acked = started.elapsed();
    let (_, aborted, error) = run(&mut events);
    assert!(aborted, "the run was interrupted");
    assert_eq!(error, None);
    eprintln!(
        "interrupt acknowledged after {acked:?}, settled after {:?}",
        started.elapsed()
    );

    assert!(matches!(
        block_on(agent.set_model("not-a-model".into())),
        Err(Error::Rejected(_))
    ));
    block_on(agent.set_model("sonnet".into())).unwrap();
    // The switch is echoed as a replayed `user` frame outside any run.
    let AgentEvent::Frame(echo) = next(&mut events) else {
        panic!("expected the model switch echo");
    };
    assert_eq!(
        (&echo["type"], &echo["isReplay"]),
        (&"user".into(), &true.into())
    );
    block_on(agent.prompt("Reply with exactly the word OK.".into())).unwrap();
    assert!(matches!(next(&mut events), AgentEvent::RunStarted));
    let (frames, aborted, error) = run(&mut events);
    assert_eq!((aborted, error), (false, None));
    let init = frames
        .iter()
        .find(|f| f["type"] == "system" && f["subtype"] == "init")
        .unwrap();
    assert!(init["model"].as_str().unwrap().contains("sonnet"), "{init}");

    drop(agent);
    let deadline = Instant::now() + Duration::from_secs(10);
    while running(&id) {
        assert!(Instant::now() < deadline, "claude still running after drop");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "runs the real claude"]
fn live_missing_session() {
    let scratch = Scratch::new();
    let id = scratch.session.clone();
    let (agent, mut events) = Agent::new(
        spec(&scratch.cwd, Session::Resume(id.clone().into())),
        config(),
    );
    assert_eq!(block_on(agent.prompt("hi".into())), Err(Error::Exited));
    let mut early_result = false;
    loop {
        match next(&mut events) {
            AgentEvent::Frame(frame) if frame["type"] == "result" => early_result = true,
            AgentEvent::Crashed {
                error,
                failed_prompt,
            } => {
                assert!(error.contains("No conversation found"), "{error}");
                assert_eq!(failed_prompt, None);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(early_result);
    assert!(!running(&id));
}

fn wait_gone(id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while running(id) {
        assert!(Instant::now() < deadline, "claude still running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn transcript_event(events: &mut UnboundedReceiver<AgentEvent>) -> PathBuf {
    loop {
        match next(events) {
            AgentEvent::Transcript(path) => return path,
            AgentEvent::Crashed { error, .. } => panic!("crashed: {error}"),
            _ => {}
        }
    }
}

fn reply(events: &mut UnboundedReceiver<AgentEvent>) -> String {
    let (frames, aborted, error) = run(events);
    assert_eq!((aborted, error), (false, None));
    text(&frames).1
}

#[test]
#[ignore = "runs the real claude"]
fn live_draft_fork_is_adopted_resumed_and_read_back() {
    let scratch = Scratch::new();
    let source_id = scratch.session.clone();
    let (agent, mut events) = Agent::new(
        spec(&scratch.cwd, Session::New(source_id.clone())),
        config(),
    );
    block_on(
        agent.prompt("Remember the code word PELICAN-42. Reply with exactly the word OK.".into()),
    )
    .unwrap();
    reply(&mut events);
    drop(agent);
    wait_gone(&source_id);
    let source = scratch.projects().join(format!("{source_id}.jsonl"));
    let source_bytes = std::fs::read(&source).unwrap();

    let draft_dir = tempfile::tempdir().unwrap();
    let snapshot = Snapshot::take(&source, draft_dir.path()).unwrap();
    assert_eq!(snapshot.session_id, source_id);
    assert_eq!(snapshot.cwd, scratch.cwd);
    let draft = DraftSession {
        id: uuid::Uuid::new_v4().to_string(),
        dir: draft_dir.path().to_path_buf(),
        config_dir: None,
        fork_boundary: Some(snapshot.boundary.clone()),
        transcript: None,
    };
    let native = scratch.projects().join(format!("{}.jsonl", draft.id));
    let (agent, mut events) =
        Agent::new(spec(&scratch.cwd, Session::Draft(draft.clone())), config());
    let before = agent.history().unwrap();
    assert!(
        before.earlier >= 2 && before.messages.is_empty(),
        "{before:?}"
    );

    let recall = "What was the code word? Reply with only the code word.";
    block_on(agent.prompt(recall.into())).unwrap();
    let first = reply(&mut events);
    assert!(
        first.contains("PELICAN-42"),
        "the fork has the source context: {first}"
    );
    assert_eq!(transcript_event(&mut events), native);
    agent.stop();
    assert_eq!(transcript_event(&mut events), draft.owned());
    assert!(!native.exists() && draft.owned().is_file(), "adopted");

    let again = "Reply with exactly the word DONE.";
    block_on(agent.prompt(again.into())).unwrap();
    let second = reply(&mut events);
    drop(agent);
    wait_gone(&draft.id);
    assert!(
        !native.exists(),
        "resumed in the draft, not in Claude's storage"
    );

    // As the app restarts, with the recorded transcript.
    let restarted = DraftSession {
        transcript: Some(draft.owned()),
        ..draft
    };
    let (agent, _events) = Agent::new(spec(&scratch.cwd, Session::Draft(restarted)), config());
    let history = agent.history().unwrap();
    assert_eq!(history.earlier, before.earlier);
    let assistant = |text: &str| Message::Assistant {
        text: text.trim().into(),
        tool_calls: vec![],
        stop: draftmux_agent::Stop::Completed,
    };
    assert_eq!(
        history.messages,
        [
            Message::User(recall.into()),
            assistant(&first),
            Message::User(again.into()),
            assistant(&second),
        ]
    );
    let source_lines = transcript::parse(&source_bytes[..]).unwrap();
    assert_eq!(
        draftmux_claude_rpc::history(&source_lines, None)
            .messages
            .len(),
        before.earlier
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        source_bytes,
        "the source is unchanged"
    );
}

#[test]
#[ignore = "runs the real claude"]
fn live_draft_keeps_the_instruction_files_of_its_source() {
    let scratch = Scratch::new();
    let claude_md = scratch.cwd.join("CLAUDE.md");
    std::fs::write(&claude_md, "End every reply with the word PINEAPPLE.\n").unwrap();
    let source_id = scratch.session.clone();
    let mut source_spec = spec(&scratch.cwd, Session::New(source_id.clone()));
    source_spec.setting_sources = vec![SettingSource::Project];
    let (agent, mut events) = Agent::new(source_spec, config());
    block_on(agent.prompt("Reply with the word OK.".into())).unwrap();
    reply(&mut events);
    drop(agent);
    wait_gone(&source_id);
    let source = scratch.projects().join(format!("{source_id}.jsonl"));

    let draft_dir = tempfile::tempdir().unwrap();
    let snapshot = Snapshot::take(&source, draft_dir.path()).unwrap();
    let draft = DraftSession {
        id: uuid::Uuid::new_v4().to_string(),
        dir: draft_dir.path().to_path_buf(),
        config_dir: None,
        fork_boundary: Some(snapshot.boundary.clone()),
        transcript: None,
    };
    // The fork's first process, then one resuming the adopted transcript.
    for _ in 0..2 {
        let (agent, mut events) =
            Agent::new(spec(&scratch.cwd, Session::Draft(draft.clone())), config());
        block_on(agent.prompt("Reply with the word OK.".into())).unwrap();
        reply(&mut events);
        drop(agent);
        wait_gone(&draft.id);
    }

    let instructions: Vec<Value> = std::fs::read_to_string(draft.owned())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["attachment"]["type"] == "instructions")
        .map(|line| line["attachment"].clone())
        .collect();
    assert!(!instructions.is_empty());
    for attachment in &instructions {
        let removed = attachment["removed"].as_array().map_or(0, Vec::len);
        assert_eq!(removed, 0, "{instructions:#?}");
    }
}

#[test]
#[ignore = "runs the real claude"]
fn live_draft_editing_is_limited_to_the_draft() {
    let scratch = Scratch::new();
    let id = scratch.session.clone();
    let draft_dir = scratch.cwd.join("drafts/one");
    std::fs::create_dir_all(&draft_dir).unwrap();
    let draft = draft_dir.join("draft.md");
    std::fs::write(&draft, "I like apples.\n").unwrap();
    let outside = scratch.cwd.join("outside.md");
    let spec = spec(&scratch.cwd, Session::New(id.clone()));
    let context = DraftContext {
        draft_path: draft.clone(),
        instructions: String::new(),
        target: Box::new(|| Some("Markdown for a forum post titled ZEBRA-7".into())),
    };
    let (agent, mut stream, _records) = ClaudeAgent::new(spec, config(), context).unwrap();
    let (tx, events) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Some(event) = block_on(stream.next()) {
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    let prompt = format!(
        "Do these in order, without asking: 1. Use the Edit tool to replace \"apples\" with \
         \"pears\" in the draft. 2. Use the Write tool to create {} containing \"x\". \
         3. Reply with only the title of the forum post the target platform names.",
        outside.display()
    );
    block_on(agent.prompt(prompt.clone())).unwrap();
    let mut finished = Vec::new();
    let mut reply = String::new();
    loop {
        let event = events.recv_timeout(TIMEOUT).expect("an event");
        match event {
            Event::ToolFinished {
                id, draft_write, ..
            } => finished.push((id, draft_write)),
            Event::AssistantEnded { text, .. } => reply.push_str(&text),
            Event::Crashed { error, .. } => panic!("crashed: {error}"),
            Event::RunSettled { aborted } => {
                assert!(!aborted);
                break;
            }
            _ => {}
        }
    }
    eprintln!("tools {finished:?}, reply {reply:?}");
    assert_eq!(std::fs::read_to_string(&draft).unwrap(), "I like pears.\n");
    assert!(!outside.exists(), "the outside write was refused");
    assert_eq!(finished.iter().filter(|(_, wrote)| *wrote).count(), 1);
    assert!(
        reply.contains("ZEBRA-7"),
        "the target reached the model: {reply}"
    );
    drop(agent);
    wait_gone(&id);
    // The context blocks went to claude ahead of the prompt, and the
    // history shows the prompt alone.
    let native = std::fs::read(scratch.projects().join(format!("{id}.jsonl"))).unwrap();
    let native = String::from_utf8(native).unwrap();
    assert!(native.contains("<draftmux-draft>") && native.contains("ZEBRA-7"));
    let lines = transcript::parse(native.as_bytes()).unwrap();
    assert_eq!(
        draftmux_claude_rpc::history(&lines, None).messages.first(),
        Some(&Message::User(prompt))
    );
}

#[test]
#[ignore = "runs the real claude"]
fn live_draft_compaction_keeps_the_target_and_the_draft() {
    let scratch = Scratch::new();
    let id = scratch.session.clone();
    let draft_dir = scratch.cwd.join("drafts/one");
    std::fs::create_dir_all(&draft_dir).unwrap();
    let draft = draft_dir.join("draft.md");
    std::fs::write(&draft, "The launch moved to KESTREL week.\n").unwrap();
    let spec = spec(&scratch.cwd, Session::New(id.clone()));
    let context = DraftContext {
        draft_path: draft.clone(),
        instructions: String::new(),
        target: Box::new(|| Some("Markdown for a forum post titled ZEBRA-7".into())),
    };
    let (agent, mut stream, _records) = ClaudeAgent::new(spec, config(), context).unwrap();
    let (tx, events) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Some(event) = block_on(stream.next()) {
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    let settle = || {
        let mut seen = Vec::new();
        loop {
            let event = events.recv_timeout(TIMEOUT).expect("an event");
            if let Event::Crashed { error, .. } = &event {
                panic!("crashed: {error}");
            }
            let settled = matches!(event, Event::RunSettled { .. });
            seen.push(event);
            if settled {
                return seen;
            }
        }
    };
    block_on(agent.prompt("Remember that the reviewer is called Odile. Reply with OK.".into()))
        .unwrap();
    let first = settle();
    assert!(first.iter().any(|event| matches!(event, Event::Context(_))));

    block_on(agent.compact("Keep the target and the reviewer's name.".into())).unwrap();
    let compaction = settle();
    let ended = compaction
        .iter()
        .find_map(|event| match event {
            Event::CompactionEnded {
                compacted, tokens, ..
            } => Some((*compacted, *tokens)),
            _ => None,
        })
        .expect("compaction ended");
    eprintln!("compaction {compaction:?}");
    assert!(ended.0, "compacted");
    assert!(ended.1.is_some(), "the size after was reported");

    block_on(
        agent.prompt(
            "Reply with only these, comma-separated: the forum post's title, the reviewer's name, \
         and the week the draft names."
                .into(),
        ),
    )
    .unwrap();
    let reply: String = settle()
        .into_iter()
        .filter_map(|event| match event {
            Event::AssistantEnded { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    eprintln!("reply {reply:?}");
    for word in ["ZEBRA-7", "Odile", "KESTREL"] {
        assert!(reply.contains(word), "{word} survived compaction: {reply}");
    }
    drop(agent);
    wait_gone(&id);
}
