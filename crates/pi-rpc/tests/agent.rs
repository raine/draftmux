//! `Agent` against `fake-pi` replaying scripts. `fixtures/*.jsonl` are
//! recordings of pi 1.5.0 with the bundled extension, scrubbed of private
//! content.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use draftmux_pi_rpc::{
    Agent, AgentEvent, AssistantDelta, Disposition, Error, Event, LaunchSpec, ThinkingLevel,
};
use draftmux_storage::Target;
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use futures::executor::block_on;
use serde_json::Value;
use tempfile::TempDir;

const IDLE: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
    agent: Agent,
    events: UnboundedReceiver<AgentEvent>,
    spawns: Arc<AtomicUsize>,
    dir: TempDir,
}

impl Harness {
    /// Spawn `n` runs `scripts[n]`.
    fn new(scripts: &[&str]) -> Self {
        Self::with_idle(scripts, IDLE)
    }

    fn with_idle(scripts: &[&str], idle: Duration) -> Self {
        Self::with_spec(scripts, idle, |_| {})
    }

    fn with_spec(scripts: &[&str], idle: Duration, edit: impl FnOnce(&mut LaunchSpec)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = scripts
            .iter()
            .enumerate()
            .map(|(i, script)| {
                let path = dir.path().join(format!("script-{i}"));
                std::fs::write(&path, script).unwrap();
                path
            })
            .collect();
        let spawns = Arc::new(AtomicUsize::new(0));
        let record = dir.path().join("record");
        let counter = spawns.clone();
        let command = Arc::new(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let mut command = Command::new(env!("CARGO_BIN_EXE_fake-pi"));
            command
                .env_clear()
                .env("FAKE_PI_SCRIPT", &paths[n])
                .env("FAKE_PI_RECORD", record.with_extension(n.to_string()))
                .env("DRAFTMUX_INSTRUCTIONS", "stale");
            command
        });
        let spec = LaunchSpec {
            command,
            session_file: dir.path().join("session.jsonl"),
            extension: PathBuf::from("/app/draftmux.ts"),
            cwd: dir.path().to_path_buf(),
            agent_dir: PathBuf::from("/profiles/work/agent"),
            draft_path: dir.path().join("draft.md"),
            target: Target::Slack,
            instructions: None,
            context_files: true,
        };
        let mut spec = spec;
        edit(&mut spec);
        let (agent, events) = Agent::new(spec, idle);
        Self {
            agent,
            events,
            spawns,
            dir,
        }
    }

    fn record_path(&self, n: usize) -> PathBuf {
        self.dir.path().join("record").with_extension(n.to_string())
    }

    fn record(&self, n: usize) -> Value {
        let path = self.record_path(n);
        wait_for(|| path.exists());
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Whether spawn `n` saw stdin close.
    fn saw_eof(&self, n: usize) -> bool {
        self.record_path(n)
            .with_extension(format!("{n}.eof"))
            .exists()
    }

    /// The next event other than [`AgentEvent::Ready`].
    fn next(&mut self) -> AgentEvent {
        loop {
            match self.next_any() {
                AgentEvent::Ready(_) => {}
                event => return event,
            }
        }
    }

    fn next_any(&mut self) -> AgentEvent {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(event) => return event,
                Err(TryRecvError::Closed) => panic!("event stream closed"),
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => panic!("no event within {TIMEOUT:?}"),
            }
        }
    }

    /// Events up to and including the first one matching `until`.
    fn until(&mut self, until: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        loop {
            let event = self.next_any();
            if let AgentEvent::Crashed { .. } = &event
                && !until(&event)
            {
                panic!("unexpected crash: {event:?}");
            }
            let done = until(&event);
            events.push(event);
            if done {
                return events;
            }
        }
    }

    fn until_settled(&mut self) -> Vec<AgentEvent> {
        self.until(|event| matches!(event, AgentEvent::Pi(Event::AgentSettled { .. })))
    }

    fn no_more_events(&mut self) {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(event) = self.events.try_recv() {
            panic!("unexpected event {event:?}");
        }
    }
}

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "condition not met in {TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

const STATE: &str = r#"{"id":"1","type":"response","command":"get_state","success":true,"data":{"thinkingLevel":"medium","isStreaming":false,"isCompacting":false,"steeringMode":"all","followUpMode":"all","sessionId":"s","autoCompactionEnabled":true,"messageCount":0,"pendingMessageCount":0}}"#;

/// A script that answers the startup `get_state`, then runs `rest`.
fn script(rest: &str) -> String {
    format!(">>> {{\"id\":\"1\",\"type\":\"get_state\"}}\n{STATE}\n{rest}")
}

fn assistant_text(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Pi(Event::MessageEnd { message }) if message["role"] == "assistant" => {
                Some(message["content"][0]["text"].as_str()?.to_string())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn prompt_round_trip_from_recording() {
    let mut h = Harness::new(&[&fixture("text.jsonl")]);
    h.agent.focus().unwrap();
    h.agent.focus().unwrap();
    let disposition = block_on(
        h.agent
            .prompt("Reply with exactly the word: pong. Do not use tools.".into()),
    );
    assert_eq!(disposition, Ok(Disposition::Started));
    let events = h.until_settled();

    assert!(events.contains(&AgentEvent::Pi(Event::Status {
        key: "draftmux.trust".into(),
        text: Some("trusted".into()),
    })));
    assert_eq!(assistant_text(&events), ["pong"]);
    let deltas: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Pi(Event::MessageUpdate(AssistantDelta::TextDelta { delta, .. })) => {
                Some(delta.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "pong");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Pi(Event::AgentSettled { aborted: false }))
    );
    assert!(h.agent.deadline().is_some());

    h.agent.blur();
    wait_for(|| h.saw_eof(0));
    h.no_more_events();
    assert_eq!(h.spawns.load(Ordering::SeqCst), 1);
}

#[test]
fn spawn_contract() {
    let h = Harness::new(&[&script("### eof")]);
    h.agent.focus().unwrap();
    let record = h.record(0);
    let session = h.dir.path().join("session.jsonl");
    assert_eq!(
        record["args"],
        serde_json::json!([
            "--mode",
            "rpc",
            "--session",
            session,
            "-e",
            "/app/draftmux.ts"
        ])
    );
    assert_eq!(
        Path::new(record["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        h.dir.path().canonicalize().unwrap()
    );
    let env = &record["env"];
    assert_eq!(env["PI_CODING_AGENT_DIR"], "/profiles/work/agent");
    assert_eq!(env["DRAFTMUX_MODE"], "draft");
    assert_eq!(env["DRAFTMUX_TARGET"], "slack");
    assert_eq!(
        env["DRAFTMUX_DRAFT_PATH"],
        h.dir.path().join("draft.md").to_str().unwrap()
    );
    assert!(env.get("DRAFTMUX_INSTRUCTIONS").is_none());
}

#[test]
fn settings_reach_the_process() {
    let h = Harness::with_spec(&[&script("### eof")], IDLE, |spec| {
        spec.instructions = Some("Short paragraphs.".into());
        spec.context_files = false;
    });
    h.agent.focus().unwrap();
    let record = h.record(0);
    let args = record["args"].as_array().unwrap();
    assert!(
        args.iter().any(|arg| arg == "--no-context-files"),
        "{args:?}"
    );
    assert_eq!(record["env"]["DRAFTMUX_INSTRUCTIONS"], "Short paragraphs.");
}

#[test]
fn tool_events_and_draft_write_from_recording() {
    let mut h = Harness::new(&[&fixture("tool.jsonl")]);
    let prompt =
        "Use the edit tool to change 'world' to 'there' in the draft file. Then reply 'done'.";
    assert_eq!(
        block_on(h.agent.prompt(prompt.into())),
        Ok(Disposition::Started)
    );
    let events = h.until_settled();
    let tools: Vec<(&str, bool)> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Pi(event @ Event::ToolExecutionEnd { tool_name, .. }) => {
                Some((tool_name.as_str(), event.is_draft_write()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(tools, [("read", false), ("edit", true)]);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Pi(Event::MessageUpdate(AssistantDelta::ToolcallStart { tool_name, .. }))
            if tool_name == "edit"
    )));
}

/// The abort reply arrives after `agent_settled` and after the reply to a
/// later command.
#[test]
fn abort_and_model_commands_from_recording() {
    let mut h = Harness::new(&[&fixture("abort.jsonl")]);
    let prompt = "Count slowly from 1 to 200, one number per line.";
    assert_eq!(
        block_on(h.agent.prompt(prompt.into())),
        Ok(Disposition::Started)
    );
    h.until(|event| {
        matches!(
            event,
            AgentEvent::Pi(Event::MessageUpdate(AssistantDelta::TextDelta { .. }))
        )
    });
    assert_eq!(block_on(h.agent.prompt("again".into())), Err(Error::Busy));
    let abort = h.agent.abort();
    let events = h.until_settled();
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Pi(Event::AgentSettled { aborted: true }))
    );
    let thinking = h.agent.set_thinking_level(ThinkingLevel::Low);
    assert_eq!(block_on(abort), Ok(()));
    assert_eq!(block_on(thinking), Ok(()));
    assert_eq!(
        h.next(),
        AgentEvent::Pi(Event::ThinkingLevelChanged {
            level: ThinkingLevel::Low
        })
    );
    assert_eq!(
        block_on(h.agent.set_model("nope".into(), "nope".into())),
        Err(Error::Rejected("Model not found: nope/nope".into()))
    );
    let models = block_on(h.agent.get_available_models()).unwrap();
    assert_eq!(models.len(), 3);
    assert!(models.iter().all(|model| !model.provider.is_empty()));
}

#[test]
fn get_state() {
    let h = Harness::new(&[&script(&format!(
        ">>> {{\"id\":\"2\",\"type\":\"get_state\"}}\n{}\n### eof",
        STATE.replace(r#""id":"1""#, r#""id":"2""#)
    ))]);
    let state = block_on(h.agent.get_state()).unwrap();
    assert_eq!(state.thinking_level, ThinkingLevel::Medium);
    assert!(state.model.is_none());
}

/// The switch reports the `get_state` that follows it, not `set_model`'s
/// answer: pi resolves thinking for the new model.
#[test]
fn switch_model_reads_state_back() {
    let model =
        r#"{"id":"m1","name":"M1","provider":"p","api":"x","contextWindow":1,"maxTokens":1}"#;
    let h = Harness::new(&[&script(&format!(
        ">>> {{\"id\":\"2\",\"type\":\"set_model\",\"provider\":\"p\",\"modelId\":\"m1\"}}\n\
         {{\"id\":\"2\",\"type\":\"response\",\"command\":\"set_model\",\"success\":true,\"data\":{model}}}\n\
         >>> {{\"id\":\"3\",\"type\":\"get_state\"}}\n{}\n### eof",
        STATE.replace(r#""id":"1""#, r#""id":"3""#).replace(
            r#""thinkingLevel":"medium""#,
            &format!(r#""model":{model},"thinkingLevel":"high""#)
        )
    ))]);
    let state = block_on(h.agent.switch_model("p".into(), "m1".into())).unwrap();
    assert_eq!(state.model.unwrap().id, "m1");
    assert_eq!(state.thinking_level, ThinkingLevel::High);
}

/// Switching drafts while the switch is in flight stops pi once it
/// answers; the read-back doesn't start another process.
#[test]
fn switch_model_doesnt_restart_a_blurred_agent() {
    let h = Harness::new(&[&script(
        r#">>> {"id":"2","type":"set_model","provider":"p","modelId":"m1"}
### sleep 200
{"id":"2","type":"response","command":"set_model","success":true,"data":{"id":"m1","name":"M1","provider":"p","api":"x","contextWindow":1,"maxTokens":1}}
### eof"#,
    )]);
    let switch = h.agent.switch_model("p".into(), "m1".into());
    h.agent.blur();
    assert_eq!(block_on(switch), Err(Error::Stopped));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(h.spawns.load(Ordering::SeqCst), 1);
}

#[test]
fn rejected_switch_sends_no_get_state() {
    let h = Harness::new(&[&script(
        r#">>> {"id":"2","type":"set_model","provider":"p","modelId":"gone"}
{"id":"2","type":"response","command":"set_model","success":false,"error":"Model not found: p/gone"}
### eof"#,
    )]);
    assert_eq!(
        block_on(h.agent.switch_model("p".into(), "gone".into())),
        Err(Error::Rejected("Model not found: p/gone".into()))
    );
}

#[test]
fn rejected_and_handled_prompts_end_the_turn() {
    let h = Harness::new(&[&script(
        r#">>> {"id":"2","type":"prompt","message":"a"}
{"id":"2","type":"response","command":"prompt","success":false,"error":"No API key for anthropic/claude"}
>>> {"id":"3","type":"prompt","message":"b"}
{"id":"3","type":"response","command":"prompt","success":true,"data":{"disposition":"handled"}}
>>> {"id":"4","type":"prompt","message":"c"}
{"id":"4","type":"response","command":"prompt","success":true,"data":{"disposition":"handled"}}
### eof"#,
    )]);
    assert_eq!(
        block_on(h.agent.prompt("a".into())),
        Err(Error::Rejected("No API key for anthropic/claude".into()))
    );
    assert!(h.agent.deadline().is_some());
    assert_eq!(
        block_on(h.agent.prompt("b".into())),
        Ok(Disposition::Handled)
    );
    assert_eq!(
        block_on(h.agent.prompt("c".into())),
        Ok(Disposition::Handled)
    );
    assert!(h.agent.deadline().is_some());
}

#[test]
fn extension_dialogs_are_cancelled() {
    let mut h = Harness::new(&[&script(
        r#"{"type":"extension_ui_request","id":"d1","method":"confirm","title":"Allow?","message":"x"}
>>> {"type":"extension_ui_response","id":"d1","cancelled":true}
{"type":"turn_start"}
### eof"#,
    )]);
    h.agent.focus().unwrap();
    assert_eq!(
        h.next(),
        AgentEvent::Pi(Event::DialogCancelled {
            method: "confirm".into(),
            title: "Allow?".into()
        })
    );
    assert_eq!(h.next(), AgentEvent::Pi(Event::TurnStart));
}

#[test]
fn idle_stop() {
    let mut h = Harness::with_idle(
        &[&script("### eof"), &script("### eof")],
        Duration::from_millis(50),
    );
    assert_eq!(h.agent.deadline(), None);
    h.agent.focus().unwrap();
    // Busy until the startup get_state is answered, which is reported.
    let ready = h.until(|event| matches!(event, AgentEvent::Ready(_)));
    let Some(AgentEvent::Ready(state)) = ready.last() else {
        unreachable!()
    };
    assert_eq!(state.thinking_level, ThinkingLevel::Medium);
    let due = h.agent.deadline().unwrap();
    assert!(!h.agent.poll(due - Duration::from_millis(1)));
    assert!(h.agent.poll(due));
    assert_eq!(h.agent.deadline(), None);
    wait_for(|| h.saw_eof(0));
    h.no_more_events();

    h.agent.focus().unwrap();
    assert_eq!(h.spawns.load(Ordering::SeqCst), 2);
}

#[test]
fn blur_waits_for_the_run_to_settle() {
    let mut h = Harness::new(&[&script(
        r#">>> {"id":"2","type":"prompt","message":"go"}
{"id":"2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
{"type":"agent_start"}
### sleep 200
{"type":"agent_settled","aborted":false}
### eof"#,
    )]);
    assert_eq!(
        block_on(h.agent.prompt("go".into())),
        Ok(Disposition::Started)
    );
    h.agent.blur();
    assert_eq!(h.agent.deadline(), None);
    assert_eq!(h.next(), AgentEvent::Pi(Event::AgentStart));
    assert!(!h.saw_eof(0));
    h.until_settled();
    wait_for(|| h.saw_eof(0));
    h.no_more_events();
}

/// An extension can start a run before pi subscribes to events, so the
/// startup `get_state` decides whether pi is running.
#[test]
fn run_started_before_subscription() {
    let streaming = STATE.replace(r#""isStreaming":false"#, r#""isStreaming":true"#);
    let mut h = Harness::with_idle(
        &[&format!(
            ">>> {{\"id\":\"1\",\"type\":\"get_state\"}}\n{streaming}\n{{\"type\":\"turn_start\"}}\n### sleep 200\n{{\"type\":\"agent_settled\",\"aborted\":false}}\n### eof"
        )],
        Duration::ZERO,
    );
    h.agent.focus().unwrap();
    assert_eq!(h.next(), AgentEvent::Pi(Event::TurnStart));
    assert_eq!(h.agent.deadline(), None);
    assert_eq!(block_on(h.agent.prompt("x".into())), Err(Error::Busy));
    h.until_settled();
    assert!(h.agent.poll(Instant::now()));
    wait_for(|| h.saw_eof(0));
}

#[test]
fn crash_mid_turn_reports_the_turn_and_respawns() {
    let mut h = Harness::new(&[
        &script(
            r#">>> {"id":"2","type":"prompt","message":"draft it"}
{"id":"2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
{"type":"agent_start"}
>>> {"id":"3","type":"get_state"}
### stderr boom: out of memory
### exit 3"#,
        ),
        &script(
            r#">>> {"id":"2","type":"prompt","message":"draft it"}
{"id":"2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
### eof"#,
        ),
    ]);
    assert_eq!(
        block_on(h.agent.prompt("draft it".into())),
        Ok(Disposition::Started)
    );
    assert_eq!(h.next(), AgentEvent::Pi(Event::AgentStart));
    assert_eq!(block_on(h.agent.get_state()), Err(Error::Exited));
    let AgentEvent::Crashed { error, failed_turn } = h.next() else {
        panic!("expected a crash");
    };
    assert!(error.contains("exit status: 3"), "{error}");
    assert!(error.contains("boom: out of memory"), "{error}");
    assert_eq!(failed_turn.as_deref(), Some("draft it"));
    assert_eq!(h.agent.deadline(), None);

    // Retry: the next prompt starts a new process.
    assert_eq!(
        block_on(h.agent.prompt("draft it".into())),
        Ok(Disposition::Started)
    );
    assert_eq!(h.spawns.load(Ordering::SeqCst), 2);
}

/// pi can print its last records and exit before the exit is noticed; they
/// are still delivered, before the crash report.
#[test]
fn output_before_exit_is_delivered_first() {
    let mut h = Harness::new(&[&script(
        r#">>> {"id":"2","type":"prompt","message":"x"}
{"id":"2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
{"type":"agent_start"}
{"type":"agent_settled","aborted":false}
### exit 0"#,
    )]);
    assert_eq!(
        block_on(h.agent.prompt("x".into())),
        Ok(Disposition::Started)
    );
    h.until_settled();
    assert!(matches!(
        h.next(),
        AgentEvent::Crashed {
            failed_turn: None,
            ..
        }
    ));
}

/// pi ignores an abort sent before its run exists, so it is sent again on
/// `agent_start`.
#[test]
fn abort_before_run_start_is_resent() {
    let mut h = Harness::new(&[&script(
        r#">>> {"id":"2","type":"prompt","message":"x"}
>>> {"id":"3","type":"abort"}
{"id":"3","type":"response","command":"abort","success":true}
{"id":"2","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
{"type":"agent_start"}
>>> {"type":"abort"}
{"type":"agent_settled","aborted":true}
### eof"#,
    )]);
    let prompt = h.agent.prompt("x".into());
    assert_eq!(block_on(h.agent.abort()), Ok(()));
    assert_eq!(block_on(prompt), Ok(Disposition::Started));
    assert_eq!(
        h.until_settled().last(),
        Some(&AgentEvent::Pi(Event::AgentSettled { aborted: true }))
    );
}

#[test]
fn abort_without_a_process_does_nothing() {
    let h = Harness::new(&[]);
    assert_eq!(block_on(h.agent.abort()), Ok(()));
    assert_eq!(h.spawns.load(Ordering::SeqCst), 0);
}

/// pi that ignores stdin EOF is terminated.
#[test]
fn stop_escalates_to_sigterm() {
    let mut h = Harness::new(&[&script("### hang")]);
    h.agent.focus().unwrap();
    h.until(|event| matches!(event, AgentEvent::Ready(_)));
    let pid = h.record(0)["pid"].as_i64().unwrap() as libc::pid_t;
    let started = Instant::now();
    h.agent.stop();
    // SAFETY: signal 0 only checks that the process exists.
    wait_for(|| unsafe { libc::kill(pid, 0) } != 0);
    assert!(started.elapsed() >= Duration::from_secs(2));
    h.no_more_events();
}
