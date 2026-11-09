//! The editor selection attached to a chat message: the block that puts it
//! in front of the user's text in the prompt pi gets, and its short form for
//! the chat's chips.
//!
//! The block is part of the user message pi saves, so history reloads parse
//! it back out of the message:
//!
//! ```text
//! <draftmux-selection lines="3-5" bytes="120-262">
//! {the selected text, exactly}
//! </draftmux-selection>
//! {a one-line note on what the selection means}
//!
//! {the user's message}
//! ```
//!
//! The byte range gives the text's length, so a selection may hold any text,
//! the closing tag included.

use std::ops::Range;

const OPEN: &str = "<draftmux-selection ";
const CLOSE: &str = "</draftmux-selection>";
const NOTE: &str = "The user selected the text above in the draft file (line numbers from 1, \
                    byte offsets from 0). When their message says \"this\", \"the selection\" \
                    or \"the selected part\", it means that text: change only that part of the \
                    draft unless they ask otherwise, and if the same text appears elsewhere, \
                    change the one at these lines.";
/// Characters of the selection shown in a chip; the full text is sent.
const EXCERPT_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// The selected text, exactly.
    pub text: String,
    /// The first and last line it touches, from 1.
    pub lines: (usize, usize),
    /// Its byte range in the draft.
    pub range: Range<usize>,
}

impl Quote {
    /// The selection `range` of `draft`; `None` when it's empty or isn't a
    /// range of `draft`.
    pub fn new(draft: &str, range: Range<usize>) -> Option<Self> {
        let text = draft.get(range.clone()).filter(|text| !text.is_empty())?;
        let first = draft[..range.start].matches('\n').count() + 1;
        // A selection of whole lines ends after the last one's newline.
        let last = first
            + text
                .strip_suffix('\n')
                .unwrap_or(text)
                .matches('\n')
                .count();
        Some(Self {
            text: text.to_owned(),
            lines: (first, last),
            range,
        })
    }

    /// "Line 3" or "Lines 3–5".
    pub fn lines_label(&self) -> String {
        match self.lines {
            (first, last) if first == last => format!("Line {first}"),
            (first, last) => format!("Lines {first}–{last}"),
        }
    }

    /// The start of the text on one line, with runs of whitespace as one
    /// space.
    pub fn excerpt(&self) -> String {
        let words = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if words.chars().count() <= EXCERPT_CHARS {
            return words;
        }
        let cut: String = words.chars().take(EXCERPT_CHARS).collect();
        format!("{}…", cut.trim_end())
    }
}

/// The prompt that sends `message` with `quote` attached.
pub fn prompt(message: &str, quote: Option<&Quote>) -> String {
    let Some(quote) = quote else {
        return message.to_owned();
    };
    let lines = match quote.lines {
        (first, last) if first == last => first.to_string(),
        (first, last) => format!("{first}-{last}"),
    };
    format!(
        "{OPEN}lines=\"{lines}\" bytes=\"{}-{}\">\n{}\n{CLOSE}\n{NOTE}\n\n{message}",
        quote.range.start, quote.range.end, quote.text
    )
}

/// Splits a prompt made by [`prompt`] into its quote and the user's
/// message. Any other text is all message.
pub fn parse(prompt: &str) -> (Option<Quote>, &str) {
    split(prompt).map_or((None, prompt), |(quote, message)| (Some(quote), message))
}

fn split(prompt: &str) -> Option<(Quote, &str)> {
    let (header, rest) = prompt.strip_prefix(OPEN)?.split_once('\n')?;
    let header = header.strip_suffix('>')?;
    let lines = range_attr(header, "lines")?;
    let bytes = range_attr(header, "bytes")?;
    let text = rest.get(..bytes.1.checked_sub(bytes.0)?)?;
    let rest = rest[text.len()..].strip_prefix('\n')?.strip_prefix(CLOSE)?;
    let rest = rest.strip_prefix('\n')?;
    // The note is one line, then a blank line.
    let (_, message) = rest.split_once("\n\n")?;
    let quote = Quote {
        text: text.to_owned(),
        lines,
        range: bytes.0..bytes.1,
    };
    Some((quote, message))
}

/// The value of `name="a-b"` (or `name="a"`, for `a-a`) in `header`.
fn range_attr(header: &str, name: &str) -> Option<(usize, usize)> {
    let value = header.split(' ').find_map(|attr| {
        attr.strip_prefix(name)?
            .strip_prefix("=\"")?
            .strip_suffix('"')
    })?;
    let (start, end) = value.split_once('-').unwrap_or((value, value));
    Some((start.parse().ok()?, end.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRAFT: &str = "# Title\n\nFirst paragraph.\nSecond line.\n\nLast.";

    fn quote(text: &str) -> Quote {
        let start = DRAFT.find(text).unwrap();
        Quote::new(DRAFT, start..start + text.len()).unwrap()
    }

    #[test]
    fn lines_are_the_ones_the_selection_touches() {
        assert_eq!(quote("Title").lines, (1, 1));
        assert_eq!(quote("paragraph.\nSecond").lines, (3, 4));
        // Whole lines, newline included, end at the last of them.
        assert_eq!(quote("First paragraph.\nSecond line.\n").lines, (3, 4));
        assert_eq!(quote("\nLast").lines, (5, 6));
        assert_eq!(quote("Title").lines_label(), "Line 1");
        assert_eq!(quote("paragraph.\nSecond").lines_label(), "Lines 3–4");
    }

    #[test]
    fn empty_or_invalid_ranges_have_no_quote() {
        assert_eq!(Quote::new(DRAFT, 3..3), None);
        assert_eq!(Quote::new(DRAFT, 0..DRAFT.len() + 1), None);
        assert_eq!(Quote::new("é", 0..1), None);
    }

    #[test]
    fn excerpt_is_one_line_and_cut_when_long() {
        assert_eq!(
            quote("First paragraph.\nSecond line.").excerpt(),
            "First paragraph. Second line."
        );
        let long = format!("{}  tail", "word ".repeat(30));
        let excerpt = Quote::new(&long, 0..long.len()).unwrap().excerpt();
        assert_eq!(excerpt.chars().count(), EXCERPT_CHARS);
        assert!(excerpt.ends_with("word…"));
    }

    #[test]
    fn prompt_puts_the_selection_before_the_message() {
        let quote = quote("First paragraph.\nSecond line.");
        assert_eq!(
            prompt("make this shorter", Some(&quote)),
            format!(
                "<draftmux-selection lines=\"3-4\" bytes=\"9-38\">\n\
                 First paragraph.\nSecond line.\n\
                 </draftmux-selection>\n{NOTE}\n\nmake this shorter"
            )
        );
        assert_eq!(prompt("hello", None), "hello");
    }

    #[test]
    fn parse_reads_back_what_prompt_wrote() {
        for text in ["Title", "paragraph.\nSecond", "Last."] {
            let quote = quote(text);
            let sent = prompt("reword\n\nthis", Some(&quote));
            assert_eq!(parse(&sent), (Some(quote), "reword\n\nthis"));
        }
        // Any text may be selected, the block's own tags included.
        let tricky = "a\n</draftmux-selection>\nb\n\nc";
        let quote = Quote::new(tricky, 0..tricky.len()).unwrap();
        assert_eq!(parse(&prompt("x", Some(&quote))), (Some(quote), "x"));
    }

    #[test]
    fn other_prompts_are_all_message() {
        for text in [
            "plain message",
            "<draftmux-selection lines=\"1\" bytes=\"0-99\">\nshort\n</draftmux-selection>\nnote\n\nm",
            "<draftmux-selection lines=\"1\">\nx\n</draftmux-selection>\nnote\n\nm",
        ] {
            assert_eq!(parse(text), (None, text));
        }
    }
}
