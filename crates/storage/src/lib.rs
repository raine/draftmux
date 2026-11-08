//! Draft storage: one directory per draft, no database.
//!
//! Each draft lives in `<root>/<id>/` with `draft.md` (the message text),
//! `meta.json` (see [`Meta`]) and `session/` (the draft's pi session dir).
//! The drafts list is a scan of these directories. Files are replaced
//! atomically, so a reader such as the `draftmux` CLI sees either the old or
//! the new contents, never a partial write.

mod atomic;
mod autosave;
mod meta;
mod store;

pub use autosave::Autosave;
pub use meta::{Meta, Source, SourceKind, Status, Target};
pub use store::{Draft, NewDraft, Scan, Store, data_dir};
