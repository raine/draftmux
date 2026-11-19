//! One-off `claude -p` requests about a draft, such as selection actions,
//! that leave the draft's own session as it was.
//!
//! The request continues a copy of the conversation so far as a new session
//! (`--resume <copy> --fork-session`) that Claude Code doesn't save
//! (`--no-session-persistence`), with no tools, settings or MCP servers.
//! Claude Code 2.1.293 was observed to keep the copy byte-identical, write
//! no transcript for either session and still answer from the conversation,
//! including one with tool calls, when run this way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::launch::{clean_command, home};
use crate::protocol::{result_aborted, result_error};
use crate::session::{DraftSession, Snapshot};

/// A request on a draft's session. The prompt goes on stdin and the reply
/// comes as stream-json on stdout.
pub struct Oneshot {
    /// The `claude` executable, as for [`LaunchSpec::program`](crate::LaunchSpec::program).
    pub program: PathBuf,
    pub cwd: PathBuf,
    /// The environment to take allowlisted variables from.
    pub env: Vec<(OsString, OsString)>,
    pub session: DraftSession,
    /// None continues on the session's own model.
    pub model: Option<String>,
    /// As `--effort` takes it; none leaves the model's default.
    pub effort: Option<String>,
}

impl Oneshot {
    /// The command, once the conversation so far is copied into `temp`,
    /// which must outlive the process. A session that hasn't started yet
    /// has no conversation to continue, so the request starts a new one.
    pub fn command(&self, temp: &Path) -> Result<Command, String> {
        let storage = self.session.storage(&home(self.env.iter()));
        let mut copy = None;
        for retry in [false, true] {
            let Some(source) = self.session.conversation(&storage)? else {
                break;
            };
            match Snapshot::take(&source, temp) {
                Ok(snapshot) => {
                    copy = Some(snapshot.path);
                    break;
                }
                // The draft adopts its transcript when its process stops,
                // which can move it between finding and reading it.
                Err(_) if !retry && !source.is_file() => continue,
                Err(err) => return Err(err),
            }
        }

        let mut command = clean_command(&self.program, &self.env);
        command
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--tools",
                "",
                "--no-session-persistence",
            ])
            .current_dir(&self.cwd);
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        if let Some(effort) = &self.effort {
            command.arg("--effort").arg(effort);
        }
        if let Some(copy) = copy {
            command.arg("--resume").arg(copy).arg("--fork-session");
        }
        // The conversation's credentials and settings; an inherited one is
        // cleared with the rest.
        if let Some(dir) = &self.session.config_dir {
            command.env("CLAUDE_CONFIG_DIR", dir);
        }
        Ok(command)
    }
}

/// What a stdout frame of a request means for its reply.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply<'a> {
    /// A new assistant message, which replaces any text so far, such as the
    /// one claude asks for after a reply with nothing but thinking.
    Message,
    /// A text block starts in the current message.
    TextBlock,
    Text(&'a str),
    /// The request is over: the final reply's text, or why there is none.
    /// Later frames don't change it.
    Done(Result<&'a str, String>),
    Other,
}

impl<'a> Reply<'a> {
    pub fn of(frame: &'a Value) -> Reply<'a> {
        match frame["type"].as_str() {
            Some("stream_event") => {
                let event = &frame["event"];
                match event["type"].as_str() {
                    Some("message_start") => Reply::Message,
                    Some("content_block_start") if event["content_block"]["type"] == "text" => {
                        Reply::TextBlock
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "text_delta" => event
                        ["delta"]["text"]
                        .as_str()
                        .map_or(Reply::Other, Reply::Text),
                    _ => Reply::Other,
                }
            }
            // Precedes the `result`, which says less.
            Some("assistant") if frame["error"] == "authentication_failed" => Reply::Done(Err(
                "Claude Code isn't logged in. Run `claude` in a terminal \
                                 and log in, then try again."
                    .into(),
            )),
            Some("result") => Reply::Done(if result_aborted(frame) {
                Err("Claude Code's request was cancelled".into())
            } else if let Some(error) = result_error(frame) {
                Err(error)
            } else if frame["stop_reason"] == "max_tokens" {
                Err("Claude Code's reply was cut off".into())
            } else {
                Ok(frame["result"].as_str().unwrap_or_default())
            }),
            _ => Reply::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    const SOURCE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/transcripts/fork-source.jsonl"
    );
    const ID: &str = "11111111-1111-4111-8111-111111111111";

    fn oneshot(dir: &Path, home: &Path) -> Oneshot {
        Oneshot {
            program: "/opt/claude".into(),
            cwd: "/repo".into(),
            env: [
                ("HOME", home.to_str().unwrap()),
                ("PATH", "/usr/bin"),
                ("CLAUDECODE", "1"),
                ("CLAUDE_CONFIG_DIR", "/inherited"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
            session: DraftSession {
                id: ID.into(),
                dir: dir.to_path_buf(),
                config_dir: None,
                fork_boundary: None,
                transcript: None,
            },
            model: None,
            effort: None,
        }
    }

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    const BASE: [&str; 13] = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--tools",
        "",
        "--no-session-persistence",
        "--model",
        "haiku",
    ];

    #[test]
    fn a_session_not_started_yet_starts_a_new_unsaved_one() {
        let tmp = tempfile::tempdir().unwrap();
        let temp = tmp.path().join("temp");
        fs::create_dir(&temp).unwrap();
        let mut request = oneshot(&tmp.path().join("draft"), tmp.path());
        request.model = Some("haiku".into());
        let command = request.command(&temp).unwrap();
        assert_eq!(args(&command), BASE);
        assert_eq!(command.get_current_dir(), Some(Path::new("/repo")));
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("PATH".as_ref(), Some("/usr/bin".as_ref()))));
        for dropped in ["CLAUDECODE", "CLAUDE_CONFIG_DIR"] {
            assert!(!envs.iter().any(|(key, _)| *key == dropped), "{dropped}");
        }
        assert_eq!(fs::read_dir(&temp).unwrap().count(), 0);
    }

    #[test]
    fn the_effort_is_passed_on() {
        let tmp = tempfile::tempdir().unwrap();
        let temp = tmp.path().join("temp");
        fs::create_dir(&temp).unwrap();
        let mut request = oneshot(&tmp.path().join("draft"), tmp.path());
        request.effort = Some("low".into());
        let args = args(&request.command(&temp).unwrap());
        assert!(
            args.ends_with(&["--effort".into(), "low".into()]),
            "{args:?}"
        );
    }

    #[test]
    fn the_conversation_is_continued_from_a_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let (draft, temp) = (tmp.path().join("draft"), tmp.path().join("temp"));
        fs::create_dir(&draft).unwrap();
        fs::create_dir(&temp).unwrap();
        let mut request = oneshot(&draft, tmp.path());
        request.session.config_dir = Some(tmp.path().join("config"));

        // Still in Claude Code's storage, as after the first run.
        let native = tmp.path().join(format!("config/projects/-repo/{ID}.jsonl"));
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        fs::copy(SOURCE, &native).unwrap();
        let command = request.command(&temp).unwrap();
        let copy = temp.join("source.jsonl");
        assert_eq!(
            args(&command)[11..],
            ["--resume", copy.to_str().unwrap(), "--fork-session"]
        );
        assert_eq!(fs::read(&copy).unwrap(), fs::read(SOURCE).unwrap());
        assert!(native.is_file(), "left where it is");
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&(
            "CLAUDE_CONFIG_DIR".as_ref(),
            Some(tmp.path().join("config").as_os_str())
        )));

        // Adopted into the draft.
        fs::rename(&native, request.session.owned()).unwrap();
        let again = tmp.path().join("again");
        fs::create_dir(&again).unwrap();
        let command = request.command(&again).unwrap();
        assert_eq!(
            args(&command)[12],
            again.join("source.jsonl").to_str().unwrap()
        );
        assert_eq!(
            fs::read(request.session.owned()).unwrap(),
            fs::read(SOURCE).unwrap()
        );
    }

    #[test]
    fn a_fork_not_started_yet_continues_its_snapshot_and_a_lost_transcript_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let (draft, temp) = (tmp.path().join("draft"), tmp.path().join("temp"));
        fs::create_dir(&draft).unwrap();
        fs::create_dir(&temp).unwrap();
        let mut request = oneshot(&draft, tmp.path());
        request.session.fork_boundary = Some("b".into());
        let missing = request.command(&temp).unwrap_err();
        assert!(missing.contains("text is unaffected"), "{missing}");

        Snapshot::take(Path::new(SOURCE), &draft).unwrap();
        let command = request.command(&temp).unwrap();
        assert_eq!(args(&command)[11], "--resume");
        assert_eq!(
            fs::read(temp.join("source.jsonl")).unwrap(),
            fs::read(SOURCE).unwrap()
        );

        // Recorded, then lost: never a new conversation.
        request.session.transcript = Some(tmp.path().join(format!("gone/{ID}.jsonl")));
        let missing = request.command(&tmp.path().join("unused")).unwrap_err();
        assert!(missing.contains("transcript"), "{missing}");
    }

    #[test]
    fn replies_read_from_frames() {
        // Shapes recorded from 2.1.293.
        let delta = json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "index": 0, "delta": {"type": "text_delta", "text": "{\"alt"}}});
        assert_eq!(Reply::of(&delta), Reply::Text("{\"alt"));
        let thinking = json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"type": "thinking_delta", "thinking": "hm"}}});
        assert_eq!(Reply::of(&thinking), Reply::Other);
        let start = json!({"type": "stream_event", "event": {"type": "message_start",
            "message": {"role": "assistant"}}});
        assert_eq!(Reply::of(&start), Reply::Message);
        let block = json!({"type": "stream_event", "event": {"type": "content_block_start",
            "content_block": {"type": "text", "text": ""}}});
        assert_eq!(Reply::of(&block), Reply::TextBlock);
        let synthetic = json!({"type": "user", "isSynthetic": true, "message": {"content":
            [{"type": "text", "text": "[Your previous response had no visible output.]"}]}});
        assert_eq!(Reply::of(&synthetic), Reply::Other);

        let done = json!({"type": "result", "subtype": "success", "is_error": false,
            "result": "{\"alternatives\": [\"a\"]}", "stop_reason": "end_turn",
            "terminal_reason": "completed"});
        assert_eq!(
            Reply::of(&done),
            Reply::Done(Ok("{\"alternatives\": [\"a\"]}"))
        );
        let auth = json!({"type": "assistant", "error": "authentication_failed",
            "message": {"content": [{"type": "text", "text": "Not logged in · Please run /login"}]}});
        assert!(matches!(Reply::of(&auth), Reply::Done(Err(e)) if e.contains("log in")));
        let failed = json!({"type": "result", "subtype": "success", "is_error": true,
            "result": "Not logged in · Please run /login", "terminal_reason": "api_error"});
        assert_eq!(
            Reply::of(&failed),
            Reply::Done(Err("Not logged in · Please run /login".into()))
        );
        let aborted = json!({"type": "result", "subtype": "error_during_execution",
            "is_error": true, "terminal_reason": "aborted_streaming"});
        assert!(matches!(Reply::of(&aborted), Reply::Done(Err(e)) if e.contains("cancelled")));
        let cut = json!({"type": "result", "subtype": "success", "is_error": false,
            "result": "{\"alternatives\": [\"a", "stop_reason": "max_tokens"});
        assert!(matches!(Reply::of(&cut), Reply::Done(Err(e)) if e.contains("cut off")));
    }
}
