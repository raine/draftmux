//! Platform-neutral block tree for message bodies.
//!
//! Each target syntax (GitHub markdown, Slack mrkdwn, plain text) parses into
//! the same [`Document`]; renderers and exporters read the tree without
//! knowing which syntax it came from. The tree holds structure and semantics
//! only, no colors, sizes or fonts.

pub mod github;
pub mod plain;
pub mod rich;
pub mod slack;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading {
        /// 1 to 6.
        level: u8,
        spans: Vec<Span>,
    },
    List(List),
    Quote(Vec<Block>),
    Alert {
        kind: AlertKind,
        blocks: Vec<Block>,
    },
    Code {
        /// First word of the fence info string, if any.
        lang: Option<String>,
        /// The code without a trailing newline.
        text: String,
    },
    Table(Table),
    Rule,
    /// Raw HTML shown as written; it is never interpreted.
    Html(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// Number of the first item for ordered lists; `None` for bullet lists.
    pub start: Option<u64>,
    /// Tight lists have no blank lines between items.
    pub tight: bool,
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    /// `Some(checked)` for task list items.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn label(self) -> &'static str {
        match self {
            AlertKind::Note => "Note",
            AlertKind::Tip => "Tip",
            AlertKind::Important => "Important",
            AlertKind::Warning => "Warning",
            AlertKind::Caution => "Caution",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// One entry per column.
    pub alignments: Vec<Alignment>,
    /// Header cells, one per column.
    pub header: Vec<Vec<Span>>,
    /// Body rows; every row has one cell per column.
    pub rows: Vec<Vec<Vec<Span>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

/// A run of text with uniform styling. Inline content is a flat sequence of
/// spans; nesting such as a bold word inside a link is expressed by each span
/// carrying all styles that apply to it. Line breaks are `\n` in `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    pub link: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub strong: bool,
    pub emph: bool,
    pub strike: bool,
    pub code: bool,
}

/// Appends `text` to `out`, extending the last span when its style and link
/// match.
fn push_span(out: &mut Vec<Span>, text: &str, style: SpanStyle, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.style == style
        && last.link.as_deref() == link
    {
        last.text.push_str(text);
        return;
    }
    out.push(Span {
        text: text.to_owned(),
        style,
        link: link.map(str::to_owned),
    });
}
