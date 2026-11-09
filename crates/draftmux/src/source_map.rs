//! Maps text selected in a rendered body back to the source it was rendered
//! from.
//!
//! Rendering drops markup (`**`, link URLs, list markers) and changes some
//! characters (curled quotes, emoji shortcodes), but keeps the rest of the
//! text in order. A character diff between the source and the rendered
//! paragraphs therefore lines up every rendered character with its source,
//! or with the source it replaced.

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Duration;

use similar::{Algorithm, DiffOp, TextDiff};

use crate::selectable::{Key, Pos};

/// How long the diff may run before it settles for a coarser alignment.
const DIFF_TIMEOUT: Duration = Duration::from_millis(100);

pub struct SourceMap {
    /// Byte offset of each paragraph in the rendered text.
    starts: BTreeMap<Key, usize>,
    /// Byte offset of each rendered character.
    rendered_chars: Vec<usize>,
    /// For each rendered character, the source bytes it came from; empty
    /// where it has no source of its own.
    sources: Vec<Range<usize>>,
    rendered_len: usize,
}

impl SourceMap {
    /// Lines up `paragraphs`, in reading order, with `source`.
    pub fn new<'a>(source: &str, paragraphs: impl IntoIterator<Item = (Key, &'a str)>) -> Self {
        let mut starts = BTreeMap::new();
        let mut rendered = String::new();
        for (key, text) in paragraphs {
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            starts.insert(key, rendered.len());
            rendered.push_str(text);
        }
        let source_chars: Vec<usize> = char_offsets(source);
        let rendered_chars: Vec<usize> = char_offsets(&rendered);
        let byte = |chars: &[usize], ix: usize| chars.get(ix).copied().unwrap_or(source.len());
        let mut sources = vec![0..0; rendered_chars.len()];
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Myers)
            .timeout(DIFF_TIMEOUT)
            .diff_chars(source, &rendered);
        for op in diff.ops() {
            match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    for k in 0..len {
                        sources[new_index + k] = byte(&source_chars, old_index + k)
                            ..byte(&source_chars, old_index + k + 1);
                    }
                }
                DiffOp::Insert {
                    old_index,
                    new_index,
                    new_len,
                } => {
                    let at = byte(&source_chars, old_index);
                    sources[new_index..new_index + new_len].fill(at..at);
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let replaced =
                        byte(&source_chars, old_index)..byte(&source_chars, old_index + old_len);
                    sources[new_index..new_index + new_len].fill(replaced);
                }
                DiffOp::Delete { .. } => {}
            }
        }
        Self {
            starts,
            rendered_chars,
            sources,
            rendered_len: rendered.len(),
        }
    }

    /// The source of the rendered text from `start` to `end`.
    pub fn range(&self, start: Pos, end: Pos) -> Option<Range<usize>> {
        let first = self.char_at(start)?;
        let last = self.char_at(end)?.checked_sub(1)?;
        if last < first {
            return None;
        }
        let from = self.sources.get(first)?.start;
        let to = self.sources.get(last)?.end;
        (from < to).then_some(from..to)
    }

    /// The index of the rendered character at `pos`, or of the end.
    fn char_at(&self, pos: Pos) -> Option<usize> {
        let byte = (self.starts.get(&pos.key)? + pos.offset).min(self.rendered_len);
        Some(self.rendered_chars.partition_point(|&start| start < byte))
    }
}

fn char_offsets(text: &str) -> Vec<usize> {
    text.char_indices().map(|(ix, _)| ix).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(paragraph: usize, offset: usize) -> Pos {
        Pos {
            key: (0, paragraph),
            offset,
        }
    }

    fn mapped<'a>(source: &'a str, paragraphs: &[&str], start: Pos, end: Pos) -> &'a str {
        let map = SourceMap::new(
            source,
            paragraphs
                .iter()
                .enumerate()
                .map(|(ix, text)| ((0, ix + 1), *text)),
        );
        &source[map.range(start, end).unwrap()]
    }

    #[test]
    fn maps_through_dropped_markup() {
        let source = "## Title\n\nSome **bold** and [a link](https://example.com/a) here.";
        let paragraphs = ["Title", "Some bold and a link here."];
        assert_eq!(mapped(source, &paragraphs, pos(2, 5), pos(2, 9)), "bold");
        assert_eq!(
            mapped(source, &paragraphs, pos(2, 0), pos(2, 9)),
            "Some **bold"
        );
        assert_eq!(
            mapped(source, &paragraphs, pos(2, 14), pos(2, 26)),
            "a link](https://example.com/a) here."
        );
        assert_eq!(
            mapped(source, &paragraphs, pos(1, 2), pos(2, 4)),
            "tle\n\nSome"
        );
    }

    #[test]
    fn maps_changed_characters_to_what_they_replaced() {
        let source = "It's :smile: done";
        let paragraphs = ["It’s 😄 done"];
        assert_eq!(
            mapped(source, &paragraphs, pos(1, 0), pos(1, "It’s".len())),
            "It's"
        );
        let emoji = "It’s ".len();
        assert_eq!(
            mapped(source, &paragraphs, pos(1, emoji), pos(1, emoji + 4)),
            ":smile:"
        );
    }

    #[test]
    fn empty_and_unknown_ranges_map_to_nothing() {
        let map = SourceMap::new("text", [((0, 1), "text")]);
        assert_eq!(map.range(pos(1, 2), pos(1, 2)), None);
        assert_eq!(map.range(pos(5, 0), pos(5, 1)), None);
    }
}
