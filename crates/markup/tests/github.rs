//! Structural snapshots of the block tree produced by the GitHub parser.

use std::fmt::Write;

use markup::{Block, Document, Span, github};

/// Renders a document as an indented outline, one block or span per line.
fn outline(doc: &Document) -> String {
    let mut out = String::new();
    write_blocks(&mut out, &doc.blocks, 0);
    out
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

fn snapshot(markdown: &str) -> String {
    outline(&github::parse(markdown))
}

#[test]
fn headings() {
    insta::assert_snapshot!(snapshot(
        "# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six\n\nSetext\n===\n"
    ));
}

#[test]
fn inline_styles() {
    insta::assert_snapshot!(snapshot(
        "plain **bold** *italic* ***both*** ~~gone~~ `code` **bold `code` inside**\n"
    ));
}

#[test]
fn line_breaks() {
    insta::assert_snapshot!(snapshot(
        "soft\nbreak\n\nhard  \nbreak\n\nbackslash\\\nbreak\n"
    ));
}

#[test]
fn links() {
    insta::assert_snapshot!(snapshot(
        "[**bold** label](https://a.example) and [ref][r] and <https://b.example>\n\n\
         [r]: https://ref.example\n"
    ));
}

#[test]
fn autolinks() {
    insta::assert_snapshot!(snapshot(
        "see https://example.com/path. or www.example.com, mail a@example.com\n"
    ));
}

#[test]
fn images() {
    insta::assert_snapshot!(snapshot(
        "![alt text](https://img.example/a.png)\n\n\
         [![CI](https://img.example/badge.svg)](https://ci.example)\n"
    ));
}

#[test]
fn entities_and_unicode() {
    insta::assert_snapshot!(snapshot("&amp; &copy; \\*not em\\* 日本語 — ünïcode\n"));
}

#[test]
fn bullet_and_ordered_lists() {
    insta::assert_snapshot!(snapshot(
        "- one\n- two\n  - nested\n    1. deep\n\n3. three\n4. four\n"
    ));
}

#[test]
fn loose_list() {
    insta::assert_snapshot!(snapshot(
        "- first\n\n  second paragraph\n\n- item\n\n  ```\n  code in item\n  ```\n"
    ));
}

#[test]
fn task_lists() {
    insta::assert_snapshot!(snapshot(
        "- [x] done\n- [ ] todo\n- plain\n  - [X] nested done\n\n> - [ ] in quote\n"
    ));
}

#[test]
fn quotes() {
    insta::assert_snapshot!(snapshot("> quoted\nlazy continuation\n>\n> > nested\n"));
}

#[test]
fn alerts() {
    insta::assert_snapshot!(snapshot(
        "> [!NOTE]\n> Note body\n\n\
         > [!TIP]\n> Tip body\n\n\
         > [!IMPORTANT]\n> Important body\n\n\
         > [!WARNING]\n> Warning body\n\n\
         > [!CAUTION]\n> Caution **body**\n> - list in alert\n"
    ));
}

#[test]
fn non_alert_markers_stay_quotes() {
    insta::assert_snapshot!(snapshot(
        "> [!FOO]\n> unknown kind\n\n> [!NOTE] Custom title\n> body\n"
    ));
}

#[test]
fn code_blocks() {
    insta::assert_snapshot!(snapshot(
        "```rust title=x\nfn main() {\n    println!(\"hi\");\n}\n```\n\n\
         ```\nno lang\n\n```\n\n    indented code\n\n```\nunclosed fence\n"
    ));
}

#[test]
fn tables() {
    insta::assert_snapshot!(snapshot(
        "| Left | Center | Right | None |\n|:-----|:------:|------:|------|\n\
         | `a\\|b` | **c** | 3 | |\n| ragged |\n"
    ));
}

#[test]
fn rule_and_html() {
    insta::assert_snapshot!(snapshot(
        "above\n\n---\n\n<details>\n<summary>More</summary>\n</details>\n\n\
         <!-- template comment -->\n\ninline <b>tag</b> and <!-- hidden --> comment\n"
    ));
}

#[test]
fn empty_document() {
    insta::assert_snapshot!(snapshot(""));
}
