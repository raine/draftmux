//! The sample draft: a normal draft that explains the app, created on the
//! first launch and on request from the empty drafts list. It has no source
//! session, so its pi session starts empty.

use std::path::Path;

use anyhow::Result;
use draftmux_settings::Settings;
use draftmux_storage::{Draft, NewDraft, Store, Target};

use crate::{open, source};

pub const TITLE: &str = "Welcome to Draftmux";

pub const TEXT: &str = "\
## Welcome to Draftmux

This is a sample draft. Real drafts arrive from your coding agent: run `/draftmux` in Claude Code or `/skill:draftmux` in pi and it opens here with the conversation attached.

### Try it on this draft

- Edit this text; the preview on the right follows as you type
- Switch the target to Slack, Rich text or Plain text above the preview
- Select a sentence for quick rewrites
- Ask pi on the right to *make it shorter*

### When you're done

Copy the message out. With a handed-off draft you can instead run `/draftmux done` and the agent reads the draft back. Archive this one once you've had a look.
";

/// Whether `store` has never held a draft: no drafts, archived or
/// unreadable, and no sample created before. A store that can't be read
/// counts as used.
pub fn first_launch(store: &Store) -> bool {
    let empty = store
        .scan()
        .is_ok_and(|scan| scan.drafts.is_empty() && scan.errors.is_empty());
    empty && store.sample_created().is_ok_and(|created| !created)
}

/// Creates a sample draft in the home directory with the settings a handoff
/// from there would get, and records that the sample was created.
pub fn create(store: &Store, settings: &Settings, home: &Path) -> Result<Draft> {
    let resolved = settings.resolve(home, home);
    let pi_agent_dir = source::settings_profile(resolved.profile.value.as_deref(), home);
    // Without an override the session names no model and pi uses the
    // profile's default.
    let model = match resolved.model_override() {
        Some(model) => open::model_selection(model),
        None => claude_convert::ModelSelection::default(),
    };
    let new = NewDraft {
        target: Target::Github,
        title: TITLE.to_owned(),
        source: None,
        cwd: home.to_owned(),
        pi_agent_dir,
        note: None,
        settings: resolved.draft_settings(),
    };
    let draft = store.create_with_session(new, |draft| {
        draftmux_storage::write_atomic(&draft.draft_path(), TEXT.as_bytes())?;
        claude_convert::empty(home, &model).write_into(&draft.session_dir())
    })?;
    // The draft itself keeps the store from counting as unused.
    if let Err(err) = store.mark_sample_created() {
        eprintln!("draftmux: recording the sample draft: {err:#}");
    }
    Ok(draft)
}
