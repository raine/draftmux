use std::time::{Duration, Instant};

use anyhow::Result;

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
/// - After reloading `draft.md` from an agent edit, call [`Autosave::reset`]
///   so the reloaded text isn't written back as a user change.
pub struct Autosave {
    store: Store,
    id: String,
    delay: Duration,
    /// Text known to be in `draft.md`; `None` after a failed save, which may
    /// have written the body before failing.
    saved: Option<String>,
    pending: Option<Pending>,
}

struct Pending {
    text: String,
    due: Instant,
}

impl Autosave {
    /// `saved` is the text currently in `draft.md`.
    pub fn new(store: Store, id: String, saved: String, delay: Duration) -> Self {
        Self {
            store,
            id,
            delay,
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

    /// Saves the pending text if its deadline has passed. Returns whether it
    /// wrote.
    pub fn poll(&mut self, now: Instant) -> Result<bool> {
        match self.deadline() {
            Some(due) if due <= now => self.flush(),
            _ => Ok(false),
        }
    }

    /// Saves the pending text now. Returns whether it wrote. On error the
    /// text stays pending.
    pub fn flush(&mut self) -> Result<bool> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(false);
        };
        if let Err(err) = self.store.save_text(&self.id, &pending.text) {
            self.saved = None;
            return Err(err);
        }
        self.saved = Some(std::mem::take(&mut pending.text));
        self.pending = None;
        Ok(true)
    }

    /// Treats `text` as what's on disk and drops any pending save.
    pub fn reset(&mut self, text: String) {
        self.saved = Some(text);
        self.pending = None;
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

        assert!(!autosave.poll(t0 + DELAY).unwrap());
        assert_eq!(on_disk(&store), "");

        assert!(autosave.poll(t0 + Duration::from_millis(800)).unwrap());
        assert_eq!(on_disk(&store), "Hi");
        assert_eq!(autosave.deadline(), None);
    }

    #[test]
    fn flush_saves_before_deadline() {
        let (_root, store, mut autosave) = setup();
        autosave.edited("before prompt".into(), Instant::now());
        assert!(autosave.flush().unwrap());
        assert_eq!(on_disk(&store), "before prompt");
        assert!(!autosave.flush().unwrap());
    }

    #[test]
    fn unchanged_text_and_reset_skip_writes() {
        let (_root, store, mut autosave) = setup();
        let t0 = Instant::now();
        autosave.edited("x".into(), t0);
        autosave.edited(String::new(), t0);
        assert_eq!(autosave.deadline(), None);

        store
            .save_text(&store.scan().unwrap().drafts[0].id, "agent")
            .unwrap();
        autosave.edited("typed".into(), t0);
        autosave.reset("agent".into());
        assert!(!autosave.flush().unwrap());
        assert_eq!(on_disk(&store), "agent");

        autosave.edited("agent".into(), t0);
        assert_eq!(autosave.deadline(), None);
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
        autosave.edited(String::new(), Instant::now());
        assert!(autosave.flush().unwrap());
        assert_eq!(on_disk(&store), "");
    }
}
