//! `draftmux open`: hands a session off to a new draft.
//!
//! Creates the draft directory itself with the draft's pi session, forked
//! from a pi source or converted from a Claude Code transcript, then asks the
//! app (launching it if needed) to focus the draft. The draft exists even if
//! the app can't be reached.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use claude_convert::ModelSelection;
use draftmux_pi_rpc::{ForkSpec, ModelOverride};
use draftmux_settings::Settings;
use draftmux_storage::{Draft, NewDraft, Source, SourceKind, Store, Target};

use crate::{ipc, resources, source};

/// How long pi gets to start and fork the source session.
const FORK_TIMEOUT: Duration = Duration::from_secs(30);

/// Create a draft from a pi or Claude Code session, focus it in the app and
/// print its ID.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// pi session file or Claude Code transcript to hand off from.
    session_file: PathBuf,
    /// pi profile to pin the draft to (`PI_CODING_AGENT_DIR`). Defaults to the
    /// profile the session file is stored in, else the profile Draftmux's
    /// settings give the session's directory, else ~/.pi/agent.
    #[arg(long, value_name = "DIR")]
    pi_agent_dir: Option<PathBuf>,
    /// Platform the message is for. Defaults to the one Draftmux's settings
    /// give the session's directory, else github.
    #[arg(long, value_enum)]
    target: Option<TargetArg>,
    /// Note for the draft from the handing-off agent, such as what to write.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TargetArg {
    Github,
    Slack,
    Email,
}

/// Runs the command and returns the process exit code.
pub fn main(args: Args) -> i32 {
    // pi runs with the environment of the agent shell that called `open`,
    // which has the PATH and API keys the source session used.
    let pi = || Command::new("pi");
    // A settings problem never blocks a handoff.
    let settings = draftmux_storage::data_dir()
        .and_then(|dir| Settings::load(&dir))
        .unwrap_or_else(|err| {
            eprintln!("draftmux: ignoring Draftmux's settings: {err:#}");
            Settings::default()
        });
    let created =
        Store::default_root().and_then(|root| create(&args, &Store::new(root), &pi, &settings));
    let draft = match created {
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

/// `pi` builds the base pi command. `settings` are resolved for the source
/// session's directory and kept in the draft. Unless they set a drafting
/// model, a pi source continues on its own model and a Claude Code source on
/// the profile's default.
fn create(
    args: &Args,
    store: &Store,
    pi: &dyn Fn() -> Command,
    settings: &Settings,
) -> Result<Draft> {
    // `show --source` matches this path exactly and normalizes its argument
    // the same way.
    let path = std::path::absolute(&args.session_file)
        .with_context(|| format!("resolving {}", args.session_file.display()))?;
    let info = source::inspect(&path)?;
    let home = std::env::home_dir().context("no home directory")?;
    let resolved = settings.resolve(&info.cwd, &home);
    let pi_agent_dir = source::resolve_profile(
        args.pi_agent_dir.as_deref(),
        &path,
        info.kind,
        resolved.profile.value.as_deref(),
        &home,
    )?;
    let target = match args.target {
        Some(TargetArg::Github) => Target::Github,
        Some(TargetArg::Slack) => Target::Slack,
        Some(TargetArg::Email) => Target::Email,
        None => resolved.target.value,
    };
    let model = resolved.model_override();
    let new = |path, cwd, pi_agent_dir| NewDraft {
        target,
        title: String::new(),
        source: Some(Source {
            kind: info.kind,
            path,
        }),
        cwd,
        pi_agent_dir,
        note: args.note.clone(),
        settings: resolved.draft_settings(),
    };
    match info.kind {
        SourceKind::Pi => {
            if !info.cwd.is_dir() {
                bail!(
                    "{} was started in {}, which no longer exists",
                    path.display(),
                    info.cwd.display()
                );
            }
            let extension = resources::pi_extension()?;
            store.create_with_session(new(path.clone(), info.cwd, pi_agent_dir), |draft| {
                let meta = &draft.meta;
                let spec = ForkSpec {
                    source: path.clone(),
                    session_dir: draft.session_dir(),
                    extension,
                    cwd: meta.cwd.clone(),
                    agent_dir: meta.pi_agent_dir.clone(),
                    draft_path: draft.draft_path(),
                    target: meta.target,
                    model,
                    timeout: FORK_TIMEOUT,
                };
                let forked = draftmux_pi_rpc::fork(pi(), &spec)
                    .map_err(|err| anyhow!("forking {}: {err}", path.display()))?;
                if let Some(warning) = forked.warning {
                    eprintln!("draftmux: {warning}");
                }
                Ok(forked.session_file)
            })
        }
        // Converted before the draft exists, so a transcript that can't be
        // converted creates nothing.
        SourceKind::Claude => {
            let session = convert_claude(&path, &info.cwd, &pi_agent_dir, model)?;
            store.create_with_session(new(path, info.cwd, pi_agent_dir), |draft| {
                session.write_into(&draft.session_dir())
            })
        }
    }
}

/// The session continues on `model`, else the profile's default model. A
/// `settings.json` that doesn't parse is reported and skipped, as pi does,
/// leaving the model to pi.
fn convert_claude(
    transcript: &Path,
    cwd: &Path,
    pi_agent_dir: &Path,
    model: Option<ModelOverride>,
) -> Result<claude_convert::Session> {
    let model = match model {
        Some(model) => model_selection(model),
        None => ModelSelection::profile_default(pi_agent_dir).unwrap_or_else(|err| {
            eprintln!("draftmux: ignoring the profile's default model: {err:#}");
            ModelSelection::default()
        }),
    };
    let file = std::fs::File::open(transcript)
        .with_context(|| format!("opening {}", transcript.display()))?;
    claude_convert::convert(std::io::BufReader::new(file), cwd, &model)
        .with_context(|| format!("converting {}", transcript.display()))
}

/// `model` as the model a new pi session continues on.
pub fn model_selection(model: ModelOverride) -> ModelSelection {
    ModelSelection {
        model: Some(claude_convert::Model {
            provider: model.provider,
            id: model.model_id,
        }),
        thinking_level: Some(model.thinking.as_str().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Instant;

    use draftmux_pi_rpc::{Agent, LaunchSpec, State, ThinkingLevel};
    use draftmux_settings::{DraftingModel, Rule, SelectionModel};
    use draftmux_storage::{DraftSettings, ModelChoice};
    use futures::executor::block_on;

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

    /// A stand-in for `pi --mode rpc --fork`: records its arguments, creates
    /// a session file in `--session-dir` and answers `get_state` with it.
    const FAKE_PI: &str = r#"
printf '%s\n' "$@" > "$FAKE_PI_ARGS"
while [ $# -gt 0 ]; do [ "$1" = --session-dir ] && dir=$2; shift; done
file="$dir/fork.jsonl"
: > "$file"
read -r line
id=$(printf '%s' "$line" | sed 's/.*"id":"\([^"]*\)".*/\1/')
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"thinkingLevel":"off","isStreaming":false,"isCompacting":false,"sessionFile":"%s","sessionId":"s","messageCount":0,"pendingMessageCount":0}}\n' "$id" "$file"
cat > /dev/null
"#;

    fn fake_pi(args_file: &Path) -> impl Fn() -> Command + use<> {
        let args_file = args_file.to_owned();
        move || {
            let mut command = Command::new("sh");
            command
                .args(["-c", FAKE_PI, "fake-pi"])
                .env("FAKE_PI_ARGS", &args_file);
            command
        }
    }

    fn no_pi() -> Command {
        panic!("pi isn't needed")
    }

    /// Settings whose All directories scope sets the drafting model.
    fn drafting_on(model: ModelOverride) -> Settings {
        let mut settings = Settings::default();
        settings.defaults.drafting_model = Some(DraftingModel::Model(model));
        settings
    }

    /// A v3 pi session in `<profile>/sessions/`, started in `cwd`.
    fn pi_session(profile: &Path, cwd: &Path) -> PathBuf {
        let sessions = profile.join("sessions/--repo--");
        fs::create_dir_all(&sessions).unwrap();
        let session = sessions.join("s.jsonl");
        let header = json!({"type":"session","version":3,"id":"x","timestamp":"t","cwd":cwd});
        fs::write(&session, header.to_string() + "\n").unwrap();
        session
    }

    #[test]
    fn pi_source_is_forked_into_the_draft_session() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).unwrap();
        let session = pi_session(&profile, &cwd);
        let store = Store::new(tmp.path().join("drafts"));
        let args_file = tmp.path().join("args");

        let session_arg = session.to_str().unwrap();
        let args = parse(&[session_arg, "--target", "slack", "--note", "release notes"]);
        let draft = create(&args, &store, &fake_pi(&args_file), &Settings::default()).unwrap();

        let meta = store.load(&draft.id).unwrap().meta;
        assert_eq!(meta.source.as_ref().unwrap().kind, SourceKind::Pi);
        assert_eq!(meta.source.as_ref().unwrap().path, session);
        assert_eq!(meta.cwd, cwd);
        assert_eq!(meta.pi_agent_dir, profile);
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(meta.note.as_deref(), Some("release notes"));
        assert_eq!(
            meta.pi_session_file,
            Some(draft.session_dir().join("fork.jsonl"))
        );

        let recorded = fs::read_to_string(&args_file).unwrap();
        let recorded: Vec<&str> = recorded.lines().collect();
        let session_dir = draft.session_dir();
        let extension = resources::pi_extension().unwrap();
        assert_eq!(
            recorded,
            [
                "--mode",
                "rpc",
                "--fork",
                session_arg,
                "--session-dir",
                session_dir.to_str().unwrap(),
                "-e",
                extension.to_str().unwrap(),
            ]
        );
    }

    #[test]
    fn failed_fork_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).unwrap();
        let session = pi_session(&tmp.path().join("agent"), &cwd);
        let store = Store::new(tmp.path().join("drafts"));
        let pi = || {
            let mut command = Command::new("sh");
            command.args(["-c", "echo 'Error: no such model' >&2; exit 1"]);
            command
        };

        let err = create(
            &parse(&[session.to_str().unwrap()]),
            &store,
            &pi,
            &Settings::default(),
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("forking"), "{err}");
        assert!(err.contains("Error: no such model"), "{err}");
        assert!(store.scan().unwrap().drafts.is_empty());
        assert_eq!(fs::read_dir(tmp.path().join("drafts")).unwrap().count(), 0);
    }

    #[test]
    fn pi_source_whose_cwd_is_gone_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let session = pi_session(&tmp.path().join("agent"), &tmp.path().join("deleted"));
        let store = Store::new(tmp.path().join("drafts"));

        let err = create(
            &parse(&[session.to_str().unwrap()]),
            &store,
            &no_pi,
            &Settings::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no longer exists"), "{err}");
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    #[test]
    fn pre_v3_pi_source_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("s.jsonl");
        let header = json!({"type":"session","version":2,"cwd":tmp.path()});
        fs::write(&session, header.to_string()).unwrap();
        let store = Store::new(tmp.path().join("drafts"));

        let err = create(
            &parse(&[session.to_str().unwrap()]),
            &store,
            &no_pi,
            &Settings::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("needs version 3"), "{err}");
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    #[test]
    fn bad_source_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("s.jsonl");
        fs::write(&session, "{}\n").unwrap();
        let store = Store::new(tmp.path().join("drafts"));

        assert!(
            create(
                &parse(&[session.to_str().unwrap()]),
                &store,
                &no_pi,
                &Settings::default()
            )
            .is_err()
        );
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
        let draft = create(&args, &store, &no_pi, &Settings::default()).unwrap();
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
        assert_eq!(meta.source.as_ref().unwrap().kind, SourceKind::Claude);
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
        assert!(create(&args, &store, &no_pi, &Settings::default()).is_err());
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    /// Settings with a rule on `dir` that sets the profile, target,
    /// instructions, context files and selection model.
    fn rule_on(dir: &Path, profile: &Path) -> Settings {
        let mut settings = Settings::default();
        settings.defaults.instructions = Some("No emoji.".into());
        let mut rule = Rule::new(dir.to_owned());
        rule.scope.profile = Some(profile.to_owned());
        rule.scope.target = Some(Target::Slack);
        rule.scope.instructions = Some("Casual.".into());
        rule.scope.context_files = Some(false);
        rule.scope.selection_model = Some(SelectionModel::Model(ModelOverride {
            provider: "anthropic".into(),
            model_id: "claude-haiku-5-5".into(),
            thinking: ThinkingLevel::Low,
        }));
        settings.directories.push(rule);
        settings
    }

    #[test]
    fn directory_settings_fill_in_what_the_flags_leave_out() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("work-agent");
        fs::create_dir_all(&profile).unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let transcript = tmp.path().join("transcript.jsonl");
        fs::write(&transcript, claude_transcript(&cwd)).unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let mut settings = rule_on(tmp.path(), &profile);

        let draft = create(
            &parse(&[transcript.to_str().unwrap()]),
            &store,
            &no_pi,
            &settings,
        )
        .unwrap();
        let meta = store.load(&draft.id).unwrap().meta;
        assert_eq!(meta.pi_agent_dir, profile);
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(
            meta.settings,
            DraftSettings {
                instructions: Some("No emoji.\n\nCasual.".into()),
                context_files: false,
                selection_model: Some(ModelChoice {
                    provider: "anthropic".into(),
                    model_id: "claude-haiku-5-5".into(),
                    thinking: "low".into(),
                }),
            }
        );

        // Flags win over settings.
        let flagged = parse(&[
            transcript.to_str().unwrap(),
            "--pi-agent-dir",
            tmp.path().to_str().unwrap(),
            "--target",
            "github",
        ]);
        let other = create(&flagged, &store, &no_pi, &settings).unwrap().meta;
        assert_eq!(other.pi_agent_dir, tmp.path());
        assert_eq!(other.target, Target::Github);

        // Later changes leave existing drafts alone.
        settings.directories.clear();
        let _ = create(
            &parse(&[transcript.to_str().unwrap()]),
            &store,
            &no_pi,
            &settings,
        );
        assert_eq!(store.load(&draft.id).unwrap().meta, meta);
    }

    #[test]
    fn rejected_model_override_still_creates_the_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).unwrap();
        let session = pi_session(&profile, &cwd);
        let store = Store::new(tmp.path().join("drafts"));
        // Answers get_state, then refuses set_model.
        let pi = || {
            let mut command = Command::new("sh");
            command.args(["-c", REFUSING_PI, "fake-pi"]);
            command
        };
        let settings = drafting_on(ModelOverride {
            provider: "portkey".into(),
            model_id: "missing".into(),
            thinking: ThinkingLevel::High,
        });
        let draft = create(&parse(&[session.to_str().unwrap()]), &store, &pi, &settings).unwrap();
        assert_eq!(
            store.load(&draft.id).unwrap().meta.pi_session_file,
            Some(draft.session_dir().join("fork.jsonl"))
        );
    }

    const REFUSING_PI: &str = r#"
while [ $# -gt 0 ]; do [ "$1" = --session-dir ] && dir=$2; shift; done
file="$dir/fork.jsonl"
: > "$file"
read -r line
id=$(printf '%s' "$line" | sed 's/.*"id":"\([^"]*\)".*/\1/')
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"thinkingLevel":"off","isStreaming":false,"isCompacting":false,"sessionFile":"%s","sessionId":"s","messageCount":0,"pendingMessageCount":0}}\n' "$id" "$file"
read -r line
id=$(printf '%s' "$line" | sed 's/.*"id":"\([^"]*\)".*/\1/')
printf '{"type":"response","id":"%s","command":"set_model","success":false,"error":"Model not found"}\n' "$id"
cat > /dev/null
"#;

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

    #[test]
    fn model_override_replaces_the_claude_profile_default() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("settings.json"),
            r#"{"defaultProvider":"openai","defaultModel":"gpt-6"}"#,
        )
        .unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let transcript = tmp.path().join("transcript.jsonl");
        fs::write(&transcript, claude_transcript(&cwd)).unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let args = parse(&[
            transcript.to_str().unwrap(),
            "--pi-agent-dir",
            profile.to_str().unwrap(),
        ]);
        let model = ModelOverride {
            provider: "anthropic".into(),
            model_id: "claude-opus-5-5".into(),
            thinking: ThinkingLevel::High,
        };
        let draft = create(&args, &store, &no_pi, &drafting_on(model)).unwrap();

        let entries = session_entries(&draft.meta.pi_session_file.unwrap());
        let tail = &entries[entries.len() - 2..];
        assert_eq!(tail[0]["provider"], "anthropic");
        assert_eq!(tail[0]["modelId"], "claude-opus-5-5");
        assert_eq!(tail[1]["thinkingLevel"], "high");
    }

    /// What a real pi started on `session_file` with `--session`, as the app
    /// starts drafts, resolves for it.
    fn real_pi_state(
        env: &Arc<shell_env::ShellEnv>,
        profile: &Path,
        session_file: &Path,
        cwd: &Path,
    ) -> (State, Vec<draftmux_pi_rpc::Model>) {
        let pi = env.pi().unwrap().to_owned();
        let env = env.clone();
        let spec = LaunchSpec {
            command: Arc::new(move || env.command(&pi)),
            session_file: session_file.to_owned(),
            extension: resources::pi_extension().unwrap(),
            cwd: cwd.to_owned(),
            agent_dir: profile.to_owned(),
            draft_path: cwd.join("draft.md"),
            target: Target::Github,
            instructions: None,
            context_files: true,
        };
        let (agent, _events) = Agent::new(spec, Duration::from_secs(60));
        let state = block_on(agent.get_state()).expect("get_state");
        let models = block_on(agent.get_available_models()).expect("get_available_models");
        (state, models)
    }

    /// Real pi forks a pi session at handoff: the draft continues on the
    /// source's model, an override sticks across restarts, and the source and
    /// the profile's settings are left alone. Opt-in; calls no model. Run with
    /// `cargo test -p draftmux -- --ignored real_pi`; `DRAFTMUX_SMOKE_PI_AGENT_DIR`
    /// picks the profile (default `~/.pi/agent`), which needs two models with
    /// credentials.
    #[test]
    #[ignore = "runs real pi against the user's profile"]
    fn real_pi_forks_a_pi_session() {
        let home = std::env::home_dir().unwrap();
        let profile = std::env::var_os("DRAFTMUX_SMOKE_PI_AGENT_DIR")
            .map_or_else(|| home.join(".pi/agent"), PathBuf::from);
        let settings = fs::read(profile.join("settings.json")).ok();
        let env = Arc::new(shell_env::ShellEnv::capture());
        let tmp = tempfile::tempdir().unwrap();
        // Resolved like pi's own cwd, which it records in session headers.
        let cwd = tmp.path().canonicalize().unwrap().join("repo");
        fs::create_dir_all(&cwd).unwrap();

        // A model other than the one pi picks for a new session, so restoring
        // the source's model can't pass by falling back to the default.
        let (fresh, models) =
            real_pi_state(&env, &profile, &tmp.path().join("probe/new.jsonl"), &cwd);
        let default = fresh.model.expect("the profile has no default model");
        let source_model = models
            .iter()
            .find(|m| m.provider == default.provider && m.id != default.id)
            .or_else(|| models.iter().find(|m| m.id != default.id))
            .expect("the profile needs a second model")
            .clone();

        let entry = |value: Value| value.to_string();
        let source_lines = [
            entry(
                json!({"type":"session","version":3,"id":"src","timestamp":"2026-11-08T10:00:00.000Z","cwd":cwd}),
            ),
            entry(
                json!({"type":"model_change","id":"e1","parentId":null,"timestamp":"2026-11-08T10:00:00.000Z",
                         "provider":source_model.provider,"modelId":source_model.id}),
            ),
            entry(
                json!({"type":"message","id":"e2","parentId":"e1","timestamp":"2026-11-08T10:00:01.000Z",
                         "message":{"role":"user","content":[{"type":"text","text":"The codename is MARIGOLD-7."}],"timestamp":1}}),
            ),
            entry(
                json!({"type":"message","id":"e3","parentId":"e2","timestamp":"2026-11-08T10:00:02.000Z",
                         "message":{"role":"assistant","content":[{"type":"text","text":"Noted."}],
                                    "api":source_model.api,"provider":source_model.provider,"model":source_model.id,
                                    "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
                                             "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
                                    "stopReason":"stop","timestamp":2}}),
            ),
        ];
        // Without a final newline, which pi adds to the source.
        let original = source_lines.join("\n");
        let source = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile_in(tmp.path())
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        fs::write(&source, &original).unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let pi_path = env.pi().unwrap().to_owned();
        let pi = || env.command(&pi_path);
        let args = parse(&[
            source.to_str().unwrap(),
            "--pi-agent-dir",
            profile.to_str().unwrap(),
        ]);

        let started = Instant::now();
        let draft = create(&args, &store, &pi, &Settings::default()).unwrap();
        eprintln!("fork took {} ms", started.elapsed().as_millis());
        assert_eq!(fs::read_to_string(&source).unwrap(), original + "\n");
        let file = draft.meta.pi_session_file.clone().unwrap();
        assert_eq!(file.parent(), Some(draft.session_dir().as_path()));
        let entries = session_entries(&file);
        assert_eq!(entries[0]["version"], 3);
        assert_eq!(entries[0]["cwd"], cwd.to_str().unwrap());
        assert_eq!(entries[0]["parentSession"], source.to_str().unwrap());
        assert_ne!(entries[0]["id"], "src");
        let copied: Vec<Value> = source_lines[1..]
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries[1..=copied.len()], copied[..]);
        let (state, _) = real_pi_state(&env, &profile, &file, &cwd);
        let model = state.model.unwrap();
        assert_eq!(
            (model.provider, model.id),
            (source_model.provider.clone(), source_model.id.clone())
        );

        // The override: back to the default model, on a thinking level the
        // model supports.
        let thinking = if default.reasoning {
            ThinkingLevel::Low
        } else {
            ThinkingLevel::Off
        };
        let model = ModelOverride {
            provider: default.provider.clone(),
            model_id: default.id.clone(),
            thinking,
        };
        let draft = create(&args, &store, &pi, &drafting_on(model)).unwrap();
        let file = draft.meta.pi_session_file.unwrap();
        let (state, _) = real_pi_state(&env, &profile, &file, &cwd);
        let model = state.model.unwrap();
        assert_eq!((model.provider, model.id), (default.provider, default.id));
        assert_eq!(state.thinking_level, thinking);

        assert_eq!(
            fs::read_to_string(&source).unwrap(),
            source_lines.join("\n") + "\n"
        );
        assert_eq!(
            fs::read(profile.join("settings.json")).ok(),
            settings,
            "pi's settings changed"
        );
    }
}
