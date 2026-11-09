use markup::replace_shortcodes;

#[test]
fn converts_sidebar_titles_and_directory_labels() {
    assert_eq!(replace_shortcodes(":mega: Picnic plans"), "📣 Picnic plans");
    assert_eq!(
        replace_shortcodes("~/work/:rocket:/日本語"),
        "~/work/🚀/日本語"
    );
}

#[test]
fn shares_slack_alias_case_and_skin_tone_rules() {
    assert_eq!(
        replace_shortcodes(":Tada: :+1::skin-tone-2: :thumbsup:"),
        "🎉 👍🏻 👍"
    );
    assert_eq!(replace_shortcodes("a:smile:b::mega:"), "a😄b:📣");
}

#[test]
fn preserves_unknown_shortcodes_and_other_literal_text() {
    for text in [
        "",
        "📣 日本語",
        ":not_an_emoji:",
        ":",
        "::",
        ":mega",
        ":not valid:",
    ] {
        assert_eq!(replace_shortcodes(text), text);
    }
    assert_eq!(
        replace_shortcodes("*Title* `:mega:` &amp; :unknown: :tada:"),
        "*Title* `📣` &amp; :unknown: 🎉"
    );
}

#[test]
fn leaves_source_text_unchanged() {
    let source = String::from(":mega: Picnic plans");
    let displayed = replace_shortcodes(&source);
    assert_eq!(displayed, "📣 Picnic plans");
    assert_eq!(source, ":mega: Picnic plans");
}
