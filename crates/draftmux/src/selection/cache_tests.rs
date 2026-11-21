//! Opt-in checks against the real agents that a draft's requests read what
//! the earlier ones cached, in the order a user makes them. A request that
//! starts over writes the whole conversation again, which is slower and
//! costs more, so after the first request each must read most of its input
//! from the cache. They call models and need working logins:
//!
//! ```sh
//! cargo test -j 2 -p draftmux --bin draftmux cache_ -- --ignored --nocapture
//! ```
//!
//! pi uses the profile in `PI_CODING_AGENT_DIR` (default `~/.pi/agent`) and
//! its default model, or `DRAFTMUX_TEST_PI_MODEL` (`provider/model`). How a
//! provider caches differs (Anthropic by content and breakpoints with a
//! retention, OpenAI by a key pi takes from the session ID), so run it on
//! each kind of model the drafts use.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use std::{fs, io::Write};

use draftmux_pi_rpc::{Agent as PiAgent, AgentEvent, Disposition, Event, LaunchSpec};
use futures::executor::block_on;
use serde_json::Value;

use super::stream::Usage;
use super::*;

/// The least share of a request's input that must come from the cache.
const CACHED: f64 = 0.8;

/// Enough text that the conversation is far above every provider's
/// minimum cacheable prompt. Varied words, not one repeated, like real text.
fn notes() -> String {
    let words = [
        "release", "build", "test", "deploy", "cache", "token", "latency", "session", "draft",
        "model", "agent", "prompt", "review", "merge", "branch", "issue",
    ];
    let mut seed = 7u32;
    let filler: Vec<&str> = (0..9000)
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            words[(seed >> 16) as usize % words.len()]
        })
        .collect();
    format!(
        "Here are my notes for the release post. Acknowledge them with OK.\n\n{}",
        filler.join(" ")
    )
}

fn assert_cached(name: &str, usage: Option<Usage>) {
    let usage = usage.unwrap_or_else(|| panic!("{name}: no usage reported"));
    let total = usage.input + usage.cache_read + usage.cache_write;
    let share = usage.cache_read as f64 / total.max(1) as f64;
    eprintln!(
        "{name}: {total} input tokens, {} read from the cache ({:.0}%), {} written",
        usage.cache_read,
        share * 100.0,
        usage.cache_write
    );
    assert!(
        share >= CACHED,
        "{name} read {:.0}% of its input from the cache, under {:.0}%: {usage:?}",
        share * 100.0,
        CACHED * 100.0
    );
}

fn alternatives(cwd: &Path, word: &str) -> Request {
    let text = format!("Hello team, a {word} update on the release.\n");
    let start = text.find(word).unwrap();
    Request {
        cwd: cwd.to_path_buf(),
        target: Target::Slack,
        text: text.clone(),
        range: start..start + word.len(),
        action: Action::Alternatives,
    }
}

fn select(backend: Backend, request: &Request, temp: &Path) -> Option<Usage> {
    let mut usage = None;
    run_with_usage(
        backend,
        request,
        &SelectionConfig::default(),
        temp,
        None,
        &AtomicBool::new(false),
        &mut usage,
        |_| {},
    )
    .unwrap();
    usage
}

/// The draft's chat on pi, as the app runs it.
struct PiChat {
    agent: PiAgent,
    events: futures::channel::mpsc::UnboundedReceiver<AgentEvent>,
    session_file: PathBuf,
}

impl PiChat {
    /// Sends `message` and returns the usage of the turn's last reply, from
    /// the session, where pi records it.
    fn turn(&mut self, message: &str) -> Option<Usage> {
        let disposition = block_on(self.agent.prompt(message.into()));
        assert_eq!(disposition, Ok(Disposition::Started));
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            assert!(Instant::now() < deadline, "the chat turn didn't settle");
            match self.events.try_recv() {
                Ok(AgentEvent::Crashed { error, .. }) => panic!("pi crashed: {error}"),
                Ok(AgentEvent::Pi(Event::AgentSettled { aborted })) => {
                    assert!(!aborted);
                    break;
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let session = fs::read_to_string(&self.session_file).unwrap();
        session.lines().rev().find_map(|line| {
            let entry: Value = serde_json::from_str(line).ok()?;
            let message = &entry["message"];
            (message["role"] == "assistant").then(|| {
                let usage = &message["usage"];
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                Usage {
                    input: count("input"),
                    cache_read: count("cacheRead"),
                    cache_write: count("cacheWrite"),
                    output: count("output"),
                }
            })
        })
    }
}

/// A chat turn, selection calls after it, another turn and another call:
/// each reads what the one before it cached, whichever kind it was.
#[test]
#[ignore = "runs the real pi and calls a model"]
fn cache_pi_chat_and_selection_calls_share_it() {
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR").map_or_else(
        || std::env::home_dir().unwrap().join(".pi/agent"),
        PathBuf::from,
    );
    let model = std::env::var("DRAFTMUX_TEST_PI_MODEL").ok();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("cwd");
    let draft = tmp.path().join("draft");
    let sessions = draft.join("session");
    let temp = tmp.path().join("temp");
    for dir in [&cwd, &sessions, &temp] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(
        draft.join("draft.md"),
        "Hello team, a quick update on the release.\n",
    )
    .unwrap();
    fs::write(draft.join("meta.json"), r#"{"target":"slack"}"#).unwrap();

    // The draft's session, with a conversation worth caching.
    let mut source = Command::new("pi");
    source
        .args(["-p", "--session-dir"])
        .arg(&sessions)
        .current_dir(&cwd)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    if let Some(model) = &model {
        source.arg("--model").arg(model);
    }
    let mut child = source.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(notes().as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "creating the draft's session"
    );
    let session_file = fs::read_dir(&sessions)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    let extension = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pi-extension/draftmux.ts");
    let spec = LaunchSpec {
        command: Arc::new(|| Command::new("pi")),
        session_file: session_file.clone(),
        extension: extension.clone(),
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        draft_path: draft.join("draft.md"),
        instructions: None,
        summary: None,
        context_files: true,
    };
    let (agent, events) = PiAgent::new(spec, Duration::from_secs(60), Duration::from_secs(300));
    let mut chat = PiChat {
        agent,
        events,
        session_file: session_file.clone(),
    };
    let backend = || Backend::Pi {
        pi: Command::new("pi"),
        extension: extension.clone(),
        session_file: session_file.clone(),
        agent_dir: agent_dir.clone(),
    };
    let ask = "Reply with exactly OK. Don't use tools or edit the draft.";

    // The draft's first turn can't read a cache another retention wrote.
    chat.turn(ask);
    assert_cached(
        "selection after the chat",
        select(backend(), &alternatives(&cwd, "quick"), &temp),
    );
    assert_cached(
        "another selection",
        select(backend(), &alternatives(&cwd, "short"), &temp),
    );
    assert_cached("the next chat turn", chat.turn(ask));
    assert_cached(
        "selection after that turn",
        select(backend(), &alternatives(&cwd, "brief"), &temp),
    );
    chat.agent.stop();
}

/// Selection calls on a Claude Code draft read what the one before cached.
/// They run without the chat's tools, so they keep a cache of their own.
#[test]
#[ignore = "runs the real Claude Code and calls a model"]
fn cache_claude_selection_calls_share_it() {
    let program = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("claude"))
        .find(|path| path.is_file())
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().canonicalize().unwrap().join("cwd");
    let draft = tmp.path().join("draft");
    let temp = tmp.path().join("temp");
    for dir in [&cwd, &draft, &temp] {
        fs::create_dir_all(dir).unwrap();
    }

    // The draft's conversation: a real session, adopted into the draft.
    let id = uuid::Uuid::new_v4().to_string();
    let mut child = Command::new(&program)
        .args([
            "-p",
            "--strict-mcp-config",
            "--session-id",
            &id,
            "--tools",
            "",
        ])
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(notes().as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "creating the draft's session"
    );
    let session = DraftSession {
        id: id.clone(),
        dir: draft.clone(),
        // Keychain logins don't follow another config dir.
        config_dir: None,
        fork_boundary: None,
        transcript: None,
    };
    let stored = std::env::home_dir()
        .unwrap()
        .join(".claude/projects")
        .read_dir()
        .unwrap()
        .map(|dir| dir.unwrap().path().join(format!("{id}.jsonl")))
        .find(|path| path.is_file())
        .expect("the session's transcript");
    fs::rename(&stored, session.owned()).unwrap();
    // Claude's directory for the temporary cwd, empty now.
    let _ = fs::remove_dir(stored.parent().unwrap());

    let backend = || Backend::Claude {
        program: program.clone(),
        env: std::env::vars_os().collect(),
        session: session.clone(),
    };
    select(backend(), &alternatives(&cwd, "quick"), &temp);
    assert_cached(
        "another selection",
        select(backend(), &alternatives(&cwd, "short"), &temp),
    );
    assert_cached(
        "a third selection",
        select(backend(), &alternatives(&cwd, "brief"), &temp),
    );
}
