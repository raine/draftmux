use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::meta::Meta;
use crate::store::Store;

/// Debounced saving of one draft's editor text to `draft.md`.
///
/// Holds no timer: the caller reports edits with [`Autosave::edited`],
/// schedules a wake-up for [`Autosave::deadline`] and calls
/// [`Autosave::poll`] then. Contract for the app:
///
/// - Call [`Autosave::flush`] before each prompt, and when switching drafts
///   or quitting, so `draft.md` holds the editor text before pi reads it.
/// - Don't report edits while the agent runs; the editor is read-only then.
/// - A save refuses with [`Conflict`] when `draft.md` no longer holds what
///   this autosave last read or wrote, so a write made outside the editor is
///   never replaced unseen. Poll [`Autosave::changed_on_disk`] to find such
///   writes.
/// - To load a changed file into the editor, call [`Autosave::adopt`] with
///   the file's text, then [`Autosave::reset`] with the editor's text, so
///   the reload isn't written back as a user change. To keep the editor's
///   text over it instead, call [`Autosave::adopt`] and then flush.
///
/// Comparing then replacing the file isn't atomic: a write landing between
/// the two is still replaced.
pub struct Autosave {
    store: Store,
    id: String,
    delay: Duration,
    /// Text the editor held when it last matched `draft.md`, which may
    /// differ from the file in line endings; `None` after a failed save,
    /// which may have written the body before failing.
    saved: Option<String>,
    /// `draft.md` as last read or written by this autosave.
    disk: String,
    pending: Option<Pending>,
}

struct Pending {
    text: String,
    due: Instant,
}

/// The save was refused because `draft.md` changed outside the editor.
#[derive(Debug)]
pub struct Conflict;

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the draft was changed outside the editor")
    }
}

impl std::error::Error for Conflict {}

impl Autosave {
    /// `saved` is the text currently in `draft.md`.
    pub fn new(store: Store, id: String, saved: String, delay: Duration) -> Self {
        Self {
            store,
            id,
            delay,
            disk: saved.clone(),
            saved: Some(saved),
            pending: None,
        }
    }

    /// Records the editor's current text and restarts the debounce.
    pub fn edited(&mut self, text: String, now: Instant) {
        self.pending = (self.saved.as_ref() != Some(&text)).then(|| Pending {
            text,
            due: now + self.delay,
        });
    }

    /// When the pending text should be saved; `None` when nothing is unsaved.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.due)
    }

    /// Saves the pending text if its deadline has passed. Returns the
    /// draft's metadata when it wrote.
    pub fn poll(&mut self, now: Instant) -> Result<Option<Meta>> {
        match self.deadline() {
            Some(due) if due <= now => self.flush(),
            _ => Ok(None),
        }
    }

    /// Saves the pending text now. Returns the draft's metadata when it
    /// wrote. On error, including [`Conflict`], the text stays pending.
    pub fn flush(&mut self) -> Result<Option<Meta>> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        if self.changed_on_disk()?.is_some() {
            return Err(Conflict.into());
        }
        let meta = match self.store.save_text(&self.id, &pending.text) {
            Ok(meta) => meta,
            Err(err) => {
                self.saved = None;
                // The body may have been written before the failure; if so it
                // is the editor's own write, not an outside change.
                if let Ok(Some(disk)) = self.read()
                    && disk == pending.text
                {
                    self.disk = disk;
                }
                return Err(err);
            }
        };
        self.disk = pending.text.clone();
        self.saved = self.pending.take().map(|pending| pending.text);
        Ok(Some(meta))
    }

    /// The editor's copy of the text known to be in `draft.md`.
    pub fn saved(&self) -> Option<&str> {
        self.saved.as_deref()
    }

    /// `draft.md`'s text when it changed since this autosave last read or
    /// wrote it. A missing file isn't a change: the next save recreates it.
    pub fn changed_on_disk(&self) -> Result<Option<String>> {
        Ok(self.read()?.filter(|disk| *disk != self.disk))
    }

    /// Takes `disk` as what `draft.md` holds, keeping any pending text.
    pub fn adopt(&mut self, disk: String) {
        self.disk = disk;
    }

    /// Treats `text` as the editor's copy of what's on disk and drops any
    /// pending save.
    pub fn reset(&mut self, text: String) {
        self.saved = Some(text);
        self.pending = None;
    }

    /// `draft.md`'s text, `None` when the file is missing.
    fn read(&self) -> Result<Option<String>> {
        match self.store.read_text(&self.id) {
            Ok(text) => Ok(Some(text)),
            Err(err)
                if err
                    .downcast_ref::<io::Error>()
                    .is_some_and(|err| err.kind() == io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{Source, SourceKind, Target};
    use crate::store::NewDraft;

    const DELAY: Duration = Duration::from_millis(500);

    fn setup() -> (tempfile::TempDir, Store, Autosave) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = store
            .create(NewDraft {
                target: Target::Slack,
                title: String::new(),
                source: Some(Source {
                    kind: SourceKind::Claude,
                    path: "/tmp/s.jsonl".into(),
                }),
                cwd: "/tmp".into(),
                pi_agent_dir: "/tmp/.pi/agent".into(),
                note: None,
                settings: Default::default(),
            })
            .unwrap();
        let autosave = Autosave::new(store.clone(), draft.id, String::new(), DELAY);
        (root, store, autosave)
    }

    /// Writes `draft.md` the way an agent outside the app would.
    fn write_outside(store: &Store, text: &str) {
        let dir = store.scan().unwrap().drafts[0].dir.clone();
        std::fs::write(dir.join("draft.md"), text).unwrap();
    }

    fn on_disk(store: &Store) -> String {
        let id = &store.scan().unwrap().drafts[0].id;
        store.read_text(id).unwrap()
    }

    #[test]
    fn saves_latest_text_once_debounce_elapses() {
        let (_root, store, mut autosave) = setup();
        let t0 = Instant::now();
        autosave.edited("H".into(), t0);
        autosave.edited("Hi".into(), t0 + Duration::from_millis(300));

        assert!(autosave.poll(t0 + DELAY).unwrap().is_none());
        assert_eq!(on_disk(&store), "");

        assert!(
            autosave
                .poll(t0 + Duration::from_millis(800))
                .unwrap()
                .is_some()
        );
        assert_eq!(on_disk(&store), "Hi");
        assert_eq!(autosave.deadline(), None);
    }

    #[test]
    fn flush_saves_before_deadline() {
        let (_root, store, mut autosave) = setup();
        autosave.edited("before prompt".into(), Instant::now());
        assert!(autosave.flush().unwrap().is_some());
        assert_eq!(on_disk(&store), "before prompt");
        assert!(autosave.flush().unwrap().is_none());
    }

    #[test]
    fn unchanged_text_and_reset_skip_writes() {
        let (_root, store, mut autosave) = setup();
        let t0 = Instant::now();
        autosave.edited("x".into(), t0);
        autosave.edited(String::new(), t0);
        assert_eq!(autosave.deadline(), None);

        write_outside(&store, "agent");
        autosave.edited("typed".into(), t0);
        autosave.adopt("agent".into());
        autosave.reset("agent".into());
        assert!(autosave.flush().unwrap().is_none());
        assert_eq!(on_disk(&store), "agent");

        autosave.edited("agent".into(), t0);
        assert_eq!(autosave.deadline(), None);

        autosave.edited("agent, edited".into(), t0);
        assert!(autosave.flush().unwrap().is_some());
        assert_eq!(on_disk(&store), "agent, edited");
    }

    #[test]
    fn outside_write_refuses_save_and_keeps_text_pending() {
        let (_root, store, mut autosave) = setup();
        let t0 = Instant::now();
        write_outside(&store, "outside");
        assert_eq!(
            autosave.changed_on_disk().unwrap().as_deref(),
            Some("outside")
        );

        autosave.edited("mine".into(), t0);
        let err = autosave.poll(t0 + DELAY).unwrap_err();
        assert!(err.is::<Conflict>());
        assert!(autosave.flush().unwrap_err().is::<Conflict>());
        assert_eq!(on_disk(&store), "outside");
        assert!(autosave.deadline().is_some());
        // Refusing doesn't take the outside text as seen.
        assert!(autosave.changed_on_disk().unwrap().is_some());
    }

    #[test]
    fn keeping_mine_overwrites_only_the_adopted_text() {
        let (_root, store, mut autosave) = setup();
        write_outside(&store, "first");
        autosave.edited("mine".into(), Instant::now());
        autosave.adopt("first".into());
        assert!(autosave.flush().unwrap().is_some());
        assert_eq!(on_disk(&store), "mine");
        assert_eq!(autosave.changed_on_disk().unwrap(), None);

        // Adopting a version that has since been replaced again still
        // conflicts.
        write_outside(&store, "second");
        autosave.edited("mine again".into(), Instant::now());
        write_outside(&store, "third");
        autosave.adopt("second".into());
        assert!(autosave.flush().unwrap_err().is::<Conflict>());
        assert_eq!(on_disk(&store), "third");
    }

    #[test]
    fn normalized_load_is_not_an_outside_change() {
        let (_root, store, _) = setup();
        let id = store.scan().unwrap().drafts[0].id.clone();
        write_outside(&store, "a\r\nb\r\n");
        let mut autosave = Autosave::new(store.clone(), id, "a\r\nb\r\n".into(), DELAY);
        // What the editor reports after loading the CRLF file.
        autosave.reset("a\nb\n".into());
        assert_eq!(autosave.changed_on_disk().unwrap(), None);

        autosave.edited("a\nb\nc\n".into(), Instant::now());
        assert!(autosave.flush().unwrap().is_some());
        assert_eq!(on_disk(&store), "a\nb\nc\n");
    }

    #[test]
    fn missing_file_is_not_a_change_and_is_recreated() {
        let (_root, store, mut autosave) = setup();
        let dir = store.scan().unwrap().drafts[0].dir.clone();
        std::fs::remove_file(dir.join("draft.md")).unwrap();
        assert_eq!(autosave.changed_on_disk().unwrap(), None);

        autosave.edited("back".into(), Instant::now());
        assert!(autosave.flush().unwrap().is_some());
        assert_eq!(on_disk(&store), "back");
    }

    #[test]
    fn failed_save_keeps_text_pending() {
        let (root, store, mut autosave) = setup();
        let dir = store.scan().unwrap().drafts[0].dir.clone();
        autosave.edited("keep me".into(), Instant::now());
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(autosave.flush().is_err());
        assert!(autosave.deadline().is_some());
        drop(root);
    }

    #[test]
    fn edit_back_after_partial_failure_still_saves() {
        let (_root, store, mut autosave) = setup();
        let dir = store.scan().unwrap().drafts[0].dir.clone();
        let meta = std::fs::read(dir.join("meta.json")).unwrap();
        std::fs::write(dir.join("meta.json"), "{ not json").unwrap();

        autosave.edited("B".into(), Instant::now());
        assert!(autosave.flush().is_err());
        assert_eq!(std::fs::read_to_string(dir.join("draft.md")).unwrap(), "B");

        std::fs::write(dir.join("meta.json"), meta).unwrap();
        // The body the failed save wrote is the app's own, not an outside
        // change.
        assert_eq!(autosave.changed_on_disk().unwrap(), None);
        autosave.edited(String::new(), Instant::now());
        assert!(autosave.flush().unwrap().is_some());
        assert_eq!(on_disk(&store), "");
    }
}
