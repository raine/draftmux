//! Structural snapshots of the block tree produced by the Slack parser.

mod common;

use common::outline;
use markup::{Document, slack};

fn snapshot(mrkdwn: &str) -> String {
    outline(&slack::parse(mrkdwn))
}

#[test]
fn inline_styles() {
    insta::assert_snapshot!(snapshot(
        "plain *bold* _italic_ ~gone~ `code` *_both_* ~*struck bold*~ *bold `code` inside*\n"
    ));
}

#[test]
fn word_boundaries() {
    insta::assert_snapshot!(snapshot(
        "snake_case_name 2*3*4 foo*bar* * spaced * _trailing _ (*paren*) *end*. \
         **double** __dunder__ ~~tilde~~\n"
    ));
}

#[test]
fn styles_stay_on_one_line() {
    insta::assert_snapshot!(snapshot("*starts here\nends here* and `code\nspan`\n"));
}

#[test]
fn unmatched_delimiters_stay_literal() {
    insta::assert_snapshot!(snapshot("*open _also `tick ~tilde\n"));
}

#[test]
fn code_inside_is_literal() {
    insta::assert_snapshot!(snapshot("`*not bold* <https://a.b|x>` and *a `*` b*\n"));
}

#[test]
fn links() {
    insta::assert_snapshot!(snapshot(
        "<https://example.com|the site> <https://example.com> <mailto:a@b.co|mail> \
         *<https://bold.link|bold link>* <@U123> <#C1|general> <!here> <not a link> a < b\n"
    ));
}

#[test]
fn bare_urls() {
    insta::assert_snapshot!(snapshot(
        "see https://example.com/a_b_c. and (https://example.com/x) or \
         https://en.wikipedia.org/wiki/Foo_(bar), nothttps://x.y\n"
    ));
}

#[test]
fn entities() {
    insta::assert_snapshot!(snapshot(
        "a &amp; b &lt;tag&gt; &amp;lt; `&lt;code&gt;` &gt; not a quote\n"
    ));
}

#[test]
fn line_breaks_and_blank_lines() {
    insta::assert_snapshot!(snapshot("\n\nfirst\nsecond\n\nthird\n\n\nfourth\n\n\n"));
}

#[test]
fn code_blocks() {
    insta::assert_snapshot!(snapshot(
        "```\nfn main() {\n    *not bold*\n}\n```\n\
         ```rust\nlet x = 1;\n```\n\
         before ```inline block``` after\n\
         ```\n\nkeeps blank lines\n\n```\n"
    ));
}

#[test]
fn unclosed_fence_is_text() {
    insta::assert_snapshot!(snapshot("```\nnever closed *bold*\n"));
}

#[test]
fn quotes() {
    insta::assert_snapshot!(snapshot(
        "> first *line*\n>second\n>\n> > nested stays literal\nafter\n\n>>> rest\nof the\n\nmessage\n"
    ));
}

#[test]
fn lists() {
    insta::assert_snapshot!(snapshot(
        "Changes:\n- one\n* two\n• three\n  - nested *bold*\n    - deeper\n  continued line\n- back\n\n\
         3. third\n4. fourth\n   1. sub\n- bullet after ordered\n\n- [ ] not a task\n-no space\n"
    ));
}

#[test]
fn blank_lines_between_blocks() {
    insta::assert_snapshot!(snapshot(
        "- a\n\n- b\n\n\n```\ncode\n```\n> quote\n\ntext\n"
    ));
}

#[test]
fn headings() {
    insta::assert_snapshot!(snapshot(
        "# One\n### Three *bold* ###\n####### seven\n#channel\n#\n"
    ));
}

#[test]
fn tables() {
    insta::assert_snapshot!(snapshot(
        "| Left | Center | Right | None |\n|:-----|:------:|------:|------|\n\
         | `a|b` | *c* | <https://x.y|l|k> | |\n| ragged |\nafter\n\nnot | a table\n"
    ));
}

#[test]
fn empty_document() {
    insta::assert_snapshot!(snapshot("\n\n"));
}

fn fallbacks(mrkdwn: &str) -> String {
    let blocks = slack::parse(mrkdwn)
        .blocks
        .iter()
        .filter_map(slack::fallback)
        .map(|(block, note)| {
            format!(
                "note {note:?}\n{}",
                outline(&Document {
                    blocks: vec![block]
                })
            )
        })
        .collect::<Vec<_>>();
    blocks.join("")
}

#[test]
fn heading_fallback() {
    insta::assert_snapshot!(fallbacks("## Steps for _today_\n"));
}

#[test]
fn table_fallback() {
    insta::assert_snapshot!(fallbacks(
        "| Backend | Latency | Notes |\n|:--|--:|:-:|\n| LM Studio | 0.8s | local |\n| ollama | 12.1s | `fast` |\n| é | 1 | |\n"
    ));
}
