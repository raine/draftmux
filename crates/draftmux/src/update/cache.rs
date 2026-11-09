//! When the next automatic check is due, in `update.json` in the user's
//! cache directory. Checks that fail back off
//! from minutes to a day, so an offline app doesn't ask GitHub every hour.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Release, current_version, release};

const SUCCESS_INTERVAL: u64 = 24 * 60 * 60;
const FAILURE_BACKOFF: [u64; 4] = [15 * 60, 60 * 60, 6 * 60 * 60, 24 * 60 * 60];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct UpdateCache {
    next_check_after: Option<u64>,
    failure_count: u32,
}

pub fn check_due() -> bool {
    check_due_at(&load(), now())
}

/// Fetches the latest release and returns it when it's newer than this
/// copy. Records the attempt either way.
pub fn check() -> Result<Option<Release>> {
    let mut cache = load();
    let now = now();
    let result = release::fetch_latest();
    match &result {
        Ok(_) => {
            cache.next_check_after = Some(now + SUCCESS_INTERVAL);
            cache.failure_count = 0;
        }
        Err(_) => {
            cache.failure_count = cache.failure_count.saturating_add(1);
            cache.next_check_after = Some(now + failure_delay(cache.failure_count));
        }
    }
    save(&cache);
    Ok(result?.filter(|release| release.version > current_version()))
}

fn check_due_at(cache: &UpdateCache, now: u64) -> bool {
    // A time further ahead than any interval came from a clock that has
    // since been set back.
    !matches!(cache.next_check_after, Some(next) if next > now && next - now <= SUCCESS_INTERVAL)
}

fn failure_delay(failure_count: u32) -> u64 {
    let ix = (failure_count.max(1) as usize - 1).min(FAILURE_BACKOFF.len() - 1);
    FAILURE_BACKOFF[ix]
}

fn path() -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches")
            .join(super::BUNDLE_ID)
            .join("update.json")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| home.join(".cache"), PathBuf::from)
            .join("draftmux/update.json")
    })
}

fn load() -> UpdateCache {
    path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Best effort: without a cache, checks are just due more often.
fn save(cache: &UpdateCache) {
    let Some(path) = path() else { return };
    let Some(dir) = path.parent() else { return };
    let saved = std::fs::create_dir_all(dir)
        .and_then(|()| tempfile::NamedTempFile::new_in(dir))
        .and_then(|mut file| {
            file.write_all(&serde_json::to_vec(cache)?)?;
            file.persist(&path)?;
            Ok(())
        });
    let _ = saved;
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_are_due_daily() {
        let cache = UpdateCache {
            next_check_after: Some(1_000 + SUCCESS_INTERVAL),
            ..UpdateCache::default()
        };
        assert!(!check_due_at(&cache, 1_000));
        assert!(!check_due_at(&cache, 1_000 + SUCCESS_INTERVAL - 1));
        assert!(check_due_at(&cache, 1_000 + SUCCESS_INTERVAL));
        assert!(check_due_at(&UpdateCache::default(), 1_000));
    }

    #[test]
    fn a_next_check_beyond_the_interval_is_due() {
        let cache = UpdateCache {
            next_check_after: Some(1_000 + SUCCESS_INTERVAL + 1),
            ..UpdateCache::default()
        };
        assert!(check_due_at(&cache, 1_000));
    }

    #[test]
    fn failures_back_off_up_to_a_day() {
        assert_eq!(failure_delay(1), 15 * 60);
        assert_eq!(failure_delay(2), 60 * 60);
        assert_eq!(failure_delay(4), 24 * 60 * 60);
        assert_eq!(failure_delay(40), 24 * 60 * 60);
    }
}
