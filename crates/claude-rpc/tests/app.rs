//! Claude drafts through Draftmux's core: a real store and core session,
//! `ClaudeAgent` against `fake-claude`, and the app's glue between them —
//! a prompt is sent only when core saved the draft, a reported draft write
//! is read from that draft's file and recorded for that draft, and records
//! are written into that draft's descriptor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use chrono::Utc;
use draftmux_agent::{Agent as _, Event};
use draftmux_claude_rpc::{ClaudeAgent, Config, DraftContext, DraftSession, LaunchSpec, Record};
use draftmux_core::session::{Effect, Outcome, Session};
use draftmux_storage::{
    Backend, ClaudeSession, DraftSettings, NewDraft, Source, SourceKind, Store, Target,
};
use futures::StreamExt;
use futures::executor::block_on;
use serde_json::json;
use tempfile::TempDir;

mod common;

use common::*;

const GITHUB: &str = "GitHub";

/// What a draft's agent reports, in order.
enum Incoming {
    Event(Event),
    Record(Record),
}

struct Draft {
    agent: ClaudeAgent,
    incoming: std_mpsc::Receiver<Incoming>,
    scripts: PathBuf,
    path: PathBuf,
}

/// The app with Claude drafts.
struct App {
    root: TempDir,
    store: Store,
    session: Session,
    drafts: HashMap<String, Draft>,
    /// Prompts core refused to send, with why.
    refused: Vec<String>,
}

impl App {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().canonicalize().unwrap().join("drafts"));
        Self {
            session: Session::new(store.clone()),
            store,
            root,
            drafts: HashMap::new(),
            refused: Vec::new(),
        }
    }

    fn root(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    /// Claude's storage for the drafts' sessions.
    fn config(&self) -> PathBuf {
        self.root().join("claude")
    }

    /// A Claude draft holding `text`, whose processes run the scripts
    /// `scripts` builds from the draft's path and session id.
    fn draft(&mut self, text: &str, scripts: impl Fn(&Path, &str) -> Vec<String>) -> String {
        let new = NewDraft {
            target: Target::Github,
            title: String::new(),
            source: Some(Source {
                kind: SourceKind::Claude,
                path: "/tmp/source.jsonl".into(),
            }),
            cwd: self.root(),
            backend: Backend::Claude(ClaudeSession {
                session_id: uuid::Uuid::new_v4(),
                transcript: None,
                config_dir: Some(self.config()),
                fork: None,
                model: None,
                effort: None,
                selection_model: None,
            }),
            note: None,
            summary: None,
            settings: DraftSettings::default(),
            text: text.into(),
            file: None,
        };
        let draft = self.store.create(new).unwrap();
        let Backend::Claude(claude) = &draft.meta.backend else {
            unreachable!()
        };
        let id = claude.session_id.to_string();
        let dir = self.root().join(format!("scripts-{}", draft.id));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, script) in scripts(&draft.draft_path(), &id).iter().enumerate() {
            std::fs::write(dir.join(format!("script-{i}")), script).unwrap();
        }
        let spec = LaunchSpec {
            program: PathBuf::from(env!("CARGO_BIN_EXE_fake-claude")),
            cwd: draft.meta.cwd.clone(),
            env: vec![
                ("PATH".into(), std::env::var_os("PATH").unwrap()),
                ("HOME".into(), self.root().into()),
            ],
            extra_env: vec![("FAKE_CLAUDE_DIR".into(), dir.clone().into())],
            model: claude.model.clone(),
            effort: claude.effort.clone(),
            tools: vec![],
            setting_sources: vec![],
            session: Some(draftmux_claude_rpc::Session::Draft(DraftSession {
                id,
                dir: draft.session_dir(),
                config_dir: claude.config_dir.clone(),
                fork_boundary: None,
                transcript: claude.transcript.clone(),
            })),
            hooks: vec![],
            permission_prompt: false,
            system_prompt: None,
        };
        let context = DraftContext {
            draft_path: draft.draft_path(),
            instructions: String::new(),
            target: Box::new(|| Some(GITHUB.into())),
            summary: None,
        };
        let config = Config {
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(5),
            max_record: 1 << 20,
        };
        let (agent, events, records) = ClaudeAgent::new(spec, config, context).unwrap();
        let (tx, incoming) = std_mpsc::channel();
        let mut merged =
            futures::stream::select(events.map(Incoming::Event), records.map(Incoming::Record));
        std::thread::spawn(move || {
            while let Some(item) = block_on(merged.next()) {
                if tx.send(item).is_err() {
                    return;
                }
            }
        });
        self.drafts.insert(
            draft.id.clone(),
            Draft {
                agent,
                incoming,
                scripts: dir,
                path: draft.draft_path(),
            },
        );
        self.session.drafts_mut().refresh();
        draft.id
    }

    fn agent(&self, id: &str) -> &ClaudeAgent {
        &self.drafts[id].agent
    }

    /// Selects `id` as the app does: the agent of the draft left is
    /// blurred, `id`'s focused.
    fn select(&mut self, id: &str) {
        if let Some(left) = self.session.drafts().current_id().map(str::to_owned) {
            self.agent(&left).blur();
        }
        let outcome = self.session.select(id).unwrap();
        self.apply(outcome);
        self.agent(id).focus().unwrap();
    }

    fn apply(&mut self, outcome: Outcome) {
        for effect in outcome.effects {
            match effect {
                Effect::LoadText { epoch, text } | Effect::ReplaceText { epoch, text } => {
                    self.session.loaded(epoch, &text);
                }
                // What `Chat::prompt` does with the prompt core hands over.
                Effect::Prompt { text, saved } => match saved {
                    Ok(_) => {
                        let id = self.session.drafts().current_id().unwrap().to_owned();
                        block_on(self.agent(&id).prompt(text)).unwrap();
                        self.session.prompt_accepted(&id);
                    }
                    Err(err) => self.refused.push(err),
                },
                _ => {}
            }
        }
    }

    #[cfg(unix)]
    fn type_text(&mut self, text: &str) {
        let outcome = self.session.edited(text.into(), Instant::now());
        self.apply(outcome);
    }

    fn send(&mut self, message: &str) {
        let outcome = self.session.send(message.into(), message);
        self.apply(outcome);
    }

    /// Handles `id`'s agent's reports, as the chat and the window do,
    /// until its run settles.
    fn settle(&mut self, id: &str) {
        loop {
            let incoming = self.drafts[id]
                .incoming
                .recv_timeout(TIMEOUT)
                .expect("an event");
            match incoming {
                Incoming::Event(Event::RunStarted) => self.session.agent_running(id, true),
                Incoming::Event(Event::RunSettled { .. }) => {
                    self.session.agent_running(id, false);
                    return;
                }
                Incoming::Event(Event::ToolFinished {
                    draft_write: true, ..
                }) => {
                    let text = std::fs::read_to_string(&self.drafts[id].path).unwrap();
                    let (recorded, outcome) = self.session.agent_wrote(id, &text, Utc::now());
                    recorded.unwrap();
                    self.apply(outcome);
                }
                Incoming::Record(record) => self.record(id, record),
                Incoming::Event(_) => {}
            }
        }
    }

    /// What `chat::record` keeps, through core.
    fn record(&mut self, id: &str, record: Record) {
        self.session
            .drafts_mut()
            .update_backend(id, |backend| {
                let Backend::Claude(claude) = backend else {
                    panic!("not Claude");
                };
                match record {
                    Record::Transcript(path) => claude.transcript = Some(path),
                    Record::Model(model) => claude.model = Some(model),
                    Record::Effort(effort) => claude.effort = effort,
                }
            })
            .unwrap();
    }

    /// Handles the records `id`'s agent reports within `wait`.
    fn records(&mut self, id: &str, wait: Duration) {
        while let Ok(incoming) = self.drafts[id].incoming.recv_timeout(wait) {
            if let Incoming::Record(record) = incoming {
                self.record(id, record);
            }
        }
    }

    /// The script of `id`'s process `n` ran to its `### eof`.
    fn finished(&self, id: &str, n: usize) {
        let eof = self.drafts[id].scripts.join(format!("eof-{n}"));
        let deadline = Instant::now() + TIMEOUT;
        while !eof.exists() {
            assert!(Instant::now() < deadline, "script {n} unfinished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    fn meta_path(&self, id: &str) -> PathBuf {
        self.store.load(id).unwrap().dir.join("meta.json")
    }

    fn prompts(&self, id: &str) -> Vec<String> {
        let history = self.store.history(id).unwrap();
        history.edits.into_iter().map(|edit| edit.prompt).collect()
    }
}

/// A run that edits the draft at `path` from `before` to `after`, after a
/// write outside it that the hook refuses.
fn editing_run(path: &Path, before: &str, after: &str, message: &str) -> Vec<String> {
    let outside = path.parent().unwrap().join("notes.txt");
    let edit = edit(path, before.trim_end(), after.trim_end());
    let write = json!({"file_path": outside, "content": "x"});
    vec![
        prompt_in(&[target_block(GITHUB), draft_block(before)], message),
        state("running"),
        tool_use("t1", "Write", write.clone()),
        hook("h1", "t1", "Write", write),
        answer(
            "h1",
            denied(&format!(
                "Draftmux: Write is limited to the draft file {}; \"{}\" is outside it. \
                 Give the draft's absolute path, and use Read, Glob, Grep or Bash to inspect \
                 other files.",
                path.display(),
                outside.display()
            )),
        ),
        tool_result("t1", true),
        tool_use("t2", "Edit", edit.clone()),
        hook("h2", "t2", "Edit", edit),
        proceed("h2"),
        write_file(path, after),
        tool_result("t2", false),
        settle("completed"),
    ]
}

// fake-claude's `no-input` directive polls stdin, which it can do only on
// Unix.
#[cfg(unix)]
#[test]
fn a_draft_that_cant_be_saved_sends_nothing() {
    let mut app = App::new();
    let a = app.draft("Hello\n", |_, _| {
        vec![[INIT, "### no-input 300", "### eof"].join("\n")]
    });
    app.select(&a);
    app.type_text("Hello there\n");
    let meta_path = app.meta_path(&a);
    let meta = std::fs::read(&meta_path).unwrap();
    std::fs::write(&meta_path, "{").unwrap();

    app.send("Shorten it");

    assert_eq!(app.refused.len(), 1, "core refused the prompt");
    app.agent(&a).blur();
    app.finished(&a, 0);
    std::fs::write(&meta_path, meta).unwrap();
    assert!(app.prompts(&a).is_empty());
}

#[test]
fn an_allowed_write_updates_its_draft_and_history_and_a_refused_one_nothing() {
    let mut app = App::new();
    let a = app.draft("Hello world\n", |path, _| {
        let mut script = vec![INIT.to_owned()];
        script.extend(editing_run(
            path,
            "Hello world\n",
            "Hi world\n",
            "Shorten it",
        ));
        script.push("### eof".into());
        vec![script.join("\n")]
    });
    app.select(&a);

    app.send("Shorten it");
    app.settle(&a);

    assert_eq!(app.session.text(), "Hi world\n");
    assert_eq!(app.store.read_text(&a).unwrap(), "Hi world\n");
    assert_eq!(app.prompts(&a), ["Shorten it"]);
    let edits = app.store.history(&a).unwrap().edits;
    assert_eq!(edits[0].before, "Hello world\n");
    let outside = app.drafts[&a].path.parent().unwrap().join("notes.txt");
    assert!(!outside.exists());
}

#[test]
fn late_writes_after_switching_or_archiving_go_to_their_own_draft() {
    for archive in [false, true] {
        let mut app = App::new();
        let a = app.draft("Hello world\n", |path, _| {
            let mut script = vec![INIT.to_owned()];
            script.extend(editing_run(
                path,
                "Hello world\n",
                "Hi world\n",
                "Shorten it",
            ));
            script.push("### eof".into());
            vec![script.join("\n")]
        });
        let b = app.draft("Other\n", |_, _| vec![[INIT, "### eof"].join("\n")]);
        app.select(&a);
        app.send("Shorten it");

        // Before any of the run's reports are handled.
        if archive {
            let next = app.session.archive(&a).unwrap();
            assert_eq!(next.as_deref(), Some(b.as_str()));
        }
        app.select(&b);
        app.settle(&a);

        assert_eq!(app.session.drafts().current_id(), Some(b.as_str()));
        assert_eq!(app.session.text(), "Other\n", "archived: {archive}");
        assert_eq!(app.store.read_text(&b).unwrap(), "Other\n");
        assert!(app.prompts(&b).is_empty());
        assert_eq!(app.store.read_text(&a).unwrap(), "Hi world\n");
        assert_eq!(app.prompts(&a), ["Shorten it"], "archived: {archive}");

        if archive {
            app.session.drafts_mut().unarchive(&a).unwrap();
        }
        app.select(&a);
        assert_eq!(app.session.text(), "Hi world\n");
        assert_eq!(app.session.log().edits().len(), 1);
    }
}

#[test]
fn the_transcript_and_model_are_kept_in_the_drafts_descriptor() {
    let mut app = App::new();
    let config = app.config();
    let a = app.draft("Hello\n", |_, id| {
        let native = config.join(format!("projects/-draft/{id}.jsonl"));
        let first = [
            INIT.to_owned(),
            expect(json!({"type": "control_request", "request_id": "m",
                "request": {"subtype": "set_model", "model": "sonnet"}})),
            line(json!({"type": "control_response", "response":
                {"subtype": "success", "request_id": "m", "response": null}})),
            prompt_in(&[target_block(GITHUB), draft_block("Hello\n")], "Hi"),
            state("running"),
            format!(
                "### append {} {}",
                native.display(),
                json!({"type": "summary"})
            ),
            settle("completed"),
            "### eof".into(),
        ];
        vec![first.join("\n")]
    });
    app.select(&a);
    let switched = app.agent(&a).switch_model(draftmux_agent::Model {
        provider: "claude".into(),
        id: "sonnet".into(),
    });
    block_on(switched).unwrap();
    app.send("Hi");
    app.settle(&a);
    // Leaving the draft stops claude, and the draft adopts the transcript.
    app.agent(&a).blur();
    app.finished(&a, 0);
    app.records(&a, Duration::from_millis(500));

    let draft = app.store.load(&a).unwrap();
    let Backend::Claude(claude) = &draft.meta.backend else {
        panic!("not Claude");
    };
    let owned = draft
        .session_dir()
        .join(format!("{}.jsonl", claude.session_id));
    assert!(owned.is_file());
    assert_eq!(claude.transcript.as_deref(), Some(owned.as_path()));
    assert_eq!(claude.model.as_deref(), Some("sonnet"));
    let current = app.session.drafts().current().unwrap();
    assert_eq!(current.meta.backend, draft.meta.backend);
}
