//! Agent turns and each draft's history of the edits they made.
//!
//! A turn starts when the user sends a message and remembers the draft as
//! it was. The turn's first write that changes the draft adds one [`Edit`]
//! to the draft's saved [`History`]; later writes in the same turn belong to
//! that edit.

use std::collections::{HashMap, HashSet};
use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use draftmux_storage::{Edit, History, Store};

use crate::text::normalize_line_endings;

/// The agent turn a draft's next edit belongs to: what the draft was when
/// the user asked for it, and what they asked.
pub struct Turn {
    before: String,
    prompt: String,
    /// The turn's first write was recorded; later ones belong to the same
    /// edit.
    recorded: bool,
}

impl Turn {
    /// `message` is the user's own message, without the selection attached
    /// to it; the turn's prompt is its first non-empty line.
    pub fn new(before: String, message: &str) -> Self {
        Self {
            before,
            prompt: message
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .to_owned(),
            recorded: false,
        }
    }
}

/// Turns of every draft whose agent may write next.
#[derive(Default)]
pub struct Turns(HashMap<String, Turn>);

/// What [`Turns::agent_wrote`] did with a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// The write added nothing to the history: there is no turn, the turn
    /// was already recorded, the write changed nothing but line endings, or
    /// it was the first draft of an empty draft.
    Ignored,
    /// The write's turn was added to the saved history as a new edit.
    Added,
}

impl Turns {
    /// The user asked draft `id`'s agent for `message` while the draft read
    /// `before`.
    pub fn started(&mut self, id: &str, before: String, message: &str) {
        self.0.insert(id.to_owned(), Turn::new(before, message));
    }

    /// Records the edit an agent turn made with its first write to draft
    /// `id`, which now reads `text`. `log` is the draft's loaded log while
    /// it is selected; it gets the new edit and starts comparing from it.
    ///
    /// The turn counts as recorded only once the history is saved, so after
    /// an error its next write tries again.
    pub fn agent_wrote(
        &mut self,
        store: &Store,
        id: &str,
        text: &str,
        at: DateTime<Utc>,
        log: Option<&mut EditLog>,
    ) -> Result<Recorded> {
        let Some(turn) = self.0.get_mut(id) else {
            return Ok(Recorded::Ignored);
        };
        // An empty draft's first draft isn't an edit to compare with.
        if turn.recorded || same_lines(&turn.before, text) || turn.before.trim().is_empty() {
            return Ok(Recorded::Ignored);
        }
        let edit = Edit {
            before: turn.before.clone(),
            at,
            prompt: turn.prompt.clone(),
        };
        add_edit(store, id, edit, log)?;
        turn.recorded = true;
        Ok(Recorded::Added)
    }
}

/// Records the edit that took draft `id` from `edit.before` to `text`
/// outside an agent turn, such as an option the agent offered for a
/// selection. `log` is as for [`Turns::agent_wrote`].
pub fn record_edit(
    store: &Store,
    id: &str,
    edit: Edit,
    text: &str,
    log: Option<&mut EditLog>,
) -> Result<Recorded> {
    if same_lines(&edit.before, text) {
        return Ok(Recorded::Ignored);
    }
    add_edit(store, id, edit, log)?;
    Ok(Recorded::Added)
}

/// Saves `edit` in draft `id`'s history and has `log`, when given, compare
/// from it.
fn add_edit(store: &Store, id: &str, edit: Edit, log: Option<&mut EditLog>) -> Result<()> {
    let history = save_edit(store, id, log.as_deref(), edit)?;
    if let Some(log) = log {
        if log.unreadable {
            *log = EditLog::new(history);
        } else {
            log.since = history.edits.len() - 1;
            log.history = history;
        }
    }
    Ok(())
}

/// `a` and `b` read the same once their line endings are normalized, as the
/// editor shows them.
fn same_lines(a: &str, b: &str) -> bool {
    a == b || normalize_line_endings(a) == normalize_line_endings(b)
}

/// Adds `edit` to draft `id`'s saved history and returns the history.
/// `log` is the draft's loaded history while it is selected; if that
/// couldn't be read, the saved history is read again rather than replaced.
fn save_edit(store: &Store, id: &str, log: Option<&EditLog>, edit: Edit) -> Result<History> {
    let mut history = match log {
        Some(log) if !log.unreadable => log.history.clone(),
        _ => store.history(id)?,
    };
    history.edits.push(edit);
    store.save_history(id, &history)?;
    Ok(history)
}

/// A draft's edit history, the changes kept in it, and the edit a
/// comparison starts before.
#[derive(Default)]
pub struct EditLog {
    history: History,
    kept: HashSet<String>,
    /// The edit the comparison starts before.
    since: usize,
    /// The saved history couldn't be read, so `history` is empty and must
    /// not be saved over it.
    unreadable: bool,
}

impl EditLog {
    pub fn load(store: &Store, id: &str) -> Self {
        match store.history(id) {
            Ok(history) => Self::new(history),
            Err(err) => {
                eprintln!("draftmux: reading the edit history: {err:#}");
                Self {
                    unreadable: true,
                    ..Self::default()
                }
            }
        }
    }

    /// The log of `history`, comparing from its newest edit.
    pub fn new(history: History) -> Self {
        Self {
            kept: history.kept.iter().cloned().collect(),
            since: history.edits.len().saturating_sub(1),
            history,
            ..Self::default()
        }
    }

    /// Saves the history as draft `id`'s, unless the saved one couldn't be
    /// read.
    pub fn save(&self, store: &Store, id: &str) -> Result<()> {
        if self.unreadable {
            bail!("the saved edit history couldn't be read");
        }
        store.save_history(id, &self.history)
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// The agent's edits, oldest first.
    pub fn edits(&self) -> &[Edit] {
        &self.history.edits
    }

    /// The edit the comparison starts before.
    pub fn since(&self) -> usize {
        self.since
    }

    pub fn set_since(&mut self, since: usize) {
        self.since = since;
    }

    pub fn is_kept(&self, key: &ChangeKey) -> bool {
        self.kept.contains(&key.to_string())
    }

    pub fn keep(&mut self, key: &ChangeKey, kept: bool) {
        let key = key.to_string();
        if kept {
            if self.kept.insert(key.clone()) {
                self.history.kept.push(key);
            }
        } else if self.kept.remove(&key) {
            self.history.kept.retain(|other| *other != key);
        }
    }

    /// The range menu's name for the comparison, counted from the newest
    /// edit back.
    pub fn range_name(&self) -> String {
        let count = self.history.edits.len();
        if self.since + 1 >= count {
            "Last edit".to_owned()
        } else if self.since == 0 {
            format!("All {count} edits")
        } else {
            format!("Last {} edits", count - self.since)
        }
    }
}

/// Names a change in a comparison by where it starts in the text before
/// edit `since`, which never changes, so typing elsewhere leaves the name
/// alone while identical changes elsewhere get their own. Its [`Display`]
/// form is what [`History::kept`] stores.
///
/// [`Display`]: fmt::Display
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeKey<'a> {
    /// The edit the comparison starts before.
    pub since: usize,
    /// Where the change starts in the text before edit `since`.
    pub old_start: usize,
    pub removed: &'a str,
    pub added: &'a str,
}

impl fmt::Display for ChangeKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\0{}\0{}\0{}",
            self.since, self.old_start, self.removed, self.added
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_prompts_are_the_users_first_line() {
        let turn = Turn::new("before".into(), "\nTighten the summary\nand more");
        assert_eq!(turn.prompt, "Tighten the summary");
    }

    #[test]
    fn change_keys_keep_their_stored_format() {
        let key = ChangeKey {
            since: 2,
            old_start: 17,
            removed: "the cache",
            added: "a cache",
        };
        assert_eq!(
            key.to_string().as_bytes(),
            b"2\x0017\x00the cache\x00a cache"
        );
    }

    fn edit() -> Edit {
        Edit {
            before: String::new(),
            at: DateTime::UNIX_EPOCH,
            prompt: String::new(),
        }
    }

    fn draft(store: &Store) -> String {
        store
            .create(draftmux_storage::NewDraft {
                target: draftmux_storage::Target::Slack,
                title: String::new(),
                source: None,
                cwd: "/home/u/code/app".into(),
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

    #[test]
    fn unreadable_history_is_never_saved_over() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = draft(&store);
        let path = root.path().join(&id).join("history.json");
        std::fs::write(&path, "{not json").unwrap();

        let mut log = EditLog::load(&store, &id);
        assert!(log.unreadable);
        log.history.kept.push("key".into());
        assert!(log.save(&store, &id).is_err());
        assert!(save_edit(&store, &id, Some(&log), edit()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");

        // Once the file reads again, the edit is added to what it holds.
        let mut saved = History::default();
        saved.edits.push(edit());
        store.save_history(&id, &saved).unwrap();
        let history = save_edit(&store, &id, Some(&log), edit()).unwrap();
        assert_eq!(history.edits.len(), 2);
        assert_eq!(store.history(&id).unwrap().edits.len(), 2);
    }

    #[test]
    fn applied_options_are_recorded_as_edits() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let id = draft(&store);
        let mut log = EditLog::load(&store, &id);
        let option = |before: &str| Edit {
            before: before.into(),
            prompt: "Alternatives".into(),
            ..edit()
        };

        let recorded = record_edit(
            &store,
            &id,
            option("Hi there\n"),
            "Hi friend\n",
            Some(&mut log),
        );
        assert_eq!(recorded.unwrap(), Recorded::Added);
        assert_eq!(log.edits().len(), 1);
        assert_eq!(log.since(), 0);
        let saved = store.history(&id).unwrap();
        assert_eq!(saved.edits[0].before, "Hi there\n");
        assert_eq!(saved.edits[0].prompt, "Alternatives");

        // An option that only changes line endings isn't an edit.
        let recorded = record_edit(&store, &id, option("a\r\n"), "a\n", Some(&mut log));
        assert_eq!(recorded.unwrap(), Recorded::Ignored);
        assert_eq!(store.history(&id).unwrap().edits.len(), 1);
    }

    #[test]
    fn range_names_count_back_from_the_newest_edit() {
        let mut log = EditLog {
            history: History {
                edits: vec![edit(), edit(), edit()],
                kept: Vec::new(),
            },
            ..EditLog::default()
        };
        log.since = 2;
        assert_eq!(log.range_name(), "Last edit");
        log.since = 1;
        assert_eq!(log.range_name(), "Last 2 edits");
        log.since = 0;
        assert_eq!(log.range_name(), "All 3 edits");
    }
}
