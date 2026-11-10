//! Picks up edits made to a configuration file outside the app by polling
//! its contents, which also catches editors that replace the file.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{Context, Task};

/// How often watched files are read.
pub const POLL: Duration = Duration::from_secs(1);

pub struct WatchedFile {
    path: PathBuf,
    /// The contents last seen; `None` when the file was missing.
    seen: Option<Vec<u8>>,
}

impl WatchedFile {
    /// Starts from the file as it is now.
    pub fn new(path: PathBuf) -> Self {
        let seen = std::fs::read(&path).ok();
        Self { path, seen }
    }

    /// Whether the file changed since it was last seen.
    pub fn changed(&mut self) -> bool {
        let now = std::fs::read(&self.path).ok();
        if now == self.seen {
            return false;
        }
        self.seen = now;
        true
    }

    /// Takes the file as it is now as seen, so a write the app made itself
    /// doesn't count as a change.
    pub fn mark_seen(&mut self) {
        self.seen = std::fs::read(&self.path).ok();
    }
}

/// Calls `poll` on the entity every [`POLL`] while it exists.
pub fn every_poll<T: 'static>(cx: &mut Context<T>, poll: fn(&mut T, &mut Context<T>)) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            if this.update(cx, poll).is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_count_once_and_own_writes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut file = WatchedFile::new(path.clone());
        assert!(!file.changed());

        std::fs::write(&path, "a").unwrap();
        assert!(file.changed());
        assert!(!file.changed());

        std::fs::write(&path, "b").unwrap();
        file.mark_seen();
        assert!(!file.changed());

        std::fs::remove_file(&path).unwrap();
        assert!(file.changed());
    }
}
