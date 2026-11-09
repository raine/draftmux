//! Snapshots of the shared fixture drafts in `tests/fixtures`, the same
//! drafts compared against GitHub's and Slack's own rendering.

mod common;

use common::{outline, posted};
use markup::{github, rich, slack};

/// What Slack posts for a fixture (with each changed block's note), and the
/// HTML and plain text Copy for Slack puts on the pasteboard.
fn slack_snapshot(mrkdwn: &str) -> String {
    let doc = slack::parse(mrkdwn);
    let posted = posted(&doc);
    let html = rich::html(&doc).replace("</p>", "</p>\n");
    let plain = rich::plain(&doc).replace('\u{feff}', "␣");
    format!("# posted\n{posted}\n# html\n{html}\n\n# plain\n{plain}\n")
}

macro_rules! fixtures {
    ($($name:ident: $target:ident $file:literal,)*) => {$(
        #[test]
        fn $name() {
            let text = include_str!(concat!("fixtures/", $file));
            insta::assert_snapshot!(fixtures!(@render $target text));
        }
    )*};
    (@render slack $text:ident) => { slack_snapshot($text) };
    (@render github $text:ident) => { outline(&github::parse($text)) };
}

fixtures! {
    slack_inline: slack "slack/inline.md",
    slack_lists: slack "slack/lists.md",
    slack_blocks: slack "slack/blocks.md",
    slack_fallbacks: slack "slack/fallbacks.md",
    slack_lookalikes: slack "slack/lookalikes.md",
    github_headings: github "github/headings.md",
    github_lists: github "github/lists.md",
    github_tables: github "github/tables.md",
    github_alerts: github "github/alerts.md",
    github_code_quotes: github "github/code-quotes.md",
}
