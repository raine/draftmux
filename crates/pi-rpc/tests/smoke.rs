//! Opt-in smoke test against the real pi: a prompt round trip through the
//! bundled extension. It calls the profile's default model.
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
use draftmux_storage::Target;
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
    let session_file = dir.path().join("session/session.jsonl");

    let spec = LaunchSpec {
        command: Arc::new(move || Command::new(&pi)),
        session_file: session_file.clone(),
        extension: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pi-extension/draftmux.ts"),
        cwd: dir.path().to_path_buf(),
        agent_dir: agent_dir.clone(),
        draft_path,
        target: Target::Github,
        instructions: None,
    };
    let (agent, mut events) = Agent::new(spec, Duration::from_secs(60));

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
    assert!(session_file.exists());

    agent.stop();
    assert_eq!(
        std::fs::read(agent_dir.join("settings.json")).ok(),
        settings,
        "pi's settings changed"
    );
}
