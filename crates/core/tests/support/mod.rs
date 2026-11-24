//! A scripted app for workflow tests: a store in a temporary directory, the
//! session the app keeps, a fake clock, a fake editor and the autosave timer.

#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use draftmux_core::history::{EditLog, Recorded};
use draftmux_core::outside::{Choice, Question};
use draftmux_core::review::ReviewState;
use draftmux_core::session::{Effect, Epoch, Outcome, Session};
use draftmux_core::text::normalize_line_endings;
use draftmux_storage::{History, NewDraft, Source, SourceKind, Store, Target};
use tempfile::TempDir;

pub struct Script {
    root: TempDir,
    pub store: Store,
    pub session: Session,
    pub editor: FakeEditor,
    /// The armed autosave timer: the selection it was armed in, and when it
    /// fires.
    pub timer: Option<(Epoch, Instant)>,
    /// Prompts sent to the agent.
    pub prompts: Vec<(String, Result<String, String>)>,
    /// Questions asked about outside edits, oldest first.
    pub questions: Vec<Question>,
    /// Prompts left in the composer for the user to send.
    pub offers: Vec<String>,
    /// `meta.json` as it was before [`Script::break_meta`].
    saved_meta: Option<Vec<u8>>,
    t0: DateTime<Utc>,
    i0: Instant,
    elapsed: Duration,
}

/// The editor's text, normalized the way the real buffer normalizes it.
#[derive(Default)]
pub struct FakeEditor {
    pub text: String,
}

impl FakeEditor {
    fn load(&mut self, text: &str) {
        self.text = normalize_line_endings(text);
    }
}

impl Script {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        Self {
            root,
            session: Session::new(store.clone()),
            store,
            editor: FakeEditor::default(),
            timer: None,
            prompts: Vec::new(),
            questions: Vec::new(),
            offers: Vec::new(),
            saved_meta: None,
            t0: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            i0: Instant::now(),
            elapsed: Duration::zero(),
        }
    }

    /// The fake clock, for history timestamps.
    pub fn at(&self) -> DateTime<Utc> {
        self.t0 + self.elapsed
    }

    /// The fake clock, for the autosave debounce.
    pub fn now(&self) -> Instant {
        self.i0 + self.elapsed.to_std().unwrap()
    }

    /// Lets `by` pass, firing the autosave timer whenever it comes due.
    pub fn advance(&mut self, by: Duration) {
        let end = self.elapsed + by;
        while let Some((epoch, due)) = self.timer
            && due <= self.i0 + end.to_std().unwrap()
        {
            self.timer = None;
            self.elapsed = Duration::from_std(due - self.i0).unwrap().max(self.elapsed);
            let outcome = self.session.wake(epoch, self.now());
            self.apply(outcome);
        }
        self.elapsed = end;
    }

    /// Carries out `outcome` as the app does: the effects in order, then the
    /// autosave timer armed for its deadline.
    pub fn apply(&mut self, outcome: Outcome) {
        for effect in outcome.effects {
            match effect {
                Effect::LoadText { epoch, text } | Effect::ReplaceText { epoch, text } => {
                    self.editor.load(&text);
                    self.session.loaded(epoch, &self.editor.text);
                }
                Effect::Prompt { text, saved } => self.prompts.push((text, saved)),
                Effect::AskOutsideEdit(question) => self.questions.push(question),
                Effect::Offer(text) => self.offers.push(text),
                Effect::ReplaceRange {
                    epoch,
                    revision,
                    range,
                    text,
                } => {
                    if self.session.is_current(epoch, revision) {
                        let mut edited = self.editor.text.clone();
                        edited.replace_range(range, &text);
                        self.type_text(&edited);
                    }
                }
                Effect::ReplaceAll {
                    epoch,
                    revision,
                    text,
                } => {
                    if self.session.is_current(epoch, revision) {
                        self.type_text(&text);
                    }
                }
            }
        }
        if let Some(deadline) = outcome.deadline {
            self.timer = Some((self.session.epoch(), deadline));
        }
        // The review follows every change, as the app's does.
        self.session.sync_review();
    }

    pub fn review(&self) -> &ReviewState {
        self.session.review()
    }

    /// The user selects change `ix` in the review, or none.
    pub fn select_change(&mut self, ix: Option<usize>) {
        self.session.review_mut().select(ix);
    }

    /// The user keeps the selected change, or un-keeps it if it was kept.
    /// Keeping moves on to the next change not kept, as the review bar
    /// does.
    pub fn keep_change(&mut self) -> anyhow::Result<()> {
        let at = self.review().selected().expect("a selected change");
        let kept = !self.review().is_kept(at);
        let saved = self.session.keep(at, kept);
        if let Some(next) = kept.then(|| self.review().next_unkept(at)).flatten() {
            self.select_change(Some(next));
        }
        saved
    }

    /// The user reverts the selected change.
    pub fn revert_change(&mut self) -> anyhow::Result<()> {
        let at = self.review().selected().expect("a selected change");
        let outcome = self.session.revert(at)?;
        self.apply(outcome);
        Ok(())
    }

    /// The user picks "Show Changes Since" edit `since`.
    pub fn compare_since(&mut self, since: usize) {
        self.session.review_mut().compare_since(since);
        self.session.sync_review();
    }

    /// The selected draft's loaded log.
    pub fn log(&self) -> &EditLog {
        self.session.log()
    }

    /// The selected draft's id.
    pub fn selected(&self) -> Option<String> {
        self.session.drafts().current_id().map(str::to_owned)
    }

    /// The user types: the editor's text becomes `text`.
    pub fn type_text(&mut self, text: &str) {
        self.editor.text = text.to_owned();
        let outcome = self.session.edited(text.to_owned(), self.now());
        self.apply(outcome);
    }

    /// The window loses focus, which saves the pending edit.
    pub fn leave_window(&mut self) -> anyhow::Result<()> {
        self.session.flush()
    }

    /// Creates a draft holding `text`, as `draftmux open` does, and lists
    /// it as the app does on its next refresh. Returns its id.
    pub fn draft(&mut self, text: &str) -> String {
        self.create(text, None)
    }

    /// Creates an empty draft handed off from an agent session, as
    /// `draftmux open` without `--draft-file` does: its first draft is still
    /// to be requested. Returns its id.
    pub fn handed_off(&mut self) -> String {
        self.create(
            "",
            Some(Source {
                kind: SourceKind::Pi,
                path: "/home/u/.pi/sessions/source.jsonl".into(),
            }),
        )
    }

    /// Creates a draft started blank in the app, as New Draft does, and
    /// lists it. Returns its id.
    pub fn blank(&mut self) -> String {
        let id = self
            .store
            .create_blank(new_draft("", None), |draft| Ok(draft.meta.backend.clone()))
            .unwrap()
            .id;
        self.session.drafts_mut().refresh();
        id
    }

    /// Selects draft `id` as the app does, then deletes the draft left if
    /// it is a blank draft left empty. Returns whether it was deleted.
    pub fn leave_for(&mut self, id: &str) -> bool {
        let left = self.selected();
        self.select(id);
        left.is_some_and(|left| self.session.discard(&left).unwrap())
    }

    /// Whether draft `id` is still in the store.
    pub fn exists(&self, id: &str) -> bool {
        self.root.path().join(id).exists()
    }

    fn create(&mut self, text: &str, source: Option<Source>) -> String {
        let id = self.store.create(new_draft(text, source)).unwrap().id;
        self.session.drafts_mut().refresh();
        id
    }

    /// Selects draft `id`: loads its text into the editor.
    pub fn select(&mut self, id: &str) {
        self.try_select(id).unwrap();
    }

    /// Selects draft `id`, unless the draft being left can't be saved.
    pub fn try_select(&mut self, id: &str) -> anyhow::Result<()> {
        let outcome = self.session.select(id)?;
        self.timer = None;
        self.apply(outcome);
        Ok(())
    }

    pub fn deselect(&mut self) {
        let outcome = self.session.deselect();
        self.timer = None;
        self.apply(outcome);
    }

    /// The user sends `message` to the selected draft's agent, which starts
    /// from the saved text.
    pub fn send(&mut self, message: &str) {
        let outcome = self.session.send(message.to_owned(), message);
        self.apply(outcome);
    }

    /// The chat reports that draft `id`'s agent started or finished a run.
    pub fn running(&mut self, id: &str, running: bool) {
        self.session.agent_running(id, running);
    }

    /// Draft `id`'s agent is ready to write its first draft.
    pub fn first_draft_ready(&mut self, id: &str) {
        let outcome = self
            .session
            .first_draft_ready(id, "Write the first draft".to_owned());
        self.apply(outcome);
    }

    /// The chat reports that draft `id`'s agent accepted a prompt.
    pub fn accepted(&mut self, id: &str) {
        self.session.prompt_accepted(id);
    }

    /// The app starts again after quitting, and draft `id` is selected.
    pub fn restart_with(&mut self, id: &str) {
        self.crash();
        self.session.drafts_mut().refresh();
        self.select(id);
    }

    /// The agent writes `text` to draft `id`'s file, and the app hasn't
    /// seen it yet.
    pub fn agent_saves(&self, id: &str, text: &str) {
        std::fs::write(self.draft_path(id), text).unwrap();
    }

    /// The app sees the agent's write to draft `id`: it reads the file and
    /// reports it to the session.
    pub fn observe_write(&mut self, id: &str) -> anyhow::Result<Recorded> {
        let text = std::fs::read_to_string(self.draft_path(id)).unwrap();
        let (recorded, outcome) = self.session.agent_wrote(id, &text, self.at());
        self.apply(outcome);
        recorded
    }

    /// The agent writes `text` to draft `id`, and the app sees it.
    pub fn agent_writes(&mut self, id: &str, text: &str) -> anyhow::Result<Recorded> {
        self.agent_saves(id, text);
        self.observe_write(id)
    }

    /// Archives draft `id`, selecting nothing if it was selected.
    pub fn archive(&mut self, id: &str) {
        self.try_archive(id).unwrap();
    }

    /// Archives draft `id` as the sidebar does, unless it is selected and
    /// can't be saved.
    pub fn try_archive(&mut self, id: &str) -> anyhow::Result<()> {
        let was_current = self.selected().as_deref() == Some(id);
        self.session.archive(id)?;
        if was_current {
            self.deselect();
        }
        Ok(())
    }

    /// Changes draft `id`'s file the way a program outside the app would.
    pub fn write_outside(&self, id: &str, text: &str) {
        std::fs::write(self.draft_path(id), text).unwrap();
    }

    /// The app's periodic look at the selected draft's file.
    pub fn check_outside(&mut self) {
        let outcome = self.session.check_outside();
        self.apply(outcome);
    }

    /// The user answers the oldest open question with `choice`.
    pub fn answer(&mut self, choice: Choice) {
        let question = self.questions.remove(0);
        self.answer_with(question, choice);
    }

    /// The user answers `question` with `choice`.
    pub fn answer_with(&mut self, question: Question, choice: Choice) {
        let outcome = self.session.answer(question, choice);
        self.apply(outcome);
    }

    /// Makes saving draft `id` fail after its text is written, by making
    /// its `meta.json` unreadable.
    pub fn break_meta(&mut self, id: &str) {
        self.saved_meta = Some(std::fs::read(self.meta_path(id)).unwrap());
        std::fs::write(self.meta_path(id), "{").unwrap();
    }

    pub fn fix_meta(&mut self, id: &str) {
        let meta = self.saved_meta.take().expect("a broken meta.json");
        std::fs::write(self.meta_path(id), meta).unwrap();
    }

    /// Makes saving draft `id`'s history fail by putting a directory where
    /// the file goes. Reading it fails too, so break it after it's loaded.
    pub fn break_history_save(&self, id: &str) {
        let path = self.history_path(id);
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("blocker"), "").unwrap();
    }

    pub fn fix_history_save(&self, id: &str) {
        std::fs::remove_dir_all(self.history_path(id)).unwrap();
    }

    /// Makes draft `id`'s history unreadable.
    pub fn corrupt_history(&self, id: &str) {
        std::fs::write(self.history_path(id), "{not json").unwrap();
    }

    /// Drops everything the app holds in memory and reopens the same store.
    pub fn crash(&mut self) {
        self.store = Store::new(self.root.path());
        self.session = Session::new(self.store.clone());
        self.editor = FakeEditor::default();
        self.timer = None;
        self.questions.clear();
        self.offers.clear();
    }

    pub fn text(&self, id: &str) -> String {
        self.store.read_text(id).unwrap()
    }

    pub fn history(&self, id: &str) -> History {
        self.store.history(id).unwrap()
    }

    /// The history file's bytes, or `None` while there is none.
    pub fn history_bytes(&self, id: &str) -> Option<Vec<u8>> {
        std::fs::read(self.history_path(id)).ok()
    }

    fn draft_path(&self, id: &str) -> PathBuf {
        self.root.path().join(id).join("draft.md")
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.root.path().join(id).join("meta.json")
    }

    fn history_path(&self, id: &str) -> PathBuf {
        self.root.path().join(id).join("history.json")
    }
}

fn new_draft(text: &str, source: Option<Source>) -> NewDraft {
    NewDraft {
        target: Target::Github,
        title: String::new(),
        source,
        cwd: "/home/u/code/app".into(),
        backend: draftmux_storage::Backend::pi("/tmp/.pi/agent"),
        note: None,
        summary: None,
        settings: Default::default(),
        text: text.to_owned(),
        file: None,
    }
}
