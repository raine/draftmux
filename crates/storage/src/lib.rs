//! Draft storage: one directory per draft, no database.
//!
//! Each draft lives in `<root>/<id>/` with `draft.md` (the message text),
//! `meta.json` (see [`Meta`]) and `session/` (the draft's pi session dir).
//! The drafts list is a scan of these directories. Files are replaced
//! atomically, so a reader such as the `draftmux` CLI sees either the old or
//! the new contents, never a partial write.

mod atomic;
mod autosave;
mod config;
mod meta;
mod store;

pub use atomic::write as write_atomic;
pub use autosave::Autosave;
pub use config::{CONFIG_DIR_VAR, CONFIG_FILES, config_dir, prepare_config_dir};
pub use meta::{DraftSettings, Meta, ModelChoice, Source, SourceKind, Status, Syntax, Target};
pub use store::{DATA_DIR_VAR, Draft, NewDraft, Scan, Store, data_dir};
