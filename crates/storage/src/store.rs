use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::atomic;
use crate::meta::{Meta, Source, Status, Target};

const DRAFT_FILE: &str = "draft.md";
const META_FILE: &str = "meta.json";
const SESSION_DIR: &str = "session";

/// The drafts directory. Stateless: every call reads or writes the files
/// directly, so the app and the CLI can share it.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// Fields supplied at handoff; status and timestamps are set by
/// [`Store::create`].
#[derive(Debug, Clone)]
pub struct NewDraft {
    pub target: Target,
    pub title: String,
    pub source: Source,
    pub cwd: PathBuf,
    pub pi_agent_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub id: String,
    pub dir: PathBuf,
    pub meta: Meta,
}

impl Draft {
    pub fn draft_path(&self) -> PathBuf {
        self.dir.join(DRAFT_FILE)
    }

    pub fn session_dir(&self) -> PathBuf {
        self.dir.join(SESSION_DIR)
    }
}

/// Result of scanning the drafts directory.
#[derive(Debug, Default)]
pub struct Scan {
    /// Every readable draft, archived included, most recently active first.
    pub drafts: Vec<Draft>,
    /// Draft directories whose `meta.json` couldn't be read or parsed.
    pub errors: Vec<(PathBuf, anyhow::Error)>,
}

impl Scan {
    /// The drafts list: active drafts, most recently active first.
    pub fn active(&self) -> impl Iterator<Item = &Draft> {
        self.drafts
            .iter()
            .filter(|draft| draft.meta.status == Status::Active)
    }
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `~/Library/Application Support/Draftmux/drafts`.
    pub fn default_root() -> Result<PathBuf> {
        let data = dirs::data_dir().context("no application data directory")?;
        Ok(data.join("Draftmux").join("drafts"))
    }

    /// Creates `<id>/` with an empty `draft.md` and `session/`, then writes
    /// `meta.json` last so a scan never lists a half-created draft.
    pub fn create(&self, new: NewDraft) -> Result<Draft> {
        let id = uuid::Uuid::now_v7().to_string();
        let dir = self.root.join(&id);
        let session = dir.join(SESSION_DIR);
        std::fs::create_dir_all(&session)
            .with_context(|| format!("creating {}", session.display()))?;
        atomic::write(&dir.join(DRAFT_FILE), b"")?;

        let now = Utc::now();
        let meta = Meta {
            target: new.target,
            status: Status::Active,
            title: new.title,
            source: new.source,
            cwd: new.cwd,
            pi_session_file: None,
            pi_agent_dir: new.pi_agent_dir,
            created_at: now,
            updated_at: now,
        };
        write_meta(&dir, &meta)?;
        Ok(Draft { id, dir, meta })
    }

    pub fn load(&self, id: &str) -> Result<Draft> {
        let dir = self.draft_dir(id)?;
        let meta = read_meta(&dir)?;
        Ok(Draft {
            id: id.to_owned(),
            dir,
            meta,
        })
    }

    pub fn read_text(&self, id: &str) -> Result<String> {
        let path = self.draft_dir(id)?.join(DRAFT_FILE);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// Writes `draft.md` and records the activity in `meta.json`.
    pub fn save_text(&self, id: &str, text: &str) -> Result<Meta> {
        let path = self.draft_dir(id)?.join(DRAFT_FILE);
        atomic::write(&path, text.as_bytes())?;
        self.touch(id)
    }

    /// Records activity that doesn't change the text, such as a prompt.
    pub fn touch(&self, id: &str) -> Result<Meta> {
        self.update_meta(id, |meta| meta.updated_at = Utc::now())
    }

    /// Hides the draft from the list. Leaves `updated_at` alone so unarchiving
    /// puts it back where it was.
    pub fn archive(&self, id: &str) -> Result<Meta> {
        self.update_meta(id, |meta| meta.status = Status::Archived)
    }

    pub fn unarchive(&self, id: &str) -> Result<Meta> {
        self.update_meta(id, |meta| meta.status = Status::Active)
    }

    pub fn update_meta(&self, id: &str, update: impl FnOnce(&mut Meta)) -> Result<Meta> {
        let dir = self.draft_dir(id)?;
        let mut meta = read_meta(&dir)?;
        update(&mut meta);
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    /// Reads every draft directory. A missing root is an empty store;
    /// directories without `meta.json` (being created, or not drafts) are
    /// skipped.
    pub fn scan(&self) -> Result<Scan> {
        let mut scan = Scan::default();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(scan),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.root.display()));
            }
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("reading {}", self.root.display()))?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_id(&id).is_err() || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let dir = entry.path();
            match dir.join(META_FILE).try_exists() {
                Ok(true) => {}
                Ok(false) => continue,
                Err(err) => {
                    scan.errors.push((dir, err.into()));
                    continue;
                }
            }
            match read_meta(&dir) {
                Ok(meta) => scan.drafts.push(Draft { id, dir, meta }),
                Err(err) => scan.errors.push((dir, err)),
            }
        }
        scan.drafts.sort_by(|a, b| {
            b.meta
                .updated_at
                .cmp(&a.meta.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(scan)
    }

    fn draft_dir(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(id))
    }
}

/// IDs come from the CLI, so only allow what [`Store::create`] generates and
/// never anything that could leave the drafts directory.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("invalid draft id {id:?}");
    }
    Ok(())
}

fn read_meta(dir: &Path) -> Result<Meta> {
    let path = dir.join(META_FILE);
    let json = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&json).with_context(|| format!("parsing {}", path.display()))
}

fn write_meta(dir: &Path, meta: &Meta) -> Result<()> {
    let mut json = serde_json::to_vec_pretty(meta)?;
    json.push(b'\n');
    atomic::write(&dir.join(META_FILE), &json)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::meta::SourceKind;

    fn new_draft(title: &str) -> NewDraft {
        NewDraft {
            target: Target::Github,
            title: title.to_owned(),
            source: Source {
                kind: SourceKind::Pi,
                path: "/tmp/source session.jsonl".into(),
            },
            cwd: "/tmp/repo".into(),
            pi_agent_dir: "/tmp/.pi/agent".into(),
        }
    }

    fn set_updated(store: &Store, id: &str, minutes: i64) {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::minutes(minutes);
        store.update_meta(id, |meta| meta.updated_at = at).unwrap();
    }

    fn ids<'a>(drafts: impl Iterator<Item = &'a Draft>) -> Vec<&'a str> {
        drafts.map(|d| d.id.as_str()).collect()
    }

    #[test]
    fn create_lays_out_directory_and_load_reads_it_back() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("drafts"));
        let draft = store.create(new_draft("PR body")).unwrap();

        assert!(draft.session_dir().is_dir());
        assert_eq!(std::fs::read_to_string(draft.draft_path()).unwrap(), "");
        assert_eq!(draft.meta.status, Status::Active);
        assert_eq!(draft.meta.created_at, draft.meta.updated_at);

        let loaded = store.load(&draft.id).unwrap();
        assert_eq!(loaded.meta, draft.meta);
        assert_eq!(loaded.dir, draft.dir);
    }

    #[test]
    fn save_text_writes_body_and_bumps_activity() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = store.create(new_draft("a")).unwrap();
        set_updated(&store, &draft.id, 0);
        let before = store.load(&draft.id).unwrap().meta.updated_at;

        let meta = store.save_text(&draft.id, "Hello `world`\n").unwrap();
        assert_eq!(store.read_text(&draft.id).unwrap(), "Hello `world`\n");
        assert!(meta.updated_at > before);
        assert_eq!(store.load(&draft.id).unwrap().meta, meta);
    }

    #[test]
    fn scan_sorts_by_activity_and_list_excludes_archived() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let a = store.create(new_draft("a")).unwrap().id;
        let b = store.create(new_draft("b")).unwrap().id;
        let c = store.create(new_draft("c")).unwrap().id;
        set_updated(&store, &a, 2);
        set_updated(&store, &b, 3);
        set_updated(&store, &c, 1);

        let scan = store.scan().unwrap();
        assert_eq!(ids(scan.drafts.iter()), [&b, &a, &c]);
        assert!(scan.errors.is_empty());

        store.archive(&a).unwrap();
        let scan = store.scan().unwrap();
        assert_eq!(ids(scan.active()), [&b, &c]);
        assert_eq!(ids(scan.drafts.iter()), [&b, &a, &c]);
        assert!(store.load(&a).unwrap().draft_path().exists());

        store.unarchive(&a).unwrap();
        assert_eq!(ids(store.scan().unwrap().active()), [&b, &a, &c]);
    }

    #[test]
    fn scan_skips_non_drafts_and_reports_bad_meta() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let good = store.create(new_draft("good")).unwrap().id;
        let bad = store.create(new_draft("bad")).unwrap();
        std::fs::write(bad.dir.join(META_FILE), "{ not json").unwrap();

        std::fs::create_dir(root.path().join("half-created")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();
        std::fs::write(root.path().join(".DS_Store"), "").unwrap();
        std::fs::write(root.path().join("stray-file"), "").unwrap();

        let scan = store.scan().unwrap();
        assert_eq!(ids(scan.drafts.iter()), [&good]);
        assert_eq!(scan.errors.len(), 1);
        assert_eq!(scan.errors[0].0, bad.dir);
    }

    #[test]
    fn scan_of_missing_root_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let scan = Store::new(root.path().join("missing")).scan().unwrap();
        assert!(scan.drafts.is_empty() && scan.errors.is_empty());
    }

    #[test]
    fn rejects_ids_outside_the_drafts_directory() {
        let store = Store::new("/tmp/drafts");
        for id in ["", "..", "../x", "a/b", "/etc", "A"] {
            assert!(store.load(id).is_err(), "{id:?} accepted");
        }
    }
}
