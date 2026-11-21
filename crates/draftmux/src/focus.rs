//! `draftmux focus`: selects a draft in the app and brings it forward.
//!
//! Launches the app if it isn't running, as `open` does. Archived drafts
//! aren't in the sidebar, so they're refused with a pointer to `restore`.

use anyhow::{Result, bail};
use draftmux_storage::{Status, Store};

use crate::ipc;

/// Select a draft in the app and bring the window to the front.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Draft ID, as printed by `draftmux open`.
    id: String,
}

/// Runs the command against the default drafts directory.
pub fn main(args: Args) -> Result<()> {
    run(&args, &Store::open_default()?, ipc::send)
}

fn run(args: &Args, store: &Store, send: impl FnOnce(&ipc::Request) -> Result<()>) -> Result<()> {
    let draft = crate::drafts::load(store, &args.id)?;
    if draft.meta.status == Status::Archived {
        bail!(
            "draft {} is archived; run `draftmux restore {}` first",
            draft.id,
            draft.id
        );
    }
    send(&ipc::Request::Focus { id: draft.id })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

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

    /// Runs `focus <id>`, returning the result and the IDs sent to the app.
    fn focus(store: &Store, id: &str) -> (Result<()>, Vec<String>) {
        let args = Cli::try_parse_from(["focus", id]).unwrap().args;
        let sent = RefCell::new(Vec::new());
        let result = run(&args, store, |request| {
            let ipc::Request::Focus { id } = request else {
                panic!("unexpected request {request:?}");
            };
            sent.borrow_mut().push(id.clone());
            Ok(())
        });
        (result, sent.into_inner())
    }

    #[test]
    fn asks_the_app_to_focus_an_active_draft() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store);

        let (result, sent) = focus(&store, &id);
        result.unwrap();
        assert_eq!(sent, [id]);
    }

    #[test]
    fn unknown_and_archived_drafts_are_clear_errors() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = create(&store);
        store.archive(&id).unwrap();

        let (result, sent) = focus(&store, &id);
        assert_eq!(
            result.unwrap_err().to_string(),
            format!("draft {id} is archived; run `draftmux restore {id}` first")
        );
        assert!(sent.is_empty());

        let (result, sent) = focus(&store, "0000000000000000");
        assert_eq!(
            result.unwrap_err().to_string(),
            "no draft with id 0000000000000000"
        );
        assert!(sent.is_empty());
    }
}
