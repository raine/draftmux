//! The editing session: which draft is selected, the text the editor holds,
//! when its pending edit is saved, how a failed save is reported, how
//! changes to the draft's file from outside the editor reach it, what the
//! drafts' agents are doing to them, and the review of what they changed.
//!
//! The app reports what happens (a selection, an edit, its autosave timer
//! firing, a prompt) and carries out the returned [`Outcome`]: the effects in
//! order, then its autosave timer rearmed from [`Outcome::deadline`].

use std::collections::HashSet;
use std::ops::Range;
use std::time::Instant;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use draftmux_storage::{Conflict, Edit, Store};

use crate::drafts::Drafts;
use crate::history::{EditLog, Recorded, Turns};
use crate::outside::{Choice, Question};
use crate::review::ReviewState;

/// Identifies one stay on a selection. A new one starts on every select and
/// deselect, so a load or timer meant for an earlier one is told apart even
/// when the same draft is selected again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Epoch(u64);

/// What the app does after a call.
#[derive(Debug, Default)]
pub struct Outcome {
    pub effects: Vec<Effect>,
    /// When the pending edit is due to be saved. While it is set, the app
    /// arms its autosave timer for it and then calls [`Session::wake`].
    pub deadline: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Show `text` in the editor, then report what it holds with
    /// [`Session::loaded`].
    LoadText { epoch: Epoch, text: String },
    /// Replace the editor's text with `text` as one edit the user can undo,
    /// then report what it holds with [`Session::loaded`]. The file already
    /// holds `text`.
    ReplaceText { epoch: Epoch, text: String },
    /// Ask the user which version to keep, and report the answer with
    /// [`Session::answer`].
    AskOutsideEdit(Question),
    /// Send `text` to the agent. `saved` is the draft text the agent
    /// starts from, or why it couldn't be saved first.
    Prompt {
        text: String,
        saved: Result<String, String>,
    },
    /// Leave `text` in the composer for the user to send.
    Offer(String),
    /// Replace `range` of the editor's text with `text` as one edit the
    /// user can undo, if the editor still holds the text of `epoch` and
    /// `revision` ([`Session::is_current`]).
    ReplaceRange {
        epoch: Epoch,
        revision: u64,
        range: Range<usize>,
        text: String,
    },
    /// Replace the editor's whole text with `text` as one edit the user can
    /// undo, keeping the selection where the text around it is unchanged,
    /// if the editor still holds the text of `epoch` and `revision`.
    ReplaceAll {
        epoch: Epoch,
        revision: u64,
        text: String,
    },
}

pub struct Session {
    drafts: Drafts,
    /// Each draft's latest agent turn, until its first write is recorded as
    /// an edit.
    turns: Turns,
    /// The selected draft's edit history and its review.
    review: ReviewState,
    /// Why the selected draft's last save failed; cleared by the next save.
    save_error: Option<String>,
    epoch: Epoch,
    /// The text the editor holds, as last reported.
    text: String,
    /// Counts the reports of the editor's text, so a change worked out from
    /// one text isn't applied to another.
    revision: u64,
    /// A [`Question`] is open. Only one is asked at a time.
    asking: bool,
    /// Why the autosave timer leaves the pending edit alone; see [`Held`].
    held: Option<Held>,
    /// The drafts whose agent is running, as last reported.
    running: HashSet<String>,
    /// The drafts whose first-draft prompt was sent and whose acceptance
    /// hasn't been persisted. Not persisted: after a restart the first
    /// draft of a draft still marked pending is requested again.
    requested: HashSet<String>,
    /// Accepted requests whose pending flag still needs to be saved.
    accepted: HashSet<String>,
    /// The drafts whose agent accepted a prompt since the app started.
    prompted: HashSet<String>,
}

/// The pending edit isn't saved by the autosave timer because the file
/// changed outside the editor. Nothing retries the save: what happens to the
/// edit is the user's answer, and a flush still refuses to replace the file.
/// While a question is open the timer does nothing either.
#[derive(Debug)]
enum Held {
    /// A save found the change. The next check asks about it.
    Found,
    /// The user closed the question about the file holding `disk` without
    /// choosing, while the editor held `text`. It isn't asked again until
    /// there is something new to ask about: the file changes again, or an
    /// edit made since is due to be saved.
    Dismissed { disk: String, text: String },
}

impl Session {
    pub fn new(store: Store) -> Self {
        Self {
            drafts: Drafts::new(store),
            turns: Turns::default(),
            review: ReviewState::default(),
            save_error: None,
            epoch: Epoch::default(),
            text: String::new(),
            revision: 0,
            asking: false,
            held: None,
            running: HashSet::new(),
            requested: HashSet::new(),
            accepted: HashSet::new(),
            prompted: HashSet::new(),
        }
    }

    /// The drafts list and the selected draft.
    pub fn drafts(&self) -> &Drafts {
        &self.drafts
    }

    /// The drafts list, for changes that leave the selection and its
    /// pending edit alone. Selecting, saving and archiving go through the
    /// session.
    pub fn drafts_mut(&mut self) -> &mut Drafts {
        &mut self.drafts
    }

    pub fn turns_mut(&mut self) -> &mut Turns {
        &mut self.turns
    }

    /// The selected draft's edit history, loaded when it was selected.
    pub fn log(&self) -> &EditLog {
        self.review.log()
    }

    /// The review of the selected draft's edits.
    pub fn review(&self) -> &ReviewState {
        &self.review
    }

    /// Compares the session's reported editor text with the review baseline.
    pub fn sync_review(&mut self) -> Option<std::rc::Rc<crate::review::Compared>> {
        self.review.compare(&self.text)
    }

    /// The review, for comparing and selecting changes. Keeping and
    /// reverting go through the session.
    pub fn review_mut(&mut self) -> &mut ReviewState {
        &mut self.review
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// The text the editor holds, as last reported by
    /// [`edited`](Self::edited) or [`loaded`](Self::loaded).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Changes with every report of the editor's text.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the editor still holds the text it held in `epoch` at
    /// `revision`.
    pub fn is_current(&self, epoch: Epoch, revision: u64) -> bool {
        epoch == self.epoch && revision == self.revision
    }

    /// Why the selected draft's last save failed. Autosave conflicts wait
    /// for the outside-edit question; an explicit flush reports them.
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Selects `id` and asks for its text to be loaded. Saves the previous
    /// draft first and stays on it if that fails.
    pub fn select(&mut self, id: &str) -> Result<Outcome> {
        let text = self.drafts.select(id)?;
        self.save_error = None;
        self.asking = false;
        self.held = None;
        self.epoch.0 += 1;
        self.review = ReviewState::new(EditLog::load(self.drafts.store(), id));
        Ok(self.load(text))
    }

    /// Empties the editor after the selected draft left the list.
    pub fn deselect(&mut self) -> Outcome {
        self.save_error = None;
        self.asking = false;
        self.held = None;
        self.epoch.0 += 1;
        self.review = ReviewState::default();
        self.load(String::new())
    }

    fn load(&mut self, text: String) -> Outcome {
        self.text.clone_from(&text);
        Outcome {
            effects: vec![Effect::LoadText {
                epoch: self.epoch,
                text,
            }],
            deadline: None,
        }
    }

    /// Records the text the editor holds after loading the selected
    /// draft's text, which may differ from the file in line endings, so
    /// loading isn't saved as an edit. Ignored for an earlier selection.
    pub fn loaded(&mut self, epoch: Epoch, text: &str) {
        if epoch != self.epoch {
            return;
        }
        self.drafts.loaded(text);
        text.clone_into(&mut self.text);
        self.revision += 1;
    }

    /// Reports the editor text after a user edit, and restarts the
    /// debounce.
    pub fn edited(&mut self, text: String, now: Instant) -> Outcome {
        self.drafts.edited(&text, now);
        self.text = text;
        self.revision += 1;
        self.due()
    }

    /// The autosave timer armed during `epoch` fired: saves the pending
    /// edit if it is due. A failed save keeps it pending and is retried
    /// after another delay, unless the file changed outside the editor:
    /// then the edit is [`Held`] until the user is asked.
    pub fn wake(&mut self, epoch: Epoch, now: Instant) -> Outcome {
        if epoch != self.epoch || self.asking {
            return Outcome::default();
        }
        if let Some(Held::Dismissed { text, .. }) = &self.held
            && *text == self.text
        {
            return Outcome::default();
        }
        match self.drafts.poll(now) {
            Ok(()) => self.saved(),
            Err(err) if err.is::<Conflict>() => {
                // A dismissed question is asked again: the edit made since
                // would be lost to it otherwise.
                self.held = Some(Held::Found);
                self.save_error = None;
                return Outcome::default();
            }
            Err(err) => {
                self.save_failed(&err);
                self.drafts.edited(&self.text, now);
            }
        }
        self.due()
    }

    /// Saves the pending edit now, as the window is left or closed, the app
    /// quits, or before a prompt. The edit stays pending on failure, so a
    /// close or quit waiting on this should be refused.
    pub fn flush(&mut self) -> Result<()> {
        let result = self.drafts.flush();
        match &result {
            Ok(()) => self.saved(),
            Err(err) => self.save_failed(err),
        }
        result
    }

    /// Nothing is pending or the pending edit was saved.
    fn saved(&mut self) {
        self.save_error = None;
        if self.drafts.deadline().is_none() {
            self.held = None;
        }
    }

    fn save_failed(&mut self, err: &anyhow::Error) {
        eprintln!("draftmux: saving draft: {err:#}");
        self.save_error = Some(format!("Couldn't save: {err:#}"));
    }

    /// Sends `prompt`, whose user-written part is `message`, to the selected
    /// draft's agent. The agent reads the draft from disk, so it is saved
    /// first, and the turn starts from the saved text only if that worked.
    pub fn send(&mut self, prompt: String, message: &str) -> Outcome {
        let saved = self
            .flush()
            .map(|()| self.text.clone())
            .map_err(|err| format!("{err:#}"));
        if let (Ok(before), Some(id)) = (&saved, self.drafts.current_id()) {
            self.turns.started(id, before.clone(), message);
        }
        Outcome {
            effects: vec![Effect::Prompt {
                text: prompt,
                saved,
            }],
            deadline: self.drafts.deadline(),
        }
    }

    /// Archives `id`; see [`Drafts::archive`]. The selected draft is saved
    /// first and stays selected if that fails.
    pub fn archive(&mut self, id: &str) -> Result<Option<String>> {
        self.drafts.archive(id)
    }

    /// Whether active draft `id` is a blank draft left empty
    /// ([`crate::drafts::left_empty`]), with its agent idle and never
    /// prompted. The selected draft is judged by the editor's text.
    pub fn discardable(&self, id: &str) -> bool {
        if self.running.contains(id) || self.prompted.contains(id) {
            return false;
        }
        let drafts = &self.drafts;
        let Some(draft) = drafts.rows().iter().find(|row| row.draft.id == id) else {
            return false;
        };
        let text = if drafts.current_id() == Some(id) {
            self.text.clone()
        } else {
            match drafts.store().read_text(id) {
                Ok(text) => text,
                Err(_) => return false,
            }
        };
        crate::drafts::left_empty(drafts.store(), &draft.draft, &text)
    }

    /// Deletes `id` if it is [`discardable`](Self::discardable), and
    /// returns whether it did. The app stops the draft's agent first. A
    /// selected draft is deleted without saving and the selection cleared,
    /// as when the app quits.
    pub fn discard(&mut self, id: &str) -> Result<bool> {
        if !self.discardable(id) {
            return Ok(false);
        }
        let current = self.drafts.current_id() == Some(id);
        self.drafts.delete(id)?;
        if current {
            self.deselect();
        }
        Ok(true)
    }

    /// Picks up a change to the selected draft's file made outside the
    /// editor, counting it as activity. With no edit pending, the editor
    /// takes the new text; otherwise the user is asked which version to
    /// keep. Nothing is checked while a question is open, and a dismissed
    /// question isn't asked again while the file still holds what it was
    /// about. A [`Held`] edit is saved again once the file is back to what
    /// the editor last read or wrote.
    pub fn check_outside(&mut self) -> Outcome {
        if self.asking {
            return Outcome::default();
        }
        let disk = match self.drafts.changed_on_disk() {
            Ok(Some(disk)) => disk,
            Ok(None) if self.held.take().is_some() => return self.due(),
            _ => return Outcome::default(),
        };
        if self.drafts.deadline().is_none() {
            self.touched(&disk);
            return self.replace(disk);
        }
        if let Some(Held::Dismissed {
            disk: dismissed, ..
        }) = &self.held
            && *dismissed == disk
        {
            return Outcome::default();
        }
        let Some(id) = self.drafts.current_id().map(str::to_owned) else {
            return Outcome::default();
        };
        self.touched(&disk);
        self.asking = true;
        // The edit isn't saved while the question is open, so an earlier
        // save failure no longer says why.
        self.save_error = None;
        Outcome {
            effects: vec![Effect::AskOutsideEdit(Question {
                epoch: self.epoch,
                id,
                disk,
            })],
            deadline: None,
        }
    }

    /// The user answered `question`. The answer is about the pending edit
    /// to the draft as it was, and the file's text when asked: it is
    /// ignored if the draft was left, the edit was saved meanwhile, or the
    /// file changed again. Then autosave goes on, and a save the file
    /// still refuses is asked about by the next check.
    pub fn answer(&mut self, question: Question, choice: Choice) -> Outcome {
        if question.epoch != self.epoch || self.drafts.current_id() != Some(question.id.as_str()) {
            return self.due();
        }
        self.asking = false;
        if self.drafts.deadline().is_none()
            || self.drafts.changed_on_disk().ok().flatten().as_ref() != Some(&question.disk)
        {
            self.held = None;
            return self.due();
        }
        match choice {
            Choice::UseNew => self.replace(question.disk),
            Choice::KeepMine => {
                self.held = None;
                self.drafts.adopt(question.disk);
                self.flush().ok();
                self.due()
            }
            Choice::Dismissed => {
                self.held = Some(Held::Dismissed {
                    disk: question.disk,
                    text: self.text.clone(),
                });
                Outcome::default()
            }
        }
    }

    /// An agent wrote `text` to draft `id`'s file at `at`. Records the edit
    /// its turn made, then shows the text if `id` is selected and has no
    /// pending edit, else counts the write as the draft's activity.
    ///
    /// Returns whether the turn was recorded, which a failure to save the
    /// history leaves for the turn's next write.
    pub fn agent_wrote(
        &mut self,
        id: &str,
        text: &str,
        at: DateTime<Utc>,
    ) -> (Result<Recorded>, Outcome) {
        let current = self.drafts.current_id() == Some(id);
        let recorded = self.turns.agent_wrote(
            self.drafts.store(),
            id,
            text,
            at,
            current.then_some(self.review.log_mut()),
        );
        if current && matches!(recorded, Ok(Recorded::Added)) {
            self.review.edit_added();
        }
        if !current {
            if let Err(err) = self.drafts.touched(id, text) {
                eprintln!("draftmux: recording the agent's edit: {err:#}");
            }
            return (recorded, Outcome::default());
        }
        // A file that can't be read is reported by the next save, and a
        // pending edit is asked about by the next check.
        let Ok(Some(disk)) = self.drafts.changed_on_disk() else {
            return (recorded, Outcome::default());
        };
        self.touched(&disk);
        let outcome = if self.drafts.deadline().is_none() {
            self.replace(disk)
        } else {
            Outcome::default()
        };
        (recorded, outcome)
    }

    /// The user replaced part of the selected draft, which read `before` and
    /// now reads `text`, with an option the agent offered for `prompt`.
    /// Records it in the history like an agent turn's edit.
    pub fn option_applied(
        &mut self,
        before: String,
        text: &str,
        prompt: &str,
        at: DateTime<Utc>,
    ) -> Result<Recorded> {
        let Some(id) = self.drafts.current_id().map(str::to_owned) else {
            return Ok(Recorded::Ignored);
        };
        let edit = Edit {
            before,
            at,
            prompt: prompt.to_owned(),
        };
        let recorded = crate::history::record_edit(
            self.drafts.store(),
            &id,
            edit,
            text,
            Some(self.review.log_mut()),
        )?;
        if recorded == Recorded::Added {
            self.review.edit_added();
        }
        Ok(recorded)
    }

    /// Counts the change that left the selected draft's file holding `disk`
    /// as the draft's activity.
    fn touched(&mut self, disk: &str) {
        let Some(id) = self.drafts.current_id().map(str::to_owned) else {
            return;
        };
        if let Err(err) = self.drafts.touched(&id, disk) {
            eprintln!("draftmux: recording the outside edit: {err:#}");
        }
    }

    /// Takes `disk`, which the file holds, as the editor's text.
    fn replace(&mut self, disk: String) -> Outcome {
        self.held = None;
        self.drafts.adopt(disk.clone());
        Outcome {
            effects: vec![Effect::ReplaceText {
                epoch: self.epoch,
                text: disk,
            }],
            deadline: None,
        }
    }

    /// Draft `id`'s agent started or finished a run.
    pub fn agent_running(&mut self, id: &str, running: bool) {
        if running {
            self.running.insert(id.to_owned());
        } else {
            self.running.remove(id);
        }
    }

    /// The editor is read-only while the selected draft's agent runs, so
    /// the user's edits and the agent's don't meet.
    pub fn editor_read_only(&self) -> bool {
        self.drafts
            .current_id()
            .is_some_and(|id| self.running.contains(id))
    }

    /// The agent is writing into the selected draft, or its first draft is
    /// still to be requested.
    pub fn writing(&self) -> bool {
        self.editor_read_only()
            || self.drafts.current().is_some_and(|draft| {
                draft.meta.first_draft_pending && !self.requested.contains(&draft.id)
            })
    }

    /// Draft `id` was handed off and its agent is ready for `prompt`, which
    /// asks for the first draft. The agent writes the first draft only into
    /// an empty draft, so text the user typed while it was starting is kept
    /// and the prompt is offered for them to send or drop; the first draft is
    /// then no longer pending. Otherwise the prompt is sent once the draft is
    /// saved, and the first draft stays pending until the agent accepts a
    /// prompt ([`Session::prompt_accepted`]), so a request lost to a quit or
    /// a failed start is made again next time. Ignored unless `id` is
    /// selected, its first draft is pending, and it wasn't already requested
    /// since the app started. Starts no turn, so the first draft isn't
    /// recorded as an edit.
    pub fn first_draft_ready(&mut self, id: &str, prompt: String) -> Outcome {
        if self.accepted.contains(id) {
            self.first_draft_requested(id);
            return Outcome::default();
        }
        let pending = self
            .drafts
            .current()
            .is_some_and(|draft| draft.id == id && draft.meta.first_draft_pending);
        if !pending || self.requested.contains(id) {
            return Outcome::default();
        }
        let effect = if self.text.trim().is_empty() {
            self.requested.insert(id.to_owned());
            let saved = self
                .flush()
                .map(|()| self.text.clone())
                .map_err(|err| format!("{err:#}"));
            Effect::Prompt {
                text: prompt,
                saved,
            }
        } else {
            self.first_draft_requested(id);
            Effect::Offer(prompt)
        };
        // The autosave timer is left as it was.
        Outcome {
            effects: vec![effect],
            deadline: None,
        }
    }

    /// Draft `id`'s agent accepted a prompt, selected or not. When its first
    /// draft was requested, that request, or whatever the user sent in its
    /// place, is now the agent's to carry out, and it is no longer pending.
    /// A prompt the agent rejects or never answers leaves it pending: the
    /// chat offers Retry, and the next start requests it again.
    pub fn prompt_accepted(&mut self, id: &str) {
        self.prompted.insert(id.to_owned());
        if self.requested.contains(id) {
            self.accepted.insert(id.to_owned());
            self.first_draft_requested(id);
        }
    }

    fn first_draft_requested(&mut self, id: &str) {
        if let Err(err) = self.drafts.first_draft_requested(id) {
            eprintln!("draftmux: recording the first-draft request: {err:#}");
        } else {
            self.accepted.remove(id);
            self.requested.remove(id);
        }
    }

    /// Keeps change `ix` of the review's comparison, or un-keeps it, and
    /// saves the history.
    pub fn keep(&mut self, ix: usize, kept: bool) -> Result<()> {
        if !self.review.keep(ix, kept) {
            return Ok(());
        }
        self.save_kept()
    }

    /// Keeps every change in the review's comparison and saves the history.
    pub fn finish(&mut self) -> Result<()> {
        self.review.keep_all();
        self.save_kept()
    }

    fn save_kept(&self) -> Result<()> {
        let Some(id) = self.drafts.current_id() else {
            return Ok(());
        };
        self.review.log().save(self.drafts.store(), id)
    }

    /// Asks for change `ix` of the review's comparison to be undone in the
    /// editor. Reverting rejects the change, so it is un-kept first and is
    /// asked about again if it comes back. Refused while the agent runs.
    pub fn revert(&mut self, ix: usize) -> Result<Outcome> {
        let Some(hunk) = self.review.hunks().get(ix).cloned() else {
            return Ok(Outcome::default());
        };
        self.refuse_while_running()?;
        self.refuse_stale_review()?;
        if self.review.is_kept(ix) {
            self.review.keep(ix, false);
            if let Err(err) = self.save_kept() {
                eprintln!("draftmux: saving the reviewed changes: {err:#}");
            }
        }
        Ok(Outcome {
            effects: vec![Effect::ReplaceRange {
                epoch: self.epoch,
                revision: self.revision,
                range: hunk.new,
                text: hunk.removed,
            }],
            deadline: None,
        })
    }

    /// Asks for the editor to go back to the text the review compares with,
    /// dropping every edit since, the agent's and the user's. Refused while
    /// the agent runs.
    pub fn restore_before(&mut self) -> Result<Outcome> {
        let Some(base) = self.review.base().map(str::to_owned) else {
            return Ok(Outcome::default());
        };
        self.refuse_while_running()?;
        self.refuse_stale_review()?;
        Ok(Outcome {
            effects: vec![Effect::ReplaceAll {
                epoch: self.epoch,
                revision: self.revision,
                text: base,
            }],
            deadline: None,
        })
    }

    fn refuse_stale_review(&self) -> Result<()> {
        if !self.review.matches(&self.text) {
            bail!("The draft changed. Refresh the comparison before reverting.");
        }
        Ok(())
    }

    fn refuse_while_running(&self) -> Result<()> {
        if self.editor_read_only() {
            bail!("Wait for the agent to finish before reverting.");
        }
        Ok(())
    }

    fn due(&self) -> Outcome {
        Outcome {
            effects: Vec::new(),
            deadline: self.drafts.deadline(),
        }
    }
}
