//! The editor's text, selection and undo history, independent of gpui.
//!
//! Offsets are UTF-8 byte offsets into `text` and always sit on char
//! boundaries. Line endings are normalised to `\n` on every way in.

use std::ops::Range;

/// Which visual row an offset on a soft-wrap boundary belongs to. The end of
/// one wrapped row and the start of the next are the same offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Affinity {
    /// The start of the following row.
    #[default]
    Downstream,
    /// The end of the preceding row.
    Upstream,
}

/// A selection from `anchor` to `head`; the caret is at `head`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    pub affinity: Affinity,
}

impl Selection {
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
            affinity: Affinity::Downstream,
        }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }
}

/// How an edit groups with its neighbours in the undo history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Inserting typed text; a run of adjacent keystrokes is one undo step.
    Typing,
    /// Deleting backwards; adjacent deletions are one step.
    Backspace,
    /// Deleting forwards; repeated deletions at one offset are one step.
    DeleteForward,
    /// An IME composition update; a whole composition is one step.
    Ime,
    /// Paste, cut and programmatic replacements: always a step of its own.
    Atomic,
}

/// Where the selection goes after an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionAfter {
    /// A caret after the inserted text.
    Collapse,
    /// The existing selection, moved by the edit: offsets before it stay, offsets
    /// after it shift, and offsets inside it move to its start.
    Map,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    /// Replaced range in the text as it was before this change.
    start: usize,
    old: String,
    new: String,
}

#[derive(Debug)]
struct Transaction {
    kind: EditKind,
    changes: Vec<Change>,
    before: Selection,
    after: Selection,
}

#[derive(Debug, Default)]
pub struct Buffer {
    text: String,
    selection: Selection,
    /// The x position vertical movement aims for, kept across consecutive
    /// up/down moves so the caret returns to its column after short rows.
    pub(crate) goal_x: Option<f32>,
    /// The IME composition in progress.
    marked: Option<Range<usize>>,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Whether the next edit may join the top undo transaction.
    can_coalesce: bool,
    revision: u64,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self {
            text: normalize_line_endings(text),
            ..Default::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Bumped on every text change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    #[cfg(test)]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[cfg(test)]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Replaces the whole text without recording history, for loading a draft.
    pub fn set_text(&mut self, text: &str) {
        self.text = normalize_line_endings(text);
        self.selection = Selection::caret(0);
        self.goal_x = None;
        self.marked = None;
        self.undo.clear();
        self.redo.clear();
        self.can_coalesce = false;
        self.revision += 1;
    }

    /// Moves the selection. Ends any undo grouping and the vertical goal.
    pub fn set_selection(&mut self, selection: Selection) {
        let anchor = self.clamp(selection.anchor);
        let head = self.clamp(selection.head);
        self.selection = Selection {
            anchor,
            head,
            affinity: selection.affinity,
        };
        self.goal_x = None;
        self.can_coalesce = false;
    }

    /// Selects `range` with the caret at its end, or at its start if `reversed`.
    pub fn select_range(&mut self, range: Range<usize>, reversed: bool) {
        let (anchor, head) = if reversed {
            (range.end, range.start)
        } else {
            (range.start, range.end)
        };
        self.set_selection(Selection {
            anchor,
            head,
            affinity: Affinity::Downstream,
        });
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.range()]
    }

    /// Nearest char boundary at or before `offset`, within the text.
    pub fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
        let start = self.clamp(range.start);
        start..self.clamp(range.end).max(start)
    }

    /// Replaces `range` with `text` as one undo step, leaving a caret after the
    /// new text. Out-of-range or mid-character offsets are clamped.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let range = self.clamp_range(range);
        self.edit(
            range,
            &normalize_line_endings(text),
            EditKind::Atomic,
            SelectionAfter::Collapse,
        );
    }

    /// Replaces the whole text as one undo step. Only the span that differs is
    /// rewritten, so the selection outside it stays put.
    pub fn replace_all(&mut self, text: &str) {
        let text = normalize_line_endings(text);
        let (range, new) = diff(&self.text, &text);
        self.edit(range, new, EditKind::Atomic, SelectionAfter::Map);
    }

    /// Replaces the selection (or the IME composition) with typed text.
    pub fn insert(&mut self, text: &str) {
        self.insert_in(None, text);
    }

    /// `range` clamped, else the IME composition, else the selection.
    fn input_range(&self, range: Option<Range<usize>>) -> Range<usize> {
        range
            .map(|r| self.clamp_range(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.range())
    }

    /// Replaces `range` with typed text, defaulting to the IME composition and
    /// then the selection. Commits any composition.
    pub fn insert_in(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = self.input_range(range);
        let kind = if self.marked.is_some() {
            EditKind::Ime
        } else {
            EditKind::Typing
        };
        let text = normalize_line_endings(text);
        // Committing a composition unchanged edits nothing but still ends with
        // a caret after it.
        if !self.edit(range.clone(), &text, kind, SelectionAfter::Collapse) {
            self.set_selection(Selection::caret(range.start + text.len()));
        }
        self.end_composition();
    }

    /// Applies one edit and records it in the undo history. Returns whether the
    /// text changed.
    pub(crate) fn edit(
        &mut self,
        range: Range<usize>,
        new: &str,
        kind: EditKind,
        selection_after: SelectionAfter,
    ) -> bool {
        if self.text[range.clone()] == *new {
            return false;
        }
        let before = self.selection;
        let change = Change {
            start: range.start,
            old: self.text[range.clone()].to_string(),
            new: new.to_string(),
        };
        self.text.replace_range(range.clone(), new);
        self.revision += 1;
        self.marked = None;

        self.selection = match selection_after {
            SelectionAfter::Collapse => Selection::caret(range.start + new.len()),
            SelectionAfter::Map => {
                let map = |offset: usize| {
                    if offset < range.start {
                        offset
                    } else if offset >= range.end {
                        offset - range.len() + new.len()
                    } else {
                        range.start
                    }
                };
                Selection {
                    anchor: map(before.anchor),
                    head: map(before.head),
                    affinity: before.affinity,
                }
            }
        };
        self.goal_x = None;
        self.record(change, kind, before);
        true
    }

    fn record(&mut self, change: Change, kind: EditKind, before: Selection) {
        self.redo.clear();
        let after = self.selection;
        if self.can_coalesce
            && let Some(top) = self.undo.last_mut()
            && top.kind == kind
            && coalesce(kind, top.changes.last_mut().unwrap(), &change)
        {
            if kind == EditKind::Ime {
                top.changes.push(change);
            }
            top.after = after;
        } else {
            self.undo.push(Transaction {
                kind,
                changes: vec![change],
                before,
                after,
            });
        }
        self.can_coalesce = kind != EditKind::Atomic;
    }

    /// Replaces `range` (or the composition, or the selection) with composing
    /// text, marking it, and selects `selected` relative to the new text.
    pub fn compose(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let range = self.input_range(range);
        let text = normalize_line_endings(text);
        // A new composition must not join the edit before it.
        if self.marked.is_none() {
            self.can_coalesce = false;
        }
        self.edit(
            range.clone(),
            &text,
            EditKind::Ime,
            SelectionAfter::Collapse,
        );
        if text.is_empty() {
            self.end_composition();
            return;
        }
        let start = range.start;
        self.marked = Some(start..start + text.len());
        if let Some(selected) = selected {
            let a = self.clamp(start + selected.start.min(text.len()));
            let b = self.clamp(start + selected.end.min(text.len()));
            self.selection = Selection {
                anchor: a,
                head: b,
                affinity: Affinity::Downstream,
            };
        }
    }

    /// Commits the composition in progress as it stands.
    pub fn end_composition(&mut self) {
        self.marked = None;
        if self.undo.last().is_some_and(|t| t.kind == EditKind::Ime) {
            self.can_coalesce = false;
            // A composition typed and then cancelled leaves nothing to undo.
            if self.top_is_noop() {
                self.undo.pop();
            }
        }
    }

    fn top_is_noop(&self) -> bool {
        let Some(top) = self.undo.last() else {
            return false;
        };
        let mut text = self.text.clone();
        for change in top.changes.iter().rev() {
            text.replace_range(change.start..change.start + change.new.len(), &change.old);
        }
        text == self.text
    }

    pub fn undo(&mut self) -> bool {
        self.end_composition();
        let Some(transaction) = self.undo.pop() else {
            return false;
        };
        for change in transaction.changes.iter().rev() {
            self.text
                .replace_range(change.start..change.start + change.new.len(), &change.old);
        }
        self.selection = transaction.before;
        self.finish_history_step();
        self.redo.push(transaction);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.end_composition();
        let Some(transaction) = self.redo.pop() else {
            return false;
        };
        for change in &transaction.changes {
            self.text
                .replace_range(change.start..change.start + change.old.len(), &change.new);
        }
        self.selection = transaction.after;
        self.finish_history_step();
        self.undo.push(transaction);
        true
    }

    fn finish_history_step(&mut self) {
        self.revision += 1;
        self.goal_x = None;
        self.can_coalesce = false;
    }
}

/// Merges `next` into `prev` when it continues the same gesture. IME changes
/// are kept separately but always group, so this only checks the others.
fn coalesce(kind: EditKind, prev: &mut Change, next: &Change) -> bool {
    match kind {
        EditKind::Typing => {
            let joins = next.old.is_empty()
                && next.start == prev.start + prev.new.len()
                && !prev.new.contains('\n')
                && !next.new.contains('\n');
            if joins {
                prev.new.push_str(&next.new);
            }
            joins
        }
        EditKind::Backspace => {
            let joins = prev.new.is_empty()
                && next.new.is_empty()
                && next.start + next.old.len() == prev.start;
            if joins {
                prev.old.insert_str(0, &next.old);
                prev.start = next.start;
            }
            joins
        }
        EditKind::DeleteForward => {
            let joins = prev.new.is_empty() && next.new.is_empty() && next.start == prev.start;
            if joins {
                prev.old.push_str(&next.old);
            }
            joins
        }
        EditKind::Ime => true,
        EditKind::Atomic => false,
    }
}

/// The smallest range of `old` and its replacement that turn `old` into
/// `new`, split on char boundaries.
fn diff<'a>(old: &str, new: &'a str) -> (Range<usize>, &'a str) {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    (prefix..old.len() - suffix, &new[prefix..new.len() - suffix])
}

pub fn normalize_line_endings(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

pub fn offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .chars()
        .map(char::len_utf16)
        .sum()
}

/// The byte offset of a UTF-16 offset, rounded down to a char boundary.
pub fn offset_from_utf16(text: &str, offset_utf16: usize) -> usize {
    let mut utf16 = 0;
    for (ix, ch) in text.char_indices() {
        if utf16 + ch.len_utf16() > offset_utf16 {
            return ix;
        }
        utf16 += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(buffer: &mut Buffer, text: &str) {
        for ch in text.chars() {
            buffer.insert(&ch.to_string());
        }
    }

    #[test]
    fn typing_run_is_one_undo_step() {
        let mut b = Buffer::new("");
        type_str(&mut b, "hello");
        assert_eq!(b.text(), "hello");
        assert!(b.undo());
        assert_eq!(b.text(), "");
        assert!(!b.can_undo());
        assert!(b.redo());
        assert_eq!(b.text(), "hello");
        assert_eq!(b.selection(), Selection::caret(5));
    }

    #[test]
    fn moving_the_caret_breaks_a_typing_run() {
        let mut b = Buffer::new("");
        type_str(&mut b, "ab");
        b.set_selection(Selection::caret(0));
        type_str(&mut b, "x");
        assert_eq!(b.text(), "xab");
        b.undo();
        assert_eq!(b.text(), "ab");
        b.undo();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn newline_starts_a_new_step() {
        let mut b = Buffer::new("");
        type_str(&mut b, "ab\ncd");
        b.undo();
        assert_eq!(b.text(), "ab\n");
        b.undo();
        assert_eq!(b.text(), "ab");
        b.undo();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn typing_then_backspace_are_separate_steps() {
        let mut b = Buffer::new("");
        type_str(&mut b, "abc");
        b.backspace();
        b.backspace();
        assert_eq!(b.text(), "a");
        b.undo();
        assert_eq!(b.text(), "abc");
        assert_eq!(b.selection(), Selection::caret(3));
        b.undo();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn forward_deletes_group() {
        let mut b = Buffer::new("abcd");
        b.set_selection(Selection::caret(1));
        b.delete_forward();
        b.delete_forward();
        assert_eq!(b.text(), "ad");
        b.undo();
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.selection(), Selection::caret(1));
        assert!(!b.can_undo());
    }

    #[test]
    fn typing_over_a_selection_restores_it_on_undo() {
        let mut b = Buffer::new("hello world");
        b.select_range(6..11, true);
        type_str(&mut b, "there");
        assert_eq!(b.text(), "hello there");
        b.undo();
        assert_eq!(b.text(), "hello world");
        assert_eq!(b.selection().range(), 6..11);
        assert!(b.selection().reversed());
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut b = Buffer::new("");
        type_str(&mut b, "a");
        b.undo();
        assert!(b.can_redo());
        type_str(&mut b, "b");
        assert!(!b.can_redo());
    }

    #[test]
    fn replace_range_is_one_step_and_collapses_after_new_text() {
        let mut b = Buffer::new("one two three");
        type_str(&mut b, "");
        b.set_selection(Selection::caret(13));
        type_str(&mut b, "!");
        b.replace_range(4..7, "2");
        assert_eq!(b.text(), "one 2 three!");
        assert_eq!(b.selection(), Selection::caret(5));
        b.undo();
        assert_eq!(b.text(), "one two three!");
        assert_eq!(b.selection(), Selection::caret(14));
        b.undo();
        assert_eq!(b.text(), "one two three");
    }

    #[test]
    fn replace_range_does_not_join_a_typing_run() {
        let mut b = Buffer::new("");
        type_str(&mut b, "ab");
        b.replace_range(2..2, "c");
        type_str(&mut b, "d");
        assert_eq!(b.text(), "abcd");
        b.undo();
        assert_eq!(b.text(), "abc");
        b.undo();
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn replace_range_clamps_invalid_ranges() {
        let mut b = Buffer::new("aé");
        // 2 is inside "é", 99 is past the end.
        b.replace_range(2..99, "x");
        assert_eq!(b.text(), "ax");
        // A reversed range becomes empty at its clamped start.
        let (start, end) = (5, 1);
        b.replace_range(start..end, "y");
        assert_eq!(b.text(), "axy");
    }

    #[test]
    fn replace_all_keeps_selection_outside_the_change() {
        let mut b = Buffer::new("first\nsecond\nthird");
        b.select_range(0..5, false);
        b.replace_all("first\nSECOND!\nthird");
        assert_eq!(b.text(), "first\nSECOND!\nthird");
        assert_eq!(b.selection().range(), 0..5);

        b.set_selection(Selection::caret(16)); // in "third"
        b.replace_all("first\nx\nthird");
        assert_eq!(b.selection(), Selection::caret(10));
        assert_eq!(&b.text()[10..], "ird");

        b.undo();
        assert_eq!(b.text(), "first\nSECOND!\nthird");
        b.undo();
        assert_eq!(b.text(), "first\nsecond\nthird");
        assert!(!b.can_undo());
    }

    #[test]
    fn replace_all_moves_a_caret_inside_the_change_to_its_start() {
        let mut b = Buffer::new("aaa bbb ccc");
        b.set_selection(Selection::caret(5));
        b.replace_all("aaa x ccc");
        assert_eq!(b.selection(), Selection::caret(4));
    }

    #[test]
    fn replace_all_with_same_text_is_a_noop() {
        let mut b = Buffer::new("a\nb");
        let revision = b.revision();
        b.replace_all("a\r\nb");
        assert_eq!(b.text(), "a\nb");
        assert_eq!(b.revision(), revision);
        assert!(!b.can_undo());
    }

    #[test]
    fn replace_all_normalises_before_diffing() {
        let mut b = Buffer::new("a\nb");
        b.replace_all("a\r\nb\r\nc");
        assert_eq!(b.text(), "a\nb\nc");
        b.undo();
        assert_eq!(b.text(), "a\nb");
    }

    #[test]
    fn replace_all_diff_respects_char_boundaries() {
        let mut b = Buffer::new("café");
        b.replace_all("cafè");
        assert_eq!(b.text(), "cafè");
        b.undo();
        assert_eq!(b.text(), "café");
    }

    #[test]
    fn paste_normalises_line_endings() {
        let mut b = Buffer::new("");
        b.paste("a\r\nb\rc");
        assert_eq!(b.text(), "a\nb\nc");
    }

    #[test]
    fn paste_is_its_own_step() {
        let mut b = Buffer::new("");
        type_str(&mut b, "ab");
        b.paste("cd");
        type_str(&mut b, "e");
        b.undo();
        assert_eq!(b.text(), "abcd");
        b.undo();
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn ime_composition_is_one_step() {
        let mut b = Buffer::new("x");
        b.set_selection(Selection::caret(1));
        b.compose(None, "n", None);
        b.compose(None, "に", None);
        b.compose(None, "にほ", None);
        b.compose(None, "日本", Some(0..6));
        assert_eq!(b.marked(), Some(1..7));
        assert_eq!(b.selection().range(), 1..7);
        b.insert("日本");
        assert_eq!(b.text(), "x日本");
        assert_eq!(b.marked(), None);
        assert_eq!(b.selection(), Selection::caret(7));
        b.undo();
        assert_eq!(b.text(), "x");
        assert!(!b.can_undo());
    }

    #[test]
    fn ime_composition_does_not_join_preceding_typing() {
        let mut b = Buffer::new("");
        type_str(&mut b, "ab");
        b.compose(None, "か", None);
        b.insert("か");
        b.undo();
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn cancelled_composition_leaves_no_step() {
        let mut b = Buffer::new("ab");
        b.set_selection(Selection::caret(2));
        b.compose(None, "k", None);
        b.compose(None, "", None);
        assert_eq!(b.text(), "ab");
        assert_eq!(b.marked(), None);
        assert!(!b.can_undo());
    }

    #[test]
    fn utf16_round_trip() {
        let text = "e🙂漢x";
        assert_eq!(offset_to_utf16(text, 0), 0);
        assert_eq!(offset_to_utf16(text, 1), 1);
        assert_eq!(offset_to_utf16(text, 5), 3);
        assert_eq!(offset_to_utf16(text, 8), 4);
        assert_eq!(offset_from_utf16(text, 3), 5);
        assert_eq!(offset_from_utf16(text, 4), 8);
        // Inside the surrogate pair rounds down.
        assert_eq!(offset_from_utf16(text, 2), 1);
        assert_eq!(offset_from_utf16(text, 99), text.len());
    }
}
