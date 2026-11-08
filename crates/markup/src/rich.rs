//! Rich text for pasting a message into Slack's composer.
//!
//! Slack reads HTML from the pasteboard; RTF is for native apps such as Mail
//! and Notes. Both follow Slack's newline-literal layout: every line of a
//! paragraph, empty ones included, is its own line, and blocks follow each
//! other without extra spacing. Blocks Slack can't show are replaced by
//! [`slack::fallback`], as in the preview.

use std::borrow::Cow;
use std::fmt::Write;

use crate::{Block, Document, List, Span, slack};

/// An HTML fragment of `doc`, with a charset declaration so pasting decodes
/// it as UTF-8.
pub fn html(doc: &Document) -> String {
    let mut out = String::from("<meta charset=\"utf-8\">");
    html_blocks(&mut out, &doc.blocks);
    out
}

/// An RTF document of `doc`.
pub fn rtf(doc: &Document) -> String {
    let mut out = String::from(
        "{\\rtf1\\ansi\\ansicpg1252\\deff0\
         {\\fonttbl{\\f0\\fswiss Helvetica;}{\\f1\\fmodern Menlo;}}\n",
    );
    rtf_blocks(&mut out, &doc.blocks, 0);
    out.push('}');
    out
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

/// The block Slack receives in place of `block`.
fn for_slack(block: &Block) -> Cow<'_, Block> {
    match slack::fallback(block) {
        Some((replacement, _)) => Cow::Owned(replacement),
        None => Cow::Borrowed(block),
    }
}

fn html_blocks(out: &mut String, blocks: &[Block]) {
    for block in blocks {
        match &*for_slack(block) {
            Block::Paragraph(spans) => {
                for line in lines(spans) {
                    html_line(out, &line);
                }
            }
            Block::Heading { .. } => unreachable!("slack::fallback replaces headings"),
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
            Block::Table(_) => unreachable!("slack::fallback replaces tables"),
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
        match &*for_slack(block) {
            Block::Paragraph(spans) => {
                for line in lines(spans) {
                    write!(out, "\\pard\\li{indent} ").unwrap();
                    rtf_spans(out, &line);
                    out.push_str("\\par\n");
                }
            }
            Block::Heading { .. } => unreachable!("slack::fallback replaces headings"),
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
            Block::Table(_) => unreachable!("slack::fallback replaces tables"),
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
