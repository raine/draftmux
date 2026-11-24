//! The drafts list and the selected draft, from [`draftmux_core::drafts`],
//! plus the sample and blank drafts, which need the app's settings.

use std::path::Path;

use anyhow::Result;
use draftmux_settings::Settings;

pub use draftmux_core::drafts::*;

use crate::edit::Env;
use crate::{blank, sample};

/// Creates a blank draft and returns its id.
pub fn create_blank(drafts: &mut Drafts, settings: &Settings, env: &Env) -> Result<String> {
    let draft = blank::create(drafts.store(), settings, env)?;
    drafts.refresh();
    Ok(draft.id)
}

/// Creates a sample draft and returns its id.
pub fn create_sample(drafts: &mut Drafts, settings: &Settings, home: &Path) -> Result<String> {
    let draft = sample::create(drafts.store(), settings, home)?;
    drafts.refresh();
    Ok(draft.id)
}

/// Creates the sample draft if the store has never held a draft, and
/// returns its id.
pub fn first_launch_sample(
    drafts: &mut Drafts,
    settings: &Settings,
    home: &Path,
) -> Result<Option<String>> {
    if !sample::first_launch(drafts.store()) {
        return Ok(None);
    }
    create_sample(drafts, settings, home).map(Some)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, Utc};
    use draftmux_storage::{NewDraft, Source, SourceKind, Store, Target};

    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        (root, store)
    }

    /// Creates a draft with `text`, active `age` ago.
    fn create(store: &Store, text: &str, age: TimeDelta) -> String {
        let draft = store
            .create(NewDraft {
                target: Target::Github,
                title: String::new(),
                source: Some(Source {
                    kind: SourceKind::Pi,
                    path: "/tmp/s.jsonl".into(),
                }),
                cwd: "/tmp".into(),
                backend: draftmux_storage::Backend::pi("/tmp/.pi/agent"),
                note: None,
                summary: None,
                settings: Default::default(),
                text: String::new(),
                file: None,
            })
            .unwrap();
        store.save_text(&draft.id, text).unwrap();
        store
            .update_meta(&draft.id, |meta| meta.updated_at = Utc::now() - age)
            .unwrap();
        draft.id
    }

    fn order(drafts: &Drafts) -> Vec<&str> {
        drafts
            .rows()
            .iter()
            .map(|row| row.draft.id.as_str())
            .collect()
    }

    #[test]
    fn first_launch_creates_the_sample_once() {
        let (root, store) = store();
        let home = root.path().join("home");
        let settings = Settings::default();
        let mut drafts = Drafts::new(store.clone());

        let id = first_launch_sample(&mut drafts, &settings, &home)
            .unwrap()
            .unwrap();
        let sample = store.load(&id).unwrap();
        assert!(sample.meta.is_sample());
        assert_eq!(sample.meta.title, sample::TITLE);
        assert_eq!(sample.meta.target, Target::Github);
        assert_eq!(sample.meta.cwd, home);
        assert_eq!(store.read_text(&id).unwrap(), sample::TEXT);
        let session =
            std::fs::read_to_string(sample.meta.pi().unwrap().session_file.clone().unwrap())
                .unwrap();
        assert_eq!(session.lines().count(), 1);
        assert!(session.contains(r#""type":"session""#) && session.contains(r#""version":3"#));
        assert_eq!(order(&drafts), [id.as_str()]);

        // Archived or deleted, it doesn't come back by itself.
        drafts.archive(&id).unwrap();
        let mut drafts = Drafts::new(store.clone());
        assert_eq!(
            first_launch_sample(&mut drafts, &settings, &home).unwrap(),
            None
        );
        std::fs::remove_dir_all(&sample.dir).unwrap();
        assert_eq!(
            first_launch_sample(&mut drafts, &settings, &home).unwrap(),
            None
        );

        // On request it does, as a new draft.
        let again = create_sample(&mut drafts, &settings, &home).unwrap();
        assert_ne!(again, id);
        assert_eq!(order(&drafts), [again.as_str()]);
    }

    #[test]
    fn a_store_that_held_drafts_gets_no_sample() {
        let settings = Settings::default();
        let home = Path::new("/tmp");

        let (_root, archived_store) = store();
        let archived = create(&archived_store, "a", TimeDelta::zero());
        archived_store.archive(&archived).unwrap();
        assert_eq!(
            first_launch_sample(&mut Drafts::new(archived_store), &settings, home).unwrap(),
            None
        );

        let (_root, unreadable_store) = store();
        let unreadable = create(&unreadable_store, "a", TimeDelta::zero());
        let meta = unreadable_store
            .load(&unreadable)
            .unwrap()
            .dir
            .join("meta.json");
        std::fs::write(meta, "{").unwrap();
        assert_eq!(
            first_launch_sample(&mut Drafts::new(unreadable_store), &settings, home).unwrap(),
            None
        );
    }

    #[test]
    fn sample_session_continues_on_the_drafting_model() {
        let (root, store) = store();
        let mut settings = Settings::default();
        settings.defaults.drafting_model = Some(draftmux_settings::DraftingModel::Model(
            draftmux_pi_rpc::ModelOverride {
                provider: "anthropic".into(),
                model_id: "claude-sonnet-5-5".into(),
                thinking: draftmux_pi_rpc::ThinkingLevel::Low,
            },
        ));
        let mut drafts = Drafts::new(store.clone());
        let id = create_sample(&mut drafts, &settings, root.path()).unwrap();

        let file = store
            .load(&id)
            .unwrap()
            .meta
            .pi()
            .unwrap()
            .session_file
            .clone()
            .unwrap();
        let session = std::fs::read_to_string(file).unwrap();
        assert!(
            session.contains(r#""modelId":"claude-sonnet-5-5""#),
            "{session}"
        );
        assert!(session.contains(r#""thinkingLevel":"low""#), "{session}");
    }
}
