//! GitHub-flavored markdown parser.
//!
//! Follows GitHub's comment rendering: single newlines inside a paragraph are
//! line breaks, HTML comments are hidden, and other HTML is kept literally.

use comrak::nodes::{AstNode, ListType, NodeValue, TableAlignment};
use comrak::{Arena, Options, parse_document};

use crate::{AlertKind, Alignment, Block, Document, List, ListItem, Span, SpanStyle, Table};

pub fn parse(markdown: &str) -> Document {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.alerts = true;
    let root = parse_document(&arena, markdown, &options);
    Document {
        blocks: blocks(root),
    }
}

fn blocks<'a>(parent: &'a AstNode<'a>) -> Vec<Block> {
    parent.children().filter_map(block).collect()
}

fn block<'a>(node: &'a AstNode<'a>) -> Option<Block> {
    let block = match &node.data().value {
        NodeValue::Paragraph => Block::Paragraph(spans(node)),
        NodeValue::Heading(heading) => Block::Heading {
            level: heading.level,
            spans: spans(node),
        },
        NodeValue::List(list) => Block::List(List {
            start: (list.list_type == ListType::Ordered).then_some(list.start as u64),
            tight: list.tight,
            items: node.children().map(list_item).collect(),
        }),
        NodeValue::BlockQuote => Block::Quote(blocks(node)),
        NodeValue::Alert(alert) => {
            let kind = match alert.alert_type {
                comrak::nodes::AlertType::Note => AlertKind::Note,
                comrak::nodes::AlertType::Tip => AlertKind::Tip,
                comrak::nodes::AlertType::Important => AlertKind::Important,
                comrak::nodes::AlertType::Warning => AlertKind::Warning,
                comrak::nodes::AlertType::Caution => AlertKind::Caution,
            };
            match &alert.title {
                None => Block::Alert {
                    kind,
                    blocks: blocks(node),
                },
                // GitHub only recognizes the marker alone on its line; with
                // text after it the quote stays a plain quote.
                Some(title) => {
                    let marker = format!("[!{}] {title}", kind.label().to_uppercase());
                    let mut quoted = vec![Block::Paragraph(vec![plain(marker)])];
                    quoted.extend(blocks(node));
                    Block::Quote(quoted)
                }
            }
        }
        NodeValue::CodeBlock(code) => Block::Code {
            lang: code.info.split_whitespace().next().map(str::to_owned),
            text: code
                .literal
                .strip_suffix('\n')
                .unwrap_or(&code.literal)
                .to_owned(),
        },
        NodeValue::Table(table) => {
            let mut header = Vec::new();
            let mut rows = Vec::new();
            for row in node.children() {
                let cells = row.children().map(spans).collect();
                match row.data().value {
                    NodeValue::TableRow(true) => header = cells,
                    _ => rows.push(cells),
                }
            }
            Block::Table(Table {
                alignments: table.alignments.iter().map(alignment).collect(),
                header,
                rows,
            })
        }
        NodeValue::ThematicBreak => Block::Rule,
        NodeValue::HtmlBlock(html) if is_html_comment(&html.literal) => return None,
        NodeValue::HtmlBlock(html) => Block::Html(html.literal.trim_end().to_owned()),
        _ => return None,
    };
    Some(block)
}

fn list_item<'a>(node: &'a AstNode<'a>) -> ListItem {
    let task = match &node.data().value {
        NodeValue::TaskItem(task) => Some(task.symbol.is_some()),
        _ => None,
    };
    ListItem {
        task,
        blocks: blocks(node),
    }
}

fn alignment(alignment: &TableAlignment) -> Alignment {
    match alignment {
        TableAlignment::None => Alignment::None,
        TableAlignment::Left => Alignment::Left,
        TableAlignment::Center => Alignment::Center,
        TableAlignment::Right => Alignment::Right,
    }
}

fn is_html_comment(html: &str) -> bool {
    html.trim_start().starts_with("<!--")
}

fn plain(text: String) -> Span {
    Span {
        text,
        style: SpanStyle::default(),
        link: None,
    }
}

fn spans<'a>(node: &'a AstNode<'a>) -> Vec<Span> {
    let mut out = Vec::new();
    collect_spans(node, SpanStyle::default(), None, &mut out);
    out
}

fn collect_spans<'a>(
    parent: &'a AstNode<'a>,
    style: SpanStyle,
    link: Option<&str>,
    out: &mut Vec<Span>,
) {
    for node in parent.children() {
        match &node.data().value {
            NodeValue::Text(text) => push(out, text, style, link),
            NodeValue::SoftBreak | NodeValue::LineBreak => push(out, "\n", style, link),
            NodeValue::Code(code) => push(
                out,
                &code.literal,
                SpanStyle {
                    code: true,
                    ..style
                },
                link,
            ),
            NodeValue::HtmlInline(html) if is_html_comment(html) => {}
            NodeValue::HtmlInline(html) => push(out, html, style, link),
            NodeValue::Emph => collect_spans(
                node,
                SpanStyle {
                    emph: true,
                    ..style
                },
                link,
                out,
            ),
            NodeValue::Strong => collect_spans(
                node,
                SpanStyle {
                    strong: true,
                    ..style
                },
                link,
                out,
            ),
            NodeValue::Strikethrough => collect_spans(
                node,
                SpanStyle {
                    strike: true,
                    ..style
                },
                link,
                out,
            ),
            // Images are not loaded; the alt text links to the image unless
            // an enclosing link already applies, as with linked badges.
            NodeValue::Link(target) | NodeValue::Image(target) => {
                collect_spans(node, style, Some(link.unwrap_or(&target.url)), out)
            }
            _ => collect_spans(node, style, link, out),
        }
    }
}

fn push(out: &mut Vec<Span>, text: &str, style: SpanStyle, link: Option<&str>) {
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
