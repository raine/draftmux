//! The drafts list and the selected draft, independent of the UI.
//!
//! Owns the order of operations that could lose or misfile text: the selected
//! draft's pending edits are flushed before it is left, archived or the app
//! quits, and a switch is refused while they can't be saved.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Datelike, Local, Utc};
use draftmux_storage::{
    Autosave, Backend, Draft, DraftSettings, FileSave, Meta, Status, Store, Syntax, Target,
    by_activity,
};

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

/// Loads draft `id` for a CLI command, reporting a missing draft by its ID.
pub fn load(store: &Store, id: &str) -> Result<Draft> {
    store.load(id).map_err(|err| {
        if err
            .downcast_ref::<io::Error>()
            .is_some_and(|err| err.kind() == io::ErrorKind::NotFound)
        {
            anyhow!("no draft with id {id}")
        } else {
            err
        }
    })
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
    /// Archived drafts, most recently active first.
    archived: Vec<Row>,
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
            archived: Vec::new(),
            current: None,
        };
        drafts.refresh();
        drafts
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn archived(&self) -> &[Row] {
        &self.archived
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
        scan.report_errors(&mut std::io::stderr()).ok();
        let title = |draft: &Draft| {
            display_title(&draft.meta.title, || {
                self.store.read_text(&draft.id).unwrap_or_default()
            })
        };
        self.rows = scan
            .active()
            .map(|draft| Row {
                title: title(draft),
                draft: draft.clone(),
            })
            .collect();
        // Archived drafts accumulate, so an unchanged one keeps its title
        // rather than having its text read again.
        let mut titled: HashMap<String, Row> = std::mem::take(&mut self.archived)
            .into_iter()
            .map(|row| (row.draft.id.clone(), row))
            .collect();
        self.archived = scan
            .drafts
            .iter()
            .filter(|draft| draft.meta.status == Status::Archived)
            .map(|draft| Row {
                title: match titled.remove(&draft.id) {
                    Some(row) if row.draft.meta == draft.meta => row.title,
                    _ => title(draft),
                },
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

    /// Puts `id`'s row where a rescan would, from `meta` and its `title`,
    /// without reading other drafts.
    fn update_row(&mut self, id: &str, meta: Meta, title: String) -> Result<()> {
        let draft = match self.rows.iter().position(|row| row.draft.id == id) {
            Some(index) => Draft {
                meta,
                ..self.rows.remove(index).draft
            },
            None => Draft {
                meta,
                ..self.store.load(id)?
            },
        };
        if let Some(current) = &mut self.current
            && current.draft.id == id
        {
            current.draft = draft.clone();
        }
        if draft.meta.status == Status::Active {
            let index = self
                .rows
                .partition_point(|row| by_activity(&row.draft, &draft).is_lt());
            self.rows.insert(index, Row { draft, title });
        }
        Ok(())
    }

    /// Makes `id` the selected draft and returns its text, rewriting a
    /// Slack draft written in mrkdwn as Markdown. Saves the previous draft
    /// first and stays on it if that fails.
    pub(crate) fn select(&mut self, id: &str) -> Result<String> {
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
    pub(crate) fn loaded(&mut self, text: &str) {
        if let Some(current) = &mut self.current {
            current.autosave.reset(text.to_owned());
        }
    }

    /// The selected draft's file text, when it was changed outside the
    /// editor since it was loaded or saved.
    pub fn changed_on_disk(&self) -> Result<Option<String>> {
        match &self.current {
            Some(current) => current.autosave.changed_on_disk(),
            None => Ok(None),
        }
    }

    /// Takes `disk` as the selected draft's file text, keeping any pending
    /// edit; see [`Autosave::adopt`].
    pub fn adopt(&mut self, disk: String) {
        if let Some(current) = &mut self.current {
            current.autosave.adopt(disk);
        }
    }

    /// Reports the editor text after a user edit.
    pub(crate) fn edited(&mut self, text: &str, now: Instant) {
        if let Some(current) = &mut self.current {
            current.autosave.edited(text.to_owned(), now);
        }
    }

    /// When the pending edit is due to be saved.
    pub fn deadline(&self) -> Option<Instant> {
        self.current.as_ref()?.autosave.deadline()
    }

    /// Saves the pending edit if it is due.
    pub(crate) fn poll(&mut self, now: Instant) -> Result<()> {
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        let meta = current.autosave.poll(now)?;
        self.saved(meta)
    }

    /// Saves the pending edit now.
    pub(crate) fn flush(&mut self) -> Result<()> {
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        let meta = current.autosave.flush()?;
        self.saved(meta)
    }

    /// Updates the selected draft's row after its autosave wrote `meta`.
    fn saved(&mut self, meta: Option<Meta>) -> Result<()> {
        let (Some(meta), Some(current)) = (meta, &self.current) else {
            return Ok(());
        };
        let id = current.draft.id.clone();
        let title = display_title(&meta.title, || {
            current.autosave.saved().unwrap_or_default().to_owned()
        });
        self.update_row(&id, meta, title)
    }

    /// Saves the selected draft's pending edit, then writes `text`, the
    /// editor's text, to the file the draft was opened from; see
    /// [`Store::save_to_file`]. `None` with no draft selected.
    pub fn save_to_file(&mut self, text: &str, overwrite: bool) -> Result<Option<FileSave>> {
        self.flush()?;
        let Some(current) = &self.current else {
            return Ok(None);
        };
        let id = current.draft.id.clone();
        let saved = self.store.save_to_file(&id, text, overwrite)?;
        if let FileSave::Saved(meta) = &saved {
            let title = display_title(&meta.title, || text.to_owned());
            self.update_row(&id, meta.as_ref().clone(), title)?;
        }
        Ok(Some(saved))
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

    /// Counts a change to `id`'s file, which now holds `text`, made outside
    /// the editor as activity.
    pub fn touched(&mut self, id: &str, text: &str) -> Result<()> {
        let meta = self
            .store
            .update_meta(id, |meta| meta.updated_at = Utc::now())?;
        let title = display_title(&meta.title, || text.to_owned());
        self.update_row(id, meta, title)
    }

    /// Records that the first draft of `id` no longer needs requesting.
    pub fn first_draft_requested(&mut self, id: &str) -> Result<()> {
        self.store
            .update_meta(id, |meta| meta.first_draft_pending = false)?;
        self.refresh();
        Ok(())
    }

    /// Changes `id`'s backend descriptor, as its agent reports what a later
    /// start needs, such as where its transcript went. Not counted as
    /// activity.
    pub fn update_backend(&mut self, id: &str, update: impl FnOnce(&mut Backend)) -> Result<()> {
        let meta = self
            .store
            .update_meta(id, |meta| update(&mut meta.backend))?;
        if let Some(current) = &mut self.current
            && current.draft.id == id
        {
            current.draft.meta = meta;
        }
        self.refresh();
        Ok(())
    }

    /// Moves `id` to a new session, `backend`, with the draft settings kept
    /// beside it.
    pub fn set_backend(
        &mut self,
        id: &str,
        backend: Backend,
        settings: DraftSettings,
    ) -> Result<()> {
        let meta = self.store.update_meta(id, |meta| {
            meta.backend = backend;
            meta.settings = settings;
        })?;
        if let Some(current) = &mut self.current
            && current.draft.id == id
        {
            current.draft.meta = meta;
        }
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

    /// The store the drafts are read from and saved to.
    pub fn store(&self) -> &Store {
        &self.store
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

    #[test]
    fn set_backend_moves_the_draft_and_its_settings_together() {
        let (_root, store) = store();
        let id = create(&store, "Text", TimeDelta::zero());
        // A pi selection model can't stay beside a Claude Code session.
        store
            .update_meta(&id, |meta| {
                meta.settings.selection_model = Some(draftmux_storage::ModelChoice {
                    provider: "anthropic".into(),
                    model_id: "claude-haiku-5-5".into(),
                    thinking: "low".into(),
                })
            })
            .unwrap();
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&id).unwrap();
        let claude = Backend::Claude(draftmux_storage::ClaudeSession {
            session_id: "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60".parse().unwrap(),
            transcript: None,
            config_dir: None,
            fork: None,
            model: None,
            effort: None,
            selection_model: None,
        });

        drafts
            .set_backend(&id, claude.clone(), DraftSettings::default())
            .unwrap();
        let meta = store.load(&id).unwrap().meta;
        assert_eq!(meta.backend, claude);
        assert_eq!(meta.settings, DraftSettings::default());
        assert_eq!(drafts.current().unwrap().meta, meta);
        assert_eq!(store.read_text(&id).unwrap(), "Text");
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
    fn a_backend_update_changes_only_that_drafts_descriptor() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::zero());
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();
        let before = store.load(&a).unwrap().meta;

        let session = |draft: &Draft| draft.meta.pi().unwrap().session_file.clone();
        let file = std::path::PathBuf::from("/tmp/s.jsonl");
        for id in [&a, &b] {
            drafts
                .update_backend(id, |backend| {
                    let Backend::Pi(pi) = backend else {
                        unreachable!()
                    };
                    pi.session_file = Some(file.clone());
                })
                .unwrap();
        }
        assert_eq!(session(drafts.current().unwrap()), Some(file.clone()));
        let after = store.load(&a).unwrap().meta;
        assert_eq!(after.updated_at, before.updated_at, "not activity");
        assert_eq!(order(&drafts), [b.as_str(), a.as_str()]);
        let row = drafts.rows().iter().find(|row| row.draft.id == b).unwrap();
        assert_eq!(session(&row.draft), Some(file));
    }

    fn archived_order(drafts: &Drafts) -> Vec<&str> {
        drafts
            .archived()
            .iter()
            .map(|row| row.draft.id.as_str())
            .collect()
    }

    #[test]
    fn archived_drafts_are_listed_newest_first_and_follow_restores() {
        let (_root, store) = store();
        let active = create(&store, "active", TimeDelta::zero());
        let old = create(&store, "old", TimeDelta::hours(2));
        let new = create(&store, "new", TimeDelta::hours(1));
        store.archive(&old).unwrap();

        let mut drafts = Drafts::new(store.clone());
        drafts.archive(&new).unwrap();
        assert_eq!(order(&drafts), [active.as_str()]);
        assert_eq!(archived_order(&drafts), [new.as_str(), old.as_str()]);
        assert_eq!(drafts.archived()[0].title, "new");

        drafts.unarchive(&new).unwrap();
        assert_eq!(order(&drafts), [active.as_str(), new.as_str()]);
        assert_eq!(archived_order(&drafts), [old.as_str()]);
    }

    #[test]
    fn archived_titles_follow_changes_on_disk() {
        let (_root, store) = store();
        let id = create(&store, "before", TimeDelta::hours(1));
        store.archive(&id).unwrap();
        let mut drafts = Drafts::new(store.clone());
        assert_eq!(drafts.archived()[0].title, "before");

        store.save_text(&id, "after").unwrap();
        drafts.refresh();
        assert_eq!(drafts.archived()[0].title, "after");
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
        drafts.touched(&a, "a rewritten").unwrap();
        assert_eq!(order(&drafts), [a.as_str(), b.as_str()]);
        assert_eq!(drafts.rows()[0].title, "a rewritten");
    }

    fn rows(drafts: &Drafts) -> Vec<(String, String, Meta)> {
        drafts
            .rows()
            .iter()
            .map(|row| {
                (
                    row.draft.id.clone(),
                    row.title.clone(),
                    row.draft.meta.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn saves_and_outside_writes_leave_other_drafts_unread() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::minutes(1));
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();
        // A rescan would drop B's row and couldn't title it.
        let dir = store.load(&b).unwrap().dir;
        std::fs::write(dir.join("meta.json"), "{").unwrap();
        std::fs::remove_file(dir.join("draft.md")).unwrap();

        let t0 = Instant::now();
        drafts.edited("a typed", t0);
        drafts.poll(t0 + AUTOSAVE_DELAY).unwrap();
        assert_eq!(order(&drafts), [a.as_str(), b.as_str()]);
        assert_eq!(drafts.rows()[0].title, "a typed");

        drafts.edited("a flushed", Instant::now());
        drafts.flush().unwrap();
        assert_eq!(drafts.rows()[0].title, "a flushed");

        drafts.touched(&a, "a by agent").unwrap();
        assert_eq!(drafts.rows()[0].title, "a by agent");
        assert_eq!(order(&drafts), [a.as_str(), b.as_str()]);
        assert_eq!(drafts.rows()[1].title, "b");
        assert_eq!(drafts.current().unwrap().meta, store.load(&a).unwrap().meta);
    }

    #[test]
    fn row_updates_match_a_rescan() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(3));
        let b = create(&store, "b", TimeDelta::hours(2));
        let c = create(&store, "c", TimeDelta::hours(1));
        create(&store, "future", -TimeDelta::days(1));
        store
            .update_meta(&b, |meta| meta.title = "Stored title".into())
            .unwrap();
        let mut drafts = Drafts::new(store.clone());

        drafts.select(&a).unwrap();
        drafts.edited("# a heading", Instant::now());
        drafts.select(&b).unwrap();
        drafts.edited("b body", Instant::now());
        drafts.flush().unwrap();
        std::fs::write(store.load(&c).unwrap().draft_path(), "- c item").unwrap();
        drafts.touched(&c, "- c item").unwrap();

        assert_eq!(rows(&drafts), rows(&Drafts::new(store.clone())));
    }

    #[test]
    fn outside_writes_follow_the_drafts_status() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::minutes(1));
        let mut drafts = Drafts::new(store.clone());

        store.archive(&a).unwrap();
        drafts.touched(&a, "a").unwrap();
        assert_eq!(order(&drafts), [b.as_str()]);

        store.unarchive(&a).unwrap();
        drafts.touched(&a, "a again").unwrap();
        assert_eq!(order(&drafts), [a.as_str(), b.as_str()]);
        assert_eq!(drafts.rows()[0].title, "a again");
    }

    #[test]
    fn row_update_breaks_activity_ties_by_id() {
        let (_root, store) = store();
        let x = create(&store, "x", TimeDelta::hours(1));
        let y = create(&store, "y", TimeDelta::hours(2));
        let z = create(&store, "z", TimeDelta::hours(3));
        assert!(x < z);
        let mut drafts = Drafts::new(store.clone());

        // Z becomes as recent as X, and sorts after it by id.
        let mut meta = store.load(&z).unwrap().meta;
        meta.updated_at = drafts.rows()[0].draft.meta.updated_at;
        drafts.update_row(&z, meta, "z".into()).unwrap();
        assert_eq!(order(&drafts), [x.as_str(), z.as_str(), y.as_str()]);
    }

    #[test]
    fn outside_write_to_the_open_draft_reloads_or_blocks_leaving() {
        let (_root, store) = store();
        let a = create(&store, "a", TimeDelta::hours(1));
        let b = create(&store, "b", TimeDelta::minutes(1));
        let path = store.load(&a).unwrap().draft_path();
        let mut drafts = Drafts::new(store.clone());
        drafts.select(&a).unwrap();
        assert_eq!(drafts.changed_on_disk().unwrap(), None);

        // Nothing pending: the outside text is loaded and isn't saved back.
        std::fs::write(&path, "a outside").unwrap();
        let disk = drafts.changed_on_disk().unwrap().unwrap();
        drafts.adopt(disk.clone());
        drafts.loaded(&disk);
        assert_eq!(drafts.deadline(), None);
        assert_eq!(drafts.changed_on_disk().unwrap(), None);

        // An edit pending: neither text is lost and A can't be left.
        drafts.edited("a mine", Instant::now());
        std::fs::write(&path, "a outside again").unwrap();
        let err = drafts.select(&b).unwrap_err();
        assert!(err.is::<draftmux_storage::Conflict>());
        assert_eq!(drafts.current_id(), Some(a.as_str()));
        assert_eq!(store.read_text(&a).unwrap(), "a outside again");
        assert!(drafts.deadline().is_some());
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
