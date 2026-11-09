//! Slack's own clipboard format: the composer's Quill delta, which Slack
//! copies and pastes as `slack/texty`. It is the only pasted form in which
//! user mentions stay mentions; HTML and plain text paste `<@U123>` as
//! text.
//!
//! Every line ends with a `\n` insert carrying the line's attributes (list,
//! indent, quote, code block), and inline inserts carry bold, italic,
//! strike, code, link and `slackmention`. Lines follow Slack's
//! newline-literal layout, as [`crate::rich`] does.

use std::fmt::Write;

use crate::{Block, Document, Span, rich, slack};

/// The `slack/texty` JSON for `doc`, or `None` when it mentions no users and
/// the HTML flavour pastes it as well.
pub fn texty(doc: &Document) -> Option<String> {
    let blocks: Vec<Block> = slack::posted(doc)
        .into_iter()
        .map(|(block, _)| block)
        .collect();
    let mut delta = Delta::default();
    delta.blocks(&blocks, &Line::default());
    delta
        .mentions
        .then(|| format!("{{\"ops\":[{}]}}", delta.ops))
}

/// Attributes of the lines a block is in.
#[derive(Clone, Default)]
struct Line {
    quote: bool,
    /// The list item a line starts: `bullet` or `ordered`, and its depth.
    list: Option<(&'static str, usize)>,
    code: bool,
}

#[derive(Default)]
struct Delta {
    ops: String,
    mentions: bool,
}

impl Delta {
    fn blocks(&mut self, blocks: &[Block], line: &Line) {
        for block in blocks {
            match block {
                Block::Paragraph(spans) => {
                    for spans in rich::lines(spans) {
                        self.spans(&spans);
                        self.end_line(line);
                    }
                }
                Block::List(list) => {
                    let depth = line.list.map_or(0, |(_, depth)| depth + 1);
                    let kind = if list.start.is_some() {
                        "ordered"
                    } else {
                        "bullet"
                    };
                    for item in &list.items {
                        let item_line = Line {
                            list: Some((kind, depth)),
                            ..line.clone()
                        };
                        if let Some(checked) = item.task {
                            self.insert(if checked { "☑ " } else { "☐ " }, "");
                        }
                        match item.blocks.split_first() {
                            Some((Block::Paragraph(spans), rest)) => {
                                self.spans(spans);
                                self.end_line(&item_line);
                                self.blocks(rest, &item_line);
                            }
                            _ => {
                                self.end_line(&item_line);
                                self.blocks(&item.blocks, &item_line);
                            }
                        }
                    }
                }
                Block::Quote(blocks) | Block::Alert { blocks, .. } => {
                    let quoted = Line {
                        quote: true,
                        ..line.clone()
                    };
                    if let Block::Alert { kind, .. } = block {
                        self.insert(kind.label(), "\"bold\":true");
                        self.end_line(&quoted);
                    }
                    self.blocks(blocks, &quoted);
                }
                Block::Code { text, .. } => {
                    let code = Line {
                        code: true,
                        ..line.clone()
                    };
                    for text in text.split('\n') {
                        self.insert(text, "");
                        self.end_line(&code);
                    }
                }
                Block::Rule => {
                    self.insert("———", "");
                    self.end_line(line);
                }
                Block::Html(html) => {
                    for text in html.split('\n') {
                        self.insert(text, "");
                        self.end_line(line);
                    }
                }
                Block::Heading { .. } => unreachable!("slack::posted replaces headings"),
                Block::Table(_) => unreachable!("slack::posted replaces tables"),
            }
        }
    }

    fn spans(&mut self, spans: &[Span]) {
        for span in spans {
            let mut attrs = String::new();
            let mut attr = |entry: &str| {
                if !attrs.is_empty() {
                    attrs.push(',');
                }
                attrs.push_str(entry);
            };
            if span.style.strong {
                attr("\"bold\":true");
            }
            if span.style.emph {
                attr("\"italic\":true");
            }
            if span.style.strike {
                attr("\"strike\":true");
            }
            if span.style.code {
                attr("\"code\":true");
            }
            if let Some(link) = &span.link {
                attr(&format!("\"link\":{}", json(link)));
            }
            if span.style.code || span.link.is_some() {
                self.insert(&span.text, &attrs);
                continue;
            }
            let mut text = span.text.as_str();
            while let Some(at) = text.find("<@") {
                match slack::user_mention(&text[at..]) {
                    Some((len, id, label)) => {
                        self.insert(&text[..at], &attrs);
                        let label = format!("@{}", label.unwrap_or(id));
                        let mention = format!(
                            "\"slackmention\":{{\"id\":{},\"label\":{},\"mention\":false,\"unverified\":false}}",
                            json(id),
                            json(&label),
                        );
                        let attrs = if attrs.is_empty() {
                            mention
                        } else {
                            format!("{attrs},{mention}")
                        };
                        self.insert(&label, &attrs);
                        self.mentions = true;
                        text = &text[at + len..];
                    }
                    None => {
                        self.insert(&text[..at + 2], &attrs);
                        text = &text[at + 2..];
                    }
                }
            }
            self.insert(text, &attrs);
        }
    }

    fn end_line(&mut self, line: &Line) {
        let mut attrs = Vec::new();
        if let Some((kind, depth)) = line.list {
            attrs.push(format!("\"list\":\"{kind}\""));
            if depth > 0 {
                attrs.push(format!("\"indent\":{depth}"));
            }
        }
        if line.quote {
            attrs.push("\"blockquote\":true".to_owned());
        }
        if line.code {
            attrs.push("\"code-block\":true".to_owned());
        }
        self.insert("\n", &attrs.join(","));
    }

    /// Appends an insert of `text` with `attrs`, the JSON object's members.
    fn insert(&mut self, text: &str, attrs: &str) {
        if text.is_empty() {
            return;
        }
        if !self.ops.is_empty() {
            self.ops.push(',');
        }
        self.ops.push('{');
        if !attrs.is_empty() {
            write!(self.ops, "\"attributes\":{{{attrs}}},").unwrap();
        }
        write!(self.ops, "\"insert\":{}}}", json(text)).unwrap();
    }
}

/// `text` as a JSON string.
fn json(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::texty;
    use crate::slack::parse;

    #[test]
    fn needs_a_user_mention() {
        assert_eq!(texty(&parse("*Hi* <https://example.com|there>\n")), None);
    }

    #[test]
    fn writes_mentions_and_formatting() {
        let doc = parse(
            "*Thanks* <@W010J2WH545> and <@U1|raine> · <https://example.com|Docs>\n\n\
             • one `code`\n\
             > quoted _note_\n",
        );
        assert_eq!(
            texty(&doc).unwrap(),
            concat!(
                r#"{"ops":[{"attributes":{"bold":true},"insert":"Thanks"},{"insert":" "},"#,
                r#"{"attributes":{"slackmention":{"id":"W010J2WH545","label":"@W010J2WH545","mention":false,"unverified":false}},"insert":"@W010J2WH545"},"#,
                r#"{"insert":" and "},"#,
                r#"{"attributes":{"slackmention":{"id":"U1","label":"@raine","mention":false,"unverified":false}},"insert":"@raine"},"#,
                r#"{"insert":" · "},{"attributes":{"link":"https://example.com"},"insert":"Docs"},"#,
                r#"{"insert":"\n"},{"insert":"\n"},"#,
                r#"{"insert":"one "},{"attributes":{"code":true},"insert":"code"},{"attributes":{"list":"bullet"},"insert":"\n"},"#,
                r#"{"insert":"quoted "},{"attributes":{"italic":true},"insert":"note"},{"attributes":{"blockquote":true},"insert":"\n"}]}"#,
            )
        );
    }
}
