//! `draftmux restore`: unarchives a draft.
//!
//! Writes the draft's status directly, so it works whether or not the app is
//! running. A running app is then asked to focus the draft, which rescans the
//! drafts and puts it back in the sidebar; a stopped one isn't launched.

use std::io::{self, Write};

use anyhow::{Result, bail};
use draftmux_storage::{Status, Store};

use crate::ipc;

/// Unarchive a draft, putting it back in the sidebar.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Draft ID, as printed by `draftmux open` or `draftmux list --all`.
    id: String,
}

/// Runs the command and returns the process exit code.
pub fn main(args: Args) -> Result<u8> {
    run(
        &args,
        &Store::open_default()?,
        ipc::send_if_running,
        &mut io::stderr(),
    )
}

/// `focus_if_running` sends a request to the app if it's running and returns
/// whether it did.
fn run(
    args: &Args,
    store: &Store,
    focus_if_running: impl FnOnce(&ipc::Request) -> Result<bool>,
    err: &mut impl Write,
) -> Result<u8> {
    let draft = crate::drafts::load(store, &args.id)?;
    if draft.meta.status != Status::Archived {
        bail!("draft {} isn't archived", draft.id);
    }
    store.unarchive(&draft.id)?;
    match focus_if_running(&ipc::Request::Focus {
        id: draft.id.clone(),
    }) {
        Ok(_) => Ok(0),
        Err(e) => {
            writeln!(
                err,
                "draftmux: restored the draft but couldn't focus it in the app: {e:#}\n\
                 draftmux: run `draftmux focus {}` to try again",
                draft.id
            )?;
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use clap::Parser;
    use draftmux_storage::{NewDraft, Target};

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn create(store: &Store) -> String {
        store
            .create(NewDraft {
                target: Target::Github,
                title: "PR".into(),
                source: None,
                cwd: "/tmp".into(),
                backend: draftmux_storage::Backend::pi("/tmp/.pi/agent"),
                note: None,
                summary: None,
                settings: Default::default(),
                text: String::new(),
                file: None,
            })
            .unwrap()
            .id
    }

    /// Runs `restore <id>` with `focus` standing in for the app, returning
    /// the exit code and stderr.
    fn restore(
        store: &Store,
        id: &str,
        focus: impl FnOnce(&ipc::Request) -> Result<bool>,
    ) -> Result<(u8, String)> {
        let args = Cli::try_parse_from(["restore", id]).unwrap().args;
        let mut err = Vec::new();
        let code = run(&args, store, focus, &mut err)?;
        Ok((code, String::from_utf8(err).unwrap()))
    }

    fn status(store: &Store, id: &str) -> Status {
        store.load(id).unwrap().meta.status
    }

    #[test]
    fn unarchives_and_focuses_the_draft_in_a_running_app() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store);
        store.archive(&id).unwrap();

        let mut focused = None;
        let (code, err) = restore(&store, &id, |request| {
            // The app rescans on focus, so the status is written first.
            assert_eq!(status(&store, &id), Status::Active);
            focused = Some(format!("{request:?}"));
            Ok(true)
        })
        .unwrap();
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(focused.unwrap(), format!("Focus {{ id: {id:?} }}"));
    }

    #[test]
    fn works_without_the_app() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store);
        store.archive(&id).unwrap();

        let (code, _) = restore(&store, &id, |_| Ok(false)).unwrap();
        assert_eq!(code, 0);
        assert_eq!(status(&store, &id), Status::Active);
    }

    #[test]
    fn a_failed_focus_still_restores_the_draft() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store);
        store.archive(&id).unwrap();

        let (code, err) = restore(&store, &id, |_| Err(anyhow!("no reply"))).unwrap();
        assert_eq!(code, 1);
        assert_eq!(status(&store, &id), Status::Active);
        assert!(
            err.contains("couldn't focus it in the app: no reply"),
            "{err}"
        );
        assert!(err.contains(&format!("`draftmux focus {id}`")), "{err}");
    }

    #[test]
    fn unknown_and_active_drafts_are_clear_errors() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store);
        let unreachable = |_: &ipc::Request| -> Result<bool> { panic!("nothing to focus") };

        let err = restore(&store, &id, unreachable).unwrap_err();
        assert_eq!(err.to_string(), format!("draft {id} isn't archived"));

        let err = restore(&store, "0000000000000000", unreachable).unwrap_err();
        assert_eq!(err.to_string(), "no draft with id 0000000000000000");
    }
}
