//! Rich text for pasting a Markdown draft into mail clients, documents and
//! other rich-text editors.
//!
//! [`pasted`] is the document the preview shows and [`html`] and [`rtf`]
//! serialize: GitHub-only syntax with no rich-text equivalent becomes
//! ordinary text. The HTML styles code, quotes and tables inline, as mail
//! clients drop style sheets; the RTF is for native apps that read no HTML.

use std::fmt::Write;

use crate::rich::{html_escape, lines, rtf_escape, rtf_spans};
use crate::{Alignment, Block, Document, List, ListItem, Span, SpanStyle, Table};

/// `doc` as it pastes: alerts become quotes led by their bold label, task
/// list items lead with ☐ or ☑, and raw HTML is literal text.
pub fn pasted(doc: &Document) -> Document {
    Document {
        blocks: doc.blocks.iter().map(pasted_block).collect(),
    }
}

fn pasted_block(block: &Block) -> Block {
    match block {
        Block::List(list) => Block::List(List {
            items: list.items.iter().map(pasted_item).collect(),
            ..list.clone()
        }),
        Block::Quote(blocks) => Block::Quote(blocks.iter().map(pasted_block).collect()),
        Block::Alert { kind, blocks } => {
            let label = text_span(kind.label(), true);
            let mut quoted = vec![Block::Paragraph(vec![label])];
            quoted.extend(blocks.iter().map(pasted_block));
            Block::Quote(quoted)
        }
        Block::Html(html) => Block::Paragraph(vec![text_span(html, false)]),
        block => block.clone(),
    }
}

fn pasted_item(item: &ListItem) -> ListItem {
    let mut blocks: Vec<Block> = item.blocks.iter().map(pasted_block).collect();
    if let Some(checked) = item.task {
        let mark = text_span(if checked { "☑ " } else { "☐ " }, false);
        match blocks.first_mut() {
            Some(Block::Paragraph(spans)) => spans.insert(0, mark),
            _ => blocks.insert(0, Block::Paragraph(vec![mark])),
        }
    }
    ListItem { task: None, blocks }
}

fn text_span(text: &str, strong: bool) -> Span {
    Span {
        text: text.to_owned(),
        style: SpanStyle {
            strong,
            ..SpanStyle::default()
        },
        link: None,
    }
}

/// An HTML fragment of `doc`, with a charset declaration so pasting decodes
/// it as UTF-8.
pub fn html(doc: &Document) -> String {
    let mut out = String::from("<meta charset=\"utf-8\">");
    html_blocks(&mut out, &pasted(doc).blocks);
    out
}

const MONO: &str = "font-family:Menlo,Consolas,monospace";
const BORDER: &str = "1px solid #d0d7de";

fn html_blocks(out: &mut String, blocks: &[Block]) {
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                out.push_str("<p>");
                html_spans(out, spans);
                out.push_str("</p>");
            }
            Block::Heading { level, spans } => {
                write!(out, "<h{level}>").unwrap();
                html_spans(out, spans);
                write!(out, "</h{level}>").unwrap();
            }
            Block::List(list) => html_list(out, list),
            Block::Quote(blocks) => {
                out.push_str(
                    "<blockquote style=\"margin:0;border-left:3px solid #d0d7de;padding-left:8px\">",
                );
                html_blocks(out, blocks);
                out.push_str("</blockquote>");
            }
            Block::Code { text, .. } => {
                write!(
                    out,
                    "<pre style=\"{MONO};font-size:90%;border:{BORDER};border-radius:4px;padding:8px;white-space:pre-wrap\">"
                )
                .unwrap();
                html_escape(out, text);
                out.push_str("</pre>");
            }
            Block::Table(table) => html_table(out, table),
            Block::Rule => out.push_str("<hr>"),
            Block::Alert { .. } | Block::Html(_) => unreachable!("pasted replaces {block:?}"),
        }
    }
}

fn html_list(out: &mut String, list: &List) {
    match list.start {
        None => out.push_str("<ul>"),
        Some(1) => out.push_str("<ol>"),
        Some(start) => write!(out, "<ol start=\"{start}\">").unwrap(),
    }
    for item in &list.items {
        out.push_str("<li>");
        for (ix, block) in item.blocks.iter().enumerate() {
            match block {
                // A tight item's text goes straight into the `<li>`; a `<p>`
                // would space the items apart.
                Block::Paragraph(spans) if list.tight => {
                    if ix > 0 {
                        out.push_str("<br>");
                    }
                    html_spans(out, spans);
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

fn html_table(out: &mut String, table: &Table) {
    out.push_str("<table style=\"border-collapse:collapse\"><thead><tr>");
    html_row(out, "th", &table.header, &table.alignments);
    out.push_str("</tr></thead><tbody>");
    for row in &table.rows {
        out.push_str("<tr>");
        html_row(out, "td", row, &table.alignments);
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
}

fn html_row(out: &mut String, tag: &str, cells: &[Vec<Span>], alignments: &[Alignment]) {
    for (cell, alignment) in cells.iter().zip(alignments) {
        // Without an alignment, header cells keep the browser's centering.
        let align = match alignment {
            Alignment::None => "",
            Alignment::Left => ";text-align:left",
            Alignment::Center => ";text-align:center",
            Alignment::Right => ";text-align:right",
        };
        write!(
            out,
            "<{tag} style=\"border:{BORDER};padding:4px 10px{align}\">"
        )
        .unwrap();
        html_spans(out, cell);
        write!(out, "</{tag}>").unwrap();
    }
}

/// Writes inline content; each `\n` is a `<br>`.
fn html_spans(out: &mut String, spans: &[Span]) {
    for span in spans {
        if let Some(link) = &span.link {
            out.push_str("<a href=\"");
            html_escape(out, link);
            out.push_str("\">");
        }
        let tags = [
            (span.style.strong, "b", "b"),
            (span.style.emph, "i", "i"),
            (span.style.strike, "s", "s"),
            (
                span.style.code,
                "code style=\"font-family:Menlo,Consolas,monospace\"",
                "code",
            ),
        ];
        for (_, open, _) in tags.iter().filter(|(on, ..)| *on) {
            write!(out, "<{open}>").unwrap();
        }
        for (ix, text) in span.text.split('\n').enumerate() {
            if ix > 0 {
                out.push_str("<br>");
            }
            html_escape(out, text);
        }
        for (.., close) in tags.iter().rev().filter(|(on, ..)| *on) {
            write!(out, "</{close}>").unwrap();
        }
        if span.link.is_some() {
            out.push_str("</a>");
        }
    }
}

/// An RTF document of `doc`.
pub fn rtf(doc: &Document) -> String {
    let mut out = String::from(
        "{\\rtf1\\ansi\\ansicpg1252\\deff0\
         {\\fonttbl{\\f0\\fswiss Helvetica;}{\\f1\\fmodern Menlo;}}\n",
    );
    rtf_blocks(&mut out, &pasted(doc).blocks, 0);
    out.push('}');
    out
}

/// Indent per quote or list level, in twips.
const INDENT: u32 = 360;
/// Space after a block, in twips.
const GAP: u32 = 180;
/// Space after a list item, in twips.
const ITEM_GAP: u32 = 60;
/// Width a table's columns share, in twips.
const TABLE_WIDTH: u32 = 9000;

/// Writes `blocks` indented by `indent` twips.
fn rtf_blocks(out: &mut String, blocks: &[Block], indent: u32) {
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                write!(out, "\\pard\\li{indent}\\sa{GAP} ").unwrap();
                rtf_lines(out, spans);
                out.push_str("\\par\n");
            }
            Block::Heading { level, spans } => {
                // Half-points.
                let size = match level {
                    1 => 36,
                    2 => 32,
                    3 => 28,
                    _ => 24,
                };
                write!(out, "\\pard\\li{indent}\\sb{GAP}\\sa{GAP} {{\\b\\fs{size} ").unwrap();
                rtf_lines(out, spans);
                out.push_str("}\\par\n");
            }
            Block::List(list) => rtf_list(out, list, indent),
            Block::Quote(blocks) => rtf_blocks(out, blocks, indent + INDENT),
            Block::Code { text, .. } => {
                write!(out, "\\pard\\li{indent}\\sa{GAP} {{\\f1\\fs20 ").unwrap();
                for (ix, line) in text.split('\n').enumerate() {
                    if ix > 0 {
                        out.push_str("\\line ");
                    }
                    rtf_escape(out, line);
                }
                out.push_str("}\\par\n");
            }
            Block::Table(table) => rtf_table(out, table, indent),
            Block::Rule => {
                writeln!(
                    out,
                    "\\pard\\li{indent}\\sa{GAP}\\brdrb\\brdrs\\brdrw10\\brsp20 \\par"
                )
                .unwrap();
            }
            Block::Alert { .. } | Block::Html(_) => unreachable!("pasted replaces {block:?}"),
        }
    }
}

fn rtf_list(out: &mut String, list: &List, indent: u32) {
    let indent = indent + INDENT;
    for (ix, item) in list.items.iter().enumerate() {
        let marker = match list.start {
            None => "•".to_owned(),
            Some(start) => format!("{}.", start + ix as u64),
        };
        // The first paragraph hangs the marker in the indent; later ones
        // line up with its text.
        let mut first = true;
        for block in &item.blocks {
            match block {
                Block::Paragraph(spans) => {
                    write!(
                        out,
                        "\\pard\\li{indent}\\fi-{INDENT}\\tx{indent}\\sa{ITEM_GAP} "
                    )
                    .unwrap();
                    if first {
                        rtf_escape(out, &marker);
                    }
                    first = false;
                    out.push_str("\\tab ");
                    rtf_lines(out, spans);
                    out.push_str("\\par\n");
                }
                block => rtf_blocks(out, std::slice::from_ref(block), indent),
            }
        }
    }
}

fn rtf_table(out: &mut String, table: &Table, indent: u32) {
    let columns = table.alignments.len().max(1) as u32;
    let width = TABLE_WIDTH / columns;
    let rows =
        std::iter::once((&table.header, true)).chain(table.rows.iter().map(|row| (row, false)));
    for (cells, header) in rows {
        write!(out, "\\trowd\\trgaph108\\trleft{indent}").unwrap();
        for column in 1..=columns {
            for side in ["t", "l", "b", "r"] {
                write!(out, "\\clbrdr{side}\\brdrs\\brdrw10").unwrap();
            }
            write!(out, "\\cellx{}", indent + column * width).unwrap();
        }
        out.push('\n');
        for (cell, alignment) in cells.iter().zip(&table.alignments) {
            let align = match alignment {
                Alignment::None | Alignment::Left => "\\ql",
                Alignment::Center => "\\qc",
                Alignment::Right => "\\qr",
            };
            write!(out, "\\pard\\intbl{align} {{").unwrap();
            if header {
                out.push_str("\\b ");
            }
            rtf_lines(out, cell);
            out.push_str("}\\cell\n");
        }
        out.push_str("\\row\n");
    }
    // An empty line spaces the table from what follows.
    writeln!(out, "\\pard\\li{indent}\\sa0 \\par").unwrap();
}

/// Writes inline content; each `\n` is an RTF line break.
fn rtf_lines(out: &mut String, spans: &[Span]) {
    for (ix, line) in lines(spans).iter().enumerate() {
        if ix > 0 {
            out.push_str("\\line ");
        }
        rtf_spans(out, line);
    }
}
