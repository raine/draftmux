//! Outline format shared by the parser snapshot tests.

use std::fmt::Write;

use markup::{Block, Document, Span, slack};

/// Renders a document as an indented outline, one block or span per line.
pub fn outline(doc: &Document) -> String {
    let mut out = String::new();
    write_blocks(&mut out, &doc.blocks, 0);
    out
}

/// The outline of what Slack posts for `doc`, with a line for each block's
/// note.
#[allow(dead_code)]
pub fn posted(doc: &Document) -> String {
    slack::posted(doc)
        .into_iter()
        .map(|(block, note)| {
            let note = note
                .map(|note| format!("note {note:?}\n"))
                .unwrap_or_default();
            let outline = outline(&Document {
                blocks: vec![block],
            });
            format!("{note}{outline}")
        })
        .collect()
}

fn write_blocks(out: &mut String, blocks: &[Block], depth: usize) {
    for block in blocks {
        write_block(out, block, depth);
    }
}

fn write_block(out: &mut String, block: &Block, depth: usize) {
    let pad = "  ".repeat(depth);
    match block {
        Block::Paragraph(spans) => {
            writeln!(out, "{pad}paragraph").unwrap();
            write_spans(out, spans, depth + 1);
        }
        Block::Heading { level, spans } => {
            writeln!(out, "{pad}heading {level}").unwrap();
            write_spans(out, spans, depth + 1);
        }
        Block::List(list) => {
            let kind = match list.start {
                Some(start) => format!("ordered start={start}"),
                None => "bullet".to_owned(),
            };
            let tight = if list.tight { "tight" } else { "loose" };
            writeln!(out, "{pad}list {kind} {tight}").unwrap();
            for item in &list.items {
                let task = match item.task {
                    Some(true) => " [x]",
                    Some(false) => " [ ]",
                    None => "",
                };
                writeln!(out, "{pad}  item{task}").unwrap();
                write_blocks(out, &item.blocks, depth + 2);
            }
        }
        Block::Quote(blocks) => {
            writeln!(out, "{pad}quote").unwrap();
            write_blocks(out, blocks, depth + 1);
        }
        Block::Alert { kind, blocks } => {
            writeln!(out, "{pad}alert {kind:?}").unwrap();
            write_blocks(out, blocks, depth + 1);
        }
        Block::Code { lang, text } => {
            writeln!(out, "{pad}code lang={lang:?} {text:?}").unwrap();
        }
        Block::Table(table) => {
            writeln!(out, "{pad}table {:?}", table.alignments).unwrap();
            writeln!(out, "{pad}  header").unwrap();
            write_cells(out, &table.header, depth + 2);
            for row in &table.rows {
                writeln!(out, "{pad}  row").unwrap();
                write_cells(out, row, depth + 2);
            }
        }
        Block::Rule => writeln!(out, "{pad}rule").unwrap(),
        Block::Html(html) => writeln!(out, "{pad}html {html:?}").unwrap(),
    }
}

fn write_cells(out: &mut String, cells: &[Vec<Span>], depth: usize) {
    for cell in cells {
        writeln!(out, "{}cell", "  ".repeat(depth)).unwrap();
        write_spans(out, cell, depth + 1);
    }
}

fn write_spans(out: &mut String, spans: &[Span], depth: usize) {
    for span in spans {
        let pad = "  ".repeat(depth);
        let style = span.style;
        let marks: Vec<&str> = [
            (style.strong, "strong"),
            (style.emph, "emph"),
            (style.strike, "strike"),
            (style.code, "code"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        write!(out, "{pad}{:?}", span.text).unwrap();
        if !marks.is_empty() {
            write!(out, " {}", marks.join("+")).unwrap();
        }
        if let Some(link) = &span.link {
            write!(out, " -> {link}").unwrap();
        }
        out.push('\n');
    }
}
