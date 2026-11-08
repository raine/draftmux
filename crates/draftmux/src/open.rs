//! `draftmux open`: hands a session off to a new draft.
//!
//! Creates the draft directory itself, converting a Claude Code transcript
//! into the draft's pi session, then asks the app (launching it if needed) to
//! focus the draft. The draft exists even if the app can't be reached.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::ValueEnum;
use claude_convert::ModelSelection;
use draftmux_storage::{Draft, NewDraft, Source, SourceKind, Store, Target};

use crate::{ipc, source};

/// Create a draft from a pi or Claude Code session, focus it in the app and
/// print its ID.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// pi session file or Claude Code transcript to hand off from.
    session_file: PathBuf,
    /// pi profile to pin the draft to (`PI_CODING_AGENT_DIR`). Defaults to the
    /// profile the session file is stored in, else ~/.pi/agent.
    #[arg(long, value_name = "DIR")]
    pi_agent_dir: Option<PathBuf>,
    /// Platform the message is for.
    #[arg(long, value_enum, default_value_t = TargetArg::Github)]
    target: TargetArg,
    /// Note for the draft from the handing-off agent, such as what to write.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TargetArg {
    Github,
    Slack,
}

/// Runs the command and returns the process exit code.
pub fn main(args: Args) -> i32 {
    let draft = match Store::default_root().and_then(|root| create(&args, &Store::new(root))) {
        Ok(draft) => draft,
        Err(err) => {
            eprintln!("draftmux: {err:#}");
            return 1;
        }
    };
    // Printed before focusing: the draft exists and `show` can read it even
    // if the app can't be reached.
    println!("{}", draft.id);
    match ipc::send(&ipc::Request::Focus { id: draft.id }) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!(
                "draftmux: created the draft but couldn't focus it in the app: {err:#}\n\
                 draftmux: the draft is in {}; don't run open again, that creates another draft",
                draft.dir.display()
            );
            1
        }
    }
}

fn create(args: &Args, store: &Store) -> Result<Draft> {
    // `show --source` matches this path exactly and normalizes its argument
    // the same way.
    let path = std::path::absolute(&args.session_file)
        .with_context(|| format!("resolving {}", args.session_file.display()))?;
    let info = source::inspect(&path)?;
    let home = std::env::home_dir().context("no home directory")?;
    let pi_agent_dir =
        source::resolve_profile(args.pi_agent_dir.as_deref(), &path, info.kind, &home)?;
    // Converted before the draft exists, so a transcript that can't be
    // converted creates nothing. pi sources are forked by the app instead.
    let session = match info.kind {
        SourceKind::Claude => Some(convert_claude(&path, &info.cwd, &pi_agent_dir)?),
        SourceKind::Pi => None,
    };
    let mut draft = store.create(NewDraft {
        target: match args.target {
            TargetArg::Github => Target::Github,
            TargetArg::Slack => Target::Slack,
        },
        title: String::new(),
        source: Source {
            kind: info.kind,
            path,
        },
        cwd: info.cwd,
        pi_agent_dir,
        note: args.note.clone(),
    })?;
    if let Some(session) = session {
        let file = session.write_into(&draft.session_dir())?;
        draft.meta = store.update_meta(&draft.id, |meta| meta.pi_session_file = Some(file))?;
    }
    Ok(draft)
}

/// The session continues on the profile's default model. A `settings.json`
/// that doesn't parse is reported and skipped, as pi does, leaving the model
/// to pi.
fn convert_claude(
    transcript: &Path,
    cwd: &Path,
    pi_agent_dir: &Path,
) -> Result<claude_convert::Session> {
    let model = ModelSelection::profile_default(pi_agent_dir).unwrap_or_else(|err| {
        eprintln!("draftmux: ignoring the profile's default model: {err:#}");
        ModelSelection::default()
    });
    let file = std::fs::File::open(transcript)
        .with_context(|| format!("opening {}", transcript.display()))?;
    claude_convert::convert(std::io::BufReader::new(file), cwd, &model)
        .with_context(|| format!("converting {}", transcript.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use clap::Parser;
    use serde_json::{Value, json};

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn parse(argv: &[&str]) -> Args {
        Cli::try_parse_from(std::iter::once("open").chain(argv.iter().copied()))
            .unwrap()
            .args
    }

    #[test]
    fn records_source_profile_target_and_note() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        let sessions = profile.join("sessions/--repo--");
        fs::create_dir_all(&sessions).unwrap();
        let session = sessions.join("s.jsonl");
        fs::write(
            &session,
            r#"{"type":"session","version":3,"id":"x","timestamp":"t","cwd":"/repo"}"#,
        )
        .unwrap();
        let store = Store::new(tmp.path().join("drafts"));

        let session_arg = session.to_str().unwrap();
        let args = parse(&[session_arg, "--target", "slack", "--note", "release notes"]);
        let draft = create(&args, &store).unwrap();

        let meta = store.load(&draft.id).unwrap().meta;
        assert_eq!(meta.source.kind, SourceKind::Pi);
        assert_eq!(meta.source.path, session);
        assert_eq!(meta.cwd, Path::new("/repo"));
        assert_eq!(meta.pi_agent_dir, profile);
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(meta.note.as_deref(), Some("release notes"));
        assert_eq!(meta.pi_session_file, None);
    }

    #[test]
    fn bad_source_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("s.jsonl");
        fs::write(&session, "{}\n").unwrap();
        let store = Store::new(tmp.path().join("drafts"));

        assert!(create(&parse(&[session.to_str().unwrap()]), &store).is_err());
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    /// A compacted Claude Code transcript in `cwd`. The codename is only in
    /// the compact summary, the port only in a tool result the compaction
    /// preserved, and the file name only in the last answer.
    fn claude_transcript(cwd: &Path) -> String {
        let line = |mut value: Value| {
            value["sessionId"] = "s".into();
            value["cwd"] = cwd.to_str().unwrap().into();
            value["timestamp"] = "2026-11-08T10:00:00.000Z".into();
            value.to_string() + "\n"
        };
        let assistant = |uuid: &str, parent: &str, id: &str, block: Value| {
            line(json!({
                "type": "assistant", "uuid": uuid, "parentUuid": parent,
                "message": { "id": id, "model": "claude-opus-5-5", "content": [block] },
            }))
        };
        let user = |uuid: &str, parent: Option<&str>, content: Value, summary: bool| {
            line(json!({
                "type": "user", "uuid": uuid, "parentUuid": parent,
                "isCompactSummary": summary,
                "message": { "role": "user", "content": content },
            }))
        };
        [
            user("u0", None, "The codename is OLDNAME.".into(), false),
            assistant(
                "a1",
                "u0",
                "m1",
                json!({ "type": "tool_use", "id": "t1", "name": "Bash",
                        "input": { "command": "grep port config.toml" } }),
            ),
            user(
                "r1",
                Some("a1"),
                json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "port = 48217" }]),
                false,
            ),
            line(json!({
                "type": "system", "subtype": "compact_boundary", "uuid": "b", "parentUuid": null,
                "compactMetadata": { "preservedMessages": { "uuids": ["a1", "r1"] } },
            })),
            user(
                "s",
                Some("b"),
                "Summary: the release codename is MARIGOLD-7 (it was renamed from OLDNAME).".into(),
                true,
            ),
            user("u1", Some("s"), "Which file sets the port?".into(), false),
            assistant(
                "a2",
                "u1",
                "m2",
                json!({ "type": "text", "text": "The port is set in config.toml." }),
            ),
        ]
        .concat()
    }

    fn claude_draft(tmp: &Path, profile: &Path) -> (Store, Draft) {
        let cwd = tmp.join("project");
        fs::create_dir_all(&cwd).unwrap();
        let transcript = tmp.join("transcript.jsonl");
        fs::write(&transcript, claude_transcript(&cwd)).unwrap();
        let store = Store::new(tmp.join("drafts"));
        let args = parse(&[
            transcript.to_str().unwrap(),
            "--pi-agent-dir",
            profile.to_str().unwrap(),
        ]);
        let draft = create(&args, &store).unwrap();
        (store, draft)
    }

    fn session_entries(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn claude_source_gets_a_converted_session_on_the_profile_default() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("settings.json"),
            r#"{"defaultProvider":"openai","defaultModel":"gpt-6","defaultThinkingLevel":"low"}"#,
        )
        .unwrap();
        let (store, draft) = claude_draft(tmp.path(), &profile);

        let meta = store.load(&draft.id).unwrap().meta;
        assert_eq!(meta.source.kind, SourceKind::Claude);
        let file = meta.pi_session_file.unwrap();
        assert_eq!(file.parent().unwrap(), draft.session_dir());

        let entries = session_entries(&file);
        assert_eq!(entries[0]["type"], "session");
        assert_eq!(
            entries[0]["cwd"],
            tmp.path().join("project").to_str().unwrap()
        );
        let tail = &entries[entries.len() - 2..];
        assert_eq!(tail[0]["provider"], "openai");
        assert_eq!(tail[0]["modelId"], "gpt-6");
        assert_eq!(tail[1]["thinkingLevel"], "low");
    }

    #[test]
    fn unreadable_profile_settings_leave_the_model_to_pi() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("settings.json"), "{\"defaultModel\":").unwrap();
        let (_, draft) = claude_draft(tmp.path(), &profile);

        let entries = session_entries(&draft.meta.pi_session_file.unwrap());
        assert_eq!(entries.last().unwrap()["type"], "message");
    }

    #[test]
    fn unconvertible_claude_transcript_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("c.jsonl");
        // Recognized as Claude Code, but compaction hasn't finished writing.
        fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"1","sessionId":"s","cwd":"/r","message":{"content":"hi"}}"#,
                "\n",
                r#"{"type":"system","subtype":"compact_boundary","uuid":"2","sessionId":"s"}"#,
            ),
        )
        .unwrap();
        let store = Store::new(tmp.path().join("drafts"));

        let args = parse(&[
            transcript.to_str().unwrap(),
            "--pi-agent-dir",
            tmp.path().to_str().unwrap(),
        ]);
        assert!(create(&args, &store).is_err());
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    /// Real pi loads a converted session and answers from it. Opt-in: it
    /// sends the fixture to the profile's default model. Run with
    /// `cargo test -p draftmux -- --ignored real_pi`; `DRAFTMUX_SMOKE_PI_AGENT_DIR`
    /// picks the profile (default `~/.pi/agent`), which needs a default model.
    #[test]
    #[ignore = "runs real pi against the profile's default model"]
    fn real_pi_answers_from_a_converted_session() {
        let home = std::env::home_dir().unwrap();
        let profile = std::env::var_os("DRAFTMUX_SMOKE_PI_AGENT_DIR")
            .map_or_else(|| home.join(".pi/agent"), PathBuf::from);
        let tmp = tempfile::tempdir().unwrap();
        let (_, draft) = claude_draft(tmp.path(), &profile);
        let file = draft.meta.pi_session_file.unwrap();
        let tail = session_entries(&file)
            .into_iter()
            .find(|e| e["type"] == "model_change")
            .expect("the profile has no default model");

        let env = shell_env::ShellEnv::capture();
        let mut child = env
            .command(env.pi().unwrap())
            .args(["-p", "--session"])
            .arg(&file)
            .args([
                "--no-extensions",
                "--no-skills",
                "--no-context-files",
                "--no-tools",
            ])
            .arg(
                "Answer from this conversation only, in one line: what is the release \
                 codename, which port number did the grep find, and which file sets it?",
            )
            .env("PI_CODING_AGENT_DIR", &profile)
            .current_dir(&draft.meta.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > Duration::from_secs(180) {
                child.kill().unwrap();
                panic!("pi didn't answer within 180 s");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let output = child.wait_with_output().unwrap();
        let answer = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "pi failed: {stderr}");
        for fact in ["MARIGOLD-7", "48217", "config.toml"] {
            assert!(answer.contains(fact), "{fact} missing from {answer:?}");
        }

        // Print mode doesn't report a model it couldn't restore, so check the
        // model that answered: pi appends its reply to the session file.
        let reply = session_entries(&file).pop().unwrap();
        assert_eq!(reply["message"]["role"], "assistant", "{reply}");
        assert_eq!(reply["message"]["provider"], tail["provider"]);
        assert_eq!(reply["message"]["model"], tail["modelId"]);
    }
}
