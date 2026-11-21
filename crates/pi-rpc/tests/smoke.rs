//! Opt-in smoke test against the real pi: a prompt round trip through the
//! bundled extension, which reads the target from `meta.json`. It calls the
//! profile's default model.
//!
//! ```sh
//! cargo test -p draftmux-pi-rpc --test smoke -- --ignored
//! ```
//!
//! Uses `pi` from `PATH` (or `PI_BIN`) and the profile in
//! `PI_CODING_AGENT_DIR` (default `~/.pi/agent`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use draftmux_pi_rpc::{Agent, AgentEvent, Disposition, Event, LaunchSpec};
use futures::executor::block_on;

#[test]
#[ignore = "runs the real pi and calls a model"]
fn prompt_round_trip() {
    let pi = std::env::var_os("PI_BIN").map_or_else(|| PathBuf::from("pi"), PathBuf::from);
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR").map_or_else(
        || std::env::home_dir().unwrap().join(".pi/agent"),
        PathBuf::from,
    );
    let settings = std::fs::read(agent_dir.join("settings.json")).ok();
    let dir = tempfile::tempdir().unwrap();
    let draft_path = dir.path().join("draft.md");
    std::fs::write(&draft_path, "Hello world.\n").unwrap();
    std::fs::write(dir.path().join("meta.json"), r#"{"target":"slack"}"#).unwrap();
    let session_file = dir.path().join("session/session.jsonl");

    let spec = LaunchSpec {
        command: Arc::new(move || Command::new(&pi)),
        session_file: session_file.clone(),
        extension: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pi-extension/draftmux.ts"),
        cwd: dir.path().to_path_buf(),
        agent_dir: agent_dir.clone(),
        draft_path,
        instructions: None,
        summary: None,
        context_files: true,
    };
    let (agent, mut events) = Agent::new(spec, Duration::from_secs(60), Duration::from_secs(120));

    let state = block_on(agent.get_state()).expect("get_state");
    assert!(state.model.is_some(), "the profile has no default model");
    let disposition = block_on(
        agent.prompt("Reply with exactly the word pong. Don't use tools or edit the draft.".into()),
    );
    assert_eq!(disposition, Ok(Disposition::Started));

    let deadline = Instant::now() + Duration::from_secs(180);
    let (mut ready, mut trust, mut reply) = (false, None, None);
    loop {
        assert!(Instant::now() < deadline, "no agent_settled in time");
        let Ok(event) = events.try_recv() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        match event {
            AgentEvent::Crashed { error, .. } => panic!("pi crashed: {error}"),
            AgentEvent::Ready(_) => ready = true,
            AgentEvent::Pi(Event::Status { key, text }) if key == "draftmux.trust" => trust = text,
            AgentEvent::Pi(Event::MessageEnd { message }) if message["role"] == "assistant" => {
                reply = Some(message)
            }
            AgentEvent::Pi(Event::AgentSettled { aborted }) => {
                assert!(!aborted);
                break;
            }
            _ => {}
        }
    }
    let reply = reply.expect("an assistant message");
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let text = reply["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<String>();
    assert!(!text.trim().is_empty(), "{reply}");
    assert!(ready);
    assert!(matches!(trust.as_deref(), Some("trusted" | "untrusted")));
    let session = std::fs::read_to_string(&session_file).unwrap();
    assert!(
        session.lines().any(|line| line.contains("draftmux-context")
            && line.contains("Target platform: Markdown for a Slack message")),
        "no Slack context message in the session"
    );

    agent.stop();
    assert_eq!(
        std::fs::read(agent_dir.join("settings.json")).ok(),
        settings,
        "pi's settings changed"
    );
}

/// Compacting through the contract keeps what the instructions ask for,
/// and the draft's context reaches the model again afterwards. pi keeps the
/// last 20k tokens as they are, so a long message after the facts leaves
/// them for the summary.
#[test]
#[ignore = "runs the real pi and calls a model"]
fn compaction_keeps_the_target_and_the_facts() {
    use draftmux_agent::{Agent as _, Event as E};
    use draftmux_pi_rpc::PiAgent;
    use futures::StreamExt;

    let pi = std::env::var_os("PI_BIN").map_or_else(|| PathBuf::from("pi"), PathBuf::from);
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR").map_or_else(
        || std::env::home_dir().unwrap().join(".pi/agent"),
        PathBuf::from,
    );
    let dir = tempfile::tempdir().unwrap();
    let draft_path = dir.path().join("draft.md");
    std::fs::write(&draft_path, "The launch moved to KESTREL week.\n").unwrap();
    std::fs::write(dir.path().join("meta.json"), r#"{"target":"slack"}"#).unwrap();
    let spec = LaunchSpec {
        command: Arc::new(move || Command::new(&pi)),
        session_file: dir.path().join("session/session.jsonl"),
        extension: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pi-extension/draftmux.ts"),
        cwd: dir.path().to_path_buf(),
        agent_dir,
        draft_path,
        instructions: None,
        summary: None,
        context_files: false,
    };
    let (agent, mut events) = PiAgent::new(spec, Duration::from_secs(60), Duration::from_secs(300));
    let mut settle = |until: &dyn Fn(&E) -> bool| {
        let mut seen = Vec::new();
        loop {
            let event = block_on(events.next()).expect("an event");
            if let E::Crashed { error, .. } = &event {
                panic!("pi crashed: {error}");
            }
            let done = until(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
    };
    let settled = |event: &E| matches!(event, E::RunSettled { .. });
    let quiet = "Don't use tools or edit the draft.";
    block_on(agent.prompt(format!(
        "Remember that the reviewer is called Odile. Reply with OK. {quiet}"
    )))
    .unwrap();
    settle(&settled);
    let filler = "The quick brown fox jumps over the lazy dog. ".repeat(2500);
    block_on(agent.prompt(format!(
        "{filler}\n\nIgnore the text above. Reply with OK. {quiet}"
    )))
    .unwrap();
    let long = settle(&settled);
    let before = long
        .iter()
        .find_map(|event| match event {
            E::Context(tokens) => Some(*tokens),
            _ => None,
        })
        .expect("the reply reported the context");

    let compact = agent.compact("Keep the target and the reviewer's name.".into());
    let ended = settle(&|event| matches!(event, E::CompactionEnded { .. }));
    block_on(compact).expect("compacted");
    eprintln!("compaction {:?}", ended.last());
    let Some(E::CompactionEnded {
        compacted: true, ..
    }) = ended.last()
    else {
        panic!("not compacted: {ended:?}");
    };

    block_on(agent.prompt(String::from(
        "Read the draft file without editing it, then reply with only these, \
         comma-separated: the target platform, the reviewer's name, and the week the draft \
         names.",
    )))
    .unwrap();
    let last = settle(&settled);
    let after = last.iter().find_map(|event| match event {
        E::Context(tokens) => Some(*tokens),
        _ => None,
    });
    eprintln!("context {before} -> {after:?}");
    assert!(
        after.is_some_and(|after| after < before),
        "the context shrank"
    );
    let reply: String = last
        .into_iter()
        .filter_map(|event| match event {
            E::AssistantEnded { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    eprintln!("reply {reply:?}");
    for word in ["Slack", "Odile", "KESTREL"] {
        assert!(reply.contains(word), "{word} survived compaction: {reply}");
    }
}
