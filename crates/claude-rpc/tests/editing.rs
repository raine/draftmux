//! `ClaudeAgent` against `fake-claude`: the context ahead of prompts, the
//! answers to tool hooks and permission prompts, and which tool calls are
//! reported as draft writes. The scripts change the draft with `### write`
//! where claude's tool would.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use draftmux_agent::{Agent as _, Event, Stop};
use draftmux_claude_rpc::{
    ClaudeAgent, Config, DraftContext, LaunchSpec, Record, Session, instructions_block,
};
use futures::StreamExt;
use futures::executor::block_on;
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

use common::*;

struct Harness {
    agent: Option<ClaudeAgent>,
    events: std_mpsc::Receiver<Event>,
    records: futures::channel::mpsc::UnboundedReceiver<Record>,
    dir: TempDir,
    draft: PathBuf,
    target: Arc<Mutex<Option<String>>>,
}

impl Harness {
    /// A draft holding `text`; process `n` runs the script `scripts(n)`
    /// builds from the draft's path and the temporary directory.
    fn new(text: &str, scripts: impl Fn(&Path, &Path) -> Vec<String>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let draft_dir = root.join("drafts/one");
        std::fs::create_dir_all(&draft_dir).unwrap();
        let draft = draft_dir.join("draft.md");
        std::fs::write(&draft, text).unwrap();
        for (i, script) in scripts(&draft, &root).iter().enumerate() {
            std::fs::write(root.join(format!("script-{i}")), script).unwrap();
        }
        let spec = LaunchSpec {
            program: PathBuf::from(env!("CARGO_BIN_EXE_fake-claude")),
            cwd: root.clone(),
            env: vec![("PATH".into(), std::env::var_os("PATH").unwrap())],
            extra_env: vec![("FAKE_CLAUDE_DIR".into(), root.clone().into())],
            model: Some("haiku".into()),
            effort: None,
            tools: vec![],
            setting_sources: vec![],
            session: Some(Session::New("00000000-0000-4000-8000-000000000000".into())),
            hooks: vec![],
            permission_prompt: false,
            system_prompt: None,
        };
        let target = Arc::new(Mutex::new(Some(SLACK.to_owned())));
        let context = DraftContext {
            draft_path: draft.clone(),
            instructions: "Be brief.".into(),
            target: Box::new({
                let target = target.clone();
                move || target.lock().unwrap().clone()
            }),
        };
        let config = Config {
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(5),
            max_record: 1 << 20,
        };
        let (agent, mut stream, records) = ClaudeAgent::new(spec, config, context).unwrap();
        let (tx, events) = std_mpsc::channel();
        std::thread::spawn(move || {
            while let Some(event) = block_on(stream.next()) {
                if tx.send(event).is_err() {
                    return;
                }
            }
        });
        Self {
            agent: Some(agent),
            events,
            records,
            dir,
            draft,
            target,
        }
    }

    fn agent(&self) -> &ClaudeAgent {
        self.agent.as_ref().unwrap()
    }

    fn prompt(&self, text: &str) {
        block_on(self.agent().prompt(text.into())).unwrap();
    }

    fn next(&self) -> Event {
        self.events.recv_timeout(TIMEOUT).expect("an event")
    }

    /// Events up to and including the first `done` accepts.
    fn until(&self, done: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            let event = self.next();
            let stop = done(&event);
            events.push(event);
            if stop {
                return events;
            }
        }
    }

    fn settled(&self) -> Vec<Event> {
        self.until(|event| matches!(event, Event::RunSettled { .. }))
    }

    fn record(&self, n: usize) -> Value {
        let path = self.dir.path().join(format!("record-{n}"));
        let deadline = std::time::Instant::now() + TIMEOUT;
        while !path.exists() {
            assert!(std::time::Instant::now() < deadline, "no process {n}");
            std::thread::sleep(Duration::from_millis(5));
        }
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The script ran to its `### eof`, so it received exactly the lines
    /// it expected.
    fn finished(&self, n: usize) {
        let eof = self.dir.path().join(format!("eof-{n}"));
        let deadline = std::time::Instant::now() + TIMEOUT;
        while !eof.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "script {n} unfinished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn text(&self) -> String {
        std::fs::read_to_string(&self.draft).unwrap()
    }

    /// The records reported so far.
    fn records_now(&mut self) -> Vec<Record> {
        std::iter::from_fn(|| self.records.try_recv().ok()).collect()
    }
}

#[test]
fn allowed_writes_are_reported_and_writes_elsewhere_refused() {
    let h = Harness::new("Hello world\n", |draft, root| {
        let outside = root.join("notes.txt");
        let edit1 = edit(draft, "Hello", "Hi");
        let edit2 = edit(draft, "world", "all");
        let write = json!({"file_path": outside, "content": "x"});
        let script = [
            INIT.into(),
            prompt_in(
                &[target_block(SLACK), draft_block("Hello world\n")],
                "Shorten it",
            ),
            state("running"),
            tool_use("t1", "Edit", edit1.clone()),
            hook("h1", "t1", "Edit", edit1.clone()),
            proceed("h1"),
            permission("p1", "t1", "Edit", edit1.clone()),
            answer("p1", json!({"behavior": "allow", "updatedInput": edit1})),
            write_file(draft, "Hi world\n"),
            tool_result("t1", false),
            tool_use("t2", "Write", write.clone()),
            hook("h2", "t2", "Write", write),
            answer(
                "h2",
                denied(&format!(
                    "Draftmux: Write is limited to the draft file {}; \"{}\" is outside it. \
                     Give the draft's absolute path, and use Read, Glob or Grep to inspect \
                     other files.",
                    draft.display(),
                    outside.display()
                )),
            ),
            tool_result("t2", true),
            tool_use("t3", "Edit", edit2.clone()),
            hook("h3", "t3", "Edit", edit2),
            proceed("h3"),
            write_file(draft, "Hi all\n"),
            tool_result("t3", false),
            settle("completed"),
            // The model wrote the draft itself, so it isn't sent again.
            prompt_in(&[], "Thanks"),
            state("running"),
            settle("completed"),
            "### eof".into(),
        ];
        vec![script.join("\n")]
    });
    h.prompt("Shorten it");
    let events = h.settled();
    assert_eq!(
        finished_tools(&events),
        [
            ("t1".into(), true, false),
            ("t2".into(), false, true),
            ("t3".into(), true, false),
        ]
    );
    assert!(!h.dir.path().join("notes.txt").exists());
    assert_eq!(h.text(), "Hi all\n");
    h.prompt("Thanks");
    h.settled();
    h.agent().blur();
    h.finished(0);
    // The editing launch: explicit tools, permission prompts through the
    // agent, and no settings. The instructions go with the first prompt,
    // since a resumed session ignores a system prompt.
    let args: Vec<String> = serde_json::from_value(h.record(0)["args"].clone()).unwrap();
    let has = |pair: &[&str]| args.windows(pair.len()).any(|w| w == pair);
    assert!(has(&["--tools", "Read,Glob,Grep,Edit,Write"]));
    assert!(has(&["--permission-prompt-tool", "stdio"]));
    assert!(has(&["--setting-sources", ""]));
    assert!(!args.iter().any(|a| a == "--append-system-prompt"));
}

#[test]
fn context_is_sent_when_it_changed_and_after_compaction_or_a_restart() {
    let h = Harness::new("One\n", |draft, _| {
        let rules = || instructions_block(draft, "Be brief.");
        let run = || [state("running"), settle("completed")].join("\n");
        let first = [
            INIT.into(),
            prompt_in(&[rules(), target_block(SLACK), draft_block("One\n")], "a"),
            run(),
            prompt_in(&[], "b"),
            run(),
            // The user edited the draft and changed the target.
            prompt_in(&[target_block("plain text"), draft_block("Two")], "c"),
            run(),
            // A slash command goes alone and leaves the context as sent.
            prompt_in(&[], "/compact"),
            state("running"),
            line(json!({"type": "system", "subtype": "compact_boundary"})),
            settle("completed"),
            prompt_in(
                &[rules(), target_block("plain text"), draft_block("Two")],
                "d",
            ),
            run(),
            "### eof".into(),
        ];
        let second = [
            INIT.into(),
            prompt_in(
                &[rules(), target_block("plain text"), draft_block("Two")],
                "e",
            ),
            run(),
            "### eof".into(),
        ];
        vec![first.join("\n"), second.join("\n")]
    });
    for text in ["a", "b"] {
        h.prompt(text);
        h.settled();
    }
    std::fs::write(&h.draft, "Two").unwrap();
    *h.target.lock().unwrap() = Some("plain text".into());
    for text in ["c", "/compact", "d"] {
        h.prompt(text);
        h.settled();
    }
    // Switching away stops claude; coming back starts another.
    h.agent().blur();
    h.finished(0);
    h.agent().focus().unwrap();
    assert!(matches!(h.next(), Event::Ready(_)));
    h.prompt("e");
    h.settled();
    h.agent().blur();
    h.finished(1);
}

#[test]
fn the_draft_counts_as_seen_as_the_model_saw_it() {
    let h = Harness::new("One\n", |draft, _| {
        let read = json!({"file_path": draft});
        let change = edit(draft, "Two", "Four");
        let partial = json!({"file_path": draft, "limit": 1});
        let result = |id: &str, shown: u64| {
            line(
                json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": "1\tTwo"}]},
                "tool_use_result": {"type": "text", "file": {"filePath": draft,
                    "content": "Two\n", "numLines": shown, "startLine": 1, "totalLines": 2}}}),
            )
        };
        let script = [
            INIT.into(),
            prompt_in(&[target_block(SLACK), draft_block("One\n")], "a"),
            state("running"),
            tool_use("t1", "Read", partial.clone()),
            hook("h1", "t1", "Read", partial),
            proceed("h1"),
            result("t1", 1),
            settle("completed"),
            // Only part of it was read.
            prompt_in(&[draft_block("Two\n")], "b"),
            state("running"),
            tool_use("t2", "Read", read.clone()),
            hook("h2", "t2", "Read", read.clone()),
            proceed("h2"),
            result("t2", 2),
            settle("completed"),
            prompt_in(&[], "c"),
            state("running"),
            // Something else changes the draft after the model read it.
            tool_use("t3", "Read", read.clone()),
            hook("h3", "t3", "Read", read),
            proceed("h3"),
            write_file(draft, "Three\n"),
            result("t3", 2),
            settle("completed"),
            prompt_in(&[draft_block("Three\n")], "d"),
            state("running"),
            // And after the model wrote it.
            tool_use("t4", "Edit", change.clone()),
            hook("h4", "t4", "Edit", change),
            proceed("h4"),
            write_file(draft, "Four\nouter\n"),
            tool_result("t4", false),
            settle("completed"),
            prompt_in(&[draft_block("Four\nouter\n")], "e"),
            state("running"),
            settle("completed"),
            "### eof".into(),
        ];
        vec![script.join("\n")]
    });
    h.prompt("a");
    h.settled();
    // The user edits the draft; the model reads only part of it.
    std::fs::write(&h.draft, "Two\n").unwrap();
    for text in ["b", "c", "d", "e"] {
        h.prompt(text);
        h.settled();
    }
    h.agent().blur();
    h.finished(0);
}

#[test]
fn an_interrupted_or_crashed_write_counts_only_if_it_landed() {
    let h = Harness::new("Hello\nWorld\n", |draft, _| {
        let first = edit(draft, "Hello", "Hi");
        // Deleting a line takes its newline.
        let second = edit(draft, "Hi", "");
        let crashed = edit(draft, "World", "Yo");
        let script = [
            INIT.into(),
            prompt_in(&[target_block(SLACK), draft_block("Hello\nWorld\n")], "a"),
            state("running"),
            tool_use("t1", "Edit", first.clone()),
            hook("h1", "t1", "Edit", first),
            proceed("h1"),
            write_file(draft, "Hi\nWorld\n"),
            tool_use("t2", "Edit", second.clone()),
            hook("h2", "t2", "Edit", second),
            proceed("h2"),
            write_file(draft, "World\n"),
            // Interrupted before either reports.
            settle("aborted_tools"),
            prompt_in(&[draft_block("World\n")], "b"),
            state("running"),
            tool_use("t3", "Edit", crashed.clone()),
            hook("h3", "t3", "Edit", crashed),
            proceed("h3"),
            // Something else changes the draft before the edit lands.
            write_file(draft, "outer\n"),
            "### exit 1".into(),
        ];
        vec![script.join("\n")]
    });
    h.prompt("a");
    let events = h.settled();
    assert_eq!(
        finished_tools(&events),
        [("t1".into(), true, true), ("t2".into(), true, true)]
    );
    assert_eq!(events.last(), Some(&Event::RunSettled { aborted: true }));

    h.prompt("b");
    let events = h.until(|event| matches!(event, Event::Crashed { .. }));
    assert_eq!(finished_tools(&events), [("t3".into(), false, true)]);
    let Some(Event::Crashed { failed_prompt, .. }) = events.last() else {
        unreachable!();
    };
    assert_eq!(failed_prompt.as_deref(), Some("b"));
}

#[test]
fn a_write_the_hook_never_saw_turns_editing_off() {
    let h = Harness::new("Hello\n", |draft, _| {
        let unseen = edit(draft, "Hello", "Hi");
        let first = [
            INIT.into(),
            prompt_in(&[target_block(SLACK), draft_block("Hello\n")], "a"),
            state("running"),
            tool_use("t1", "Edit", unseen),
            write_file(draft, "Hi\n"),
            tool_result("t1", false),
            "### eof".into(),
        ];
        let second: [String; 2] = [INIT.into(), "### eof".into()];
        vec![first.join("\n"), second.join("\n")]
    });
    h.prompt("a");
    let events = h.until(|event| matches!(event, Event::Crashed { .. }));
    assert_eq!(finished_tools(&events), [("t1".into(), true, false)]);
    let Some(Event::Crashed { error, .. }) = events.last() else {
        unreachable!();
    };
    assert!(error.contains("Editing is off"), "{error}");
    h.finished(0);
    h.agent().focus().unwrap();
    assert!(matches!(h.next(), Event::Ready(_)));
    let args: Vec<String> = serde_json::from_value(h.record(1)["args"].clone()).unwrap();
    assert!(args.windows(2).any(|w| w == ["--tools", "Read,Glob,Grep"]));
    h.agent().blur();
    h.finished(1);
}

#[test]
fn a_write_permission_without_the_hook_is_refused_and_turns_editing_off() {
    let h = Harness::new("Hello\n", |draft, _| {
        let script = [
            INIT.into(),
            permission(
                "p1",
                "t1",
                "Write",
                json!({"file_path": draft, "content": "x"}),
            ),
            answer(
                "p1",
                json!({"behavior": "deny",
                    "message": "Draftmux: this write wasn't checked, so it isn't allowed."}),
            ),
            "### eof".into(),
        ];
        vec![script.join("\n")]
    });
    h.agent().focus().unwrap();
    assert!(matches!(h.next(), Event::Ready(_)));
    assert!(matches!(h.next(), Event::Crashed { .. }));
    h.finished(0);
    assert_eq!(h.text(), "Hello\n");
}

#[test]
fn hook_calls_that_cannot_be_checked_are_refused() {
    let h = Harness::new("Hello\n", |draft, _| {
        let no_id = line(
            json!({"type": "control_request", "request_id": "h1", "request": {
            "subtype": "hook_callback", "callback_id": "draftmux_hook_0",
            "input": {"tool_name": "Edit", "tool_input": edit(draft, "a", "b")}}}),
        );
        let script = [
            INIT.into(),
            no_id,
            answer("h1", denied("Draftmux: the tool call has no id.")),
            hook("h2", "t2", "Edit", json!({"old_string": "a"})),
            answer(
                "h2",
                denied(&format!(
                    "Draftmux: Edit needs the absolute path of the draft file {}.",
                    draft.display()
                )),
            ),
            hook("h3", "t3", "Bash", json!({"command": "true"})),
            answer(
                "h3",
                denied(
                    "Draftmux: Bash isn't available in this draft. Use Read, Glob or Grep, \
                     and Edit or Write on the draft file.",
                ),
            ),
            permission("p4", "t4", "Bash", json!({"command": "true"})),
            answer(
                "p4",
                json!({"behavior": "deny",
                    "message": "Draftmux: Bash isn't available in this draft."}),
            ),
            "### eof".into(),
        ];
        vec![script.join("\n")]
    });
    h.agent().focus().unwrap();
    assert!(matches!(h.next(), Event::Ready(_)));
    std::thread::sleep(Duration::from_millis(300));
    h.agent().blur();
    h.finished(0);
}

#[test]
fn a_failed_login_is_shown_once() {
    let h = Harness::new("Hello\n", |_, _| {
        let script = [
            INIT.into(),
            prompt_in(&[target_block(SLACK), draft_block("Hello\n")], "a"),
            state("running"),
            line(
                json!({"type": "assistant", "error": "authentication_failed",
                "message": {"id": "m1", "content": [
                    {"type": "text", "text": "Not logged in · Please run /login"}]}}),
            ),
            line(
                json!({"type": "result", "subtype": "success", "is_error": true,
                "result": "Not logged in · Please run /login"}),
            ),
            state("idle"),
            "### eof".into(),
        ];
        vec![script.join("\n")]
    });
    h.prompt("a");
    let events = h.settled();
    assert_eq!(
        events[1..],
        [
            Event::RunStarted,
            Event::AssistantStarted,
            Event::AssistantEnded {
                text: String::new(),
                stop: Stop::Error(Some(
                    "Not logged in · Please run /login\n\nClaude Code isn't logged in. Run \
                     `claude` in a terminal and log in, then send again."
                        .into()
                )),
            },
            Event::RunSettled { aborted: false },
        ]
    );
}

#[test]
fn streamed_messages_end_with_their_blocks() {
    let h = Harness::new("Hello\n", |_, _| {
        let event = |event: Value| line(json!({"type": "stream_event", "event": event}));
        let text = |id: &str, text: &str| {
            line(
                json!({"type": "assistant", "message": {"id": id, "content": [
                {"type": "text", "text": text}]}}),
            )
        };
        let script = [
            INIT.into(),
            prompt_in(&[target_block(SLACK), draft_block("Hello\n")], "a"),
            state("running"),
            event(json!({"type": "message_start", "message": {"id": "m1"}})),
            event(json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text"}})),
            event(
                json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "One"}}),
            ),
            text("m1", "One"),
            event(json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "text"}})),
            event(
                json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Two"}}),
            ),
            text("m1", "Two"),
            event(json!({"type": "message_stop"})),
            settle("completed"),
            "### eof".into(),
        ];
        vec![script.join("\n")]
    });
    h.prompt("a");
    let events = h.settled();
    assert_eq!(
        events[1..],
        [
            Event::RunStarted,
            Event::AssistantStarted,
            Event::AssistantText("One".into()),
            Event::AssistantBlock,
            Event::AssistantText("Two".into()),
            Event::AssistantEnded {
                text: "One\n\nTwo".into(),
                stop: Stop::Completed,
            },
            Event::RunSettled { aborted: false },
        ]
    );
}

#[test]
fn a_chosen_effort_is_recorded_reported_and_passed_to_the_next_process() {
    let mut h = Harness::new("Hello\n", |_, _| {
        let apply = |id: &str, effort: Value| {
            [
                expect(json!({"type": "control_request", "request_id": id,
                    "request": {"subtype": "apply_flag_settings",
                        "settings": {"effortLevel": effort}}})),
                line(json!({"type": "control_response", "response":
                    {"subtype": "success", "request_id": id, "response": null}})),
            ]
        };
        let mut first = vec![INIT.to_owned()];
        first.extend(apply("m", json!("high")));
        first.push("### eof".into());
        let mut second = vec![INIT.to_owned()];
        second.extend(apply("m", Value::Null));
        second.push("### eof".into());
        vec![first.join("\n"), second.join("\n")]
    });
    h.agent().focus().unwrap();
    let state = |thinking: Option<&str>| draftmux_agent::State {
        model: Some(draftmux_agent::Model {
            provider: "claude".into(),
            id: "haiku".into(),
        }),
        thinking: thinking.map(Into::into),
        running: None,
    };
    assert_eq!(h.next(), Event::Ready(state(None)));
    assert_eq!(
        block_on(h.agent().thinking_levels()).unwrap(),
        ["low", "high"]
    );
    let set = block_on(h.agent().set_thinking(Some("high".into()))).unwrap();
    assert_eq!(set, state(Some("high")));

    h.agent().blur();
    h.finished(0);
    h.agent().focus().unwrap();
    assert_eq!(h.next(), Event::Ready(state(Some("high"))));
    let args = h.record(1)["args"].to_string();
    assert!(args.contains(r#""--effort","high""#), "{args}");

    // Auto clears it, for this process and the next.
    let auto = block_on(h.agent().set_thinking(None)).unwrap();
    assert_eq!(auto, state(None));
    let mut records = h.records_now();
    records.retain(|record| matches!(record, Record::Effort(_)));
    assert_eq!(
        records,
        [Record::Effort(Some("high".into())), Record::Effort(None)]
    );
}

#[test]
fn a_switched_model_is_recorded_reported_and_passed_to_the_next_process() {
    let mut h = Harness::new("Hello\n", |_, _| {
        let first = [
            INIT.into(),
            expect(json!({"type": "control_request", "request_id": "m",
                "request": {"subtype": "set_model", "model": "sonnet"}})),
            line(json!({"type": "control_response", "response":
                {"subtype": "success", "request_id": "m", "response": null}})),
            prompt_in(&[target_block(SLACK), draft_block("Hello\n")], "a"),
            state("running"),
            line(json!({"type": "system", "subtype": "init", "model": "claude-sonnet-5-5"})),
            settle("completed"),
            "### eof".into(),
        ];
        let second: [String; 2] = [INIT.into(), "### eof".into()];
        vec![first.join("\n"), second.join("\n")]
    });
    let unknown = block_on(h.agent().available_models()).unwrap_err();
    assert!(unknown.message.contains("once it has started"), "{unknown}");
    h.agent().focus().unwrap();
    let state = |model: &str, running: Option<&str>| {
        Event::Ready(draftmux_agent::State {
            model: Some(draftmux_agent::Model {
                provider: "claude".into(),
                id: model.into(),
            }),
            thinking: None,
            running: running.map(Into::into),
        })
    };
    assert_eq!(h.next(), state("haiku", None));
    let models = block_on(h.agent().available_models()).unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["haiku"]
    );

    let switched = block_on(h.agent().switch_model(draftmux_agent::Model {
        provider: "claude".into(),
        id: "sonnet".into(),
    }))
    .unwrap();
    assert_eq!(Event::Ready(switched), state("sonnet", None));
    h.prompt("a");
    let events = h.settled();
    assert!(
        events.contains(&state("sonnet", Some("claude-sonnet-5-5"))),
        "{events:?}"
    );

    h.agent().blur();
    h.finished(0);
    h.agent().focus().unwrap();
    assert_eq!(h.next(), state("sonnet", None));
    let args = h.record(1)["args"].to_string();
    assert!(args.contains(r#""--model","sonnet""#), "{args}");
    let mut records = h.records_now();
    records.retain(|record| matches!(record, Record::Model(_)));
    assert_eq!(records, [Record::Model("sonnet".into())]);
}

#[test]
fn compaction_reports_its_start_end_and_the_size_after() {
    let h = Harness::new("One\n", |draft, _| {
        let rules = instructions_block(draft, "Be brief.");
        let first = [
            INIT.into(),
            prompt_in(&[rules, target_block(SLACK), draft_block("One\n")], "a"),
            state("running"),
            line(
                json!({"type": "stream_event", "event": {"type": "message_start", "message": {
                "id": "m1",
                "usage": {"input_tokens": 3, "cache_creation_input_tokens": 1000,
                          "cache_read_input_tokens": 80000}}}}),
            ),
            line(
                json!({"type": "stream_event", "event": {"type": "message_delta",
                "usage": {"output_tokens": 40}}}),
            ),
            line(json!({"type": "stream_event", "event": {"type": "message_stop"}})),
            settle("completed"),
            prompt_in(&[], "/compact Keep the target."),
            state("running"),
            line(json!({"type": "system", "subtype": "status", "status": "compacting"})),
            line(json!({"type": "system", "subtype": "status", "status": null})),
            line(json!({"type": "system", "subtype": "compact_boundary",
                "compact_metadata": {"trigger": "manual", "pre_tokens": 81043, "post_tokens": 9000}})),
            settle("completed"),
            // A compaction that ends without a boundary didn't compact.
            prompt_in(&[], "/compact Keep the target."),
            state("running"),
            settle("completed"),
            "### eof".into(),
        ];
        vec![first.join("\n")]
    });
    h.prompt("a");
    let events = h.settled();
    let sizes: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            Event::Context(tokens) => Some(*tokens),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, [81003, 81043]);

    block_on(h.agent().compact("Keep the target.".into())).unwrap();
    let events = h.settled();
    assert!(events.contains(&Event::CompactionStarted));
    assert!(events.contains(&Event::CompactionEnded {
        compacted: true,
        tokens: Some(9000),
        error: None,
    }));

    block_on(h.agent().compact("Keep the target.".into())).unwrap();
    let events = h.settled();
    assert!(events.contains(&Event::CompactionEnded {
        compacted: false,
        tokens: None,
        error: None,
    }));
    h.agent().blur();
    h.finished(0);
}
