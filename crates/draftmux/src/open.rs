//! `draftmux open`: hands a session off to a new draft.
//!
//! Creates the draft directory itself, then asks the app (launching it if
//! needed) to focus the draft. The draft exists even if the app can't be
//! reached.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::ValueEnum;
use draftmux_storage::{Draft, NewDraft, Source, Store, Target};

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
    store.create(NewDraft {
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
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;
    use draftmux_storage::SourceKind;

    use std::path::Path;

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
}
