//! Structural snapshots of the block tree produced by the Slack parser.

mod common;

use common::outline;
use markup::slack;

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
fn markdown_links() {
    insta::assert_snapshot!(snapshot(
        "Adds [Aven](https://github.com/raine/aven) to it, ([wiki](https://en.wikipedia.org/wiki/Foo_(bar))), \
         *[bold](https://a.b/x_y)* _see [docs](https://a.b/c_d)_ [mail](mailto:a@b.co). \
         `[code](https://a.b)` [no scheme](example.com) [](https://a.b) [two\nlines](https://a.b) \
         [spaced](https://a.b c) [unclosed](https://a.b [a [b](https://a.b)\n"
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
fn code_block_languages() {
    insta::assert_snapshot!(snapshot(
        "```diff\n-old\n+new\n```\n\
         ```Diff\nkept as written\n```\n\
         ``` diff\nspace after the fence\n```\n\
         ```diff -a\nmore than a word\n```\n\
         ```diff```\n\
         ```text\n```\n\
         before ```c++\nx\n``` after\n"
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
         | `a|b` | *c* | <https://x.y|l|k> | [a|b](https://x.y/?q=1|2) |\n| ragged |\nafter\n\nnot | a table\n"
    ));
}

#[test]
fn empty_document() {
    insta::assert_snapshot!(snapshot("\n\n"));
}

/// The outline of what Slack posts, with a line for each block's note.
fn posted(mrkdwn: &str) -> String {
    common::posted(&slack::parse(mrkdwn))
}

#[test]
fn heading_fallback() {
    insta::assert_snapshot!(posted("## Steps for _today_\n"));
}

#[test]
fn table_fallback() {
    insta::assert_snapshot!(posted(
        "| Backend | Latency | Notes |\n|:--|--:|:-:|\n| LM Studio | 0.8s | local |\n| ollama | 12.1s | `fast` |\n| é | 1 | |\n"
    ));
}

#[test]
fn kept_lists_post_unchanged() {
    insta::assert_snapshot!(posted("- one\n    - nested\n1. first\n2. second\n"));
}

#[test]
fn multi_line_item_posts_as_lines() {
    insta::assert_snapshot!(posted(
        "- wraps\n  onto a second line\n    - nested *bold*\n- plain\n"
    ));
}

#[test]
fn renumbered_lists_post_as_lines() {
    // A start other than 1 anywhere flattens every numbered list, not bullets.
    insta::assert_snapshot!(posted("1. one\n2. two\n\n- bullet\n\n3. three\n"));
}

#[test]
fn emoji_shortcodes() {
    insta::assert_snapshot!(snapshot(
        ":tada: :+1::skin-tone-2: (:Tada:) `:tada:` <https://a.b|:tada:> :not_an_emoji: a:smile:b ::\n"
    ));
}

#[test]
fn smart_quotes() {
    let curl = |text: &str| -> String {
        markup::slack::smart_quotes(&text.chars().collect::<Vec<_>>())
            .into_iter()
            .collect()
    };
    assert_eq!(curl("can't"), "can’t");
    assert_eq!(curl("the users' files"), "the users’ files");
    assert_eq!(curl("say 'hi' and \"bye\""), "say ‘hi’ and “bye”");
    assert_eq!(curl("the '90s"), "the ’90s");
    assert_eq!(curl("5' tall"), "5′ tall");
}

#[test]
fn code_language_names() {
    use slack::CodeLanguage::*;
    // Labels measured in Slack.
    for (lang, expected) in [
        ("diff", Known("Diff")),
        ("rs", Known("Rust")),
        ("rust", Known("Rust")),
        ("sh", Known("Shell")),
        ("bash", Known("Shell")),
        ("zsh", Known("Shell")),
        ("console", Known("Shell Session")),
        ("c++", Known("C++")),
        ("c#", Known("C#")),
        ("yml", Known("YAML")),
        ("proto", Known("Protocol Buffer 3")),
        ("log", Known("Log file")),
        ("properties", Known("INI")),
        ("hcl", Known("HashiCorp HCL")),
        ("text", Plain),
        ("plaintext", Plain),
        ("txt", Plain),
        ("plain", Plain),
        ("foobar", Unknown),
        ("DIFF", Unknown),
        ("Rust", Unknown),
        ("patch", Unknown),
        ("gitignore", Unknown),
    ] {
        assert_eq!(slack::code_language(lang), expected, "{lang}");
    }
}

#[test]
fn diff_line_colours() {
    use slack::DiffRole::*;
    // Each line as Slack colours it: `[role:text]` runs, the rest plain.
    let show = |line: &str| {
        let mut out = String::new();
        let mut pos = 0;
        for (range, role) in slack::diff_line(line) {
            out.push_str(&line[pos..range.start]);
            let role = match role {
                Deleted => "del",
                Inserted => "ins",
                Header => "hdr",
                Muted => "mut",
            };
            out.push_str(&format!("[{role}:{}]", &line[range.clone()]));
            pos = range.end;
        }
        out + &line[pos..]
    };
    for (line, expected) in [
        (
            "diff --git a/src/x.rs b/src/x.rs",
            "[hdr:diff --git a/src/x.rs b/src/x.rs]",
        ),
        ("diff -u a b", "[hdr:diff -u a b]"),
        (
            "index 83db48f..bf269f4 100644",
            "index 83db48f..bf269f4 100644",
        ),
        ("--- a/src/x.rs", "[mut:---][hdr: a/src/x.rs]"),
        ("+++ b/src/x.rs", "[mut:+++][hdr: b/src/x.rs]"),
        ("*** a/x.rs", "[mut:***][hdr: a/x.rs]"),
        (
            "@@ -10,7 +10,8 @@ fn main() {",
            "[mut:@@][hdr: -10,7 +10,8 ][mut:@@] fn main() {",
        ),
        ("@@ broken", "@@ broken"),
        ("--- 1,4 ----", "[mut:---][hdr: 1,4 ][mut:----]"),
        ("*** 1,2 ****", "[mut:***][hdr: 1,2 ][mut:****]"),
        ("5c5", "[hdr:5c5]"),
        ("12,14d11", "[hdr:12,14d11]"),
        ("-removed", "[del:-removed]"),
        ("-- ", "[del:-- ]"),
        ("< quoted del", "[del:< quoted del]"),
        ("# comment line", "[del:# comment line]"),
        ("+added", "[ins:+added]"),
        ("+", "[ins:+]"),
        ("+++", "[ins:+++]"),
        ("> quoted add", "[ins:> quoted add]"),
        ("! changed", "[mut:!] changed"),
        ("---", "[mut:---]"),
        ("***************", "[mut:***************]"),
        (&"=".repeat(67), &format!("[mut:{}]", "=".repeat(67))),
        ("Index: src/x.rs", "Index[mut::] src/x.rs"),
        (" context", " context"),
        (" -", " -"),
        ("Only in a: b", "Only in a: b"),
        (
            "\\ No newline at end of file",
            "\\ No newline at end of file",
        ),
        ("", ""),
    ] {
        assert_eq!(show(line), expected, "{line:?}");
    }
}

#[test]
fn link_syntax_is_the_first_labelled_link() {
    use slack::LinkSyntax::*;
    assert_eq!(slack::link_syntax("no links"), None);
    assert_eq!(slack::link_syntax("<https://a.b> bare"), None);
    assert_eq!(
        slack::link_syntax("`<https://a.b|x>` [Aven](https://a.b) <https://c.d|y>"),
        Some(Markdown)
    );
    assert_eq!(
        slack::link_syntax("<https://a.b|x> [Aven](https://a.b)"),
        Some(Angle)
    );
}

#[test]
fn rewrite_links_converts_labelled_links_outside_code() {
    use slack::LinkSyntax::*;
    let text = "Adds <https://github.com/raine/aven|Aven> and [wiki](https://w.org/Foo_(bar)) \
                to <https://a.b>, `<https://a.b|code>`\n```\n<https://a.b|block>\n```\n";
    assert_eq!(
        slack::rewrite_links(text, Markdown),
        "Adds [Aven](https://github.com/raine/aven) and [wiki](https://w.org/Foo_(bar)) \
         to <https://a.b>, `<https://a.b|code>`\n```\n<https://a.b|block>\n```\n"
    );
    assert_eq!(
        slack::rewrite_links(text, Angle),
        "Adds <https://github.com/raine/aven|Aven> and <https://w.org/Foo_(bar)|wiki> \
         to <https://a.b>, `<https://a.b|code>`\n```\n<https://a.b|block>\n```\n"
    );
    // Labels the other syntax can't hold stay as written.
    assert_eq!(
        slack::rewrite_links("<https://a.b|[x]> [a|b](https://a.b)", Markdown),
        "<https://a.b|[x]> [a|b](https://a.b)"
    );
    assert_eq!(
        slack::rewrite_links("<https://a.b|[x]> [a|b](https://a.b)", Angle),
        "<https://a.b|[x]> [a|b](https://a.b)"
    );
}
