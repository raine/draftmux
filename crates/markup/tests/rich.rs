//! Snapshots of the rich text copied for Slack.

use markup::{rich, slack};

const MESSAGE: &str = "Hi **team**, _quick_ ~~old~~ `code` **bold [link](https://a.example?x=1&y=2)**\n\
    second  line\n\
    \n\
    - one\n\
    - two\n  \
      1. nested\n\
    3. three\n\
    > quoted\n\
    > more\n\
    ```\nlet a = b < c;\n  indented {}\n```\n\
    # Heading\n\
    | a | b |\n|---|--:|\n| 1 | 22 |\n\
    Ünïcode — 🎉 \\path\n";

#[test]
fn html() {
    insta::assert_snapshot!(rich::html(&slack::parse(MESSAGE)).replace("</p>", "</p>\n"));
}

#[test]
fn rtf() {
    insta::assert_snapshot!(rich::rtf(&slack::parse(MESSAGE)));
}

#[test]
fn plain() {
    insta::assert_snapshot!(rich::plain(&slack::parse(MESSAGE)).replace('\u{feff}', "␣"));
}

#[test]
fn empty_message() {
    let doc = slack::parse("");
    assert_eq!(rich::html(&doc), "<meta charset=\"utf-8\">");
    assert!(rich::rtf(&doc).ends_with("}\n}"));
    assert_eq!(rich::plain(&doc), "");
}

#[test]
fn code_language() {
    let html = rich::html(&slack::parse(
        "```diff\n-a\n```\n```a\"><b>\nx\n```\n```\ny\n```",
    ));
    assert_eq!(
        html,
        "<meta charset=\"utf-8\">\
         <pre data-stringify-type=\"pre\" data-stringify-language=\"diff\">-a</pre>\
         <pre data-stringify-type=\"pre\" data-stringify-language=\"a&quot;&gt;&lt;b&gt;\">x</pre>\
         <pre>y</pre>"
    );
}
