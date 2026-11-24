//! A blank draft started in the app, to write from scratch. It has no source
//! session or file: its agent starts empty in the home directory, on the
//! agent `draftmux <FILE>` would choose, and the draft is deleted if it is
//! left empty.

use anyhow::Result;
use draftmux_settings::Settings;
use draftmux_storage::{Backend, Draft, NewDraft, Store};

use crate::edit::{self, Env};

/// Creates a blank draft with the settings resolved for the home directory.
pub fn create(store: &Store, settings: &Settings, env: &Env) -> Result<Draft> {
    let home = env.home;
    let resolved = settings.resolve(home, home);
    let kind = edit::agent_kind(&resolved, env);
    let new = NewDraft {
        target: resolved.target.value,
        title: String::new(),
        source: None,
        cwd: home.to_owned(),
        // Replaced by the session's.
        backend: Backend::pi(home),
        note: None,
        summary: None,
        settings: edit::session_settings(kind, &resolved),
        text: String::new(),
        file: None,
    };
    store.create_blank(new, |draft| {
        edit::empty_session(
            kind,
            home,
            &draft.session_dir(),
            &resolved,
            home,
            env.claude_config_dir,
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use draftmux_settings::DraftBackend;
    use draftmux_storage::Target;

    use super::*;

    fn env<'a>(home: &'a Path, claude: &'a dyn Fn() -> Result<(), String>) -> Env<'a> {
        Env {
            home,
            claude,
            claude_config_dir: None,
        }
    }

    #[test]
    fn starts_empty_in_home_with_the_settings_for_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let mut settings = Settings::default();
        settings.defaults.target = Some(Target::Slack);
        let no_claude = || Err("no claude".to_owned());

        let draft = create(&store, &settings, &env(tmp.path(), &no_claude)).unwrap();
        let meta = &store.load(&draft.id).unwrap().meta;
        assert_eq!(meta, &draft.meta);
        assert!(meta.blank && !meta.is_sample());
        assert!(meta.source.is_none() && meta.file.is_none());
        assert!(!meta.first_draft_pending);
        assert_eq!(meta.title, "");
        assert_eq!(meta.cwd, tmp.path());
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(store.read_text(&draft.id).unwrap(), "");
        // Without Claude Code, on pi, with a session that has no messages.
        assert!(meta.pi().unwrap().session_file.as_ref().unwrap().is_file());
        assert!(!meta.chat_started());
    }

    #[test]
    fn runs_on_claude_code_when_available_unless_settings_choose_pi() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().join("drafts"));
        let claude = || Ok(());

        let draft = create(&store, &Settings::default(), &env(tmp.path(), &claude)).unwrap();
        assert!(matches!(draft.meta.backend, Backend::Claude(_)));
        assert_eq!(draft.meta.settings.selection_model, None);

        let mut settings = Settings::default();
        settings.defaults.backend = Some(DraftBackend::Pi);
        let draft = create(&store, &settings, &env(tmp.path(), &claude)).unwrap();
        assert!(draft.meta.pi().is_some());
    }
}
