//! `Agent` against `fake-claude` replaying scripts. The frames follow the
//! Claude Code 2.1.293 recordings of the C0 probes, trimmed to the fields
//! that matter.

// The scripts start `sleep` children, and the checks probe processes with
// signals.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use draftmux_agent::Message;
use draftmux_claude_rpc::{
    Agent, AgentEvent, Config, DraftSession, Error, LaunchSpec, Missing, PreToolUseHook, Session,
    Tool,
};
use futures::channel::mpsc::UnboundedReceiver;
use futures::executor::block_on;
use serde_json::{Value, json};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(10);

const INIT: &str = r#"
>>> {"type":"control_request","request_id":"init","request":{"subtype":"initialize"}}
{"type":"control_response","response":{"subtype":"success","request_id":"init","response":{"account":{"email":"someone@example.com"},"models":[{"value":"haiku","resolvedModel":"claude-haiku-5-5","displayName":"Haiku 5.5","description":"Fastest"}]}}}
"#;

fn prompt_in(text: &str) -> String {
    let line = json!({"type": "user", "session_id": "", "parent_tool_use_id": null,
        "message": {"role": "user", "content": text}});
    format!(">>> {line}\n")
}

fn state(state: &str) -> String {
    json!({"type": "system", "subtype": "session_state_changed", "state": state,
        "sdk_host_only": true})
    .to_string()
}

fn control_in(id: &str, request: Value) -> String {
    let line = json!({"type": "control_request", "request_id": id, "request": request});
    format!(">>> {line}\n")
}

fn ack(id: &str, response: Value) -> String {
    json!({"type": "control_response", "response":
        {"subtype": "success", "request_id": id, "response": response}})
    .to_string()
}

fn result(terminal: &str, is_error: bool) -> String {
    json!({"type": "result", "subtype": if is_error { "error_during_execution" } else { "success" },
        "is_error": is_error, "terminal_reason": terminal, "result": "OK"})
    .to_string()
}

/// A complete turn: running, an init frame, a delta, the block, an unknown
/// frame type, the result and idle.
fn turn(text: &str) -> String {
    [
        prompt_in(text),
        state("running"),
        json!({"type": "system", "subtype": "init", "model": "claude-haiku-5-5", "extra": 1})
            .to_string(),
        json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "OK"}}})
        .to_string(),
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "OK"}]}})
            .to_string(),
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}).to_string(),
        result("completed", false),
        state("idle"),
    ]
    .join("\n")
}

struct Harness {
    agent: Option<Agent>,
    events: UnboundedReceiver<AgentEvent>,
    dir: TempDir,
}

fn config() -> Config {
    Config {
        idle_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
        max_record: 1 << 20,
    }
}

impl Harness {
    /// Process `n` runs `scripts[n]`.
    fn new(scripts: &[&str]) -> Self {
        Self::with(scripts, config(), |_| {})
    }

    fn with(scripts: &[&str], config: Config, edit: impl FnOnce(&mut LaunchSpec)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (i, script) in scripts.iter().enumerate() {
            std::fs::write(dir.path().join(format!("script-{i}")), script).unwrap();
        }
        let mut spec = LaunchSpec {
            program: PathBuf::from(env!("CARGO_BIN_EXE_fake-claude")),
            cwd: dir.path().to_path_buf(),
            env: vec![
                // Not the user's, whose Claude Code settings would apply.
                ("HOME".into(), dir.path().into()),
                ("PATH".into(), std::env::var_os("PATH").unwrap()),
                ("CLAUDECODE".into(), "1".into()),
                ("CLAUDE_CODE_SESSION_ID".into(), "parent".into()),
            ],
            extra_env: vec![("FAKE_CLAUDE_DIR".into(), dir.path().into())],
            model: Some("haiku".into()),
            effort: None,
            tools: vec![Tool::Read, Tool::Edit],
            setting_sources: vec![],
            session: Some(Session::New("00000000-0000-4000-8000-000000000000".into())),
            hooks: vec![],
            permission_prompt: true,
            system_prompt: None,
        };
        edit(&mut spec);
        let (agent, events) = Agent::new(spec, config);
        Self {
            agent: Some(agent),
            events,
            dir,
        }
    }

    fn agent(&self) -> &Agent {
        self.agent.as_ref().unwrap()
    }

    fn next(&mut self) -> AgentEvent {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(event) => return event,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(err) => panic!("no event within {TIMEOUT:?}: {err:?}"),
            }
        }
    }

    /// The next `n` events, summarized.
    fn take(&mut self, n: usize) -> Vec<String> {
        (0..n).map(|_| summary(&self.next())).collect()
    }

    /// Events until one whose summary starts with `prefix`, summarized.
    fn until(&mut self, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let event = summary(&self.next());
            let done = event.starts_with(prefix);
            out.push(event);
            if done {
                return out;
            }
        }
    }

    /// Asserts that nothing arrives for `period`.
    fn quiet(&mut self, period: Duration) {
        std::thread::sleep(period);
        if let Ok(event) = self.events.try_recv() {
            panic!("unexpected event {}", summary(&event));
        }
    }

    fn record(&self, n: usize) -> Value {
        let path = self.dir.path().join(format!("record-{n}"));
        wait_for(|| path.exists());
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn pid(&self, n: usize) -> i32 {
        self.record(n)["pid"].as_i64().unwrap() as i32
    }

    /// The script reached `### eof` and saw stdin close: everything it
    /// expected was written, and nothing else.
    fn assert_finished(&self, n: usize) {
        let pid = self.pid(n);
        assert_gone(pid);
        assert!(
            self.dir.path().join(format!("eof-{n}")).exists(),
            "script {n} didn't reach its end"
        );
    }

    fn child(&self, n: usize) -> i32 {
        let path = self.dir.path().join(format!("child-{n}"));
        wait_for(|| path.exists());
        std::fs::read_to_string(path).unwrap().parse().unwrap()
    }
}

fn summary(event: &AgentEvent) -> String {
    match event {
        AgentEvent::Ready(init) => format!("Ready({})", init.models.len()),
        AgentEvent::RunStarted => "RunStarted".into(),
        AgentEvent::Frame(frame) => {
            let kind = frame["type"].as_str().unwrap_or("?");
            match frame["subtype"].as_str() {
                Some(subtype) => format!("Frame({kind}/{subtype})"),
                None => format!("Frame({kind})"),
            }
        }
        AgentEvent::Permission(request) => format!("Permission({})", request.request_id()),
        AgentEvent::Hook(request) => format!("Hook({})", request.request_id()),
        AgentEvent::CallbackCancelled { request_id } => format!("Cancelled({request_id})"),
        AgentEvent::RunSettled { aborted, error } => {
            format!("Settled(aborted={aborted}, error={error:?})")
        }
        AgentEvent::Crashed {
            error,
            failed_prompt,
        } => format!("Crashed({error}; prompt={failed_prompt:?})"),
        AgentEvent::Transcript(path) => format!("Transcript({})", path.display()),
    }
}

/// Runs `future` on another thread, failing if it takes over [`TIMEOUT`].
fn resolve<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = std_mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(block_on(future));
    });
    rx.recv_timeout(TIMEOUT)
        .expect("the future resolved in time")
}

fn wait_for(done: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The process is gone and reaped, not just signalled.
fn assert_gone(pid: i32) {
    wait_for(|| {
        // SAFETY: probes a pid without signalling it.
        let r = unsafe { libc::kill(pid, 0) };
        r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    });
}

#[test]
fn launch_contract() {
    let script = format!("{INIT}\n### eof");
    let mut h = Harness::new(&[&script]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    let record = h.record(0);
    let pid = h.pid(0);
    // SAFETY: a plain query.
    assert_eq!(unsafe { libc::getpgid(pid) }, pid, "own process group");
    let args: Vec<&str> = record["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(args.windows(2).any(|w| w == ["--tools", "Read,Edit"]));
    assert!(args.windows(2).any(|w| w == ["--model", "haiku"]));
    assert!(!args.iter().any(|a| a.contains("Bash")));
    let mut env: Vec<&str> = record["env"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    env.sort();
    assert_eq!(
        env,
        [
            "CLAUDE_CODE_SDK_READS_SESSION_STATE",
            "FAKE_CLAUDE_DIR",
            "HOME",
            "PATH",
        ],
        "only allowlisted and explicit variables"
    );
    h.agent = None;
    h.assert_finished(0);
}

#[test]
fn turns_stream_frames_and_settle_on_idle() {
    // The first run stays open until the second prompt was refused.
    let first = turn("one").replacen('\n', "\n### no-input 300\n", 1);
    let script = format!("{INIT}\n{first}\n{}\n### eof", turn("two"));
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("one".into())).unwrap();
    assert_eq!(
        resolve(h.agent().prompt("again".into())),
        Err(Error::Busy),
        "one prompt at a time"
    );
    let turn_events = [
        "RunStarted",
        "Frame(system/init)",
        "Frame(stream_event)",
        "Frame(assistant)",
        "Frame(rate_limit_event)",
        "Frame(result/success)",
        "Settled(aborted=false, error=None)",
    ];
    let mut expected = vec!["Ready(1)"];
    expected.extend(turn_events);
    assert_eq!(h.until("Settled"), expected);
    resolve(h.agent().prompt("two".into())).unwrap();
    assert_eq!(h.until("Settled"), turn_events);
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn interleaved_control_responses_and_model_for_the_next_process() {
    let script = format!(
        "{INIT}\n{}{}{}\n{}\n{}\n### eof",
        control_in("a", json!({"subtype": "set_model", "model": "sonnet"})),
        control_in("b", json!({"subtype": "set_model", "model": "nope"})),
        json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": "b", "error": "Model 'nope' not found",
            "error_code": "catalog_unknown"}}),
        json!({"type": "rate_limit_event"}),
        ack("a", Value::Null),
    );
    let second = format!("{INIT}\n### eof");
    let mut h = Harness::new(&[&script, &second]);
    let a = h.agent().set_model("sonnet".into());
    let b = h.agent().set_model("nope".into());
    assert_eq!(
        resolve(b),
        Err(Error::Rejected("Model 'nope' not found".into()))
    );
    assert_eq!(resolve(a), Ok(()));
    assert_eq!(h.take(2), ["Ready(1)", "Frame(rate_limit_event)"]);
    h.agent().stop();
    h.assert_finished(0);

    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    let args = h.record(1)["args"].to_string();
    assert!(args.contains(r#""--model","sonnet""#), "{args}");
    h.agent().stop();
    h.assert_finished(1);
}

#[test]
fn control_timeout_and_late_responses() {
    let script = format!(
        "{INIT}\n{}### sleep 600\n{}\n{}\n{}\n{}{}\n### eof",
        control_in("slow", json!({"subtype": "set_model", "model": "sonnet"})),
        ack("slow", Value::Null),
        ack("never-sent", Value::Null),
        json!({"type": "after"}),
        control_in("next", json!({"subtype": "set_model", "model": "opus"})),
        ack("next", Value::Null),
    );
    let mut config = config();
    config.request_timeout = Duration::from_millis(300);
    let mut h = Harness::with(&[&script], config, |_| {});
    let started = Instant::now();
    assert_eq!(
        resolve(h.agent().set_model("sonnet".into())),
        Err(Error::Timeout(Duration::from_millis(300)))
    );
    assert!(started.elapsed() < Duration::from_millis(600));
    // The late and unknown responses are dropped; the process goes on.
    assert_eq!(h.take(2), ["Ready(1)", "Frame(after)"]);
    assert_eq!(resolve(h.agent().set_model("opus".into())), Ok(()));
    h.agent().stop();
    h.assert_finished(0);
}

#[test]
fn interrupt_while_a_callback_is_pending() {
    let permission = json!({"type": "control_request", "request_id": "cb1", "request":
        {"subtype": "can_use_tool", "tool_name": "Write", "tool_use_id": "toolu_1",
         "input": {"file_path": "/tmp/x", "content": "y"}}});
    let script = format!(
        "{INIT}\n{}{}\n{}\n{permission}\n{}{}\n{}\n{}\n{}\n{}\n### no-input 300\n### eof",
        prompt_in("write"),
        state("running"),
        state("requires_action"),
        control_in("int", json!({"subtype": "interrupt"})),
        // The CLI withdraws the callback as it acknowledges the interrupt.
        json!({"type": "control_cancel_request", "request_id": "cb1"}),
        ack("int", json!({"still_queued": []})),
        state("running"),
        result("aborted_tools", true),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("write".into())).unwrap();
    assert_eq!(h.take(2), ["Ready(1)", "RunStarted"]);
    let AgentEvent::Permission(request) = h.next() else {
        panic!("expected a permission request");
    };
    assert_eq!(request.tool_name, "Write");
    assert_eq!(request.tool_use_id.as_deref(), Some("toolu_1"));
    // The reader goes on while the callback is unanswered.
    resolve(h.agent().interrupt()).unwrap();
    assert_eq!(
        h.until("Settled"),
        [
            "Cancelled(cb1)",
            "Frame(result/error_during_execution)",
            "Settled(aborted=true, error=None)",
        ],
        "one run, however often claude reports running"
    );
    assert!(!request.allow(), "a withdrawn request isn't answered");
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn incoming_requests_are_answered_or_refused() {
    let deny = |reason: &str| {
        json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "deny", "permissionDecisionReason": reason}})
    };
    let reply = |id: &str, subtype: &str, body: Value| {
        let mut response = json!({"subtype": subtype, "request_id": id});
        response
            .as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        format!(
            ">>> {}",
            json!({"type": "control_response", "response": response})
        )
    };
    let request = |id: &str, request: Value| {
        json!({"type": "control_request", "request_id": id, "request": request}).to_string()
    };
    let hook = |id: &str, callback: &str| {
        request(
            id,
            json!({"subtype": "hook_callback", "callback_id": callback, "tool_use_id": "t",
                "input": {"hook_event_name": "PreToolUse", "tool_name": "Edit"}}),
        )
    };
    let script = [
        control_in(
            "init",
            json!({"subtype": "initialize", "hooks": {"PreToolUse": [
                {"matcher": "Edit|Write", "hookCallbackIds": ["draftmux_hook_0"], "timeout": 5}
            ]}}),
        ),
        ack("init", json!({"models": [], "hooks_applied": true})),
        // Answered at once: an unsupported subtype and an unknown hook.
        request(
            "m1",
            json!({"subtype": "mcp_message", "server_name": "x", "message": {}}),
        ),
        hook("h0", "unknown"),
        reply(
            "m1",
            "error",
            json!({"error": "Unsupported control request subtype: mcp_message"}),
        ),
        reply(
            "h0",
            "success",
            json!({"response": deny("Draftmux has no hook with this id")}),
        ),
        // Answered by the test: proceed, drop, deny.
        hook("h1", "draftmux_hook_0"),
        reply("h1", "success", json!({"response": {}})),
        request(
            "p1",
            json!({"subtype": "can_use_tool", "tool_name": "Edit", "input": {}}),
        ),
        reply(
            "p1",
            "success",
            json!({"response": {"behavior": "deny",
                "message": "Draftmux didn't answer the permission request"}}),
        ),
        hook("h2", "draftmux_hook_0"),
        reply(
            "h2",
            "success",
            json!({"response": deny("outside the draft")}),
        ),
        "### eof".into(),
    ]
    .join("\n");
    let mut h = Harness::with(&[&script], config(), |spec| {
        spec.hooks = vec![PreToolUseHook {
            matcher: Some("Edit|Write".into()),
            timeout: Some(5),
        }];
    });
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(0)"]);
    let AgentEvent::Hook(request) = h.next() else {
        panic!("expected a hook call");
    };
    assert_eq!((request.hook, request.request_id()), (0, "h1"));
    assert_eq!(request.input["tool_name"], "Edit");
    assert!(request.proceed());
    let AgentEvent::Permission(request) = h.next() else {
        panic!("expected a permission request");
    };
    drop(request);
    let AgentEvent::Hook(request) = h.next() else {
        panic!("expected a hook call");
    };
    assert!(request.deny("outside the draft"));
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn callbacks_are_denied_when_the_receiver_is_gone() {
    let script = format!(
        "{INIT}\n{}\n>>> {}\n### eof",
        json!({"type": "control_request", "request_id": "p1", "request":
            {"subtype": "can_use_tool", "tool_name": "Edit", "input": {}}}),
        json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "p1", "response": {"behavior": "deny",
            "message": "Draftmux didn't answer the permission request"}}}),
    );
    let mut h = Harness::new(&[&script]);
    h.agent().focus().unwrap();
    // Closing the receiver makes the event drop as it is sent.
    h.events.close();
    let path = h.dir.path().join("eof-0");
    wait_for(|| path.exists());
    h.agent().stop();
    h.assert_finished(0);
}

#[test]
fn broken_write_ends_the_process() {
    let script = format!("{INIT}\n### close-stdin\n### hang");
    let mut h = Harness::new(&[&script]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    std::thread::sleep(Duration::from_millis(100));
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(h.take(1), ["RunStarted"]);
    let crashed = h.take(1).remove(0);
    assert!(
        crashed.starts_with("Crashed(writing to claude failed")
            && crashed.contains("signal: 9")
            && crashed.ends_with(r#"prompt=Some("hi"))"#),
        "{crashed}"
    );
    assert_gone(h.pid(0));
}

#[test]
fn exit_fails_pending_requests_and_a_new_process_replaces_it() {
    let script = format!(
        "{INIT}\n### spawn-child\n{}### stderr boom\n### exit 17",
        control_in("m", json!({"subtype": "set_model", "model": "sonnet"})),
    );
    let second = format!("{INIT}\n### eof");
    let mut h = Harness::new(&[&script, &second]);
    assert_eq!(
        resolve(h.agent().set_model("sonnet".into())),
        Err(Error::Exited)
    );
    assert_eq!(h.take(1), ["Ready(1)"]);
    let crashed = h.take(1).remove(0);
    assert_eq!(
        crashed,
        "Crashed(claude exited unexpectedly (exit status: 17): boom; prompt=None)"
    );
    assert_gone(h.pid(0));
    assert_gone(h.child(0));

    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    h.agent().stop();
    h.assert_finished(1);
}

#[test]
fn a_replaced_process_is_ignored() {
    let first = format!("{INIT}\n### sleep 300\n{}\n### eof", json!({"type": "old"}));
    let second = format!("{INIT}\n### eof");
    let mut h = Harness::new(&[&first, &second]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    h.agent().stop();
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    h.quiet(Duration::from_millis(500));
    h.assert_finished(0);
    h.agent().stop();
    h.assert_finished(1);
}

#[test]
fn oversized_record_ends_the_process() {
    let script = format!(
        "{INIT}\n{}{}\n### spawn-child\n### big 4096\n### hang",
        prompt_in("hi"),
        state("running"),
    );
    let mut config = config();
    config.max_record = 1024;
    let mut h = Harness::with(&[&script], config, |_| {});
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(h.take(2), ["Ready(1)", "RunStarted"]);
    let child = h.child(0);
    assert_eq!(
        h.take(1),
        [r#"Crashed(claude sent a record over 1024 bytes (signal: 9); prompt=Some("hi"))"#]
    );
    assert_gone(h.pid(0));
    assert_gone(child);
}

#[test]
fn interrupt_ack_is_not_settlement() {
    let script = format!(
        "{INIT}\n{}{}\n{}{}\n### sleep 200\n{}\n{}\n### eof",
        prompt_in("long"),
        state("running"),
        control_in("int", json!({"subtype": "interrupt"})),
        ack("int", json!({"still_queued": []})),
        result("aborted_streaming", true),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("long".into())).unwrap();
    assert_eq!(h.take(2), ["Ready(1)", "RunStarted"]);
    resolve(h.agent().interrupt()).unwrap();
    h.quiet(Duration::from_millis(100));
    assert_eq!(
        resolve(h.agent().prompt("next".into())),
        Err(Error::Busy),
        "the run is still going after the acknowledgement"
    );
    assert_eq!(
        h.take(2),
        [
            "Frame(result/error_during_execution)",
            "Settled(aborted=true, error=None)",
        ]
    );
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn interrupt_after_the_result_does_not_abort() {
    let script = format!(
        "{INIT}\n{}{}\n{}\n{}{}\n{}\n### eof",
        prompt_in("hi"),
        state("running"),
        result("completed", false),
        control_in("int", json!({"subtype": "interrupt"})),
        ack("int", json!({"still_queued": []})),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(
        h.take(3),
        ["Ready(1)", "RunStarted", "Frame(result/success)"]
    );
    resolve(h.agent().interrupt()).unwrap();
    assert_eq!(h.take(1), ["Settled(aborted=false, error=None)"]);
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn without_state_frames_the_result_settles() {
    let script = format!(
        "{INIT}\n{}{}\n{}\n### eof",
        prompt_in("hi"),
        json!({"type": "assistant", "message": {"content": []}}),
        result("completed", false),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(
        h.take(5),
        [
            "Ready(1)",
            "RunStarted",
            "Frame(assistant)",
            "Frame(result/success)",
            "Settled(aborted=false, error=None)",
        ]
    );
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn a_missing_idle_settles_after_the_request_timeout() {
    let script = format!(
        "{INIT}\n{}{}\n{}\n### eof",
        prompt_in("hi"),
        state("running"),
        result("completed", false),
    );
    let mut config = config();
    config.request_timeout = Duration::from_millis(300);
    let mut h = Harness::with(&[&script], config, |_| {});
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(
        h.take(3),
        ["Ready(1)", "RunStarted", "Frame(result/success)"]
    );
    let started = Instant::now();
    assert_eq!(h.take(1), ["Settled(aborted=false, error=None)"]);
    assert!(started.elapsed() >= Duration::from_millis(200));
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn interrupt_before_initialize_drops_the_prompt() {
    let script = r#"
>>> {"type":"control_request","request_id":"init","request":{"subtype":"initialize"}}
### sleep 300
{"type":"control_response","response":{"subtype":"success","request_id":"init","response":{"models":[]}}}
### no-input 300
### eof
"#;
    let mut h = Harness::new(&[script]);
    let prompt = h.agent().prompt("never".into());
    resolve(h.agent().interrupt()).unwrap();
    assert_eq!(resolve(prompt), Err(Error::Stopped));
    assert_eq!(h.take(1), ["Ready(0)"]);
    h.agent().blur();
    h.assert_finished(0);
}

/// Recorded live: claude reads stdin in order, so an interrupt written
/// right after the prompt reaches the run it starts.
#[test]
fn interrupt_right_after_the_prompt() {
    let script = format!(
        "{INIT}\n{}{}{}\n{}\n{}\n{}\n### eof",
        prompt_in("hi"),
        control_in("int", json!({"subtype": "interrupt"})),
        state("running"),
        ack("int", json!({"still_queued": []})),
        result("aborted_streaming", true),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    resolve(h.agent().prompt("hi".into())).unwrap();
    resolve(h.agent().interrupt()).unwrap();
    assert_eq!(
        h.take(3),
        [
            "RunStarted",
            "Frame(result/error_during_execution)",
            "Settled(aborted=true, error=None)",
        ]
    );
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn a_late_idle_does_not_settle_the_next_run() {
    let script = format!(
        "{INIT}\n{}{}\n{}\n{}{}\n### sleep 200\n{}\n{}\n{}\n### eof",
        prompt_in("a"),
        state("running"),
        result("completed", false),
        prompt_in("b"),
        // The first run's idle, after the timeout settled it.
        state("idle"),
        state("running"),
        result("completed", false),
        state("idle"),
    );
    let mut config = config();
    config.request_timeout = Duration::from_millis(300);
    let mut h = Harness::with(&[&script], config, |_| {});
    resolve(h.agent().prompt("a".into())).unwrap();
    assert_eq!(
        h.until("Settled"),
        [
            "Ready(1)",
            "RunStarted",
            "Frame(result/success)",
            "Settled(aborted=false, error=None)",
        ]
    );
    resolve(h.agent().prompt("b".into())).unwrap();
    assert_eq!(h.take(1), ["RunStarted"]);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(resolve(h.agent().prompt("c".into())), Err(Error::Busy));
    assert_eq!(
        h.take(2),
        [
            "Frame(result/success)",
            "Settled(aborted=false, error=None)"
        ]
    );
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn missing_session_reports_the_early_result() {
    let missing = json!({"type": "result", "subtype": "error_during_execution",
        "is_error": true, "num_turns": 0,
        "errors": ["No conversation found with session ID: 0000"]});
    let script = format!(
        "### sleep 100\n{missing}\n### stderr No conversation found with session ID: 0000\n### exit 1"
    );
    let mut h = Harness::with(&[&script], config(), |spec| {
        spec.session = Some(Session::Resume("0000".into()));
    });
    assert_eq!(resolve(h.agent().prompt("hi".into())), Err(Error::Exited));
    assert_eq!(
        h.take(2),
        [
            "Frame(result/error_during_execution)",
            "Crashed(claude exited unexpectedly (exit status: 1): \
             No conversation found with session ID: 0000; prompt=None)",
        ]
    );
}

#[test]
fn missing_login_fails_the_run_not_the_process() {
    let script = format!(
        "{INIT}\n{}{}\n{}\n{}\n{}\n### eof",
        prompt_in("hi"),
        state("running"),
        json!({"type": "assistant", "error": "authentication_failed",
            "message": {"content": [{"type": "text", "text": "Not logged in · Please run /login"}]}}),
        json!({"type": "result", "subtype": "success", "is_error": true,
            "result": "Not logged in · Please run /login", "terminal_reason": "api_error"}),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(
        h.take(5),
        [
            "Ready(1)",
            "RunStarted",
            "Frame(assistant)",
            "Frame(result/success)",
            r#"Settled(aborted=false, error=Some("Not logged in · Please run /login"))"#,
        ]
    );
    h.quiet(Duration::from_millis(200));
    h.agent().blur();
    h.assert_finished(0);
}

#[test]
fn idle_timeout_stops_the_process() {
    let script = format!("{INIT}\n### spawn-child\n### eof");
    let mut config = config();
    config.idle_timeout = Duration::from_millis(200);
    let mut h = Harness::with(&[&script], config, |_| {});
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    h.assert_finished(0);
    assert_gone(h.child(0));
    h.quiet(Duration::from_millis(100));
}

#[test]
fn blur_waits_for_the_run_to_settle() {
    let script = format!(
        "{INIT}\n{}{}\n### sleep 300\n{}\n{}\n### eof",
        prompt_in("hi"),
        state("running"),
        result("completed", false),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("hi".into())).unwrap();
    h.agent().blur();
    assert_eq!(
        h.until("Settled").last().unwrap(),
        "Settled(aborted=false, error=None)"
    );
    h.assert_finished(0);
}

#[test]
fn dropping_the_agent_interrupts_the_run_and_ends_the_group() {
    let script = format!(
        "{INIT}\n### spawn-child\n{}{}\n{}{}\n{}\n{}\n### eof",
        prompt_in("hi"),
        state("running"),
        control_in("int", json!({"subtype": "interrupt"})),
        ack("int", json!({"still_queued": []})),
        result("aborted_streaming", true),
        state("idle"),
    );
    let mut h = Harness::new(&[&script]);
    resolve(h.agent().prompt("hi".into())).unwrap();
    assert_eq!(h.take(2), ["Ready(1)", "RunStarted"]);
    let child = h.child(0);
    h.agent = None;
    h.assert_finished(0);
    assert_gone(child);
}

#[test]
fn stop_escalates_when_claude_ignores_eof_and_sigterm() {
    let script = format!("{INIT}\n### ignore-term\n### spawn-child\n### hang");
    let mut h = Harness::new(&[&script]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    let child = h.child(0);
    h.agent().stop();
    assert_gone(h.pid(0));
    assert_gone(child);
}

const DRAFT_ID: &str = "11111111-1111-4111-8111-111111111111";

/// A draft session in `root/draft`, with `root/home` as the `HOME` claude
/// gets and a `CLAUDE_CONFIG_DIR` the session overrides.
fn draft_harness(
    scripts: &[&str],
    root: &std::path::Path,
    edit: impl FnOnce(&mut DraftSession),
) -> Harness {
    let dir = root.join("draft");
    std::fs::create_dir_all(&dir).unwrap();
    let mut draft = DraftSession {
        id: DRAFT_ID.into(),
        dir,
        config_dir: None,
        fork_boundary: None,
        transcript: None,
    };
    edit(&mut draft);
    let home = root.join("home");
    Harness::with(scripts, config(), |spec| {
        spec.env.push(("HOME".into(), "/nonexistent".into()));
        spec.extra_env.push(("HOME".into(), home.into()));
        spec.extra_env
            .push(("CLAUDE_CONFIG_DIR".into(), "/nonexistent".into()));
        spec.session = Some(Session::Draft(draft));
    })
}

fn args(record: &Value) -> Vec<String> {
    record["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect()
}

fn has_args(record: &Value, expected: &[&str]) -> bool {
    args(record).windows(expected.len()).any(|w| w == expected)
}

/// A turn in which claude writes `line` to the transcript at `path`.
fn writing_turn(text: &str, path: &std::path::Path, line: &str) -> String {
    format!(
        "{}{}\n### append {} {line}\n{}\n{}",
        prompt_in(text),
        state("running"),
        path.display(),
        result("completed", false),
        state("idle"),
    )
}

#[test]
fn a_new_draft_session_is_recorded_adopted_and_resumed_from_the_draft() {
    let root = tempfile::tempdir().unwrap();
    let native = root
        .path()
        .join(format!("home/.claude/projects/-tmp-x/{DRAFT_ID}.jsonl"));
    let owned = root.path().join(format!("draft/{DRAFT_ID}.jsonl"));
    let line = r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"one"}}"#;
    let first = format!("{INIT}\n{}\n### eof", writing_turn("one", &native, line));
    let second = format!("{INIT}\n### eof");
    let mut h = draft_harness(&[&first, &second], root.path(), |_| {});
    resolve(h.agent().prompt("one".into())).unwrap();
    let events = h.until("Transcript");
    assert_eq!(
        events[events.len() - 2..],
        [
            "Settled(aborted=false, error=None)".to_string(),
            format!("Transcript({})", native.display()),
        ]
    );
    let record = h.record(0);
    assert!(has_args(&record, &["--session-id", DRAFT_ID]), "{record}");
    assert!(record["env"].get("CLAUDE_CONFIG_DIR").is_none());

    h.agent().stop();
    h.assert_finished(0);
    assert_eq!(h.take(1), [format!("Transcript({})", owned.display())]);
    assert!(!native.exists() && owned.is_file(), "moved into the draft");
    let history = h.agent().history().unwrap();
    assert_eq!(history.earlier, 0);
    assert_eq!(history.messages, [Message::User("one".into())]);

    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    let record = h.record(1);
    assert!(
        has_args(&record, &["--resume", owned.to_str().unwrap()]),
        "{record}"
    );
    assert!(!args(&record).contains(&"--session-id".to_string()));
    h.agent().stop();
    h.assert_finished(1);
}

#[test]
fn a_fork_starts_from_its_snapshot_with_the_draft_config_dir() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("config");
    let snapshot = root.path().join("draft/source.jsonl");
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    std::fs::write(&snapshot, "").unwrap();
    let script = format!("{INIT}\n### eof");
    let mut h = draft_harness(&[&script], root.path(), |draft| {
        draft.fork_boundary = Some("b".into());
        draft.config_dir = Some(config_dir.clone());
    });
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    let record = h.record(0);
    assert!(
        has_args(
            &record,
            &[
                "--resume",
                snapshot.to_str().unwrap(),
                "--fork-session",
                "--session-id",
                DRAFT_ID,
            ]
        ),
        "{record}"
    );
    assert_eq!(
        record["env"]["CLAUDE_CONFIG_DIR"],
        config_dir.to_str().unwrap()
    );
    h.agent().stop();
    h.assert_finished(0);
}

#[test]
fn a_lost_transcript_or_snapshot_is_reported_without_starting_claude() {
    let root = tempfile::tempdir().unwrap();
    let recorded = root.path().join("gone.jsonl");
    let h = draft_harness(&[], root.path(), |draft| {
        draft.fork_boundary = Some("b".into());
        draft.transcript = Some(recorded.clone());
    });
    assert_eq!(
        h.agent().focus(),
        Err(Error::Missing(Missing::Transcript(recorded.clone())))
    );
    let message = h.agent().history().unwrap_err();
    assert!(message.contains("draft's text is unaffected"), "{message}");

    let h = draft_harness(&[], root.path(), |draft| {
        draft.fork_boundary = Some("b".into());
    });
    assert_eq!(
        h.agent().focus(),
        Err(Error::Missing(Missing::Snapshot(
            root.path().join("draft/source.jsonl")
        )))
    );
    assert!(!h.dir.path().join("record-0").exists(), "nothing started");
}

#[test]
fn a_draft_transcript_is_never_replaced_by_one_in_claude_storage() {
    let root = tempfile::tempdir().unwrap();
    let native = root
        .path()
        .join(format!("home/.claude/projects/-tmp-x/{DRAFT_ID}.jsonl"));
    let owned = root.path().join(format!("draft/{DRAFT_ID}.jsonl"));
    std::fs::create_dir_all(native.parent().unwrap()).unwrap();
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&native, "stray\n").unwrap();
    std::fs::write(&owned, "draft\n").unwrap();
    let script = format!("{INIT}\n### eof");
    let mut h = draft_harness(&[&script], root.path(), |_| {});
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    assert!(has_args(
        &h.record(0),
        &["--resume", owned.to_str().unwrap()]
    ));
    h.agent().stop();
    h.assert_finished(0);
    h.quiet(Duration::from_millis(100));
    assert_eq!(std::fs::read_to_string(&owned).unwrap(), "draft\n");
    assert_eq!(std::fs::read_to_string(&native).unwrap(), "stray\n");
}

#[test]
fn a_restart_waits_for_the_previous_process_to_exit() {
    let first = format!("{INIT}\n### sleep 1000\n### exit 0");
    let second = format!("{INIT}\n### eof");
    let mut h = Harness::new(&[&first, &second]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    let pid = h.pid(0);
    h.agent().stop();
    // SAFETY: probes a pid without signalling it.
    let alive = || unsafe { libc::kill(pid, 0) } == 0;
    h.agent().focus().unwrap();
    assert!(alive(), "focus returned before the first process exited");
    assert_eq!(h.take(1), ["Ready(1)"]);
    assert!(
        !alive(),
        "the first process exited before the second started"
    );
    h.agent().stop();
    h.assert_finished(1);
}

#[test]
fn switching_away_while_the_previous_process_exits_starts_none() {
    let first = format!("{INIT}\n### sleep 300\n### exit 0");
    let mut h = Harness::new(&[&first]);
    h.agent().focus().unwrap();
    assert_eq!(h.take(1), ["Ready(1)"]);
    h.agent().stop();
    h.agent().focus().unwrap();
    h.agent().blur();
    h.quiet(Duration::from_millis(600));
    assert!(!h.dir.path().join("record-1").exists());
}

#[test]
fn hooks_claude_does_not_confirm_fail_the_prompt() {
    let script = [
        control_in(
            "init",
            json!({"subtype": "initialize", "hooks": {"PreToolUse": [
                {"hookCallbackIds": ["draftmux_hook_0"], "timeout": 30}
            ]}}),
        ),
        ack("init", json!({"models": []})),
        "### eof".into(),
    ]
    .join("\n");
    let mut h = Harness::with(&[&script], config(), |spec| {
        spec.hooks = vec![PreToolUseHook {
            matcher: None,
            timeout: Some(30),
        }];
    });
    let Err(Error::Rejected(err)) = resolve(h.agent().prompt("one".into())) else {
        panic!("expected the prompt to be rejected");
    };
    assert!(err.contains("hooks"), "{err}");
    h.assert_finished(0);
    h.quiet(Duration::from_millis(100));
}
