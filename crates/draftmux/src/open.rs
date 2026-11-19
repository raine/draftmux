//! `draftmux open`: hands a session off to a new draft.
//!
//! Creates the draft directory itself with the draft's session, then asks
//! the app (launching it if needed) to focus the draft. The draft exists even
//! if the app can't be reached.
//!
//! A draft runs on the backend `--backend` names, else the one Draftmux's
//! settings give the source's directory, pi unless set. On pi, its pi
//! session is forked from a pi source or converted from a Claude Code
//! transcript. On Claude Code, a Claude Code transcript is continued as a
//! native Claude Code session, forked from a snapshot of it taken into the
//! draft's session directory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use claude_convert::ModelSelection;
use draftmux_claude_rpc::Snapshot;
use draftmux_pi_rpc::{ForkSpec, ModelOverride};
use draftmux_settings::{DraftBackend, Settings};
use draftmux_storage::{
    Backend, ClaudeFork, ClaudeSession, Draft, LinkedFile, NewDraft, PiSession, Source, SourceKind,
    Store, Target,
};

use crate::{ipc, resources, source};

/// How long pi gets to start and fork the source session.
const FORK_TIMEOUT: Duration = Duration::from_secs(30);

/// Create a draft from a pi or Claude Code session, focus it in the app and
/// print its ID.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// pi session file or Claude Code transcript to hand off from.
    session_file: PathBuf,
    /// Agent the draft runs on. `claude` continues a Claude Code transcript
    /// as a native Claude Code session; a pi session can only continue on
    /// pi. Defaults to the backend Draftmux's settings give the session's
    /// directory, else pi.
    #[arg(long, value_enum)]
    backend: Option<BackendArg>,
    /// pi profile to pin the draft to (`PI_CODING_AGENT_DIR`). Defaults to the
    /// profile the session file is stored in, else the profile Draftmux's
    /// settings give the session's directory, else ~/.pi/agent. Ignored for
    /// a draft on Claude Code.
    #[arg(long, value_name = "DIR")]
    pi_agent_dir: Option<PathBuf>,
    /// Platform the message is for. Defaults to the one Draftmux's settings
    /// give the session's directory, else github.
    #[arg(long, value_parser = target_parser())]
    target: Option<Target>,
    /// Note for the draft from the handing-off agent, such as what to write.
    #[arg(long, conflicts_with = "draft_file")]
    note: Option<String>,
    /// Markdown file whose text the draft starts with, unchanged, instead of
    /// a first draft written by Draftmux's agent. Saving the draft writes it
    /// back to the file.
    #[arg(long, value_name = "PATH")]
    draft_file: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum BackendArg {
    Pi,
    Claude,
}

/// Parses `--target`, offering every target's name and accepting its
/// aliases.
fn target_parser() -> impl TypedValueParser<Value = Target> {
    PossibleValuesParser::new(
        Target::ALL
            .iter()
            .map(|target| PossibleValue::new(target.name()).aliases(target.aliases())),
    )
    .map(|name| Target::from_name(&name).expect("the parser accepts only target names"))
}

/// Runs the command and returns the process exit code.
pub fn main(args: Args) -> Result<u8> {
    // pi runs with the environment of the agent shell that called `open`,
    // which has the PATH and API keys the source session used.
    #[cfg(unix)]
    let program = PathBuf::from("pi");
    // Windows doesn't search PATH for npm's `pi.cmd` by the bare name.
    #[cfg(windows)]
    let program = shell_env::ShellEnv::capture()
        .pi()
        .map_or_else(|_| PathBuf::from("pi"), Path::to_path_buf);
    let pi = || Command::new(&program);
    let claude = claude_available;
    let settings = load_settings();
    let draft = create(&args, &Store::open_default()?, &pi, &claude, &settings)?;
    // Printed before focusing: the draft exists and `show` can read it even
    // if the app can't be reached.
    println!("{}", draft.id);
    match ipc::send(&ipc::Request::Focus {
        id: draft.id.clone(),
    }) {
        Ok(()) => Ok(0),
        Err(err) => {
            eprintln!(
                "draftmux: created the draft but couldn't focus it in the app: {err:#}\n\
                 draftmux: the draft is in {}; run `draftmux focus {}` to try again\n\
                 draftmux: don't run open again, that creates another draft",
                draft.dir.display(),
                draft.id
            );
            Ok(1)
        }
    }
}

/// Fails, with a message for the user, unless the app can run Claude Code.
/// The app runs it with the login shell's environment, so it is checked
/// with that.
pub(crate) fn claude_available() -> Result<(), String> {
    let env = shell_env::ShellEnv::capture();
    let program = env.claude().map_err(|err| err.to_string())?;
    crate::chat::claude_version(program, env.vars()).map(drop)
}

/// Draftmux's settings, or the defaults with a warning when they can't be
/// read: a settings problem never blocks creating a draft.
pub(crate) fn load_settings() -> Settings {
    draftmux_storage::prepare_config_dir()
        .and_then(|dir| Settings::load(&dir))
        .unwrap_or_else(|err| {
            eprintln!("draftmux: ignoring Draftmux's settings: {err:#}");
            Settings::default()
        })
}

/// Whether `file` is Markdown that its git repository doesn't ignore, such
/// as README.md, rather than a scratch file holding a message.
pub(crate) fn is_repository_markdown(file: &Path) -> bool {
    let markdown = file
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["md", "markdown", "mdx"]
                .iter()
                .any(|md| ext.eq_ignore_ascii_case(md))
        });
    let Ok(file) = std::path::absolute(file) else {
        return false;
    };
    let Some(dir) = file.parent().filter(|_| markdown) else {
        return false;
    };
    // `check-ignore` exits 1 for a file it doesn't ignore, 0 for an ignored
    // one and 128 outside a repository.
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q", "--"])
        .arg(&file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.code() == Some(1))
}

/// `pi` builds the base pi command; `claude` fails, with a message for the
/// user, unless the app can run Claude Code. `settings` are resolved for the
/// source session's directory and kept in the draft. Unless they set a
/// drafting model, a pi source continues on its own model, a Claude Code
/// source converted to pi on the profile's default and one on Claude Code on
/// its own model.
fn create(
    args: &Args,
    store: &Store,
    pi: &dyn Fn() -> Command,
    claude: &dyn Fn() -> Result<(), String>,
    settings: &Settings,
) -> Result<Draft> {
    // `show --source` matches this path exactly and normalizes its argument
    // the same way.
    let path = std::path::absolute(&args.session_file)
        .with_context(|| format!("resolving {}", args.session_file.display()))?;
    let info = source::inspect(&path)?;
    let home = std::env::home_dir().context("no home directory")?;
    let resolved = settings.resolve(&info.cwd, &home);
    let on_claude = match args.backend {
        Some(backend) => backend == BackendArg::Claude,
        None => resolved.backend.value == DraftBackend::SameAsSource,
    } && info.kind == SourceKind::Claude;
    if args.backend == Some(BackendArg::Claude) && info.kind == SourceKind::Pi {
        bail!(
            "{} is a pi session, and Claude Code can only continue its own sessions; \
             leave out --backend claude to continue it on pi",
            path.display()
        );
    }
    // Read before the draft exists, so a missing file creates nothing.
    let (text, file) = match &args.draft_file {
        Some(file) => {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?;
            let path = std::path::absolute(file)
                .with_context(|| format!("resolving {}", file.display()))?;
            let linked = LinkedFile::new(path, text.as_bytes());
            (text, Some(linked))
        }
        None => (String::new(), None),
    };
    let target = match (args.target, &args.draft_file) {
        (Some(target), _) => target,
        (None, Some(file)) if is_repository_markdown(file) => {
            eprintln!(
                "draftmux: previewing {} as a file in its repository",
                file.display()
            );
            Target::GithubFile
        }
        (None, _) => resolved.target.value,
    };
    if on_claude {
        let mut settings = resolved.draft_settings();
        // Pi's; the Claude session keeps its own.
        settings.selection_model = None;
        let session = ClaudeSession {
            session_id: uuid::Uuid::new_v4(),
            transcript: None,
            config_dir: claude_config_dir(&path, &home, std::env::var_os("CLAUDE_CONFIG_DIR")),
            fork: None,
            model: resolved
                .claude_drafting_model
                .value
                .model()
                .map(str::to_owned),
            effort: None,
            selection_model: resolved
                .claude_selection_model
                .value
                .model()
                .map(str::to_owned),
        };
        let new = NewDraft {
            target,
            title: String::new(),
            source: Some(Source {
                kind: info.kind,
                path: path.clone(),
            }),
            cwd: info.cwd,
            backend: Backend::Claude(session.clone()),
            note: args.note.clone(),
            settings,
            text,
            file,
        };
        return create_claude(store, new, session, &path, claude);
    }
    let pi_agent_dir = source::resolve_profile(
        args.pi_agent_dir.as_deref(),
        &path,
        info.kind,
        resolved.profile.value.as_deref(),
        &home,
    )?;
    let model = resolved.model_override();
    // A Claude Code transcript is converted to continue on pi.
    let new = |path, cwd, pi_agent_dir: &Path| NewDraft {
        target,
        title: String::new(),
        source: Some(Source {
            kind: info.kind,
            path,
        }),
        cwd,
        backend: Backend::pi(pi_agent_dir),
        note: args.note.clone(),
        settings: resolved.draft_settings(),
        text: text.clone(),
        file: file.clone(),
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
            store.create_with_session(new(path.clone(), info.cwd, &pi_agent_dir), |draft| {
                let spec = ForkSpec {
                    source: path.clone(),
                    session_dir: draft.session_dir(),
                    extension,
                    cwd: draft.meta.cwd.clone(),
                    agent_dir: pi_agent_dir.clone(),
                    draft_path: draft.draft_path(),
                    model,
                    timeout: FORK_TIMEOUT,
                };
                let forked = draftmux_pi_rpc::fork(pi(), &spec)
                    .map_err(|err| anyhow!("forking {}: {err}", path.display()))?;
                if let Some(warning) = forked.warning {
                    eprintln!("draftmux: {warning}");
                }
                Ok(Backend::Pi(PiSession {
                    agent_dir: pi_agent_dir,
                    session_file: Some(forked.session_file),
                }))
            })
        }
        // Converted before the draft exists, so a transcript that can't be
        // converted creates nothing.
        SourceKind::Claude => {
            let session = convert_claude(&path, &info.cwd, &pi_agent_dir, model)?;
            store.create_with_session(new(path, info.cwd, &pi_agent_dir), |draft| {
                let file = session.write_into(&draft.session_dir())?;
                Ok(Backend::Pi(PiSession {
                    agent_dir: pi_agent_dir,
                    session_file: Some(file),
                }))
            })
        }
    }
}

/// Creates the Claude draft `new`, whose `session` forks a snapshot of
/// `source`, the Claude Code transcript, once `claude` finds Claude Code
/// usable. A draft's backend never changes, so an unusable Claude Code
/// creates nothing rather than falling back to pi.
fn create_claude(
    store: &Store,
    new: NewDraft,
    session: ClaudeSession,
    source: &Path,
    claude: &dyn Fn() -> Result<(), String>,
) -> Result<Draft> {
    if !new.cwd.is_dir() {
        bail!(
            "{} was started in {}, which no longer exists",
            source.display(),
            new.cwd.display()
        );
    }
    claude().map_err(|err| {
        anyhow!(
            "can't continue {} on Claude Code: {err}\n\
             draftmux: add --backend pi to continue it on pi instead",
            source.display()
        )
    })?;
    store.create_with_session(new, |draft| {
        let snapshot = Snapshot::take(source, &draft.session_dir())
            .map_err(|err| anyhow!("copying {}: {err}", source.display()))?;
        // Inspected separately, so the source may have moved on in between.
        if snapshot.cwd != draft.meta.cwd {
            bail!(
                "{} changed its working directory while being copied; try again",
                source.display()
            );
        }
        let uuid = |value: &str| {
            uuid::Uuid::parse_str(value)
                .with_context(|| format!("{} has an invalid id {value:?}", source.display()))
        };
        Ok(Backend::Claude(ClaudeSession {
            fork: Some(ClaudeFork {
                source_session_id: uuid(&snapshot.session_id)?,
                boundary: uuid(&snapshot.boundary)?,
            }),
            ..session
        }))
    })
}

/// `CLAUDE_CONFIG_DIR` for a session forked from `source`: the directory
/// Claude Code wrote the source under (`<dir>/projects/<project>/`), else
/// the `caller`'s own, so the fork's transcript goes where the source's
/// did. None for `~/.claude`, which Claude Code uses when it is unset; a
/// login kept in the keychain is only found with it unset.
fn claude_config_dir(source: &Path, home: &Path, caller: Option<OsString>) -> Option<PathBuf> {
    let written_under = source
        .parent()
        .and_then(Path::parent)
        .filter(|projects| projects.file_name() == Some("projects".as_ref()))
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let dir =
        written_under.or_else(|| caller.map(PathBuf::from).filter(|dir| dir.is_absolute()))?;
    (dir != home.join(".claude")).then_some(dir)
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
    use draftmux_settings::{ClaudeDraftingModel, DraftingModel, Rule, SelectionModel};
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
    #[cfg(unix)]
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

    #[cfg(unix)]
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

    fn no_claude() -> Result<(), String> {
        panic!("Claude Code isn't needed")
    }

    fn claude_ok() -> Result<(), String> {
        Ok(())
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

    #[cfg(unix)]
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
        let draft = create(
            &args,
            &store,
            &fake_pi(&args_file),
            &no_claude,
            &Settings::default(),
        )
        .unwrap();

        let meta = store.load(&draft.id).unwrap().meta;
        assert_eq!(meta.source.as_ref().unwrap().kind, SourceKind::Pi);
        assert_eq!(meta.source.as_ref().unwrap().path, session);
        assert_eq!(meta.cwd, cwd);
        assert_eq!(meta.pi().unwrap().agent_dir, profile);
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(meta.note.as_deref(), Some("release notes"));
        assert_eq!(
            meta.pi().unwrap().session_file,
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

    #[cfg(unix)]
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
            &no_claude,
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
            &no_claude,
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
            &no_claude,
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
                &no_claude,
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
            "--backend",
            "pi",
        ]);
        let draft = create(&args, &store, &no_pi, &no_claude, &Settings::default()).unwrap();
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
        let file = meta.pi().unwrap().session_file.clone().unwrap();
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
    fn draft_file_starts_the_draft_with_its_text() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let transcript = tmp.path().join("transcript.jsonl");
        fs::write(&transcript, claude_transcript(&cwd)).unwrap();
        let file = tmp.path().join("CONTRIBUTING.md");
        let text = "# Contributing\n\nOpen an issue first.\n";
        fs::write(&file, text).unwrap();
        let store = Store::new(tmp.path().join("drafts"));

        let args = parse(&[
            transcript.to_str().unwrap(),
            "--pi-agent-dir",
            tmp.path().to_str().unwrap(),
            "--draft-file",
            file.to_str().unwrap(),
            "--backend",
            "pi",
        ]);
        let draft = create(&args, &store, &no_pi, &no_claude, &Settings::default()).unwrap();

        assert_eq!(store.read_text(&draft.id).unwrap(), text);
        let meta = store.load(&draft.id).unwrap().meta;
        assert!(!meta.first_draft_pending);
        assert!(meta.source.is_some());
        assert!(meta.pi().unwrap().session_file.is_some());
        let linked = meta.file.unwrap();
        assert_eq!(linked.path, file);
        assert!(linked.state(text).unwrap().saved);
    }

    #[test]
    fn draft_file_in_a_repository_previews_as_a_github_file() {
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("transcript.jsonl");
        fs::write(&transcript, claude_transcript(tmp.path())).unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("history")).unwrap();
        let git = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(git.success());
        fs::write(repo.join(".gitignore"), "history/\n").unwrap();
        let readme = repo.join("README.md");
        let scratch = repo.join("history/pr.md");
        let outside = tmp.path().join("pr.md");
        for file in [&readme, &scratch, &outside] {
            fs::write(file, "# Title\n").unwrap();
        }
        let target = |file: &Path, extra: &[&str]| {
            let mut argv = vec![
                transcript.to_str().unwrap(),
                "--pi-agent-dir",
                tmp.path().to_str().unwrap(),
                "--backend",
                "pi",
                "--draft-file",
                file.to_str().unwrap(),
            ];
            argv.extend(extra);
            let draft = create(
                &parse(&argv),
                &store,
                &no_pi,
                &no_claude,
                &Settings::default(),
            )
            .unwrap();
            draft.meta.target
        };

        assert_eq!(target(&readme, &[]), Target::GithubFile);
        assert_eq!(target(&readme, &["--target", "github"]), Target::Github);
        assert_eq!(target(&scratch, &[]), Target::Github);
        assert_eq!(target(&outside, &[]), Target::Github);
    }

    #[test]
    fn missing_draft_file_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let session = pi_session(&tmp.path().join("agent"), tmp.path());
        let store = Store::new(tmp.path().join("drafts"));

        let missing = tmp.path().join("missing.md");
        let args = parse(&[
            session.to_str().unwrap(),
            "--draft-file",
            missing.to_str().unwrap(),
        ]);
        let err = create(&args, &store, &no_pi, &no_claude, &Settings::default()).unwrap_err();
        assert!(format!("{err:#}").contains("missing.md"), "{err:#}");
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    #[test]
    fn draft_file_conflicts_with_note() {
        let argv = ["open", "s.jsonl", "--note", "x", "--draft-file", "d.md"];
        assert!(Cli::try_parse_from(argv).is_err());
    }

    #[test]
    fn unreadable_profile_settings_leave_the_model_to_pi() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("agent");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("settings.json"), "{\"defaultModel\":").unwrap();
        let (_, draft) = claude_draft(tmp.path(), &profile);

        let entries = session_entries(&draft.meta.pi().unwrap().session_file.clone().unwrap());
        assert_eq!(entries.last().unwrap()["type"], "message");
    }

    const SOURCE_ID: &str = "6c1d1b9e-2f4a-4b8e-9d3c-7a5e1f0b2c4d";
    const LEAF: &str = "0cf5a4d1-79c4-4901-a3ef-8d3611211105";

    /// A Claude Code session in `cwd`, in a config dir's `projects/` under
    /// `config`, ending with a complete answer and a line cut off mid-write.
    fn native_transcript(config: &Path, cwd: &Path) -> PathBuf {
        let dir = config.join("projects/-project");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{SOURCE_ID}.jsonl"));
        let line = |value: Value| {
            let mut value = value;
            value["sessionId"] = SOURCE_ID.into();
            value["cwd"] = cwd.to_str().unwrap().into();
            value.to_string() + "\n"
        };
        let text = [
            line(
                json!({"type": "user", "uuid": "a2a9f0c6-55d6-4a9b-8d57-3f3f0b7c1e01",
                "parentUuid": null, "message": {"role": "user", "content": "Write notes"}}),
            ),
            line(json!({"type": "assistant", "uuid": LEAF,
                "parentUuid": "a2a9f0c6-55d6-4a9b-8d57-3f3f0b7c1e01",
                "message": {"id": "m1", "role": "assistant",
                    "content": [{"type": "text", "text": "Sure."}]}})),
            r#"{"type":"user","uuid":"cut"#.into(),
        ]
        .concat();
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn claude_backend_forks_a_snapshot_of_a_claude_source() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let config = tmp.path().join("claude-work");
        let transcript = native_transcript(&config, &cwd);
        let source_bytes = fs::read(&transcript).unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let mut settings = Settings::default();
        settings.defaults.selection_model = Some(SelectionModel::Model(ModelOverride {
            provider: "anthropic".into(),
            model_id: "claude-haiku-5-5".into(),
            thinking: ThinkingLevel::Low,
        }));
        settings.defaults.claude_selection_model = Some(
            draftmux_settings::ClaudeSelectionModel::Model("haiku".into()),
        );

        let args = parse(&[transcript.to_str().unwrap(), "--backend", "claude"]);
        let draft = create(&args, &store, &no_pi, &claude_ok, &settings).unwrap();

        let meta = store.load(&draft.id).unwrap().meta;
        assert!(meta.pi().is_none());
        assert_eq!(meta.source.as_ref().unwrap().kind, SourceKind::Claude);
        assert_eq!(meta.cwd, cwd);
        assert!(meta.first_draft_pending);
        assert_eq!(meta.settings.selection_model, None);
        let Backend::Claude(session) = &meta.backend else {
            panic!("not Claude: {:?}", meta.backend);
        };
        assert_ne!(session.session_id.to_string(), SOURCE_ID);
        assert_eq!(session.transcript, None);
        assert_eq!(session.model, None);
        assert_eq!(session.selection_model.as_deref(), Some("haiku"));
        assert_eq!(session.config_dir.as_deref(), Some(config.as_path()));
        assert_eq!(
            session.fork,
            Some(ClaudeFork {
                source_session_id: SOURCE_ID.parse().unwrap(),
                boundary: LEAF.parse().unwrap(),
            })
        );
        // The snapshot is in the draft's session directory, without the
        // cut-off line, and the source is untouched.
        let snapshot = fs::read(draft.session_dir().join("source.jsonl")).unwrap();
        let complete = source_bytes.iter().rposition(|&b| b == b'\n').unwrap() + 1;
        assert_eq!(snapshot, source_bytes[..complete]);
        assert_eq!(fs::read(&transcript).unwrap(), source_bytes);
    }

    #[test]
    fn claude_backend_refuses_a_pi_source_and_ignores_a_pi_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).unwrap();
        let session = pi_session(&tmp.path().join("agent"), &cwd);
        let store = Store::new(tmp.path().join("drafts"));

        let args = parse(&[session.to_str().unwrap(), "--backend", "claude"]);
        let err = create(&args, &store, &no_pi, &no_claude, &Settings::default()).unwrap_err();
        assert!(err.to_string().contains("is a pi session"), "{err}");

        let transcript = native_transcript(&tmp.path().join("config"), &cwd);
        let profile = tmp.path().join("agent");
        let profile = profile.to_str().unwrap();
        let args = parse(&[
            transcript.to_str().unwrap(),
            "--backend",
            "claude",
            "--pi-agent-dir",
            profile,
        ]);
        let draft = create(&args, &store, &no_pi, &claude_ok, &Settings::default()).unwrap();
        let meta = store.load(&draft.id).unwrap().meta;
        assert!(meta.pi().is_none());
        assert!(matches!(meta.backend, Backend::Claude(_)));

        // Choosing pi explicitly keeps the profile override.
        let args = parse(&[
            session.to_str().unwrap(),
            "--backend",
            "pi",
            "--pi-agent-dir",
            profile,
        ]);
        assert_eq!(args.backend, Some(BackendArg::Pi));
        assert_eq!(parse(&[session.to_str().unwrap()]).backend, None);
    }

    /// Settings with a rule on `dir` that continues Claude Code sources on
    /// Claude Code with `sonnet`, and sets pi's drafting model.
    fn claude_rule_on(dir: &Path) -> Settings {
        let mut settings = drafting_on(ModelOverride {
            provider: "anthropic".into(),
            model_id: "claude-opus-5-5".into(),
            thinking: ThinkingLevel::High,
        });
        let mut rule = Rule::new(dir.to_owned());
        rule.scope.backend = Some(DraftBackend::SameAsSource);
        rule.scope.claude_drafting_model = Some(ClaudeDraftingModel::Model("sonnet".into()));
        settings.directories.push(rule);
        settings
    }

    #[cfg(unix)]
    #[test]
    fn backend_setting_picks_the_backend_unless_the_flag_does() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let transcript = native_transcript(&tmp.path().join("config"), &cwd);
        let store = Store::new(tmp.path().join("drafts"));
        let settings = claude_rule_on(tmp.path());

        // The setting continues a Claude Code source on Claude Code, on the
        // Claude drafting model; pi's drafting model doesn't apply.
        let args = parse(&[transcript.to_str().unwrap()]);
        let draft = create(&args, &store, &no_pi, &claude_ok, &settings).unwrap();
        let Backend::Claude(session) = &store.load(&draft.id).unwrap().meta.backend else {
            panic!("not Claude: {:?}", draft.meta.backend);
        };
        assert_eq!(session.model.as_deref(), Some("sonnet"));
        assert!(session.fork.is_some());

        // `--backend pi` converts it to pi instead, on pi's drafting model.
        let args = parse(&[
            transcript.to_str().unwrap(),
            "--backend",
            "pi",
            "--pi-agent-dir",
            tmp.path().to_str().unwrap(),
        ]);
        let draft = create(&args, &store, &no_pi, &no_claude, &settings).unwrap();
        let file = draft.meta.pi().unwrap().session_file.clone().unwrap();
        let entries = session_entries(&file);
        assert_eq!(entries[entries.len() - 2]["modelId"], "claude-opus-5-5");

        // A pi source stays on pi.
        let mut settings = settings;
        settings.defaults.drafting_model = None;
        let session = pi_session(&tmp.path().join("agent"), &cwd);
        let args_file = tmp.path().join("args");
        let args = parse(&[session.to_str().unwrap()]);
        let draft = create(&args, &store, &fake_pi(&args_file), &no_claude, &settings).unwrap();
        assert!(draft.meta.pi().is_some());

        // Without the setting, `--backend claude` continues on Claude Code
        // on the session's model.
        let args = parse(&[transcript.to_str().unwrap(), "--backend", "claude"]);
        let draft = create(&args, &store, &no_pi, &claude_ok, &Settings::default()).unwrap();
        let Backend::Claude(session) = &draft.meta.backend else {
            panic!("not Claude: {:?}", draft.meta.backend);
        };
        assert_eq!(session.model, None);
    }

    #[test]
    fn unusable_claude_code_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let transcript = native_transcript(&tmp.path().join("config"), &cwd);
        let store = Store::new(tmp.path().join("drafts"));
        let missing = || Err("Claude Code wasn't found on your login shell's PATH.".to_owned());

        let from_setting = parse(&[transcript.to_str().unwrap()]);
        let flagged = parse(&[transcript.to_str().unwrap(), "--backend", "claude"]);
        for (args, settings) in [
            (&from_setting, claude_rule_on(tmp.path())),
            (&flagged, Settings::default()),
        ] {
            let err = create(args, &store, &no_pi, &missing, &settings).unwrap_err();
            let err = format!("{err:#}");
            assert!(err.contains("can't continue"), "{err}");
            assert!(err.contains("wasn't found"), "{err}");
            assert!(err.contains("--backend pi"), "{err}");
        }
        assert_eq!(
            fs::read_dir(tmp.path().join("drafts")).map_or(0, |d| d.count()),
            0
        );
    }

    #[test]
    fn claude_backend_with_a_source_that_cant_be_forked_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let gone = native_transcript(&tmp.path().join("config"), &tmp.path().join("deleted"));
        let args = parse(&[gone.to_str().unwrap(), "--backend", "claude"]);
        let err = create(&args, &store, &no_pi, &no_claude, &Settings::default()).unwrap_err();
        assert!(err.to_string().contains("no longer exists"), "{err}");

        // Recognized as Claude Code, but without a complete message.
        let cut = tmp.path().join("cut.jsonl");
        let line = serde_json::json!({
            "type": "user",
            "uuid": "1",
            "sessionId": "s",
            "cwd": tmp.path(),
            "message": {"content": "hi"},
        })
        .to_string();
        fs::write(&cut, line).unwrap();
        let args = parse(&[cut.to_str().unwrap(), "--backend", "claude"]);
        let err = create(&args, &store, &no_pi, &claude_ok, &Settings::default()).unwrap_err();
        assert!(format!("{err:#}").contains("copying"), "{err:#}");
        assert!(store.scan().unwrap().drafts.is_empty());
        assert_eq!(fs::read_dir(tmp.path().join("drafts")).unwrap().count(), 0);
    }

    #[test]
    fn a_claude_fork_uses_the_config_dir_its_source_was_written_under() {
        let home = Path::new("/Users/me");
        let config = |source: &str, caller: Option<&str>| {
            claude_config_dir(Path::new(source), home, caller.map(Into::into))
        };
        assert_eq!(
            config("/Users/me/.claude/projects/-a/s.jsonl", Some("/cfg")),
            None
        );
        assert_eq!(
            config("/Users/me/.claude-work/projects/-a/s.jsonl", None),
            Some("/Users/me/.claude-work".into())
        );
        // Copied out of Claude's storage: the caller's, when absolute.
        let cfg = std::path::absolute("/cfg").unwrap();
        assert_eq!(config("/tmp/s.jsonl", cfg.to_str()), Some(cfg.clone()));
        assert_eq!(config("/tmp/s.jsonl", Some("cfg")), None);
        assert_eq!(config("/tmp/s.jsonl", Some("/Users/me/.claude")), None);
        assert_eq!(config("/tmp/s.jsonl", None), None);
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
        assert!(create(&args, &store, &no_pi, &no_claude, &Settings::default()).is_err());
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
        settings.defaults.backend = Some(DraftBackend::Pi);

        let draft = create(
            &parse(&[transcript.to_str().unwrap()]),
            &store,
            &no_pi,
            &no_claude,
            &settings,
        )
        .unwrap();
        let meta = store.load(&draft.id).unwrap().meta;
        assert_eq!(meta.pi().unwrap().agent_dir, profile);
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
        let other = create(&flagged, &store, &no_pi, &no_claude, &settings)
            .unwrap()
            .meta;
        assert_eq!(other.pi().unwrap().agent_dir, tmp.path());
        assert_eq!(other.target, Target::Github);

        // Later changes leave existing drafts alone.
        settings.directories.clear();
        let _ = create(
            &parse(&[transcript.to_str().unwrap()]),
            &store,
            &no_pi,
            &no_claude,
            &settings,
        );
        assert_eq!(store.load(&draft.id).unwrap().meta, meta);
    }

    #[cfg(unix)]
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
            provider: "openrouter".into(),
            model_id: "missing".into(),
            thinking: ThinkingLevel::High,
        });
        let draft = create(
            &parse(&[session.to_str().unwrap()]),
            &store,
            &pi,
            &no_claude,
            &settings,
        )
        .unwrap();
        assert_eq!(
            store
                .load(&draft.id)
                .unwrap()
                .meta
                .pi()
                .unwrap()
                .session_file,
            Some(draft.session_dir().join("fork.jsonl"))
        );
    }

    #[cfg(unix)]
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
    /// sends the fixture to the profile's default model. Run with `cargo test
    /// -j 2 -p draftmux --bin draftmux real_pi_answers -- --ignored`;
    /// `DRAFTMUX_SMOKE_PI_AGENT_DIR`
    /// picks the profile (default `~/.pi/agent`), which needs a default model.
    #[test]
    #[ignore = "runs real pi against the profile's default model"]
    fn real_pi_answers_from_a_converted_session() {
        let home = std::env::home_dir().unwrap();
        let profile = std::env::var_os("DRAFTMUX_SMOKE_PI_AGENT_DIR")
            .map_or_else(|| home.join(".pi/agent"), PathBuf::from);
        let tmp = tempfile::tempdir().unwrap();
        let (_, draft) = claude_draft(tmp.path(), &profile);
        let file = draft.meta.pi().unwrap().session_file.clone().unwrap();
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
            "--backend",
            "pi",
        ]);
        let model = ModelOverride {
            provider: "anthropic".into(),
            model_id: "claude-opus-5-5".into(),
            thinking: ThinkingLevel::High,
        };
        let draft = create(&args, &store, &no_pi, &no_claude, &drafting_on(model)).unwrap();

        let entries = session_entries(&draft.meta.pi().unwrap().session_file.clone().unwrap());
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
            instructions: None,
            context_files: true,
        };
        let (agent, _events) = Agent::new(spec, Duration::from_secs(60), Duration::from_secs(60));
        let state = block_on(agent.get_state()).expect("get_state");
        let models = block_on(agent.get_available_models()).expect("get_available_models");
        (state, models)
    }

    /// Real pi forks a pi session at handoff: the draft continues on the
    /// source's model, an override sticks across restarts, and the source and
    /// the profile's settings are left alone. Opt-in; calls no model. Run with
    /// `cargo test -j 2 -p draftmux --bin draftmux real_pi_forks -- --ignored`;
    /// `DRAFTMUX_SMOKE_PI_AGENT_DIR`
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
        let draft = create(&args, &store, &pi, &no_claude, &Settings::default()).unwrap();
        eprintln!("fork took {} ms", started.elapsed().as_millis());
        assert_eq!(fs::read_to_string(&source).unwrap(), original + "\n");
        let file = draft.meta.pi().unwrap().session_file.clone().unwrap();
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
        let draft = create(&args, &store, &pi, &no_claude, &drafting_on(model)).unwrap();
        let file = draft.meta.pi().unwrap().session_file.clone().unwrap();
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
