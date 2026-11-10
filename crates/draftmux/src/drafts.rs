//! The drafts list and the selected draft, independent of the UI.
//!
//! Owns the order of operations that could lose or misfile text: the selected
//! draft's pending edits are flushed before it is left, archived or the app
//! quits, and a switch is refused while they can't be saved.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Local, Utc};
use draftmux_settings::Settings;
use draftmux_storage::{Autosave, Draft, Store, Syntax, Target};

use crate::sample;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(600);
const TITLE_MAX_CHARS: usize = 80;
/// Where a Slack draft's mrkdwn text is kept once it is rewritten as
/// Markdown.
const MRKDWN_BACKUP: &str = "draft.mrkdwn.md";

/// Rewrites `draft`, if it is a Slack draft written in mrkdwn, as Markdown
/// on disk, keeping the original in [`MRKDWN_BACKUP`], and returns the new
/// text. `None` when the draft is Markdown already.
pub fn convert_mrkdwn(store: &Store, draft: &mut Draft) -> Result<Option<String>> {
    if !draft.meta.is_mrkdwn() {
        return Ok(None);
    }
    let (meta, text) = store.convert_text(
        &draft.id,
        MRKDWN_BACKUP,
        Syntax::Markdown,
        markup::slack::from_mrkdwn,
    )?;
    draft.meta = meta;
    Ok(Some(text))
}

/// One sidebar row.
pub struct Row {
    pub draft: Draft,
    pub title: String,
}

pub struct Drafts {
    store: Store,
    /// Active drafts, most recently active first.
    rows: Vec<Row>,
    current: Option<Current>,
}

struct Current {
    draft: Draft,
    autosave: Autosave,
}

impl Drafts {
    pub fn new(store: Store) -> Self {
        let mut drafts = Self {
            store,
            rows: Vec::new(),
            current: None,
        };
        drafts.refresh();
        drafts
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn current(&self) -> Option<&Draft> {
        self.current.as_ref().map(|current| &current.draft)
    }

    pub fn current_id(&self) -> Option<&str> {
        self.current().map(|draft| draft.id.as_str())
    }

    /// Rescans the drafts directory, keeping the selection.
    pub fn refresh(&mut self) {
        let scan = match self.store.scan() {
            Ok(scan) => scan,
            Err(err) => {
                eprintln!("draftmux: scanning drafts: {err:#}");
                return;
            }
        };
        for (dir, err) in &scan.errors {
            eprintln!("draftmux: skipping {}: {err:#}", dir.display());
        }
        self.rows = scan
            .active()
            .map(|draft| Row {
                title: display_title(&draft.meta.title, || {
                    self.store.read_text(&draft.id).unwrap_or_default()
                }),
                draft: draft.clone(),
            })
            .collect();
        if let Some(current) = &mut self.current
            && let Some(row) = self
                .rows
                .iter()
                .find(|row| row.draft.id == current.draft.id)
        {
            current.draft = row.draft.clone();
        }
    }

    /// Makes `id` the selected draft and returns its text, rewriting a
    /// Slack draft written in mrkdwn as Markdown. Saves the previous draft
    /// first and stays on it if that fails.
    pub fn select(&mut self, id: &str) -> Result<String> {
        self.flush()?;
        let mut draft = self.store.load(id)?;
        let converted = convert_mrkdwn(&self.store, &mut draft)?;
        let text = match &converted {
            Some(text) => text.clone(),
            None => self.store.read_text(id)?,
        };
        let autosave = Autosave::new(
            self.store.clone(),
            draft.id.clone(),
            text.clone(),
            AUTOSAVE_DELAY,
        );
        self.current = Some(Current { draft, autosave });
        if converted.is_some() {
            self.refresh();
        }
        Ok(text)
    }

    /// Records the text the editor holds after loading the selected draft,
    /// which may differ from the file in line endings, so loading isn't saved
    /// as an edit.
    pub fn loaded(&mut self, text: &str) {
        if let Some(current) = &mut self.current {
            current.autosave.reset(text.to_owned());
        }
    }

    /// Reports the editor text after a user edit.
    pub fn edited(&mut self, text: &str, now: Instant) {
        if let Some(current) = &mut self.current {
            current.autosave.edited(text.to_owned(), now);
        }
    }

    /// When the pending edit is due to be saved.
    pub fn deadline(&self) -> Option<Instant> {
        self.current.as_ref()?.autosave.deadline()
    }

    /// Saves the pending edit if it is due.
    pub fn poll(&mut self, now: Instant) -> Result<()> {
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        if current.autosave.poll(now)? {
            self.refresh();
        }
        Ok(())
    }

    /// Saves the pending edit now.
    pub fn flush(&mut self) -> Result<()> {
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        if current.autosave.flush()? {
            self.refresh();
        }
        Ok(())
    }

    /// Changes the selected draft's target. The text is left as it is, since
    /// every target other than plain text reads Markdown. Not counted as
    /// activity.
    pub fn set_target(&mut self, target: Target) -> Result<()> {
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        current.draft.meta = self.store.update_meta(&current.draft.id, |meta| {
            meta.target = target;
            meta.syntax = Some(Syntax::Markdown);
        })?;
        self.refresh();
        Ok(())
    }

    /// Counts a change to `id`'s file made outside the editor as activity.
    pub fn touched(&mut self, id: &str) -> Result<()> {
        self.store
            .update_meta(id, |meta| meta.updated_at = Utc::now())?;
        self.refresh();
        Ok(())
    }

    /// Records that the first draft of `id` was requested.
    pub fn first_draft_requested(&mut self, id: &str) -> Result<()> {
        self.store
            .update_meta(id, |meta| meta.first_draft_pending = false)?;
        self.refresh();
        Ok(())
    }

    /// Archives `id`. When it is the selected draft, saves it first, clears
    /// the selection and returns the draft to select next: the row below,
    /// else the row above.
    pub fn archive(&mut self, id: &str) -> Result<Option<String>> {
        let is_current = self.current_id() == Some(id);
        // Saving moves the draft to the top, so find its neighbours after.
        if is_current {
            self.flush()?;
        }
        let Some(index) = self.rows.iter().position(|row| row.draft.id == id) else {
            bail!("draft {id} is not in the list");
        };
        self.store.archive(id)?;
        let next = is_current
            .then(|| {
                self.rows
                    .get(index + 1)
                    .or_else(|| index.checked_sub(1).and_then(|i| self.rows.get(i)))
                    .map(|row| row.draft.id.clone())
            })
            .flatten();
        if is_current {
            self.current = None;
        }
        self.refresh();
        Ok(next)
    }

    pub fn unarchive(&mut self, id: &str) -> Result<()> {
        self.store.unarchive(id)?;
        self.refresh();
        Ok(())
    }

    /// Creates a sample draft and returns its id.
    pub fn create_sample(&mut self, settings: &Settings, home: &Path) -> Result<String> {
        let draft = sample::create(&self.store, settings, home)?;
        self.refresh();
        Ok(draft.id)
    }

    /// Creates the sample draft if the store has never held a draft, and
    /// returns its id.
    pub fn first_launch_sample(
        &mut self,
        settings: &Settings,
        home: &Path,
    ) -> Result<Option<String>> {
        if !sample::first_launch(&self.store) {
            return Ok(None);
        }
        self.create_sample(settings, home).map(Some)
    }
}

/// The stored title, frontmatter title or subject, then the first body line
/// without markdown markers, else "Untitled draft".
pub fn display_title(title: &str, text: impl FnOnce() -> String) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_owned();
    }
    let text = text();
    let draft = markup::frontmatter::parse(&text);
    for key in ["title", "subject"] {
        if let Some(value) = draft
            .fields
            .get(key)
            .filter(|value| !value.trim().is_empty())
        {
            return value.trim().chars().take(TITLE_MAX_CHARS).collect();
        }
    }
    let line = draft
        .body
        .lines()
        .map(strip_markers)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.is_empty() {
        return "Untitled draft".to_owned();
    }
    line.chars().take(TITLE_MAX_CHARS).collect()
}

fn strip_markers(line: &str) -> String {
    let mut line = line.trim();
    loop {
        let before = line;
        line = line.trim_start_matches('>').trim_start();
        let unhashed = line.trim_start_matches('#');
        if unhashed.is_empty() || unhashed.starts_with([' ', '\t']) {
            line = unhashed.trim_start();
        }
        for bullet in ["- ", "* ", "+ ", "[ ] ", "[x] ", "[X] "] {
            line = line.strip_prefix(bullet).unwrap_or(line);
        }
        if let Some((number, rest)) = line.split_once(". ")
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
        {
            line = rest;
        }
        if line == before {
            break;
        }
    }
    markup::rich::plain(&markup::github::parse(line))
        .chars()
        .filter(|&c| c != markup::rich::GUARD)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Sidebar time: "now", "14m", "3h", a weekday within the week, else a date.
pub fn relative_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - at).num_minutes().max(0);
    if minutes < 1 {
        "now".to_owned()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h", minutes / 60)
    } else {
        let (at, now) = (at.with_timezone(&Local), now.with_timezone(&Local));
        if minutes < 6 * 24 * 60 {
            at.format("%a").to_string()
        } else if at.year() == now.year() {
            at.format("%b %-d").to_string()
        } else {
            at.format("%b %-d, %Y").to_string()
        }
    }
}

/// `path` with the home directory written as `~`.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use draftmux_storage::{NewDraft, Source, SourceKind, Status};

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
                pi_agent_dir: "/tmp/.pi/agent".into(),
                note: None,
                settings: Default::default(),
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
    fn frontmatter_titles_and_markdown_links() {
        let text = "---\ntitle: PR title\nsubject: Email subject\n---\nBody";
        assert_eq!(display_title("", || text.into()), "PR title");
        assert_eq!(display_title("Stored", || text.into()), "Stored");
        assert_eq!(
            display_title("", || "---\nsubject: Email\n---\nBody".into()),
            "Email"
        );
        assert_eq!(
            display_title("", || {
                "---\ntitle:\n---\nAdds [Aven](https://github.com/raine/aven).".into()
            }),
            "Adds Aven."
        );
    }

    #[test]
    fn rows_are_active_drafts_newest_first() {
        let (_root, store) = store();
        let old = create(&store, "old", TimeDelta::hours(2));
        let new = create(&store, "new", TimeDelta::minutes(1));
        let archived = create(&store, "gone", TimeDelta::zero());
        store.archive(&archived).unwrap();

        let drafts = Drafts::new(store);
        assert_eq!(order(&drafts), [new.as_str(), old.as_str()]);
        assert_eq!(drafts.rows()[0].title, "new");
    }

    #[test]
    fn switching_saves_pending_edit_to_the_previous_draft() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::minutes(1));
        let mut drafts = Drafts::new(store.clone());
        assert_eq!(drafts.select(&a).unwrap(), "a");
        drafts.edited("a edited", Instant::now());

        assert_eq!(drafts.select(&b).unwrap(), "b");
        assert_eq!(store.read_text(&a).unwrap(), "a edited");
        assert_eq!(store.read_text(&b).unwrap(), "b");
        // The save counts as activity, so A moves to the top right away.
        assert_eq!(order(&drafts), [a.as_str(), b.as_str()]);
        assert_eq!(drafts.rows()[0].title, "a edited");
    }

    #[test]
    fn an_outside_write_counts_as_activity() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::minutes(1));
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&b).unwrap();

        std::fs::write(store.load(&a).unwrap().draft_path(), "a rewritten").unwrap();
        drafts.touched(&a).unwrap();
        assert_eq!(order(&drafts), [a.as_str(), b.as_str()]);
        assert_eq!(drafts.rows()[0].title, "a rewritten");
    }

    #[test]
    fn loading_normalized_text_is_not_a_write() {
        let (_root, store) = store();
        let a = create(&store, "a\r\nb\r\n", TimeDelta::hours(1));
        let before = store.load(&a).unwrap().meta.updated_at;
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();
        // What the editor reports after loading the CRLF file.
        drafts.loaded("a\nb\n");
        drafts.edited("a\nb\n", Instant::now());

        assert_eq!(drafts.deadline(), None);
        drafts.flush().unwrap();
        assert_eq!(store.read_text(&a).unwrap(), "a\r\nb\r\n");
        assert_eq!(store.load(&a).unwrap().meta.updated_at, before);
    }

    #[test]
    fn failed_save_refuses_switch_and_archive() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::minutes(1));
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();
        drafts.edited("a edited", Instant::now());
        let meta = store.load(&a).unwrap().dir.join("meta.json");
        std::fs::write(&meta, "not json").unwrap();

        assert!(drafts.select(&b).is_err());
        assert!(drafts.archive(&a).is_err());
        assert_eq!(drafts.current_id(), Some(a.as_str()));
        assert!(drafts.deadline().is_some());
    }

    #[test]
    fn archiving_selects_the_row_below_else_above() {
        let (_root, store) = store();
        let top = create(&store, "top", TimeDelta::minutes(1));
        let middle = create(&store, "middle", TimeDelta::minutes(2));
        let bottom = create(&store, "bottom", TimeDelta::minutes(3));
        let mut drafts = Drafts::new(store.clone());

        drafts.select(&middle).unwrap();
        assert_eq!(drafts.archive(&middle).unwrap(), Some(bottom.clone()));
        assert_eq!(drafts.current_id(), None);
        drafts.select(&bottom).unwrap();
        assert_eq!(drafts.archive(&bottom).unwrap(), Some(top.clone()));
        drafts.select(&top).unwrap();
        assert_eq!(drafts.archive(&top).unwrap(), None);
        assert!(drafts.rows().is_empty());
    }

    #[test]
    fn archiving_after_an_unsaved_edit_selects_a_neighbour() {
        let (_root, store) = store();
        let top = create(&store, "top", TimeDelta::minutes(1));
        let bottom = create(&store, "bottom", TimeDelta::minutes(2));
        let mut drafts = Drafts::new(store.clone());

        drafts.select(&bottom).unwrap();
        drafts.edited("bottom edited", Instant::now());
        assert_eq!(drafts.archive(&bottom).unwrap(), Some(top.clone()));
        assert_eq!(store.read_text(&bottom).unwrap(), "bottom edited");
    }

    #[test]
    fn archiving_another_draft_keeps_the_selection() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::minutes(1));
        let b = create(&store, "b", TimeDelta::minutes(2));
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();

        assert_eq!(drafts.archive(&b).unwrap(), None);
        assert_eq!(drafts.current_id(), Some(a.as_str()));
        assert_eq!(store.load(&b).unwrap().meta.status, Status::Archived);
    }

    #[test]
    fn unarchive_restores_the_row_in_place() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::minutes(1));
        let b = create(&store, "b", TimeDelta::minutes(2));
        let c = create(&store, "c", TimeDelta::minutes(3));
        let mut drafts = Drafts::new(store);
        drafts.archive(&b).unwrap();
        assert_eq!(order(&drafts), [a.as_str(), c.as_str()]);

        drafts.unarchive(&b).unwrap();
        assert_eq!(order(&drafts), [a.as_str(), b.as_str(), c.as_str()]);
    }

    #[test]
    fn set_target_persists_without_counting_as_activity() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let before = store.load(&a).unwrap().meta.updated_at;
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();

        drafts.set_target(Target::Slack).unwrap();
        let meta = store.load(&a).unwrap().meta;
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(meta.updated_at, before);
        assert_eq!(drafts.current().unwrap().meta.target, Target::Slack);
        assert_eq!(drafts.rows()[0].draft.meta.target, Target::Slack);
        assert_eq!(meta.syntax, Some(Syntax::Markdown));
    }

    #[test]
    fn selecting_a_mrkdwn_slack_draft_rewrites_it_as_markdown() {
        let (_root, store) = store();
        let a = create(&store, "*Ship* it <https://a.b|now>", TimeDelta::hours(1));
        store
            .update_meta(&a, |meta| {
                meta.target = Target::Slack;
                meta.syntax = None;
            })
            .unwrap();
        let before = store.load(&a).unwrap().meta.updated_at;
        let mut drafts = Drafts::new(store.clone());

        let text = drafts.select(&a).unwrap();
        assert_eq!(text, "**Ship** it [now](https://a.b)");
        assert_eq!(store.read_text(&a).unwrap(), text);
        let draft = store.load(&a).unwrap();
        assert_eq!(draft.meta.syntax, Some(Syntax::Markdown));
        assert_eq!(draft.meta.updated_at, before);
        assert_eq!(
            std::fs::read_to_string(draft.dir.join(MRKDWN_BACKUP)).unwrap(),
            "*Ship* it <https://a.b|now>"
        );
        assert_eq!(drafts.current().unwrap().meta, draft.meta);
        assert_eq!(drafts.rows()[0].draft.meta, draft.meta);

        // Drafts of other targets are Markdown already.
        let b = create(&store, "*it*", TimeDelta::hours(2));
        store.update_meta(&b, |meta| meta.syntax = None).unwrap();
        assert_eq!(drafts.select(&b).unwrap(), "*it*");
        assert_eq!(store.load(&b).unwrap().meta.syntax, None);
    }

    #[test]
    fn first_launch_creates_the_sample_once() {
        let (root, store) = store();
        let home = root.path().join("home");
        let settings = Settings::default();
        let mut drafts = Drafts::new(store.clone());

        let id = drafts
            .first_launch_sample(&settings, &home)
            .unwrap()
            .unwrap();
        let sample = store.load(&id).unwrap();
        assert!(sample.meta.is_sample());
        assert_eq!(sample.meta.title, sample::TITLE);
        assert_eq!(sample.meta.target, Target::Github);
        assert_eq!(sample.meta.cwd, home);
        assert_eq!(store.read_text(&id).unwrap(), sample::TEXT);
        let session = std::fs::read_to_string(sample.meta.pi_session_file.unwrap()).unwrap();
        assert_eq!(session.lines().count(), 1);
        assert!(session.contains(r#""type":"session""#) && session.contains(r#""version":3"#));
        assert_eq!(order(&drafts), [id.as_str()]);

        // Archived or deleted, it doesn't come back by itself.
        drafts.archive(&id).unwrap();
        let mut drafts = Drafts::new(store.clone());
        assert_eq!(drafts.first_launch_sample(&settings, &home).unwrap(), None);
        std::fs::remove_dir_all(&sample.dir).unwrap();
        assert_eq!(drafts.first_launch_sample(&settings, &home).unwrap(), None);

        // On request it does, as a new draft.
        let again = drafts.create_sample(&settings, &home).unwrap();
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
            Drafts::new(archived_store)
                .first_launch_sample(&settings, home)
                .unwrap(),
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
            Drafts::new(unreadable_store)
                .first_launch_sample(&settings, home)
                .unwrap(),
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
        let id = drafts.create_sample(&settings, root.path()).unwrap();

        let file = store.load(&id).unwrap().meta.pi_session_file.unwrap();
        let session = std::fs::read_to_string(file).unwrap();
        assert!(
            session.contains(r#""modelId":"claude-sonnet-5-5""#),
            "{session}"
        );
        assert!(session.contains(r#""thinkingLevel":"low""#), "{session}");
    }

    #[test]
    fn display_title_falls_back_to_the_first_line() {
        let title = |stored: &str, text: &str| display_title(stored, || text.to_owned());
        assert_eq!(title("Release notes", "# Other"), "Release notes");
        assert_eq!(
            title("", "\n\n## **Fix** the `cache`\nmore"),
            "Fix the cache"
        );
        assert_eq!(title("", "> - [ ] 1. item"), "item");
        assert_eq!(title("", "Rename `foo_bar` to ~x"), "Rename foo_bar to ~x");
        assert_eq!(title("", "#define FOO"), "#define FOO");
        assert_eq!(title("", "  \n"), "Untitled draft");
        assert_eq!(title("", &"x".repeat(100)).len(), TITLE_MAX_CHARS);
    }

    #[test]
    fn relative_time_steps() {
        let now = Utc::now();
        let ago = |delta| relative_time(now - delta, now);
        assert_eq!(ago(TimeDelta::seconds(20)), "now");
        assert_eq!(ago(TimeDelta::minutes(14)), "14m");
        assert_eq!(ago(TimeDelta::minutes(90)), "1h");
        let weekday = (now - TimeDelta::days(2))
            .with_timezone(&Local)
            .format("%a");
        assert_eq!(ago(TimeDelta::days(2)), weekday.to_string());
    }

    #[test]
    fn tilde_abbreviates_home() {
        let home = Path::new("/Users/me");
        assert_eq!(
            tilde(Path::new("/Users/me/code/app"), Some(home)),
            "~/code/app"
        );
        assert_eq!(tilde(Path::new("/Users/me"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/tmp/x"), Some(home)), "/tmp/x");
    }
}
