//! Rich text for pasting a message into Slack's composer.
//!
//! Slack reads HTML from the pasteboard; RTF is for native apps such as Mail
//! and Notes, and [`plain`] for pasting without formatting. All three follow
//! Slack's newline-literal layout: every line of a paragraph, empty ones
//! included, is its own line, and blocks follow each other without extra
//! spacing. All three serialize [`slack::posted`], as the preview does.

use std::fmt::Write;

use crate::{Block, Document, List, Span, slack};

/// An HTML fragment of `doc`, with a charset declaration so pasting decodes
/// it as UTF-8.
pub fn html(doc: &Document) -> String {
    let mut out = String::from("<meta charset=\"utf-8\">");
    html_blocks(&mut out, &posted(doc));
    out
}

/// An RTF document of `doc`.
pub fn rtf(doc: &Document) -> String {
    let mut out = String::from(
        "{\\rtf1\\ansi\\ansicpg1252\\deff0\
         {\\fonttbl{\\f0\\fswiss Helvetica;}{\\f1\\fmodern Menlo;}}\n",
    );
    rtf_blocks(&mut out, &posted(doc), 0);
    out.push('}');
    out
}

/// The visible text of `doc` as Slack shows it: no markup, "•" and "N."
/// list markers, link labels, code as written. Slack's composer pastes
/// plain text that looks like Markdown as Markdown instead of reading the
/// HTML, so a zero-width no-break space (U+FEFF, whitespace to Slack's
/// detector) follows each `*`, `_`, `~`, `` ` `` and `]`, and leads each line
/// starting with one of `-*>#|` `` ` `` or a digit; no Markdown pattern Slack
/// looks for can match then.
pub fn plain(doc: &Document) -> String {
    let mut lines = Vec::new();
    plain_blocks(&mut lines, &posted(doc), "");
    let mut out = String::new();
    for (ix, line) in lines.iter().enumerate() {
        if ix > 0 {
            out.push('\n');
        }
        if line.starts_with(['-', '>', '#', '|', '*', '`'])
            || line.starts_with(|c: char| c.is_ascii_digit())
        {
            out.push(GUARD);
        }
        for ch in line.chars() {
            out.push(ch);
            if matches!(ch, '*' | '_' | '~' | '`' | ']') {
                out.push(GUARD);
            }
        }
    }
    out
}

/// Whitespace to Slack's Markdown detector, invisible when shown.
const GUARD: char = '\u{feff}';

fn plain_blocks(out: &mut Vec<String>, blocks: &[Block], indent: &str) {
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                for line in lines(spans) {
                    out.push(format!("{indent}{}", plain_spans(&line)));
                }
            }
            Block::List(list) => {
                for (ix, item) in list.items.iter().enumerate() {
                    let marker = slack::text_marker(list, ix);
                    if let Some(checked) = item.task {
                        out.push(format!(
                            "{indent}{marker}{}",
                            if checked { "☑ " } else { "☐ " }
                        ));
                    } else {
                        out.push(format!("{indent}{marker}"));
                    }
                    let first = out.len() - 1;
                    let nested = format!("{indent}    ");
                    let mut item_lines = Vec::new();
                    plain_blocks(&mut item_lines, &item.blocks, &nested);
                    let mut item_lines = item_lines.into_iter();
                    // The item's first paragraph line follows its marker.
                    if let Some(Block::Paragraph(_)) = item.blocks.first()
                        && let Some(line) = item_lines.next()
                    {
                        out[first].push_str(line.strip_prefix(nested.as_str()).unwrap_or(&line));
                    }
                    out.extend(item_lines);
                }
            }
            Block::Quote(blocks) | Block::Alert { blocks, .. } => plain_blocks(out, blocks, indent),
            Block::Code { text, .. } => {
                out.extend(text.split('\n').map(|line| format!("{indent}{line}")));
            }
            Block::Rule => out.push(format!("{indent}———")),
            Block::Html(html) => out.extend(html.split('\n').map(|line| format!("{indent}{line}"))),
            Block::Heading { .. } => unreachable!("slack::posted replaces headings"),
            Block::Table(_) => unreachable!("slack::posted replaces tables"),
        }
    }
}

fn plain_spans(spans: &[Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// `spans` split at each `\n` into lines.
fn lines(spans: &[Span]) -> Vec<Vec<Span>> {
    let mut lines = vec![Vec::new()];
    for span in spans {
        for (ix, text) in span.text.split('\n').enumerate() {
            if ix > 0 {
                lines.push(Vec::new());
            }
            crate::push_span(
                lines.last_mut().unwrap(),
                text,
                span.style,
                span.link.as_deref(),
            );
        }
    }
    lines
}

/// The top-level blocks Slack receives for `doc`.
fn posted(doc: &Document) -> Vec<Block> {
    slack::posted(doc)
        .into_iter()
        .map(|(block, _)| block)
        .collect()
}

fn html_blocks(out: &mut String, blocks: &[Block]) {
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                for line in lines(spans) {
                    html_line(out, &line);
                }
            }
            Block::Heading { .. } => unreachable!("slack::posted replaces headings"),
            Block::List(list) => html_list(out, list),
            Block::Quote(blocks) => {
                out.push_str("<blockquote>");
                html_blocks(out, blocks);
                out.push_str("</blockquote>");
            }
            Block::Alert { kind, blocks } => {
                write!(out, "<blockquote><p><b>{}</b></p>", kind.label()).unwrap();
                html_blocks(out, blocks);
                out.push_str("</blockquote>");
            }
            Block::Code { text, .. } => {
                out.push_str("<pre>");
                html_escape(out, text);
                out.push_str("</pre>");
            }
            Block::Table(_) => unreachable!("slack::posted replaces tables"),
            Block::Rule => out.push_str("<hr>"),
            Block::Html(html) => {
                for line in html.split('\n') {
                    out.push_str("<p>");
                    html_text(out, line, &mut true);
                    out.push_str("</p>");
                }
            }
        }
    }
}

/// One paragraph line; an empty line keeps its height with a `<br>`.
fn html_line(out: &mut String, spans: &[Span]) {
    out.push_str("<p>");
    if spans.is_empty() {
        out.push_str("<br>");
    }
    html_spans(out, spans);
    out.push_str("</p>");
}

fn html_list(out: &mut String, list: &List) {
    match list.start {
        None => out.push_str("<ul>"),
        Some(1) => out.push_str("<ol>"),
        Some(start) => write!(out, "<ol start=\"{start}\">").unwrap(),
    }
    for item in &list.items {
        out.push_str("<li>");
        if let Some(checked) = item.task {
            out.push_str(if checked { "☑ " } else { "☐ " });
        }
        // Item text goes straight into the `<li>`; a `<p>` would add a line.
        let mut first = true;
        for block in &item.blocks {
            match block {
                Block::Paragraph(spans) => {
                    for line in lines(spans) {
                        if !first {
                            out.push_str("<br>");
                        }
                        first = false;
                        html_spans(out, &line);
                    }
                }
                block => html_blocks(out, std::slice::from_ref(block)),
            }
        }
        out.push_str("</li>");
    }
    out.push_str(if list.start.is_some() {
        "</ol>"
    } else {
        "</ul>"
    });
}

/// Writes one line of spans.
fn html_spans(out: &mut String, spans: &[Span]) {
    let mut after_space = true;
    for span in spans {
        if let Some(link) = &span.link {
            out.push_str("<a href=\"");
            html_escape(out, link);
            out.push_str("\">");
        }
        let tags = [
            (span.style.strong, "b"),
            (span.style.emph, "i"),
            (span.style.strike, "s"),
            (span.style.code, "code"),
        ];
        for (_, tag) in tags.iter().filter(|(on, _)| *on) {
            write!(out, "<{tag}>").unwrap();
        }
        html_text(out, &span.text, &mut after_space);
        for (_, tag) in tags.iter().rev().filter(|(on, _)| *on) {
            write!(out, "</{tag}>").unwrap();
        }
        if span.link.is_some() {
            out.push_str("</a>");
        }
    }
}

/// Escapes `text` for HTML.
fn html_escape(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            ch => out.push(ch),
        }
    }
}

/// Escapes line text for HTML, writing spaces that HTML would collapse or
/// drop as non-breaking ones. `after_space` says whether the line so far is
/// empty or ends in a space.
fn html_text(out: &mut String, text: &str, after_space: &mut bool) {
    for (ix, part) in text.split(' ').enumerate() {
        if ix > 0 {
            out.push_str(if *after_space { "&nbsp;" } else { " " });
            *after_space = true;
        }
        if !part.is_empty() {
            html_escape(out, part);
            *after_space = false;
        }
    }
}

/// Indent per quote or list level, in twips.
const INDENT: u32 = 360;

/// Writes `blocks` indented by `indent` twips.
fn rtf_blocks(out: &mut String, blocks: &[Block], indent: u32) {
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                for line in lines(spans) {
                    write!(out, "\\pard\\li{indent} ").unwrap();
                    rtf_spans(out, &line);
                    out.push_str("\\par\n");
                }
            }
            Block::Heading { .. } => unreachable!("slack::posted replaces headings"),
            Block::List(list) => rtf_list(out, list, indent),
            Block::Quote(blocks) => rtf_blocks(out, blocks, indent + INDENT),
            Block::Alert { kind, blocks } => {
                let indent = indent + INDENT;
                writeln!(out, "\\pard\\li{indent} {{\\b {}}}\\par", kind.label()).unwrap();
                rtf_blocks(out, blocks, indent);
            }
            Block::Code { text, .. } => {
                for line in text.split('\n') {
                    write!(out, "\\pard\\li{indent} {{\\f1 ").unwrap();
                    rtf_escape(out, line);
                    out.push_str("}\\par\n");
                }
            }
            Block::Table(_) => unreachable!("slack::posted replaces tables"),
            Block::Rule => {
                writeln!(
                    out,
                    "\\pard\\li{indent}\\brdrb\\brdrs\\brdrw10\\brsp20 \\par"
                )
                .unwrap();
            }
            Block::Html(html) => {
                for line in html.split('\n') {
                    write!(out, "\\pard\\li{indent} ").unwrap();
                    rtf_escape(out, line);
                    out.push_str("\\par\n");
                }
            }
        }
    }
}

fn rtf_list(out: &mut String, list: &List, indent: u32) {
    let indent = indent + INDENT;
    for (ix, item) in list.items.iter().enumerate() {
        let mut marker = match list.start {
            None => "•".to_owned(),
            Some(start) => format!("{}.", start + ix as u64),
        };
        if let Some(checked) = item.task {
            marker.push_str(if checked { " ☑" } else { " ☐" });
        }
        // The first line hangs the marker in the indent; later lines of the
        // item's text line up with the first.
        let mut first = true;
        for block in &item.blocks {
            match block {
                Block::Paragraph(spans) => {
                    for line in lines(spans) {
                        write!(out, "\\pard\\li{indent}\\fi-{INDENT}\\tx{indent} ").unwrap();
                        if first {
                            rtf_escape(out, &marker);
                        }
                        first = false;
                        out.push_str("\\tab ");
                        rtf_spans(out, &line);
                        out.push_str("\\par\n");
                    }
                }
                block => rtf_blocks(out, std::slice::from_ref(block), indent),
            }
        }
    }
}

fn rtf_spans(out: &mut String, spans: &[Span]) {
    for span in spans {
        if let Some(link) = &span.link {
            out.push_str("{\\field{\\*\\fldinst{HYPERLINK \"");
            rtf_escape(out, &link.replace('"', "%22"));
            out.push_str("\"}}{\\fldrslt ");
        }
        out.push('{');
        let words = [
            (span.style.strong, "\\b"),
            (span.style.emph, "\\i"),
            (span.style.strike, "\\strike"),
            (span.style.code, "\\f1"),
        ];
        let mut styled = false;
        for (_, word) in words.iter().filter(|(on, _)| *on) {
            out.push_str(word);
            styled = true;
        }
        // Ends the last control word; a space after `{` would be text.
        if styled {
            out.push(' ');
        }
        rtf_escape(out, &span.text);
        out.push('}');
        if span.link.is_some() {
            out.push_str("}}");
        }
    }
}

/// Escapes `text` for RTF: control characters are backslashed and anything
/// outside ASCII becomes `\uN?` UTF-16 code units.
fn rtf_escape(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '\\' | '{' | '}' => {
                out.push('\\');
                out.push(ch);
            }
            '\t' => out.push_str("\\tab "),
            ' '..='~' => out.push(ch),
            ch => {
                let mut units = [0; 2];
                for unit in ch.encode_utf16(&mut units) {
                    write!(out, "\\u{}?", *unit as i16).unwrap();
                }
            }
        }
    }
}
