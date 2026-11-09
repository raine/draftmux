//! Single-flight, latest-request-wins preview work and source-range validation.

use std::ops::Range;

use draftmux_storage::Target;

pub(super) struct Request {
    pub revision: u64,
    pub target: Target,
    pub text: String,
}

#[derive(Default)]
pub(super) struct Work {
    revision: u64,
    running: bool,
    next: Option<Request>,
}

impl Work {
    /// Replaces queued work. Returns whether a worker needs to be started.
    pub fn request(&mut self, target: Target, text: String) -> bool {
        self.revision += 1;
        self.next = Some(Request {
            revision: self.revision,
            target,
            text,
        });
        !std::mem::replace(&mut self.running, true)
    }

    pub fn take(&mut self) -> Option<Request> {
        let next = self.next.take();
        if next.is_none() {
            self.running = false;
        }
        next
    }

    pub fn is_current(&self, revision: u64) -> bool {
        self.revision == revision
    }

    /// Invalidates both queued and in-flight results without starting another worker.
    pub fn invalidate(&mut self) {
        self.revision += 1;
        self.next = None;
    }
}

/// Maps a selection from the displayed source to current text only when it lies
/// outside the changed region. Offsets inside changed text must not target an
/// unrelated editor selection while a newer preview is being prepared.
pub(super) fn unchanged_range(old: &str, new: &str, range: Range<usize>) -> Option<Range<usize>> {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    if range.end <= prefix {
        return Some(range);
    }
    let suffix = old.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(new.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if range.start >= old.len() - suffix {
        let start = new.len() - (old.len() - range.start);
        let end = new.len() - (old.len() - range.end);
        if new.is_char_boundary(start) && new.is_char_boundary(end) {
            return Some(start..end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_edits_without_starting_parallel_workers() {
        let mut work = Work::default();
        assert!(work.request(Target::Github, "first".into()));
        let first = work.take().unwrap();
        assert!(!work.request(Target::Slack, "second".into()));
        assert!(!work.request(Target::Rich, "latest".into()));
        assert!(!work.is_current(first.revision));
        let latest = work.take().unwrap();
        assert_eq!(latest.text, "latest");
        assert_eq!(latest.target, Target::Rich);
        assert!(work.is_current(latest.revision));
        assert!(work.take().is_none());
        assert!(work.request(Target::Github, "new worker".into()));
    }

    #[test]
    fn invalidates_on_draft_or_target_change() {
        let mut work = Work::default();
        work.request(Target::Github, "old draft".into());
        let old = work.take().unwrap();
        work.invalidate();
        assert!(!work.is_current(old.revision));
        assert!(!work.request(Target::Plain, "new draft".into()));
        assert_eq!(work.take().unwrap().text, "new draft");
        work.invalidate();
        assert!(work.take().is_none());
        assert!(work.request(Target::Github, "again".into()));
    }

    #[test]
    fn maps_only_unchanged_selected_text() {
        assert_eq!(unchanged_range("abc def", "abc def", 0..7), Some(0..7));
        assert_eq!(unchanged_range("abc def", "xabc def", 4..7), Some(5..8));
        assert_eq!(unchanged_range("abc def", "abc", 0..3), Some(0..3));
        assert_eq!(unchanged_range("abc def", "def", 4..7), Some(0..3));
        assert_eq!(unchanged_range("abc def", "abc XYZ", 4..7), None);
        assert_eq!(unchanged_range("abc def", "abc xdef", 0..7), None);
        assert_eq!(unchanged_range("é cat", "è cat", 3..6), Some(3..6));
        assert_eq!(unchanged_range("é cat", "è cat", 0..2), None);
        assert_eq!(unchanged_range("猫", "dog", 0..3), None);
    }
}
