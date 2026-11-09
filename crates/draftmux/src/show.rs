//! `draftmux show`: reads a draft back for the agent that started it.
//!
//! Reads the draft directory directly, so it works whether or not the app is
//! running, and never modifies the draft. The sample draft has no source
//! session to read back for, so it is never shown.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::ArgGroup;
use draftmux_storage::{Draft, Status, Store, Target};
use serde::Serialize;

/// Print a draft's text as the app last saved it.
#[derive(clap::Args, Debug)]
#[command(group(ArgGroup::new("draft").required(true).args(["id", "source"])))]
pub struct Args {
    /// Draft ID, as printed by `draftmux open`.
    id: Option<String>,
    /// Session file the draft was handed off from, as passed to `draftmux
    /// open`. If several drafts match, the most recently active one is shown.
    #[arg(long, value_name = "SESSION_FILE")]
    source: Option<PathBuf>,
    /// Print the absolute path of the live draft.md instead of its text.
    #[arg(long, conflicts_with = "json")]
    path: bool,
    /// Print the ID, title, target, status, path and text as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct JsonOutput<'a> {
    id: &'a str,
    title: &'a str,
    target: Target,
    status: Status,
    path: &'a Path,
    body: &'a str,
}

/// Runs the command against the default drafts directory.
pub fn main(args: Args) -> Result<()> {
    let store = Store::new(Store::default_root()?);
    run(&args, &store, &mut io::stdout().lock(), &mut io::stderr())
}

fn run(args: &Args, store: &Store, out: &mut impl Write, err: &mut impl Write) -> Result<()> {
    let draft = resolve(args, store, err)?;
    let path = draft.draft_path();
    let output = if args.path {
        format!("{}\n", path.display())
    } else {
        let body = store.read_text(&draft.id)?;
        if args.json {
            let json = JsonOutput {
                id: &draft.id,
                title: &draft.meta.title,
                target: draft.meta.target,
                status: draft.meta.status,
                path: &path,
                body: &body,
            };
            let mut json = serde_json::to_string_pretty(&json)?;
            json.push('\n');
            json
        } else {
            body
        }
    };
    crate::write_output(out, output.as_bytes())
}

fn resolve(args: &Args, store: &Store, err: &mut impl Write) -> Result<Draft> {
    let Some(source) = &args.source else {
        let id = args.id.as_deref().expect("clap requires an id or --source");
        return match store.load(id) {
            Ok(draft) if draft.meta.is_sample() => Err(anyhow!("no draft with id {id}")),
            Ok(draft) => Ok(draft),
            Err(e)
                if e.downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                Err(anyhow!("no draft with id {id}"))
            }
            Err(e) => Err(e),
        };
    };

    // `open` records the source as an absolute path without resolving
    // symlinks; normalize the same way so a relative argument still matches.
    let source =
        &std::path::absolute(source).with_context(|| format!("resolving {}", source.display()))?;
    let scan = store.scan()?;
    for (dir, e) in &scan.errors {
        writeln!(err, "draftmux: skipping {}: {e:#}", dir.display())?;
    }
    // The scan is sorted by activity, newest first, and includes archived
    // drafts.
    let mut matches = scan.drafts.into_iter().filter(|draft| {
        draft
            .meta
            .source
            .as_ref()
            .is_some_and(|s| &s.path == source)
    });
    let draft = matches
        .next()
        .ok_or_else(|| anyhow!("no draft from source {}", source.display()))?;
    let others: Vec<Draft> = matches.collect();
    if !others.is_empty() {
        writeln!(
            err,
            "draftmux: showing {}, the most recently active of {} drafts from this source; others:",
            draft.id,
            others.len() + 1
        )?;
        for other in &others {
            writeln!(
                err,
                "  {}  {}  {}  {}",
                other.id,
                match other.meta.status {
                    Status::Active => "active  ",
                    Status::Archived => "archived",
                },
                other.meta.updated_at.format("%Y-%m-%d %H:%M"),
                other.meta.title
            )?;
        }
    }
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use draftmux_storage::{NewDraft, Source, SourceKind};

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn parse(argv: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(std::iter::once("show").chain(argv.iter().copied())).map(|c| c.args)
    }

    struct Output {
        result: Result<()>,
        out: String,
        err: String,
    }

    fn show(store: &Store, argv: &[&str]) -> Output {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let result = run(&parse(argv).unwrap(), store, &mut out, &mut err);
        Output {
            result,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn create(store: &Store, title: &str, source: &str, text: &str, minutes: i64) -> Draft {
        let draft = store
            .create(NewDraft {
                target: Target::Github,
                title: title.to_owned(),
                source: Some(Source {
                    kind: SourceKind::Pi,
                    path: source.into(),
                }),
                cwd: "/tmp/repo".into(),
                pi_agent_dir: "/tmp/.pi/agent".into(),
                note: None,
                settings: Default::default(),
            })
            .unwrap();
        store.save_text(&draft.id, text).unwrap();
        let at = chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(minutes);
        store
            .update_meta(&draft.id, |meta| meta.updated_at = at)
            .unwrap();
        store.load(&draft.id).unwrap()
    }

    const BODY: &str = "Fix `$HOME` \"quoting\" — ünïcode\r\n\n\nno trailing newline";

    #[test]
    fn prints_body_verbatim_by_id() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = create(&store, "PR", "/s/a.jsonl", BODY, 0);

        let output = show(&store, &[&draft.id]);
        output.result.unwrap();
        assert_eq!(output.out, BODY);
        assert_eq!(output.err, "");
    }

    #[test]
    fn source_picks_most_recent_and_lists_others_including_archived() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let old = create(&store, "old", "/s/a.jsonl", "old", 1);
        let newest = create(&store, "newest", "/s/a.jsonl", "newest", 3);
        let archived = create(&store, "archived", "/s/a.jsonl", "archived", 2);
        store.archive(&archived.id).unwrap();
        create(&store, "other session", "/s/b.jsonl", "b", 9);

        let output = show(&store, &["--source", "/s/a.jsonl"]);
        output.result.unwrap();
        assert_eq!(output.out, "newest");
        let listed: Vec<&str> = output
            .err
            .lines()
            .skip(1)
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(listed, [&archived.id, &old.id]);
        assert!(
            output
                .err
                .starts_with(&format!("draftmux: showing {}", newest.id))
        );
        assert!(output.err.contains("archived"));

        // A sole match prints nothing on stderr, and archived drafts are
        // still readable.
        store.archive(&newest.id).unwrap();
        let output = show(&store, &["--source", "/s/b.jsonl"]);
        assert_eq!((output.out.as_str(), output.err.as_str()), ("b", ""));
        assert_eq!(show(&store, &[&newest.id]).out, "newest");
    }

    #[test]
    fn relative_source_matches_the_absolute_path_open_records() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let recorded = std::env::current_dir().unwrap().join("s.jsonl");
        create(&store, "PR", recorded.to_str().unwrap(), BODY, 0);

        assert_eq!(show(&store, &["--source", "s.jsonl"]).out, BODY);
    }

    #[test]
    fn path_prints_absolute_live_draft_path() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("Application Support"));
        let draft = create(&store, "PR", "/s/a.jsonl", BODY, 0);

        let output = show(&store, &["--source", "/s/a.jsonl", "--path"]);
        output.result.unwrap();
        let path = output.out.strip_suffix('\n').unwrap();
        assert!(Path::new(path).is_absolute());
        assert_eq!(Path::new(path), draft.draft_path());
        assert_eq!(std::fs::read_to_string(path).unwrap(), BODY);
    }

    #[test]
    fn json_includes_metadata_and_body() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = create(&store, "Release notes", "/s/a.jsonl", BODY, 0);
        store.archive(&draft.id).unwrap();

        let output = show(&store, &[&draft.id, "--json"]);
        output.result.unwrap();
        let json: serde_json::Value = serde_json::from_str(&output.out).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": draft.id,
                "title": "Release notes",
                "target": "github",
                "status": "archived",
                "path": draft.draft_path(),
                "body": BODY,
            })
        );
    }

    #[test]
    fn no_match_is_a_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        create(&store, "PR", "/s/a.jsonl", "a", 0);

        let missing = "0190abcd-0000-7000-8000-000000000000";
        let output = show(&store, &[missing]);
        assert_eq!(
            output.result.unwrap_err().to_string(),
            format!("no draft with id {missing}")
        );
        assert_eq!(output.out, "");

        let output = show(&store, &["--source", "/s/other.jsonl"]);
        assert_eq!(
            output.result.unwrap_err().to_string(),
            "no draft from source /s/other.jsonl"
        );

        assert!(show(&store, &["../x"]).result.is_err());
    }

    #[test]
    fn sample_draft_is_never_shown() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let sample = create(&store, "Welcome", "/s/a.jsonl", "sample", 0);
        store
            .update_meta(&sample.id, |meta| meta.source = None)
            .unwrap();

        let output = show(&store, &[&sample.id]);
        assert_eq!(
            output.result.unwrap_err().to_string(),
            format!("no draft with id {}", sample.id)
        );
        assert_eq!(output.out, "");
        assert!(show(&store, &["--source", "/s/a.jsonl"]).result.is_err());
    }

    #[test]
    fn unreadable_drafts_are_reported_but_do_not_block_others() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        create(&store, "good", "/s/a.jsonl", "good", 0);
        let bad = create(&store, "bad", "/s/a.jsonl", "bad", 1);
        std::fs::write(bad.dir.join("meta.json"), "{").unwrap();

        let output = show(&store, &["--source", "/s/a.jsonl"]);
        output.result.unwrap();
        assert_eq!(output.out, "good");
        assert!(output.err.starts_with("draftmux: skipping "));
    }

    #[test]
    fn requires_exactly_one_selector_and_one_output_mode() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["id", "--source", "/s/a.jsonl"]).is_err());
        assert!(parse(&["id", "--path", "--json"]).is_err());
        assert!(parse(&["id", "--json"]).is_ok());
    }
}
