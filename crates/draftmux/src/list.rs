//! `draftmux list`: the drafts, for finding one without its ID.
//!
//! Reads the drafts directory directly, like `show`, so it works whether or
//! not the app is running. The sample draft isn't listed: `show` never shows
//! it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use draftmux_storage::{Draft, Status, Store, Target};
use serde::Serialize;

/// List drafts, most recently active first.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only drafts whose working directory is DIR or inside it.
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Include archived drafts.
    #[arg(long)]
    all: bool,
    /// Print the drafts as a JSON array.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct JsonDraft<'a> {
    id: &'a str,
    title: &'a str,
    /// The title the sidebar shows, derived from the text when no title is
    /// stored.
    display_title: String,
    target: Target,
    status: Status,
    cwd: &'a Path,
    source: Option<&'a Path>,
    updated_at: DateTime<Utc>,
    path: PathBuf,
}

/// Runs the command against the default drafts directory.
pub fn main(args: Args) -> Result<()> {
    let store = Store::new(Store::default_root()?);
    let home = std::env::home_dir();
    run(
        &args,
        &store,
        home.as_deref(),
        &mut io::stdout().lock(),
        &mut io::stderr(),
    )
}

fn run(
    args: &Args,
    store: &Store,
    home: Option<&Path>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    let cwd = match &args.cwd {
        Some(dir) => Some(comparable(
            &std::path::absolute(dir).with_context(|| format!("resolving {}", dir.display()))?,
        )),
        None => None,
    };
    let scan = store.scan()?;
    for (dir, e) in &scan.errors {
        writeln!(err, "draftmux: skipping {}: {e:#}", dir.display())?;
    }
    let drafts: Vec<&Draft> = scan
        .drafts
        .iter()
        .filter(|draft| !draft.meta.is_sample())
        .filter(|draft| args.all || draft.meta.status == Status::Active)
        .filter(|draft| {
            cwd.as_ref()
                .is_none_or(|dir| comparable(&draft.meta.cwd).starts_with(dir))
        })
        .collect();
    let title = |draft: &Draft| {
        crate::drafts::display_title(&draft.meta.title, || {
            store.read_text(&draft.id).unwrap_or_default()
        })
    };

    let output = if args.json {
        let json: Vec<JsonDraft> = drafts
            .iter()
            .map(|draft| JsonDraft {
                id: &draft.id,
                title: &draft.meta.title,
                display_title: title(draft),
                target: draft.meta.target,
                status: draft.meta.status,
                cwd: &draft.meta.cwd,
                source: draft.meta.source.as_ref().map(|s| s.path.as_path()),
                updated_at: draft.meta.updated_at,
                path: draft.draft_path(),
            })
            .collect();
        let mut json = serde_json::to_string_pretty(&json)?;
        json.push('\n');
        json
    } else {
        let mut text = String::new();
        for draft in &drafts {
            let meta = &draft.meta;
            let updated = meta.updated_at.with_timezone(&Local);
            text.push_str(&format!(
                "{}  {}  {:<6}  {}{}  {}\n",
                draft.id,
                updated.format("%Y-%m-%d %H:%M"),
                meta.target.name(),
                crate::drafts::tilde(&meta.cwd, home),
                if meta.status == Status::Archived {
                    "  (archived)"
                } else {
                    ""
                },
                title(draft),
            ));
        }
        text
    };
    crate::write_output(out, output.as_bytes())
}

/// `path` with symlinks resolved when it exists, so `/tmp` and
/// `/private/tmp` compare equal.
fn comparable(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
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

    fn list(store: &Store, argv: &[&str]) -> String {
        let args = Cli::try_parse_from(std::iter::once("list").chain(argv.iter().copied()))
            .unwrap()
            .args;
        let mut out = Vec::new();
        run(
            &args,
            store,
            Some(Path::new("/home/u")),
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn create(store: &Store, title: &str, cwd: &str, minutes: i64) -> Draft {
        let draft = store
            .create(NewDraft {
                target: Target::Slack,
                title: title.to_owned(),
                source: Some(Source {
                    kind: SourceKind::Claude,
                    path: "/s/a.jsonl".into(),
                }),
                cwd: cwd.into(),
                pi_agent_dir: "/tmp/.pi/agent".into(),
                note: None,
                settings: Default::default(),
            })
            .unwrap();
        let at = chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(minutes);
        store
            .update_meta(&draft.id, |meta| meta.updated_at = at)
            .unwrap();
        store.load(&draft.id).unwrap()
    }

    fn ids(output: &str) -> Vec<&str> {
        output
            .lines()
            .map(|line| line.split_whitespace().next().unwrap())
            .collect()
    }

    #[test]
    fn lists_active_drafts_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let old = create(&store, "Old", "/home/u/code/app", 0);
        let new = create(&store, "New", "/home/u/code/app", 10);
        let archived = create(&store, "Archived", "/home/u/code/app", 20);
        store.archive(&archived.id).unwrap();

        let output = list(&store, &[]);
        assert_eq!(ids(&output), [new.id.as_str(), old.id.as_str()]);
        let line = output.lines().next().unwrap();
        assert!(line.contains("  slack   ~/code/app  New"), "{line}");

        let output = list(&store, &["--all"]);
        assert_eq!(ids(&output)[0], archived.id);
        assert!(output.contains("(archived)  Archived"));
    }

    #[test]
    fn untitled_drafts_show_the_title_from_their_text() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = create(&store, "", "/home/u/code/app", 0);
        store
            .save_text(&draft.id, "---\ntitle: Fix the cache\n---\nBody")
            .unwrap();

        let output = list(&store, &[]);
        assert!(
            output.trim_end().ends_with("~/code/app  Fix the cache"),
            "{output}"
        );

        let json: serde_json::Value = serde_json::from_str(&list(&store, &["--json"])).unwrap();
        assert_eq!(json[0]["title"], "");
        assert_eq!(json[0]["display_title"], "Fix the cache");
    }

    #[test]
    fn cwd_filters_to_the_directory_and_below() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let app = create(&store, "App", "/home/u/code/app", 0);
        let sub = create(&store, "Sub", "/home/u/code/app/crates/x", 1);
        create(&store, "Other", "/home/u/code/application", 2);

        let output = list(&store, &["--cwd", "/home/u/code/app"]);
        assert_eq!(ids(&output), [sub.id.as_str(), app.id.as_str()]);
    }

    #[test]
    fn json_has_the_fields_an_agent_needs() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = create(&store, "PR", "/home/u/code/app", 0);

        let json: serde_json::Value = serde_json::from_str(&list(&store, &["--json"])).unwrap();
        let entry = &json[0];
        assert_eq!(entry["id"], draft.id.as_str());
        assert_eq!(entry["title"], "PR");
        assert_eq!(entry["display_title"], "PR");
        assert_eq!(entry["target"], "slack");
        assert_eq!(entry["status"], "active");
        assert_eq!(entry["cwd"], "/home/u/code/app");
        assert_eq!(entry["source"], "/s/a.jsonl");
        assert_eq!(entry["path"], draft.draft_path().to_str().unwrap());
    }
}
