//! Lossless separation of a draft's ordered scalar fields from its body.

use indexmap::IndexMap;
use serde_yaml_ng::{Mapping, Value};

pub type Fields = IndexMap<String, String>;

pub struct Draft<'a> {
    pub fields: Fields,
    pub body: &'a str,
    /// Byte offset of the body in the original source.
    pub body_offset: usize,
}

/// Only an exact leading delimiter pair introduces fields. Invalid YAML,
/// duplicate keys and non-scalar values leave the entire source as body.
pub fn parse(text: &str) -> Draft<'_> {
    parse_fields(text).unwrap_or_else(|| Draft {
        fields: Fields::new(),
        body: text,
        body_offset: 0,
    })
}

fn parse_fields(text: &str) -> Option<Draft<'_>> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let start = first.len();
    let mut end = start;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let mapping: Mapping = serde_yaml_ng::from_str(&text[start..end]).ok()?;
            let mut fields = Fields::new();
            for (key, value) in mapping {
                let Value::String(key) = key else { return None };
                let value = match value {
                    Value::String(value) => value,
                    Value::Null => String::new(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) => value.to_string(),
                    _ => return None,
                };
                fields.insert(key, value);
            }
            let body_offset = end + line.len();
            return Some(Draft {
                fields,
                body: &text[body_offset..],
                body_offset,
            });
        }
        end += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order_values_and_body_bytes() {
        let text = "---\r\nto: a@example.com\r\ncc:\r\nsubject: 'Hello: world'\r\ncustom: 42\r\n---\r\n\r\nBody\r\n";
        let draft = parse(text);
        assert_eq!(
            draft.fields.keys().map(String::as_str).collect::<Vec<_>>(),
            ["to", "cc", "subject", "custom"]
        );
        assert_eq!(draft.fields["cc"], "");
        assert_eq!(draft.fields["subject"], "Hello: world");
        assert_eq!(draft.fields["custom"], "42");
        assert_eq!(draft.body, "\r\nBody\r\n");
        assert_eq!(&text[draft.body_offset..], draft.body);
    }

    #[test]
    fn invalid_or_absent_frontmatter_is_never_lost() {
        for text in [
            "",
            "Body",
            "---\nA horizontal rule\n",
            "---\ntitle: unfinished",
            "\n---\ntitle: not leading\n---\n",
            "---\n[broken\n---\nBody",
            "---\ntitle: a\ntitle: b\n---\nBody",
            "---\nlabels: [a, b]\n---\nBody",
            "---\n- list\n---\nBody",
            "---\n42: value\n---\nBody",
        ] {
            let draft = parse(text);
            assert!(draft.fields.is_empty(), "{text}");
            assert_eq!(draft.body, text);
            assert_eq!(draft.body_offset, 0);
        }
    }

    #[test]
    fn empty_block_and_multiline_strings() {
        assert_eq!(parse("---\n---").body, "");
        let draft = parse("---\ntitle: |-\n  First\n  second\n---\nBody");
        assert_eq!(draft.fields["title"], "First\nsecond");
        assert_eq!(draft.body, "Body");
    }
}
