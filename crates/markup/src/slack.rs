//! Slack mrkdwn parser.
//!
//! Inline syntax follows Slack's mrkdwn: `*bold*`, `_italic_`, `~strike~`,
//! `` `code` ``, `<url|label>`, Markdown `[label](url)` links, which Slack's
//! composer turns into links, and bare web URLs, with `&amp;`, `&lt;` and
//! `&gt;` decoded. `:shortcode:`s Slack knows, outside code and link labels,
//! become the Unicode emoji Slack shows. Delimiters only apply at word
//! boundaries and never span a line, so `snake_case` and `2*3*4` stay
//! literal. Blocks follow what Slack's
//! composer shows: ```` ``` ```` code blocks (which may open and close
//! mid-line, with a language when a single word follows the opening fence
//! on its own line, as in ```` ```diff ````), `>` quotes holding one
//! paragraph, and `-`, `*`, `•` or `1.` lists nested by indentation.
//!
//! Slack text is newline-literal, so blank lines are kept: consecutive text
//! lines, blank ones included, form one paragraph whose text holds every `\n`.
//! A run of blank lines between two other blocks becomes a paragraph of empty
//! lines.
//!
//! Markdown headings and pipe tables have no Slack equivalent but are kept as
//! [`Block::Heading`] and [`Block::Table`]; [`posted`] gives the blocks Slack
//! actually receives.

use crate::{Alignment, Block, Document, List, ListItem, Span, SpanStyle, Table, push_span};

pub fn parse(mrkdwn: &str) -> Document {
    let text = mrkdwn.replace("\r\n", "\n");
    let mut parser = Parser {
        text: &text,
        pos: 0,
        blocks: Vec::new(),
        para: Vec::new(),
    };
    parser.skip_blank_lines();
    parser.run();
    Document {
        blocks: parser.blocks,
    }
}

/// The blocks Slack posts when `doc` is pasted into its composer as rich
/// text. Headings become a bold line, tables a code block, and lists Slack
/// doesn't keep as lists become "•" or "N." text lines. Each replaced block
/// except a heading, whose bold line shows the change, comes with a short
/// note on what changes.
///
/// Slack keeps a pasted list unless one of its items, nested ones included,
/// spans more than one line, or it is numbered and any numbered list in the
/// paste starts at something other than 1.
pub fn posted(doc: &Document) -> Vec<(Block, Option<&'static str>)> {
    let renumbered = doc.blocks.iter().any(|block| {
        let mut found = false;
        visit_lists(block, &mut |list| {
            found |= list.start.is_some_and(|n| n != 1)
        });
        found
    });
    doc.blocks
        .iter()
        .map(|block| match block {
            Block::Heading { spans, .. } => {
                let spans = spans
                    .iter()
                    .map(|span| Span {
                        style: SpanStyle {
                            strong: true,
                            ..span.style
                        },
                        ..span.clone()
                    })
                    .collect();
                (Block::Paragraph(spans), None)
            }
            Block::Table(table) => (
                Block::Code {
                    lang: None,
                    text: table_text(table),
                },
                Some("Slack has no tables, so this pastes as a code block."),
            ),
            Block::List(list) if multi_line_item(list) => (
                Block::Paragraph(list_lines(list, 0)),
                Some(
                    "Slack can't keep a list item on several lines, so this pastes as text lines.",
                ),
            ),
            Block::List(list) if renumbered && list.start.is_some() => (
                Block::Paragraph(list_lines(list, 0)),
                Some("Slack numbered lists always start at 1, so this pastes as text lines."),
            ),
            block => (block.clone(), None),
        })
        .collect()
}

/// Calls `f` on every list in `block`, nested ones included.
fn visit_lists(block: &Block, f: &mut impl FnMut(&List)) {
    if let Block::List(list) = block {
        f(list);
        for item in &list.items {
            for block in &item.blocks {
                visit_lists(block, f);
            }
        }
    }
}

fn multi_line_item(list: &List) -> bool {
    list.items.iter().any(|item| {
        item.blocks.iter().any(|block| match block {
            Block::Paragraph(spans) => spans.iter().any(|span| span.text.contains('\n')),
            Block::List(nested) => multi_line_item(nested),
            _ => false,
        })
    })
}

/// The "• " or "N. " marker that starts item `ix` of `list` as a text line.
pub(crate) fn text_marker(list: &List, ix: usize) -> String {
    match list.start {
        Some(start) => format!("{}. ", start + ix as u64),
        None => "• ".to_owned(),
    }
}

/// `list` as text lines with a "•" or "N." marker, nested items indented by
/// four spaces per level.
fn list_lines(list: &List, depth: usize) -> Vec<Span> {
    let mut out = Vec::new();
    for (ix, item) in list.items.iter().enumerate() {
        if ix > 0 {
            push_span(&mut out, "\n", SpanStyle::default(), None);
        }
        let marker = text_marker(list, ix);
        push_span(
            &mut out,
            &format!("{}{marker}", "    ".repeat(depth)),
            SpanStyle::default(),
            None,
        );
        for (bx, block) in item.blocks.iter().enumerate() {
            match block {
                Block::Paragraph(spans) => {
                    if bx > 0 {
                        push_span(&mut out, "\n", SpanStyle::default(), None);
                    }
                    for span in spans {
                        push_span(&mut out, &span.text, span.style, span.link.as_deref());
                    }
                }
                Block::List(nested) => {
                    push_span(&mut out, "\n", SpanStyle::default(), None);
                    for span in list_lines(nested, depth + 1) {
                        push_span(&mut out, &span.text, span.style, span.link.as_deref());
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Lays a table out as aligned plain-text columns.
fn table_text(table: &Table) -> String {
    let plain = |cell: &Vec<Span>| {
        cell.iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
    };
    let rows: Vec<Vec<String>> = std::iter::once(&table.header)
        .chain(&table.rows)
        .map(|row| row.iter().map(plain).collect())
        .collect();
    let widths: Vec<usize> = (0..table.alignments.len())
        .map(|col| {
            rows.iter()
                .map(|row| row[col].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |row: &Vec<String>| {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .zip(&table.alignments)
            .map(|((cell, &width), alignment)| {
                let pad = width - cell.chars().count();
                let left = match alignment {
                    Alignment::Right => pad,
                    Alignment::Center => pad / 2,
                    _ => 0,
                };
                format!("{}{cell}{}", " ".repeat(left), " ".repeat(pad - left))
            })
            .collect();
        cells.join(" | ").trim_end().to_owned()
    };
    let rule = widths
        .iter()
        .map(|&width| "-".repeat(width))
        .collect::<Vec<_>>()
        .join("-+-");
    let mut lines = vec![line(&rows[0]), rule];
    lines.extend(rows[1..].iter().map(line));
    lines.join("\n")
}

struct Parser<'a> {
    text: &'a str,
    /// Byte offset of the start of the next unread line.
    pos: usize,
    blocks: Vec<Block>,
    /// Pending paragraph lines.
    para: Vec<&'a str>,
}

impl<'a> Parser<'a> {
    fn done(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn peek_line(&self) -> &'a str {
        self.text[self.pos..].split('\n').next().unwrap_or_default()
    }

    fn next_line(&mut self) -> &'a str {
        let line = self.peek_line();
        self.pos = (self.pos + line.len() + 1).min(self.text.len());
        line
    }

    /// The line after the next one, if any.
    fn peek_second_line(&self) -> Option<&'a str> {
        self.text[self.pos..].split('\n').nth(1)
    }

    fn skip_blank_lines(&mut self) {
        while !self.done() && self.peek_line().trim().is_empty() {
            self.next_line();
        }
    }

    fn run(&mut self) {
        while !self.done() {
            let line = self.peek_line();
            if line.starts_with('>') {
                self.quote();
            } else if let Some((offset, body_end)) = self.fence(line) {
                self.code(offset, body_end);
            } else if let Some((level, content)) = heading(line) {
                self.next_line();
                self.push_block(Block::Heading {
                    level,
                    spans: inline(content),
                });
            } else if let Some((header, alignments)) = self.table_head(line) {
                self.table(header, alignments);
            } else if list_marker(line).is_some() {
                self.list();
            } else {
                let line = self.next_line();
                self.para.push(line);
            }
        }
        // Trailing blank lines don't show in Slack.
        while self.para.last().is_some_and(|line| line.trim().is_empty()) {
            self.para.pop();
        }
        self.flush_para();
    }

    fn flush_para(&mut self) {
        if !self.para.is_empty() {
            let text = self.para.join("\n");
            self.para.clear();
            self.blocks.push(Block::Paragraph(inline(&text)));
        }
    }

    fn push_block(&mut self, block: Block) {
        self.flush_para();
        self.blocks.push(block);
    }

    /// `>` lines up to the first unquoted line, or with `>>>` the rest of the
    /// message, as one quoted paragraph.
    fn quote(&mut self) {
        let mut lines = Vec::new();
        if self.peek_line().starts_with(">>>") {
            let rest = &self.text[self.pos + 3..];
            lines.push(rest.strip_prefix(' ').unwrap_or(rest).trim_end());
            self.pos = self.text.len();
        } else {
            while !self.done() && self.peek_line().starts_with('>') {
                let line = &self.next_line()[1..];
                lines.push(line.strip_prefix(' ').unwrap_or(line));
            }
        }
        self.push_block(Block::Quote(vec![Block::Paragraph(inline(
            &lines.join("\n"),
        ))]));
    }

    /// Offset of a ```` ``` ```` in `line` and the text position of the one
    /// closing it, if any.
    fn fence(&self, line: &str) -> Option<(usize, usize)> {
        let offset = line.find("```")?;
        let body_start = self.pos + offset + 3;
        let body_end = body_start + self.text[body_start..].find("```")?;
        Some((offset, body_end))
    }

    fn code(&mut self, offset: usize, body_end: usize) {
        let before = &self.text[self.pos..self.pos + offset];
        if !before.trim().is_empty() {
            self.para.push(before.trim_end());
        }
        let body_start = self.pos + offset + 3;
        let body = &self.text[body_start..body_end];
        let (lang, body) = match body.split_once('\n') {
            Some((word, rest)) if !word.is_empty() && !word.contains(char::is_whitespace) => {
                (Some(decode(word)), rest)
            }
            _ => (None, body.strip_prefix('\n').unwrap_or(body)),
        };
        let body = body.strip_suffix('\n').unwrap_or(body);
        self.push_block(Block::Code {
            lang,
            text: decode(body),
        });
        // Text after the closing fence continues as its own line.
        self.pos = body_end + 3;
        if self.peek_line().trim().is_empty() {
            self.next_line();
        }
    }

    /// Header cells and column alignments if `line` starts a pipe table.
    fn table_head(&self, line: &'a str) -> Option<(Vec<&'a str>, Vec<Alignment>)> {
        if !line.contains('|') {
            return None;
        }
        let header = cells(line);
        let alignments = delimiter_row(self.peek_second_line()?, header.len())?;
        Some((header, alignments))
    }

    fn table(&mut self, header: Vec<&'a str>, alignments: Vec<Alignment>) {
        self.next_line();
        self.next_line();
        let columns = header.len();
        let row = |cells: Vec<&str>| {
            let mut row: Vec<Vec<Span>> = cells.into_iter().take(columns).map(inline).collect();
            row.resize(columns, Vec::new());
            row
        };
        let header = row(header);
        let mut rows = Vec::new();
        while !self.done() {
            let line = self.peek_line();
            if line.trim().is_empty() || !line.contains('|') {
                break;
            }
            rows.push(row(cells(self.next_line())));
        }
        self.push_block(Block::Table(Table {
            alignments,
            header,
            rows,
        }));
    }

    /// Consecutive list lines, plus indented continuation lines, as one or
    /// more lists.
    fn list(&mut self) {
        let mut entries: Vec<Entry> = Vec::new();
        while !self.done() {
            let line = self.peek_line();
            if let Some(entry) = list_marker(line) {
                entries.push(entry);
            } else if line.starts_with([' ', '\t'])
                && !line.trim().is_empty()
                && let Some(last) = entries.last_mut()
            {
                last.text.push('\n');
                last.text.push_str(line.trim());
            } else {
                break;
            }
            self.next_line();
        }
        let mut ix = 0;
        while ix < entries.len() {
            let list = build_list(&entries, &mut ix);
            self.push_block(Block::List(list));
        }
    }
}

struct Entry {
    indent: usize,
    /// Number of an ordered item; `None` for bullets.
    number: Option<u64>,
    text: String,
}

/// Builds the list starting at `entries[*ix]`: following entries at the same
/// indent and kind are its items, deeper ones nest under the previous item.
fn build_list(entries: &[Entry], ix: &mut usize) -> List {
    let first = &entries[*ix];
    let mut list = List {
        start: first.number,
        tight: true,
        items: Vec::new(),
    };
    while let Some(entry) = entries.get(*ix) {
        if entry.indent > first.indent && !list.items.is_empty() {
            let nested = build_list(entries, ix);
            list.items
                .last_mut()
                .unwrap()
                .blocks
                .push(Block::List(nested));
            continue;
        }
        if entry.indent < first.indent || entry.number.is_some() != first.number.is_some() {
            break;
        }
        list.items.push(ListItem {
            task: None,
            blocks: vec![Block::Paragraph(inline(&entry.text))],
        });
        *ix += 1;
    }
    list
}

/// Parses a list item line: `-`, `*` or `•`, or a number and `.`, followed
/// by whitespace.
fn list_marker(line: &str) -> Option<Entry> {
    let content = line.trim_start_matches([' ', '\t']);
    let indent = line[..line.len() - content.len()]
        .chars()
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum();
    let (number, rest) = if let Some(rest) = content
        .strip_prefix(['-', '*'])
        .or_else(|| content.strip_prefix('•'))
    {
        (None, rest)
    } else {
        let digits = content.len()
            - content
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        let rest = content[digits..].strip_prefix('.')?;
        (Some(content[..digits].parse().ok()?), rest)
    };
    let text = rest.strip_prefix([' ', '\t'])?.trim();
    (!text.is_empty()).then(|| Entry {
        indent,
        number,
        text: text.to_owned(),
    })
}

/// `#` to `######` followed by whitespace and text.
fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let content = rest.trim();
    // An optional closing run of `#`s is not part of the heading.
    let content = match content.trim_end_matches('#') {
        stripped if stripped.ends_with([' ', '\t']) => stripped.trim_end(),
        _ => content,
    };
    (!content.is_empty()).then_some((hashes as u8, content))
}

/// Table cells of a row, split at pipes outside code and `<…>` links, with
/// the optional outer pipes dropped.
fn cells(line: &str) -> Vec<&str> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let mut cells = Vec::new();
    let mut start = 0;
    let mut closer = None;
    for (ix, c) in line.char_indices() {
        match (closer, c) {
            (Some(close), _) if c == close => closer = None,
            (Some(_), _) => {}
            (None, '`') => closer = Some('`'),
            (None, '<') => closer = Some('>'),
            (None, '|') if !line[..ix].ends_with('\\') => {
                cells.push(line[start..ix].trim());
                start = ix + 1;
            }
            _ => {}
        }
    }
    let last = line[start..].trim();
    if !last.is_empty() || start == 0 {
        cells.push(last);
    }
    cells
}

/// Column alignments if `line` is a table delimiter row such as
/// `|:--|--:|` with `columns` cells.
fn delimiter_row(line: &str, columns: usize) -> Option<Vec<Alignment>> {
    let cells = cells(line);
    if cells.len() != columns {
        return None;
    }
    cells
        .into_iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':') && cell.len() > 1;
            let dashes = cell.trim_matches(':');
            if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => Alignment::Center,
                (true, false) => Alignment::Left,
                (false, true) => Alignment::Right,
                (false, false) => Alignment::None,
            })
        })
        .collect()
}

fn inline(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    collect_spans(text, SpanStyle::default(), &mut out);
    out
}

/// Pushes the spans of `text`, which has `style` applied around it.
fn collect_spans(text: &str, style: SpanStyle, out: &mut Vec<Span>) {
    let mut plain = 0;
    let mut ix = 0;
    while ix < text.len() {
        let rest = &text[ix..];
        let c = rest.chars().next().unwrap();
        // Ends the plain text before a token.
        let flush = |out: &mut Vec<Span>| push_span(out, &decode(&text[plain..ix]), style, None);
        let token_len = match c {
            '`' => code_span(rest).map(|(len, code)| {
                flush(out);
                let style = SpanStyle {
                    code: true,
                    ..style
                };
                push_span(out, &decode(code), style, None);
                len
            }),
            '<' => angle_link(rest).map(|(len, url, label)| {
                flush(out);
                push_span(out, &decode(label), style, Some(&decode(url)));
                len
            }),
            '[' => markdown_link(rest).map(|(len, url, label)| {
                flush(out);
                push_span(out, &decode(label), style, Some(&decode(url)));
                len
            }),
            'h' if !preceded_by_alnum(text, ix) => bare_url(rest).map(|url| {
                flush(out);
                push_span(out, url, style, Some(url));
                url.len()
            }),
            ':' => crate::emoji::shortcode(rest).map(|(len, emoji)| {
                flush(out);
                push_span(out, emoji, style, None);
                len
            }),
            '*' | '_' | '~' => emphasis(text, ix, c).map(|end| {
                flush(out);
                let style = match c {
                    '*' => SpanStyle {
                        strong: true,
                        ..style
                    },
                    '_' => SpanStyle {
                        emph: true,
                        ..style
                    },
                    _ => SpanStyle {
                        strike: true,
                        ..style
                    },
                };
                collect_spans(&text[ix + 1..end], style, out);
                end + 1 - ix
            }),
            _ => None,
        };
        match token_len {
            Some(len) => {
                ix += len;
                plain = ix;
            }
            // Repeated delimiters like ``` or ** are literal as a whole run.
            None => ix += rest.len() - rest.trim_start_matches(c).len(),
        }
    }
    push_span(out, &decode(&text[plain..]), style, None);
}

fn preceded_by_alnum(text: &str, ix: usize) -> bool {
    text[..ix]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
}

/// Length and contents of a `` `code` `` span at the start of `text`.
fn code_span(text: &str) -> Option<(usize, &str)> {
    let rest = text.strip_prefix('`')?;
    if rest.starts_with('`') {
        return None;
    }
    let end = rest.find(['`', '\n'])?;
    (rest[end..].starts_with('`') && end > 0).then(|| (end + 2, &rest[..end]))
}

/// Length, user ID and label of a `<@U123>` or `<@U123|name>` user mention
/// at the start of `text`.
pub fn user_mention(text: &str) -> Option<(usize, &str, Option<&str>)> {
    let rest = text.strip_prefix("<@")?;
    let end = rest.find(['>', '<', '\n'])?;
    if !rest[end..].starts_with('>') {
        return None;
    }
    let (id, label) = match rest[..end].split_once('|') {
        Some((id, label)) => (id, Some(label).filter(|label| !label.is_empty())),
        None => (&rest[..end], None),
    };
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then_some((end + 3, id, label))
}

/// Length, target and label of a `<url>` or `<url|label>` link at the start
/// of `text`. Mentions such as `<@U123>` have no scheme and stay literal.
fn angle_link(text: &str) -> Option<(usize, &str, &str)> {
    let rest = text.strip_prefix('<')?;
    let end = rest.find(['>', '<', '\n'])?;
    if !rest[end..].starts_with('>') {
        return None;
    }
    let inner = &rest[..end];
    let (url, label) = inner.split_once('|').unwrap_or((inner, inner));
    let label = if label.is_empty() { url } else { label };
    link_target(url).then_some((end + 2, url, label))
}

/// Length, target and label of a Markdown `[label](url)` link at the start
/// of `text`. The label is one line without brackets, and the URL may hold
/// balanced parentheses, as in `[Foo](https://en.wikipedia.org/wiki/Foo_(bar))`.
fn markdown_link(text: &str) -> Option<(usize, &str, &str)> {
    let rest = text.strip_prefix('[')?;
    let label_end = rest.find([']', '[', '\n'])?;
    let label = &rest[..label_end];
    let after = rest[label_end..].strip_prefix("](")?;
    if label.trim().is_empty() {
        return None;
    }
    let mut depth = 0;
    let url_end = after.char_indices().find_map(|(ix, c)| match c {
        '(' => {
            depth += 1;
            None
        }
        ')' if depth == 0 => Some(Some(ix)),
        ')' => {
            depth -= 1;
            None
        }
        _ if c.is_whitespace() || c == '<' || c == '>' => Some(None),
        _ => None,
    })??;
    let url = &after[..url_end];
    link_target(url).then_some((label_end + url_end + 4, url, label))
}

/// Whether `url` has a scheme and no whitespace, so Slack links it.
fn link_target(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        && !url.contains(char::is_whitespace)
}

/// A bare `http://` or `https://` URL at the start of `text`, without
/// trailing punctuation or an unbalanced closing parenthesis.
fn bare_url(text: &str) -> Option<&str> {
    let after_scheme = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))?;
    let end = text
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(text.len());
    let mut url = &text[..end];
    loop {
        let trimmed =
            url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"', '*', '_', '~']);
        let trimmed = match trimmed.strip_suffix(')') {
            Some(inner) if inner.matches('(').count() < trimmed.matches(')').count() => inner,
            _ => trimmed,
        };
        if trimmed == url {
            break;
        }
        url = trimmed;
    }
    (url.len() > text.len() - after_scheme.len()).then_some(url)
}

/// Byte offset of the delimiter closing the one at `text[ix]`, if it opens
/// emphasis: the opener follows a non-word character and precedes text, and
/// the closer, on the same line, follows text and precedes a non-word
/// character. Code spans and links inside are skipped over.
fn emphasis(text: &str, ix: usize, delim: char) -> Option<usize> {
    if preceded_by_alnum(text, ix) {
        return None;
    }
    let after = text[ix + 1..].chars().next()?;
    if after.is_whitespace() || after == delim {
        return None;
    }
    let mut jx = ix + 1;
    while jx < text.len() {
        let rest = &text[jx..];
        let c = rest.chars().next().unwrap();
        match c {
            '\n' => return None,
            '`' => {
                if let Some((len, _)) = code_span(rest) {
                    jx += len;
                    continue;
                }
            }
            '<' => {
                if let Some((len, ..)) = angle_link(rest) {
                    jx += len;
                    continue;
                }
            }
            '[' => {
                if let Some((len, ..)) = markdown_link(rest) {
                    jx += len;
                    continue;
                }
            }
            _ if c == delim => {
                let before = text[..jx].chars().next_back().unwrap();
                let next = text[jx + 1..].chars().next();
                if !before.is_whitespace()
                    && before != delim
                    && !next.is_some_and(|n| n.is_alphanumeric() || n == delim)
                {
                    return Some(jx);
                }
            }
            _ => {}
        }
        jx += c.len_utf8();
    }
    None
}

/// Decodes the three entities Slack escapes: `&amp;`, `&lt;` and `&gt;`.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Slack's composer curls straight quotes in pasted text outside code and
/// URLs (its `createSmartQuotes`): `"` and `'` opening a word become “ and ‘,
/// closing ones ” and ’, an apostrophe inside or after a word ’, and a `'`
/// after a digit the prime ′. `text` stands for a message's running text,
/// with each code or URL character replaced by U+FFFC; the result has the
/// same characters apart from the quotes.
pub fn smart_quotes(text: &[char]) -> Vec<char> {
    // Letters count as word characters only in the scripts Slack checks.
    let letter = |c: Option<char>| {
        c.is_some_and(|c| c.is_alphabetic() && !('\u{ff00}'..='\u{ffef}').contains(&c))
    };
    let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
    let space = |c: Option<char>| c.is_none_or(char::is_whitespace);
    // Quotes, curled or not, are neither letters, digits nor spaces, so
    // each one's neighbours can be read from the input.
    let at = |ix: usize| text.get(ix).copied();
    let mut out = Vec::with_capacity(text.len());
    for (ix, &c) in text.iter().enumerate() {
        let prev = ix.checked_sub(1).and_then(at);
        let next = at(ix + 1);
        let opens = !letter(prev) && !digit(prev) && !space(next);
        out.push(match c {
            '"' if opens => '“',
            '"' if !digit(prev) => '”',
            '\'' if letter(prev) && letter(next) => '’',
            '\'' if opens => {
                if digit(next) && digit(at(ix + 2)) {
                    // '90s
                    '’'
                } else {
                    '‘'
                }
            }
            '\'' if letter(prev) && !digit(next) => '’',
            '\'' if digit(prev) => '′',
            c => c,
        });
    }
    out
}

/// How Slack shows a code block labelled `lang`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeLanguage {
    /// A language Slack highlights, shown by its name, with line numbers.
    Known(&'static str),
    /// Plain text (`text`, `plaintext`, `txt`, `plain`): the label as
    /// written, with line numbers.
    Plain,
    /// Anything else: the label as written, no line numbers, no colours.
    Unknown,
}

pub fn code_language(lang: &str) -> CodeLanguage {
    match crate::shiki::name(lang) {
        Some(name) => CodeLanguage::Known(name),
        None if matches!(lang, "text" | "plaintext" | "txt" | "plain") => CodeLanguage::Plain,
        None => CodeLanguage::Unknown,
    }
}

/// The colour classes of a diff line in Slack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRole {
    Deleted,
    Inserted,
    /// File names, git and command headers, and hunk ranges.
    Header,
    /// Punctuation such as `@@`, `---` and `!`.
    Muted,
}

/// The coloured byte ranges of `line` in a ```` ```diff ```` block, as
/// Slack's highlighter (shiki's TextMate diff grammar) colours them; the
/// rest of the line is plain. Comments (`#`) take the deleted colours, as
/// in Slack's theme.
pub fn diff_line(line: &str) -> Vec<(std::ops::Range<usize>, DiffRole)> {
    use DiffRole::*;
    let len = line.len();
    let is_range = |part: &str| {
        part.split(',')
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if matches!(
        line,
        "---"
            | "***************"
            | "==================================================================="
    ) {
        return vec![(0..len, Muted)];
    }
    // `5c5`, `12,14d11`: a normal diff's range.
    if let Some(ix) = line.find(['a', 'c', 'd'])
        && is_range(&line[..ix])
        && is_range(&line[ix + 1..])
    {
        return vec![(0..len, Header)];
    }
    // `@@ -1,2 +1,2 @@ context`: what follows the closing `@@` is plain.
    if line.starts_with("@@")
        && let Some(end) = line
            .get(3..)
            .and_then(|rest| rest.find("@@"))
            .map(|ix| ix + 3)
    {
        return vec![(0..2, Muted), (2..end, Header), (end..end + 2, Muted)];
    }
    // `--- 1,4 ----` and `*** 1,2 ****`: a context diff's range.
    for (open, close) in [("--- ", " ----"), ("*** ", " ****")] {
        if line.len() > open.len() + close.len() && line.starts_with(open) && line.ends_with(close)
        {
            return vec![(0..3, Muted), (3..len - 4, Header), (len - 4..len, Muted)];
        }
    }
    if line.strip_prefix("diff ").is_some_and(|rest| {
        rest.starts_with('-')
            || !rest.starts_with(char::is_whitespace) && rest.split_whitespace().nth(1).is_some()
    }) {
        return vec![(0..len, Header)];
    }
    if line.len() > 4 && ["--- ", "*** ", "+++ "].iter().any(|p| line.starts_with(p)) {
        return vec![(0..3, Muted), (3..len, Header)];
    }
    if line == ">" || line.starts_with("> ") || line.starts_with('+') {
        return vec![(0..len, Inserted)];
    }
    if line.starts_with('!') {
        return vec![(0..1, Muted)];
    }
    if line == "<" || line.starts_with("< ") || line.starts_with(['-', '#']) {
        return vec![(0..len, Deleted)];
    }
    if line.starts_with("Index: ") && line.len() > 7 {
        return vec![(5..6, Muted)];
    }
    Vec::new()
}
