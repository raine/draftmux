//! Snapshots of the rich text copied for a Rich text draft.

use markup::{Block, github, rich_text};

const MESSAGE: &str = "# Release *notes*\n\
    Hi **team**, _quick_ ~~old~~ `a < b` [link](https://a.example?x=1&y=2)\n\
    second line\n\
    \n\
    - one\n\
    - two\n  \
      1. nested\n\
    - [x] done\n\
    \n\
    > quoted\n\
    \n\
    > [!WARNING]\n\
    > Careful\n\
    \n\
    ```rust\nlet a = b < c;\n  indented {}\n```\n\
    \n\
    | a | b |\n|---|--:|\n| 1 | **22** |\n\
    \n\
    ---\n\
    \n\
    <div>raw</div>\n\
    \n\
    Ünïcode — 🎉 \\\\path\n";

#[test]
fn html() {
    let html = rich_text::html(&github::parse(MESSAGE));
    insta::assert_snapshot!(html.replace("</p>", "</p>\n").replace("</li>", "</li>\n"));
}

#[test]
fn rtf() {
    insta::assert_snapshot!(rich_text::rtf(&github::parse(MESSAGE)));
}

#[test]
fn empty_message() {
    let doc = github::parse("");
    assert_eq!(rich_text::html(&doc), "<meta charset=\"utf-8\">");
    assert!(rich_text::rtf(&doc).ends_with("}\n}"));
}

#[test]
fn pasted_has_no_github_only_blocks() {
    let doc = rich_text::pasted(&github::parse(MESSAGE));
    fn check(blocks: &[Block]) {
        for block in blocks {
            match block {
                Block::Alert { .. } | Block::Html(_) => panic!("{block:?}"),
                Block::Quote(blocks) => check(blocks),
                Block::List(list) => {
                    for item in &list.items {
                        assert_eq!(item.task, None);
                        check(&item.blocks);
                    }
                }
                _ => {}
            }
        }
    }
    check(&doc.blocks);
}

#[test]
fn loose_list_items_keep_their_paragraphs() {
    let html = rich_text::html(&github::parse("- one\n\n- two\n"));
    assert_eq!(
        html,
        "<meta charset=\"utf-8\"><ul><li><p>one</p></li><li><p>two</p></li></ul>"
    );
}
