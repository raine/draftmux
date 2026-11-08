//! `fork` against `fake-pi` replaying scripts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use draftmux_pi_rpc::{ForkSpec, ModelOverride, ThinkingLevel, fork};

/// A fork with no warning.
fn forked(file: PathBuf) -> Result<draftmux_pi_rpc::Forked, String> {
    Ok(draftmux_pi_rpc::Forked {
        session_file: file,
        warning: None,
    })
}
use draftmux_storage::Target;
use serde_json::Value;
use tempfile::TempDir;

struct Harness {
    dir: TempDir,
    spec: ForkSpec,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("draft/session");
        std::fs::create_dir_all(&session_dir).unwrap();
        let spec = ForkSpec {
            source: dir.path().join("source.jsonl"),
            session_dir,
            extension: PathBuf::from("/app/draftmux.ts"),
            cwd: dir.path().to_path_buf(),
            agent_dir: PathBuf::from("/profiles/work/agent"),
            draft_path: dir.path().join("draft/draft.md"),
            target: Target::Slack,
            model: None,
            timeout: Duration::from_secs(10),
        };
        Self { dir, spec }
    }

    /// The session file pi would create.
    fn session_file(&self) -> PathBuf {
        let file = self.spec.session_dir.join("fork.jsonl");
        std::fs::write(&file, "").unwrap();
        file
    }

    fn fork(&self, script: &str) -> Result<draftmux_pi_rpc::Forked, String> {
        let path = self.dir.path().join("script");
        std::fs::write(&path, script).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fake-pi"));
        command
            .env_clear()
            .env("FAKE_PI_SCRIPT", &path)
            .env("FAKE_PI_RECORD", self.record_path())
            .env("DRAFTMUX_INSTRUCTIONS", "stale");
        fork(command, &self.spec)
    }

    fn record_path(&self) -> PathBuf {
        self.dir.path().join("record")
    }

    fn record(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.record_path()).unwrap()).unwrap()
    }
}

fn state(session_file: Option<&Path>) -> String {
    let file = session_file.map_or(Value::Null, |file| file.to_str().unwrap().into());
    serde_json::json!({
        "id": "x", "type": "response", "command": "get_state", "success": true,
        "data": {
            "thinkingLevel": "high", "isStreaming": false, "isCompacting": false,
            "sessionFile": file, "sessionId": "s", "messageCount": 2, "pendingMessageCount": 0,
        },
    })
    .to_string()
}

const GET_STATE: &str = r#">>> {"id":"x","type":"get_state"}"#;

#[test]
fn forks_into_the_session_dir_with_the_draft_environment() {
    let h = Harness::new();
    let file = h.session_file();
    let script = format!("{GET_STATE}\n{}\n### eof", state(Some(&file)));

    assert_eq!(h.fork(&script), forked(file));

    let record = h.record();
    let args: Vec<&str> = record["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let source = h.spec.source.to_str().unwrap();
    let session_dir = h.spec.session_dir.to_str().unwrap();
    assert_eq!(
        args,
        [
            "--mode",
            "rpc",
            "--fork",
            source,
            "--session-dir",
            session_dir,
            "-e",
            "/app/draftmux.ts"
        ]
    );
    assert_eq!(
        Path::new(record["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        h.spec.cwd.canonicalize().unwrap()
    );
    let env = &record["env"];
    assert_eq!(env["PI_CODING_AGENT_DIR"], "/profiles/work/agent");
    assert_eq!(env["DRAFTMUX_MODE"], "draft");
    assert_eq!(env["DRAFTMUX_TARGET"], "slack");
    assert_eq!(
        env["DRAFTMUX_DRAFT_PATH"],
        h.spec.draft_path.to_str().unwrap()
    );
    assert!(env.get("DRAFTMUX_INSTRUCTIONS").is_none());
}

#[test]
fn override_sets_model_then_thinking() {
    let mut h = Harness::new();
    h.spec.model = Some(ModelOverride {
        provider: "openai".into(),
        model_id: "gpt-6".into(),
        thinking: ThinkingLevel::Low,
    });
    let file = h.session_file();
    let script = [
        GET_STATE.to_string(),
        state(Some(&file)),
        r#">>> {"id":"y","type":"set_model","provider":"openai","modelId":"gpt-6"}"#.into(),
        r#"{"id":"y","type":"response","command":"set_model","success":true,"data":{}}"#.into(),
        r#">>> {"id":"z","type":"set_thinking_level","level":"low"}"#.into(),
        r#"{"id":"z","type":"response","command":"set_thinking_level","success":true}"#.into(),
        "### eof".into(),
    ]
    .join("\n");

    assert_eq!(h.fork(&script), forked(file));
}

#[test]
fn rejected_override_keeps_the_fork_with_a_warning() {
    let mut h = Harness::new();
    h.spec.model = Some(ModelOverride {
        provider: "openai".into(),
        model_id: "missing".into(),
        thinking: ThinkingLevel::Low,
    });
    let file = h.session_file();
    let script = [
        GET_STATE.to_string(),
        state(Some(&file)),
        r#">>> {"id":"y","type":"set_model","provider":"openai","modelId":"missing"}"#.into(),
        r#"{"id":"y","type":"response","command":"set_model","success":false,"error":"Model not found: openai/missing"}"#.into(),
        "### eof".into(),
    ]
    .join("\n");

    let forked = h.fork(&script).unwrap();
    assert_eq!(forked.session_file, file);
    let warning = forked.warning.unwrap();
    assert!(
        warning.contains("Model not found: openai/missing"),
        "{warning}"
    );
}

#[test]
fn session_file_must_be_in_the_session_dir() {
    let h = Harness::new();
    let outside = h.dir.path().join("elsewhere.jsonl");
    std::fs::write(&outside, "").unwrap();
    let missing = h.spec.session_dir.join("missing.jsonl");
    for file in [None, Some(outside.as_path()), Some(missing.as_path())] {
        let script = format!("{GET_STATE}\n{}\n### eof", state(file));
        let err = h.fork(&script).unwrap_err();
        assert!(err.contains("instead of a file in"), "{err}");
    }
}

#[test]
fn exit_before_answering_reports_stderr() {
    let h = Harness::new();
    let script = "### stderr Error: Cannot fork: source session file is empty\n### exit 1";
    let err = h.fork(script).unwrap_err();
    assert!(err.contains("pi exited before answering"), "{err}");
    assert!(err.contains("Cannot fork"), "{err}");
}

#[test]
fn spawn_failure_is_reported() {
    let mut h = Harness::new();
    h.spec.cwd = h.dir.path().join("missing");
    let err = h.fork("").unwrap_err();
    assert!(err.starts_with("couldn't start pi"), "{err}");
}

#[test]
fn timeout_names_a_startup_dialog_and_stops_pi() {
    let mut h = Harness::new();
    h.spec.timeout = Duration::from_secs(3);
    let script = concat!(
        r#"{"type":"extension_ui_request","id":"d1","method":"confirm","title":"Continue?"}"#,
        "\n### hang"
    );
    let started = Instant::now();
    let err = h.fork(script).unwrap_err();
    assert!(err.contains("didn't answer within"), "{err}");
    assert!(err.contains("Continue?"), "{err}");

    // fork waited for pi to exit: the hanging process is gone.
    let pid = h.record()["pid"].as_i64().unwrap() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(started.elapsed() < Duration::from_secs(15));
}
