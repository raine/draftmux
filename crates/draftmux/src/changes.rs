//! Finds the text that changed between two versions of a draft, so the
//! editor can point it out.

use std::ops::Range;
use std::time::Duration;

use editor::Editor;
use gpui::Context;
use similar::{Algorithm, DiffOp, TextDiff};

/// How long each diff may run before it settles for a coarser answer.
const DIFF_TIMEOUT: Duration = Duration::from_millis(20);
/// Changed line spans longer than this are pointed out whole rather than
/// word by word.
const WORD_DIFF_LIMIT: usize = 4096;

/// Replaces `editor`'s text with `text`, which wasn't typed there, and points
/// out what changed.
pub fn replace_all(editor: &mut Editor, text: &str, cx: &mut Context<Editor>) {
    let old = editor.text().to_owned();
    editor.replace_all(text, cx);
    let ranges = changed_ranges(&old, editor.text());
    editor.flash(ranges, cx);
}

/// The byte ranges of `new` that are inserted or replaced relative to `old`,
/// in order. Lines are compared first, then the words within changed lines.
/// Deletions leave nothing to point at and are left out, as is whitespace
/// at either end of a range.
pub fn changed_ranges(old: &str, new: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let lines = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .timeout(DIFF_TIMEOUT)
        .diff_lines(old, new);
    let old_starts = starts(lines.old_slices());
    let new_starts = starts(lines.new_slices());
    for op in lines.ops() {
        match *op {
            DiffOp::Insert {
                new_index, new_len, ..
            } => ranges.push(new_starts[new_index]..new_starts[new_index + new_len]),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let old_span = old_starts[old_index]..old_starts[old_index + old_len];
                let new_span = new_starts[new_index]..new_starts[new_index + new_len];
                if new_span.len().max(old_span.len()) > WORD_DIFF_LIMIT {
                    ranges.push(new_span);
                } else {
                    let offset = new_span.start;
                    ranges.extend(
                        changed_words(&old[old_span], &new[new_span])
                            .into_iter()
                            .map(|range| range.start + offset..range.end + offset),
                    );
                }
            }
            DiffOp::Equal { .. } | DiffOp::Delete { .. } => {}
        }
    }
    merge(new, trim(new, ranges))
}

/// The byte ranges of `new` whose words are inserted or replaced.
fn changed_words(old: &str, new: &str) -> Vec<Range<usize>> {
    let words = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .timeout(DIFF_TIMEOUT)
        .diff_words(old, new);
    let new_starts = starts(words.new_slices());
    words
        .ops()
        .iter()
        .filter_map(|op| match *op {
            DiffOp::Insert {
                new_index, new_len, ..
            }
            | DiffOp::Replace {
                new_index, new_len, ..
            } => Some(new_starts[new_index]..new_starts[new_index + new_len]),
            DiffOp::Equal { .. } | DiffOp::Delete { .. } => None,
        })
        .collect()
}

/// The byte offset of each slice, and the end of the last.
fn starts(slices: &[&str]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(slices.len() + 1);
    let mut at = 0;
    starts.push(at);
    for slice in slices {
        at += slice.len();
        starts.push(at);
    }
    starts
}

/// `ranges` without whitespace at either end, dropping any that are only
/// whitespace.
fn trim(text: &str, ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges
        .into_iter()
        .filter_map(|range| {
            let slice = &text[range.clone()];
            let start = range.start + (slice.len() - slice.trim_start().len());
            let end = range.end - (slice.len() - slice.trim_end().len());
            (start < end).then_some(start..end)
        })
        .collect()
}

/// `ranges` with neighbours separated only by spaces or tabs joined, so a
/// rewritten phrase reads as one change.
fn merge(text: &str, ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last)
                if text[last.end..range.start]
                    .bytes()
                    .all(|byte| byte == b' ' || byte == b'\t') =>
            {
                last.end = range.end;
            }
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::changed_ranges;

    fn changed<'a>(old: &str, new: &'a str) -> Vec<&'a str> {
        changed_ranges(old, new)
            .into_iter()
            .map(|range| &new[range])
            .collect()
    }

    #[test]
    fn unchanged_text_has_no_changes() {
        assert!(changed("one\ntwo\n", "one\ntwo\n").is_empty());
    }

    #[test]
    fn points_at_the_words_that_changed_within_a_line() {
        assert_eq!(
            changed(
                "The quick brown fox jumps.\n",
                "The slow brown fox leaps.\n"
            ),
            ["slow", "leaps."]
        );
    }

    #[test]
    fn joins_neighbouring_changed_words() {
        assert_eq!(
            changed("Say hello there now.\n", "Say goodbye friend now.\n"),
            ["goodbye friend"]
        );
    }

    #[test]
    fn points_at_inserted_lines_without_their_line_break() {
        assert_eq!(changed("one\nthree\n", "one\ntwo\nthree\n"), ["two"]);
    }

    #[test]
    fn leaves_out_deletions_and_whitespace() {
        assert!(changed("one\ntwo\nthree\n", "one\nthree\n").is_empty());
        assert!(changed("one\nthree\n", "one\n\nthree\n").is_empty());
    }

    #[test]
    fn finds_changes_far_apart() {
        assert_eq!(changed("a\nb\nc\nd\ne\n", "A\nb\nc\nd\nE\n"), ["A", "E"]);
    }
}
