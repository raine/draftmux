//! Blank drafts started in the app are deleted when left empty.

mod support;

use chrono::Duration;
use support::Script;

#[test]
fn leaving_an_empty_blank_draft_deletes_it() {
    let mut s = Script::new();
    let other = s.draft("Kept\n");
    let blank = s.blank();
    s.select(&blank);
    // New Draft reuses it rather than starting another.
    assert!(s.session.discardable(&blank));
    // Whitespace counts as empty.
    s.type_text("  \n");

    assert!(s.leave_for(&other));
    assert!(!s.exists(&blank));
    assert!(
        s.session
            .drafts()
            .rows()
            .iter()
            .all(|row| row.draft.id != blank)
    );
    assert_eq!(s.selected().as_deref(), Some(other.as_str()));
}

#[test]
fn a_blank_draft_with_text_is_kept() {
    let mut s = Script::new();
    let other = s.draft("Other\n");
    let blank = s.blank();
    s.select(&blank);
    s.type_text("Hello");
    assert!(!s.session.discardable(&blank));

    assert!(!s.leave_for(&other));
    assert!(s.exists(&blank));
    assert_eq!(s.text(&blank), "Hello");
}

#[test]
fn a_blank_draft_the_agent_edited_or_was_prompted_in_is_kept() {
    let mut s = Script::new();
    let other = s.draft("Other\n");

    // Filled by the agent, then cleared by the user.
    let edited = s.blank();
    s.select(&edited);
    s.send("Write it");
    s.accepted(&edited);
    s.agent_writes(&edited, "Draft\n").unwrap();
    s.type_text("");
    assert!(!s.leave_for(&other));
    assert!(s.exists(&edited));

    // Prompted, with nothing written.
    let prompted = s.blank();
    s.select(&prompted);
    s.send("Hi");
    s.accepted(&prompted);
    assert!(!s.leave_for(&other));
    assert!(s.exists(&prompted));
}

#[test]
fn a_blank_draft_is_kept_while_its_agent_runs() {
    let mut s = Script::new();
    let other = s.draft("Other\n");
    let blank = s.blank();
    s.select(&blank);
    s.running(&blank, true);
    assert!(!s.leave_for(&other));
    assert!(s.exists(&blank));

    s.running(&blank, false);
    assert!(s.session.discard(&blank).unwrap());
    assert!(!s.exists(&blank));
}

#[test]
fn empty_drafts_that_werent_started_blank_are_never_deleted() {
    let mut s = Script::new();
    let other = s.draft("Other\n");
    let empty = s.draft("");
    s.select(&empty);
    assert!(!s.session.discardable(&empty));
    assert!(!s.leave_for(&other));
    assert!(s.exists(&empty));
}

#[test]
fn discarding_the_selected_draft_clears_the_selection() {
    let mut s = Script::new();
    let blank = s.blank();
    s.select(&blank);
    s.type_text("x");
    s.type_text("");
    assert!(s.session.discard(&blank).unwrap());
    assert_eq!(s.selected(), None);
    assert!(!s.exists(&blank));
    // The pending edit isn't saved into the deleted draft.
    s.advance(Duration::seconds(5));
    assert!(!s.exists(&blank));
}

#[test]
fn launch_sweep_deletes_empty_blank_drafts_left_by_a_crash() {
    let mut s = Script::new();
    let empty = s.blank();
    let filled = s.blank();
    let handed_off = s.handed_off();
    s.select(&filled);
    s.type_text("Notes");
    s.leave_window().unwrap();

    s.crash();
    s.session.drafts_mut().sweep_blank();
    assert!(!s.exists(&empty));
    assert!(s.exists(&filled));
    assert!(s.exists(&handed_off));
    assert_eq!(s.session.drafts().rows().len(), 2);
}
