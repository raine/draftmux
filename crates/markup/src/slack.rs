//! Slack mrkdwn parser.
//!
//! Inline syntax follows Slack's mrkdwn: `*bold*`, `_italic_`, `~strike~`,
//! `` `code` ``, `<url|label>` and bare web URLs, with `&amp;`, `&lt;` and
//! `&gt;` decoded. `:shortcode:`s Slack knows, outside code and link labels,
//! become the Unicode emoji Slack shows. Delimiters only apply at word
//! boundaries and never span a line, so `snake_case` and `2*3*4` stay
//! literal. Blocks follow what Slack's
//! composer shows: ```` ``` ```` code blocks (which may open and close
//! mid-line), `>` quotes holding one paragraph, and `-`, `*`, `•` or `1.`
//! lists nested by indentation.
//!
//! Slack text is newline-literal, so blank lines are kept: consecutive text
//! lines, blank ones included, form one paragraph whose text holds every `\n`.
//! A run of blank lines between two other blocks becomes a paragraph of empty
//! lines.
//!
//! Markdown headings and pipe tables have no Slack equivalent but are kept as
//! [`Block::Heading`] and [`Block::Table`]; [`fallback`] gives the block Slack
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

/// The block Slack receives in place of `block`, with a short note on what
/// changes, for blocks Slack can't show: headings become a bold line and
/// tables a code block.
pub fn fallback(block: &Block) -> Option<(Block, &'static str)> {
    match block {
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
            Some((
                Block::Paragraph(spans),
                "Slack has no headings, so this pastes as a bold line.",
            ))
        }
        Block::Table(table) => Some((
            Block::Code {
                lang: None,
                text: table_text(table),
            },
            "Slack has no tables, so this pastes as a code block.",
        )),
        _ => None,
    }
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
        let body = body.strip_prefix('\n').unwrap_or(body);
        let body = body.strip_suffix('\n').unwrap_or(body);
        self.push_block(Block::Code {
            lang: None,
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
            'h' if !preceded_by_alnum(text, ix) => bare_url(rest).map(|url| {
                flush(out);
                push_span(out, url, style, Some(url));
                url.len()
            }),
            ':' => shortcode(rest).map(|(len, emoji)| {
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

/// Length and emoji of a `:shortcode:` Slack knows at the start of `text`,
/// in any case. A `:skin-tone-N:` right after one is its own shortcode, the
/// modifier that combines with it.
fn shortcode(text: &str) -> Option<(usize, &'static str)> {
    let rest = text.strip_prefix(':')?;
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || "_+-'".contains(c)))?;
    if end == 0 || !rest[end..].starts_with(':') {
        return None;
    }
    crate::emoji::lookup(&rest[..end].to_ascii_lowercase()).map(|emoji| (end + 2, emoji))
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
    let scheme = url.split_once(':')?.0;
    let valid = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        && !url.contains(char::is_whitespace);
    let label = if label.is_empty() { url } else { label };
    valid.then_some((end + 2, url, label))
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
