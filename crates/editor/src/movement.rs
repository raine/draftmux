//! Caret movement and deletion on a [`Buffer`].
//!
//! Horizontal movement works on the text alone. Vertical movement and visual
//! line start/end need the soft-wrapped layout, supplied as [`RowGeometry`].

use std::ops::Range;

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::buffer::{Affinity, Buffer, EditKind, Selection, SelectionAfter};

/// The visual rows of the laid-out text. Rows cover the text in order; a
/// logical line's rows exclude its `\n`, and a soft-wrapped row ends at the
/// offset where the next row starts.
pub trait RowGeometry {
    fn row_count(&self) -> usize;
    fn row_range(&self, row: usize) -> Range<usize>;
    /// The row and x position of the caret at `offset`.
    fn position(&self, offset: usize, affinity: Affinity) -> (usize, f32);
    /// The offset in `row` closest to `x`.
    fn offset_at(&self, row: usize, x: f32) -> usize;

    /// Affinity for a caret placed at `offset` on `row`: upstream when it is the
    /// end of a row that soft-wraps into the next one.
    fn affinity_on_row(&self, row: usize, offset: usize) -> Affinity {
        if offset == self.row_range(row).end
            && row + 1 < self.row_count()
            && self.row_range(row + 1).start == offset
        {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        }
    }

    /// The row a caret at `offset` with `affinity` is drawn on: the end of a
    /// soft-wrapped row belongs to it only with upstream affinity.
    fn row_for(&self, offset: usize, affinity: Affinity) -> usize {
        (0..self.row_count())
            .find(|&ix| {
                let range = self.row_range(ix);
                range.contains(&offset)
                    || (offset == range.end
                        && (affinity == Affinity::Upstream
                            || self.affinity_on_row(ix, offset) == Affinity::Downstream))
            })
            .unwrap_or(self.row_count() - 1)
    }
}

pub fn prev_grapheme(text: &str, offset: usize) -> usize {
    GraphemeCursor::new(offset, text.len(), true)
        .prev_boundary(text, 0)
        .ok()
        .flatten()
        .unwrap_or(0)
}

pub fn next_grapheme(text: &str, offset: usize) -> usize {
    GraphemeCursor::new(offset, text.len(), true)
        .next_boundary(text, 0)
        .ok()
        .flatten()
        .unwrap_or(text.len())
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

/// The start of the word before `offset`, skipping whitespace and punctuation.
pub fn prev_word(text: &str, offset: usize) -> usize {
    text.split_word_bound_indices()
        .take_while(|(ix, _)| *ix < offset)
        .filter(|(_, s)| is_word(s))
        .last()
        .map_or(0, |(ix, _)| ix)
}

/// The end of the word after `offset`, skipping whitespace and punctuation.
pub fn next_word(text: &str, offset: usize) -> usize {
    text.split_word_bound_indices()
        .find(|(ix, s)| ix + s.len() > offset && is_word(s))
        .map_or(text.len(), |(ix, s)| ix + s.len())
}

/// The word (or run of whitespace or punctuation) containing `offset`.
pub fn word_range(text: &str, offset: usize) -> Range<usize> {
    text.split_word_bound_indices()
        .map(|(ix, s)| ix..ix + s.len())
        .find(|r| r.contains(&offset))
        .or_else(|| {
            // At the end of the text: the last segment.
            text.split_word_bound_indices()
                .next_back()
                .map(|(ix, s)| ix..ix + s.len())
        })
        .unwrap_or(0..0)
}

/// The logical line containing `offset`, including its `\n`.
pub fn line_range(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |ix| offset + ix + 1);
    start..end
}

impl Buffer {
    fn move_head(&mut self, head: usize, affinity: Affinity, select: bool) {
        let anchor = if select {
            self.selection().anchor
        } else {
            head
        };
        self.set_selection(Selection {
            anchor,
            head,
            affinity,
        });
    }

    pub fn move_left(&mut self, select: bool) {
        let selection = self.selection();
        if !select && !selection.is_empty() {
            self.move_head(selection.range().start, Affinity::Downstream, false);
        } else {
            let head = prev_grapheme(self.text(), selection.head);
            self.move_head(head, Affinity::Downstream, select);
        }
    }

    pub fn move_right(&mut self, select: bool) {
        let selection = self.selection();
        if !select && !selection.is_empty() {
            self.move_head(selection.range().end, Affinity::Downstream, false);
        } else {
            let head = next_grapheme(self.text(), selection.head);
            self.move_head(head, Affinity::Downstream, select);
        }
    }

    pub fn move_word_left(&mut self, select: bool) {
        let head = prev_word(self.text(), self.selection().head);
        self.move_head(head, Affinity::Downstream, select);
    }

    pub fn move_word_right(&mut self, select: bool) {
        let head = next_word(self.text(), self.selection().head);
        self.move_head(head, Affinity::Downstream, select);
    }

    pub fn move_to_start(&mut self, select: bool) {
        self.move_head(0, Affinity::Downstream, select);
    }

    pub fn move_to_end(&mut self, select: bool) {
        self.move_head(self.len(), Affinity::Downstream, select);
    }

    pub fn select_all(&mut self) {
        self.select_range(0..self.len(), false);
    }

    fn caret_row(&self, rows: &impl RowGeometry) -> (usize, f32) {
        let selection = self.selection();
        rows.position(selection.head, selection.affinity)
    }

    /// Moves to the start of the caret's visual row.
    pub fn move_to_row_start(&mut self, rows: &impl RowGeometry, select: bool) {
        let (row, _) = self.caret_row(rows);
        self.move_head(rows.row_range(row).start, Affinity::Downstream, select);
    }

    /// Moves to the end of the caret's visual row.
    pub fn move_to_row_end(&mut self, rows: &impl RowGeometry, select: bool) {
        let (row, _) = self.caret_row(rows);
        let end = rows.row_range(row).end;
        self.move_head(end, rows.affinity_on_row(row, end), select);
    }

    /// Moves `delta` visual rows up (negative) or down, aiming for the column
    /// the vertical movement started from. Moving past the first or last row
    /// goes to the start or end of the text.
    pub fn move_vertically(&mut self, rows: &impl RowGeometry, delta: isize, select: bool) {
        let selection = self.selection();
        if !select && !selection.is_empty() {
            // Collapse to the edge in the direction of travel first.
            let edge = if delta < 0 {
                selection.range().start
            } else {
                selection.range().end
            };
            self.move_head(edge, Affinity::Downstream, false);
        }
        let (row, x) = self.caret_row(rows);
        let goal_x = self.goal_x.unwrap_or(x);
        let target = row as isize + delta;
        let (head, affinity) = if target < 0 {
            (0, Affinity::Downstream)
        } else if target as usize >= rows.row_count() {
            (self.len(), Affinity::Downstream)
        } else {
            let target = target as usize;
            let offset = rows.offset_at(target, goal_x);
            (offset, rows.affinity_on_row(target, offset))
        };
        self.move_head(head, affinity, select);
        self.goal_x = Some(goal_x);
    }

    /// Deletes the selection, or `range` when the selection is empty.
    fn delete(&mut self, range: Range<usize>, kind: EditKind) {
        let selection = self.selection().range();
        let range = if selection.is_empty() {
            range
        } else {
            selection
        };
        self.edit(range, "", kind, SelectionAfter::Collapse);
    }

    pub fn backspace(&mut self) {
        let head = self.selection().head;
        self.delete(prev_grapheme(self.text(), head)..head, EditKind::Backspace);
    }

    pub fn delete_forward(&mut self) {
        let head = self.selection().head;
        self.delete(
            head..next_grapheme(self.text(), head),
            EditKind::DeleteForward,
        );
    }

    pub fn delete_word_left(&mut self) {
        let head = self.selection().head;
        self.delete(prev_word(self.text(), head)..head, EditKind::Atomic);
    }

    pub fn delete_word_right(&mut self) {
        let head = self.selection().head;
        self.delete(head..next_word(self.text(), head), EditKind::Atomic);
    }

    /// Deletes back to the start of the caret's visual row, or the previous
    /// character when already there.
    pub fn delete_to_row_start(&mut self, rows: &impl RowGeometry) {
        let head = self.selection().head;
        let (row, _) = self.caret_row(rows);
        let start = rows.row_range(row).start;
        let start = if start == head {
            prev_grapheme(self.text(), head)
        } else {
            start
        };
        self.delete(start..head, EditKind::Atomic);
    }

    pub fn paste(&mut self, text: &str) {
        let range = self.selection().range();
        self.replace_range(range, text);
    }

    /// Deletes the selection and returns its text.
    pub fn cut(&mut self) -> Option<String> {
        let range = self.selection().range();
        if range.is_empty() {
            return None;
        }
        let text = self.text()[range.clone()].to_string();
        self.replace_range(range, "");
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monospace layout with every char one unit wide, wrapping each logical
    /// line every `width` chars (no word wrap). Tests use ASCII.
    struct FixedRows {
        rows: Vec<Range<usize>>,
    }

    impl FixedRows {
        fn new(text: &str, width: usize) -> Self {
            let mut rows = Vec::new();
            let mut start = 0;
            for line in text.split('\n') {
                let end = start + line.len();
                let mut row_start = start;
                while end - row_start > width {
                    rows.push(row_start..row_start + width);
                    row_start += width;
                }
                rows.push(row_start..end);
                start = end + 1;
            }
            Self { rows }
        }
    }

    impl RowGeometry for FixedRows {
        fn row_count(&self) -> usize {
            self.rows.len()
        }

        fn row_range(&self, row: usize) -> Range<usize> {
            self.rows[row].clone()
        }

        fn position(&self, offset: usize, affinity: Affinity) -> (usize, f32) {
            let row = self.row_for(offset, affinity);
            (row, (offset - self.rows[row].start) as f32)
        }

        fn offset_at(&self, row: usize, x: f32) -> usize {
            let r = &self.rows[row];
            (r.start + x.round() as usize).min(r.end)
        }
    }

    fn caret(b: &mut Buffer, offset: usize) {
        b.set_selection(Selection::caret(offset));
    }

    #[test]
    fn grapheme_movement_skips_whole_clusters() {
        // Family emoji (ZWJ sequence) and e + combining acute.
        let text = "a👨‍👩‍👧e\u{301}b";
        let mut b = Buffer::new(text);
        b.move_right(false);
        assert_eq!(b.selection().head, 1);
        b.move_right(false);
        assert_eq!(&b.text()[1..b.selection().head], "👨‍👩‍👧");
        b.move_right(false);
        assert_eq!(&b.text()[b.selection().head..], "b");
        b.move_left(false);
        b.move_left(false);
        assert_eq!(b.selection().head, 1);
    }

    #[test]
    fn backspace_deletes_a_whole_cluster() {
        let mut b = Buffer::new("x👍🏽");
        let end = b.len();
        caret(&mut b, end);
        b.backspace();
        assert_eq!(b.text(), "x");
    }

    #[test]
    fn left_right_collapse_a_selection_to_its_edge() {
        let mut b = Buffer::new("abcdef");
        b.select_range(1..4, false);
        b.move_left(false);
        assert_eq!(b.selection(), Selection::caret(1));
        b.select_range(1..4, true);
        b.move_right(false);
        assert_eq!(b.selection(), Selection::caret(4));
    }

    #[test]
    fn shift_movement_extends_from_the_anchor() {
        let mut b = Buffer::new("abcdef");
        caret(&mut b, 3);
        b.move_right(true);
        b.move_right(true);
        assert_eq!(b.selection().range(), 3..5);
        b.move_left(true);
        b.move_left(true);
        b.move_left(true);
        assert_eq!(b.selection().range(), 2..3);
        assert!(b.selection().reversed());
    }

    #[test]
    fn word_movement_skips_punctuation_and_spaces() {
        let text = "hello, wide  world!";
        assert_eq!(next_word(text, 0), 5);
        assert_eq!(next_word(text, 5), 11);
        assert_eq!(next_word(text, 11), 18);
        assert_eq!(next_word(text, 18), text.len());
        assert_eq!(prev_word(text, text.len()), 13);
        assert_eq!(prev_word(text, 13), 7);
        assert_eq!(prev_word(text, 9), 7);
        assert_eq!(prev_word(text, 7), 0);
        assert_eq!(prev_word(text, 0), 0);
    }

    #[test]
    fn word_and_line_ranges() {
        let text = "one two\nthree";
        assert_eq!(word_range(text, 5), 4..7);
        assert_eq!(word_range(text, 3), 3..4);
        assert_eq!(word_range(text, text.len()), 8..13);
        assert_eq!(line_range(text, 2), 0..8);
        assert_eq!(line_range(text, 9), 8..13);
    }

    #[test]
    fn word_deletion() {
        let mut b = Buffer::new("one two three");
        caret(&mut b, 7);
        b.delete_word_left();
        assert_eq!(b.text(), "one  three");
        b.delete_word_right();
        assert_eq!(b.text(), "one ");
    }

    #[test]
    fn vertical_movement_keeps_goal_column_across_short_rows() {
        let text = "abcdefgh\nab\nabcdefgh";
        let rows = FixedRows::new(text, 20);
        let mut b = Buffer::new(text);
        caret(&mut b, 6);
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, 11); // end of "ab"
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, 18); // column 6 again
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, text.len());
        b.move_vertically(&rows, -5, false);
        assert_eq!(b.selection().head, 0);
    }

    #[test]
    fn goal_column_resets_after_an_edit() {
        let text = "abcdefgh\nab\nabcdefgh";
        let rows = FixedRows::new(text, 20);
        let mut b = Buffer::new(text);
        caret(&mut b, 6);
        b.move_vertically(&rows, 1, false);
        b.insert("x");
        let rows = FixedRows::new(b.text(), 20);
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, 16); // column 3 of the last line
    }

    #[test]
    fn vertical_movement_across_soft_wrapped_rows() {
        // One logical line wrapped into rows 0..5, 5..10, 10..12.
        let text = "abcdefghijkl";
        let rows = FixedRows::new(text, 5);
        let mut b = Buffer::new(text);
        caret(&mut b, 3);
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, 8);
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, 12);
        b.move_vertically(&rows, -1, false);
        assert_eq!(b.selection().head, 8);
    }

    #[test]
    fn vertical_move_onto_a_wrapped_row_end_stays_on_that_row() {
        // Rows 0..5, 5..10 and 10..12 of one logical line.
        let text = "abcdefghijkl";
        let rows = FixedRows::new(text, 5);
        let mut b = Buffer::new(text);
        caret(&mut b, 5);
        b.move_to_row_end(&rows, false);
        assert_eq!(b.selection().head, 10);
        assert_eq!(b.selection().affinity, Affinity::Upstream);
        assert_eq!(rows.position(10, Affinity::Upstream), (1, 5.0));

        // Up at x=5 lands on offset 5, the boundary shared with row 1. It must
        // be drawn at the end of row 0, and the next Up must leave row 0.
        b.move_vertically(&rows, -1, false);
        assert_eq!(b.selection().head, 5);
        assert_eq!(rows.position(5, b.selection().affinity).0, 0);
        b.move_vertically(&rows, -1, false);
        assert_eq!(b.selection().head, 0);

        // Down from the end of row 0 aims for x=5; row 2 is only 2 wide.
        b.move_to_row_end(&rows, false);
        b.move_vertically(&rows, 1, false);
        assert_eq!(
            rows.position(b.selection().head, b.selection().affinity).0,
            1
        );
        b.move_vertically(&rows, 1, false);
        assert_eq!(b.selection().head, 12);
    }

    #[test]
    fn row_start_and_end_follow_soft_wraps() {
        let text = "abcdefghij\nxy";
        let rows = FixedRows::new(text, 4);
        let mut b = Buffer::new(text);
        caret(&mut b, 6);
        b.move_to_row_start(&rows, false);
        assert_eq!(b.selection().head, 4);
        b.move_to_row_end(&rows, true);
        assert_eq!(b.selection().range(), 4..8);
        caret(&mut b, 9);
        b.move_to_row_end(&rows, false);
        assert_eq!(b.selection().head, 10);
        assert_eq!(b.selection().affinity, Affinity::Downstream);
    }

    #[test]
    fn delete_to_row_start() {
        let text = "abcdefgh";
        let rows = FixedRows::new(text, 4);
        let mut b = Buffer::new(text);
        caret(&mut b, 6);
        b.delete_to_row_start(&rows);
        assert_eq!(b.text(), "abcdgh");
        let rows = FixedRows::new(b.text(), 4);
        b.delete_to_row_start(&rows);
        assert_eq!(b.text(), "abcgh");
    }

    #[test]
    fn cut_removes_the_selection_as_one_step() {
        let mut b = Buffer::new("hello world");
        b.select_range(5..11, false);
        assert_eq!(b.cut().as_deref(), Some(" world"));
        assert_eq!(b.text(), "hello");
        assert_eq!(b.cut(), None);
        b.undo();
        assert_eq!(b.text(), "hello world");
    }
}
