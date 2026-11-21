//! `draftmux save`: writes a draft back to the file it was opened from, as
//! the app's Save does.
//!
//! Saves the text the app last saved to `draft.md`, so it works whether or
//! not the app is running.

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::ArgGroup;
use draftmux_storage::{FileSave, Store};

/// Write a draft back to the file it was opened from.
#[derive(clap::Args, Debug)]
#[command(group(ArgGroup::new("draft").required(true).args(["id", "source"])))]
pub struct Args {
    /// Draft ID, as printed by `draftmux open`.
    id: Option<String>,
    /// Session file the draft was handed off from, as passed to `draftmux
    /// open`. If several drafts match, the most recently active one is saved.
    #[arg(long, value_name = "SESSION_FILE")]
    source: Option<PathBuf>,
    /// Write the file even if it was changed or removed since the draft last
    /// read or wrote it.
    #[arg(long)]
    overwrite: bool,
}

/// Runs the command against the default drafts directory.
pub fn main(args: Args) -> Result<()> {
    let store = Store::open_default()?;
    run(&args, &store, &mut io::stdout().lock(), &mut io::stderr())
}

fn run(args: &Args, store: &Store, out: &mut impl Write, err: &mut impl Write) -> Result<()> {
    let draft = crate::show::resolve(args.id.as_deref(), args.source.as_deref(), store, err)?;
    let Some(file) = &draft.meta.file else {
        bail!("draft {} wasn't opened from a file", draft.id);
    };
    let path = file.path.display();
    let text = store.read_text(&draft.id)?;
    match store.save_to_file(&draft.id, &text, args.overwrite)? {
        FileSave::Saved(_) => writeln!(out, "{path}")?,
        FileSave::Changed => bail!(
            "{path} was changed since draft {} last read or wrote it; \
             pass --overwrite to replace it",
            draft.id
        ),
        FileSave::Missing => bail!(
            "{path} was removed since draft {} last read or wrote it; \
             pass --overwrite to recreate it",
            draft.id
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use draftmux_storage::{Backend, LinkedFile, NewDraft, Source, SourceKind, Target};

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn save(store: &Store, argv: &[&str]) -> Result<String> {
        let args = Cli::try_parse_from(std::iter::once("save").chain(argv.iter().copied()))
            .unwrap()
            .args;
        let mut out = Vec::new();
        run(&args, store, &mut out, &mut Vec::new())?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn create(store: &Store, file: Option<LinkedFile>) -> String {
        store
            .create(NewDraft {
                target: Target::GithubFile,
                title: String::new(),
                source: Some(Source {
                    kind: SourceKind::Pi,
                    path: std::path::absolute("/s/a.jsonl").unwrap(),
                }),
                cwd: "/tmp/repo".into(),
                backend: Backend::pi("/tmp/.pi/agent"),
                note: None,
                summary: None,
                settings: Default::default(),
                text: "old\n".into(),
                file,
            })
            .unwrap()
            .id
    }

    #[test]
    fn writes_the_saved_draft_to_its_file() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("drafts"));
        let path = root.path().join("README.md");
        std::fs::write(&path, "old\n").unwrap();
        let id = create(&store, Some(LinkedFile::new(&path, b"old\n")));
        store.save_text(&id, "new\n").unwrap();

        let out = save(&store, &["--source", "/s/a.jsonl"]).unwrap();
        assert_eq!(out, format!("{}\n", path.display()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    }

    #[test]
    fn refuses_a_changed_file_without_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("drafts"));
        let path = root.path().join("README.md");
        std::fs::write(&path, "old\n").unwrap();
        let id = create(&store, Some(LinkedFile::new(&path, b"old\n")));
        std::fs::write(&path, "theirs\n").unwrap();

        let err = save(&store, &[&id]).unwrap_err().to_string();
        assert!(err.contains("pass --overwrite to replace it"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n");

        save(&store, &[&id, "--overwrite"]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
    }

    #[test]
    fn needs_a_linked_file() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store, None);
        let err = save(&store, &[&id]).unwrap_err().to_string();
        assert_eq!(err, format!("draft {id} wasn't opened from a file"));
    }
}
