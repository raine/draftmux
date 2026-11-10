//! Slack mrkdwn, the inline syntax of Slack drafts that have no recorded
//! syntax, and its rewrite to Markdown.

use super::{
    Grammar, InlineRange, angle_link, bare_url, code_span, escape, inline, markdown_link,
    parse_with, preceded_by_alnum, run,
};

/// `mrkdwn` rewritten as Markdown that parses to the same document: `*bold*`
/// becomes `**bold**`, `~strike~` `~~strike~~`, `<url|label>` `[label](url)`
/// when the label and URL fit that form, and delimiters and backslashes that
/// would start Markdown syntax are escaped. Code, link labels and URLs, and
/// all block syntax, are left as written. Line endings become `\n`.
///
/// Each line is checked against its mrkdwn reading. Only contrived text,
/// such as a URL ending in a backslash before a closing delimiter, reads
/// differently.
pub fn from_mrkdwn(mrkdwn: &str) -> String {
    let text = mrkdwn.replace("\r\n", "\n");
    let (_, ranges) = parse_with(&text, Grammar::Mrkdwn);
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut pos = 0;
    for InlineRange { range, cell } in ranges {
        out.push_str(&text[pos..range.start]);
        let line = &text[range.clone()];
        let spans = inline(line, Grammar::Mrkdwn);
        // Escaping only the delimiters that could pair keeps the text
        // readable; when that misjudges, every delimiter is escaped.
        let mut markdown = String::new();
        for strict in [false, true] {
            markdown.clear();
            // A table row needs a `|`, which may be a link's, so cells
            // keep their links as written.
            let rewriter = Rewriter {
                links: !cell,
                strict,
            };
            rewriter.rewrite(line, &[], &mut false, &mut markdown);
            if inline(&markdown, Grammar::Markdown) == spans {
                break;
            }
        }
        out.push_str(&markdown);
        pos = range.end;
    }
    out.push_str(&text[pos..]);
    out
}

struct Rewriter {
    /// Whether `<url|label>` links become `[label](url)`.
    links: bool,
    /// Whether every literal delimiter run Markdown could read is escaped,
    /// rather than those [`could_pair`] finds.
    strict: bool,
}

impl Rewriter {
    /// Pushes the Markdown for the mrkdwn `text`, which sits inside emphasis
    /// with the delimiters in `enclosing`, walking it as
    /// [`super::collect_spans`] does. `url_word` tracks whether a bare URL
    /// started earlier in the current word: Markdown would read a link or an
    /// escape there as part of the URL.
    fn rewrite(&self, text: &str, enclosing: &[char], url_word: &mut bool, out: &mut String) {
        let mut ix = 0;
        while ix < text.len() {
            let rest = &text[ix..];
            let c = rest.chars().next().unwrap();
            let in_url_word = *url_word;
            if c.is_whitespace() || c == '<' || c == '>' {
                *url_word = false;
            }
            let token = match c {
                '`' => code_span(rest).map(|(len, _)| (len, rest[..len].to_owned())),
                '<' => angle_link(rest).map(|(len, url, label)| {
                    let markdown = match self.links && !in_url_word {
                        true => link(&rest[..len], url, label),
                        false => rest[..len].to_owned(),
                    };
                    (len, markdown)
                }),
                '[' => markdown_link(rest).map(|(len, ..)| (len, rest[..len].to_owned())),
                'h' if !preceded_by_alnum(text, ix) => bare_url(rest).map(|url| {
                    // Delimiters or a backslash in the URL's word could end
                    // an enclosing span early, and escaping them would join
                    // them to the URL, so the URL is closed off as `<url>`.
                    let word = rest.split(['<', '>']).next().unwrap();
                    let word = word.split(char::is_whitespace).next().unwrap();
                    let wrap = (self.strict || !enclosing.is_empty())
                        && word.contains(['*', '_', '~', '\\'])
                        && !url.contains('|')
                        && !text[..ix].ends_with('\\');
                    *url_word = !wrap;
                    let markdown = match wrap {
                        // Angle links decode entities; bare URLs keep them.
                        true => format!("<{}>", url.replace('&', "&amp;")),
                        false => url.to_owned(),
                    };
                    (url.len(), markdown)
                }),
                ':' => crate::emoji::shortcode(rest).map(|(len, _)| (len, rest[..len].to_owned())),
                // A backslash ending a span precedes its closing delimiter.
                '\\' if escape(rest).is_some() || rest.len() == 1 && !enclosing.is_empty() => {
                    Some((1, "\\\\".to_owned()))
                }
                '*' | '_' | '~' => emphasis(text, ix, c).map(|end| {
                    let delim = if c == '_' {
                        "_".to_owned()
                    } else {
                        format!("{c}{c}")
                    };
                    let mut markdown = delim.clone();
                    let enclosing = [enclosing, &[c]].concat();
                    self.rewrite(&text[ix + 1..end], &enclosing, url_word, &mut markdown);
                    markdown.push_str(&delim);
                    (end + 1 - ix, markdown)
                }),
                _ => None,
            };
            if let Some((len, markdown)) = token {
                out.push_str(&markdown);
                ix += len;
                continue;
            }
            let len = run(rest, c);
            if matches!(c, '*' | '_' | '~')
                && !*url_word
                && len <= 2
                && (self.strict || could_pair(text, ix, len, c, enclosing))
            {
                for _ in 0..len {
                    out.push('\\');
                    out.push(c);
                }
            } else {
                out.push_str(&rest[..len]);
            }
            ix += len;
        }
    }
}

/// `[label](url)` for the mrkdwn link `raw`, or `raw` when it has no label
/// or the Markdown form wouldn't read back as the same link.
fn link(raw: &str, url: &str, label: &str) -> String {
    if label == url {
        return raw.to_owned();
    }
    let markdown = format!("[{label}]({url})");
    match markdown_link(&markdown) {
        Some((len, u, l)) if len == markdown.len() && u == url && l == label => markdown,
        _ => raw.to_owned(),
    }
}

/// Whether the literal run of `len` `delim`s at `text[ix]` could open or
/// close Markdown emphasis in the rewritten text. Errs towards yes: an
/// opener needs a `delim` somewhere after it, a closer one before it or an
/// enclosing span of the same delimiter.
fn could_pair(text: &str, ix: usize, len: usize, delim: char, enclosing: &[char]) -> bool {
    let before = text[..ix].chars().next_back();
    let after = text[ix + len..].chars().next();
    let opens = !preceded_by_alnum(text, ix) && after.is_some_and(|c| !c.is_whitespace());
    let closes =
        before.is_some_and(|c| !c.is_whitespace()) && !after.is_some_and(char::is_alphanumeric);
    opens && text[ix + len..].contains(delim)
        || closes && (text[..ix].contains(delim) || enclosing.contains(&delim))
}

/// Byte offset of the delimiter closing the one at `text[ix]`, if it opens
/// mrkdwn emphasis: the opener follows a non-word character and precedes
/// text, and the closer, on the same line, follows text and precedes a
/// non-word character. Code spans and links inside are skipped over.
pub(super) fn emphasis(text: &str, ix: usize, delim: char) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Slack fixture and the mrkdwn corpus parse the same after the
    /// rewrite.
    #[test]
    fn rewrite_keeps_the_document() {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let mut checked = 0;
        for dir in ["slack", "mrkdwn"] {
            for entry in std::fs::read_dir(format!("{fixtures}/{dir}")).unwrap() {
                let path = entry.unwrap().path();
                let text = std::fs::read_to_string(&path).unwrap();
                let markdown = from_mrkdwn(&text);
                assert_eq!(
                    parse_with(&markdown, Grammar::Markdown).0,
                    parse_with(&text, Grammar::Mrkdwn).0,
                    "{}:\n{markdown}",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked > 6);
    }

    /// Random texts from pieces that interact parse the same after the
    /// rewrite.
    #[test]
    fn rewrite_keeps_random_documents() {
        const PIECES: &[&str] = &[
            "*",
            "_",
            "~",
            "**",
            "__",
            "~~",
            "\\",
            "`",
            "a",
            "b",
            "1",
            " ",
            " ",
            "\n",
            ",",
            "(",
            ")",
            "<https://a.b|x>",
            "<https://a.b|*l*>",
            "<https://a.b>",
            "[x](https://a.b)",
            "https://a.b/c",
            ":tada:",
            "| ",
            "- ",
            "> ",
            "# ",
            "```",
            "|---|---|\n",
            "&amp;",
        ];
        // xorshift, so failures reproduce.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..50_000 {
            let len = next() % 24;
            let text: String = (0..len)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            // A URL with a `|` can't be closed off as `<url>`.
            let pipe_url = text.split(char::is_whitespace).any(|word| {
                word.find("https://")
                    .is_some_and(|ix| word[ix..].contains('|'))
            });
            if pipe_url {
                continue;
            }
            let markdown = from_mrkdwn(&text);
            assert_eq!(
                parse_with(&markdown, Grammar::Markdown).0,
                parse_with(&text, Grammar::Mrkdwn).0,
                "{text:?} -> {markdown:?}"
            );
        }
    }

    #[test]
    fn rewrites_mrkdwn_outside_code_and_links() {
        for (mrkdwn, markdown) in [
            (
                "*bold* _it_ ~gone~ *a _b_ ~c~*",
                "**bold** _it_ ~~gone~~ **a _b_ ~~c~~**",
            ),
            (
                "<https://a.b|Aven> <https://a.b> [x](https://a.b) `*code*`",
                "[Aven](https://a.b) <https://a.b> [x](https://a.b) `*code*`",
            ),
            ("```\n*block*\n```\n*after*", "```\n*block*\n```\n**after**"),
            // Labels and URLs the Markdown form can't hold stay as written.
            (
                "<https://a.b|[x]> <https://a.b/)|y>",
                "<https://a.b|[x]> <https://a.b/)|y>",
            ),
            (
                "**lit** __lit__ ~~lit~~ a\\*b snake_case 2*3*4 ~5 min",
                "\\*\\*lit\\*\\* \\_\\_lit\\_\\_ \\~\\~lit\\~\\~ a\\\\\\*b snake_case 2*3*4 ~5 min",
            ),
            ("*a** b* _x_y_", "**a\\*\\* b** _x_y_"),
            (
                "| *a* | <https://a.b|l|k> |\n|---|---|",
                "| **a** | <https://a.b|l|k> |\n|---|---|",
            ),
            ("a\r\n*b*\r\n", "a\n**b**\n"),
        ] {
            assert_eq!(from_mrkdwn(mrkdwn), markdown, "{mrkdwn:?}");
        }
    }
}
