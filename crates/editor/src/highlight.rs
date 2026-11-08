//! Markdown syntax highlighting from comrak source positions.

use std::ops::Range;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

/// Markdown styles that apply to a span of the source. Flags combine, so
/// strong emphasis inside a link is all three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Styles(u8);

impl Styles {
    pub const HEADING: Self = Self(1);
    pub const EMPHASIS: Self = Self(1 << 1);
    pub const STRONG: Self = Self(1 << 2);
    pub const CODE: Self = Self(1 << 3);
    pub const LINK: Self = Self(1 << 4);
    pub const STRIKETHROUGH: Self = Self(1 << 5);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Styles {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Styled, non-overlapping byte ranges of `text` in order. Unstyled text is
/// left out.
pub fn highlight(text: &str) -> Vec<(Range<usize>, Styles)> {
    let mut options = Options::default();
    options.extension.autolink = true;
    options.extension.strikethrough = true;
    let arena = Arena::new();
    let root = parse_document(&arena, text, &options);

    let lines = LineIndex::new(text);
    let mut flags = vec![Styles::default(); text.len()];
    for node in root.descendants() {
        let Some(style) = style_of(node) else {
            continue;
        };
        let pos = node.data.borrow().sourcepos;
        let Some(start) = lines.offset(pos.start.line, pos.start.column) else {
            continue;
        };
        let Some(last) = lines.offset(pos.end.line, pos.end.column) else {
            continue;
        };
        let end = next_char_boundary(text, last);
        for range in lines.split_continuations(text, start..end) {
            for flag in &mut flags[range] {
                *flag = *flag | style;
            }
        }
    }

    let mut runs: Vec<(Range<usize>, Styles)> = Vec::new();
    for (ix, &style) in flags.iter().enumerate() {
        if style.is_empty() {
            continue;
        }
        match runs.last_mut() {
            Some((range, last)) if range.end == ix && *last == style => range.end = ix + 1,
            _ => runs.push((ix..ix + 1, style)),
        }
    }
    runs
}

fn style_of(node: &AstNode<'_>) -> Option<Styles> {
    Some(match node.data.borrow().value {
        NodeValue::Heading(_) => Styles::HEADING,
        NodeValue::Emph => Styles::EMPHASIS,
        NodeValue::Strong => Styles::STRONG,
        NodeValue::Code(_) | NodeValue::CodeBlock(_) => Styles::CODE,
        NodeValue::Link(_) | NodeValue::Image(_) => Styles::LINK,
        NodeValue::Strikethrough => Styles::STRIKETHROUGH,
        _ => return None,
    })
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    let mut end = (offset + 1).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    end
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(ix, _)| ix + 1))
            .collect();
        Self { starts }
    }

    /// Byte offset of a 1-based line and byte column.
    fn offset(&self, line: usize, column: usize) -> Option<usize> {
        let start = *self.starts.get(line.checked_sub(1)?)?;
        let offset = start + column.checked_sub(1)?;
        let next = self.starts.get(line).copied().unwrap_or(usize::MAX);
        (offset < next).then_some(offset)
    }

    /// Splits `range` into one range per line. Lines after the first start
    /// past container markup (`>` and indentation), which belongs to the
    /// enclosing block quote or list rather than to the span.
    fn split_continuations(&self, text: &str, range: Range<usize>) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = range.start;
        while start < range.end {
            let line_end = text[start..range.end]
                .find('\n')
                .map_or(range.end, |ix| start + ix);
            ranges.push(start..line_end);
            if line_end == range.end {
                break;
            }
            let next = line_end + 1;
            start = next
                + text[next..range.end]
                    .find(|c: char| !matches!(c, ' ' | '\t' | '>'))
                    .unwrap_or(range.end - next);
        }
        ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The styled substrings of `text`, for readable assertions.
    fn spans(text: &str) -> Vec<(&str, Styles)> {
        highlight(text)
            .into_iter()
            .map(|(range, style)| (&text[range], style))
            .collect()
    }

    #[test]
    fn headings() {
        assert_eq!(
            spans("# Title\n\nbody\n\n## Sub\n"),
            [("# Title", Styles::HEADING), ("## Sub", Styles::HEADING)]
        );
    }

    #[test]
    fn emphasis_strong_and_strikethrough() {
        assert_eq!(
            spans("a *b* __c__ ~~d~~"),
            [
                ("*b*", Styles::EMPHASIS),
                ("__c__", Styles::STRONG),
                ("~~d~~", Styles::STRIKETHROUGH),
            ]
        );
    }

    #[test]
    fn nested_styles_combine() {
        assert_eq!(
            spans("[a **b**](u)"),
            [
                ("[a ", Styles::LINK),
                ("**b**", Styles::LINK | Styles::STRONG),
                ("](u)", Styles::LINK),
            ]
        );
    }

    #[test]
    fn code_spans_and_blocks() {
        assert_eq!(
            spans("use `x` and ``y ` z``\n\n```rs\nfn a() {}\n```\n"),
            [
                ("`x`", Styles::CODE),
                ("``y ` z``", Styles::CODE),
                ("```rs", Styles::CODE),
                ("fn a() {}", Styles::CODE),
                ("```", Styles::CODE),
            ]
        );
    }

    #[test]
    fn links_and_autolinks() {
        assert_eq!(
            spans("see [docs](https://x.y) or https://a.b/c"),
            [
                ("[docs](https://x.y)", Styles::LINK),
                ("https://a.b/c", Styles::LINK),
            ]
        );
    }

    #[test]
    fn multibyte_text_before_and_inside_spans() {
        assert_eq!(
            spans("héllo *wörld* `日本`"),
            [("*wörld*", Styles::EMPHASIS), ("`日本`", Styles::CODE)]
        );
    }

    #[test]
    fn spans_inside_containers() {
        assert_eq!(spans("> a *b*"), [("*b*", Styles::EMPHASIS)]);
        assert_eq!(spans("- item `c`"), [("`c`", Styles::CODE)]);
    }

    #[test]
    fn continuation_lines_skip_block_quote_markers() {
        assert_eq!(
            spans("> a *b\n> c* d"),
            [("*b", Styles::EMPHASIS), ("c*", Styles::EMPHASIS)]
        );
    }

    #[test]
    fn unclosed_delimiters_are_plain() {
        assert_eq!(spans("a **b and `c"), []);
    }
}
