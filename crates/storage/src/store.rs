use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::atomic;
use crate::meta::{DraftSettings, Meta, Source, Status, Syntax, Target};

const DRAFT_FILE: &str = "draft.md";
const META_FILE: &str = "meta.json";
const SESSION_DIR: &str = "session";
/// Records that the sample draft was created. A dot name and a file, so
/// [`Store::scan`] never takes it for a draft.
const SAMPLE_MARKER: &str = ".sample-created";
const CHAT_WIDTH: &str = ".chat-width";
const SIDEBAR_HIDDEN: &str = ".sidebar-hidden";

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
    pub source: Option<Source>,
    pub cwd: PathBuf,
    pub pi_agent_dir: PathBuf,
    pub note: Option<String>,
    pub settings: DraftSettings,
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
    /// Writes a line to `out` for each draft directory that was skipped.
    pub fn report_errors(&self, out: &mut impl io::Write) -> io::Result<()> {
        for (dir, err) in &self.errors {
            writeln!(out, "draftmux: skipping {}: {err:#}", dir.display())?;
        }
        Ok(())
    }

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

    /// `drafts` in [`data_dir`].
    fn default_root() -> Result<PathBuf> {
        Ok(data_dir()?.join("drafts"))
    }

    /// The store at [`Store::default_root`].
    pub fn open_default() -> Result<Self> {
        Ok(Self::new(Self::default_root()?))
    }

    /// Creates `<id>/` with an empty `draft.md` and `session/`, then writes
    /// `meta.json` last so a scan never lists a half-created draft.
    pub fn create(&self, new: NewDraft) -> Result<Draft> {
        self.create_draft(new, |_| Ok(None))
    }

    /// Like [`Store::create`], but `session` creates the draft's pi session
    /// file before `meta.json` is written, and returns its path for
    /// `pi_session_file`. If it fails, the draft directory is removed.
    pub fn create_with_session(
        &self,
        new: NewDraft,
        session: impl FnOnce(&Draft) -> Result<PathBuf>,
    ) -> Result<Draft> {
        self.create_draft(new, |draft| session(draft).map(Some))
    }

    fn create_draft(
        &self,
        new: NewDraft,
        session: impl FnOnce(&Draft) -> Result<Option<PathBuf>>,
    ) -> Result<Draft> {
        let id = uuid::Uuid::now_v7().to_string();
        let dir = self.root.join(&id);
        let session_dir = dir.join(SESSION_DIR);
        std::fs::create_dir_all(&session_dir)
            .with_context(|| format!("creating {}", session_dir.display()))?;
        let now = Utc::now();
        let handed_off = new.source.is_some();
        let mut draft = Draft {
            id,
            dir,
            meta: Meta {
                target: new.target,
                syntax: Some(Syntax::Markdown),
                status: Status::Active,
                title: new.title,
                source: new.source,
                cwd: new.cwd,
                pi_session_file: None,
                pi_agent_dir: new.pi_agent_dir,
                note: new.note,
                first_draft_pending: handed_off,
                settings: new.settings,
                created_at: now,
                updated_at: now,
            },
        };
        let result = atomic::write(&draft.draft_path(), b"")
            .and_then(|()| session(&draft))
            .and_then(|file| {
                draft.meta.pi_session_file = file;
                write_meta(&draft.dir, &draft.meta)
            });
        if let Err(err) = result {
            if let Err(remove) = std::fs::remove_dir_all(&draft.dir) {
                eprintln!(
                    "draftmux: couldn't remove the unfinished draft {}: {remove}",
                    draft.dir.display()
                );
            }
            return Err(err);
        }
        Ok(draft)
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
        self.update_meta(id, |meta| meta.updated_at = Utc::now())
    }

    /// Rewrites the text in `syntax` with `convert`, and records the syntax.
    /// The text as it was is kept as `backup` in the draft directory, and a
    /// conversion interrupted before the syntax was recorded starts again
    /// from it. Not counted as activity.
    pub fn convert_text(
        &self,
        id: &str,
        backup: &str,
        syntax: Syntax,
        convert: impl FnOnce(&str) -> String,
    ) -> Result<(Meta, String)> {
        let dir = self.draft_dir(id)?;
        let backup = dir.join(backup);
        let original = match std::fs::read_to_string(&backup) {
            Ok(original) => original,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let original = self.read_text(id)?;
                atomic::write(&backup, original.as_bytes())?;
                original
            }
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", backup.display()));
            }
        };
        let text = convert(&original);
        atomic::write(&dir.join(DRAFT_FILE), text.as_bytes())?;
        let meta = self.update_meta(id, |meta| meta.syntax = Some(syntax))?;
        Ok((meta, text))
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
        scan.drafts.sort_by(by_activity);
        Ok(scan)
    }

    /// Whether the sample draft was ever created in this store.
    pub fn sample_created(&self) -> Result<bool> {
        let path = self.root.join(SAMPLE_MARKER);
        path.try_exists()
            .with_context(|| format!("checking {}", path.display()))
    }

    pub fn mark_sample_created(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        atomic::write(&self.root.join(SAMPLE_MARKER), b"")
    }

    /// The chat column's width in pixels as the user last left it, if it
    /// was ever resized.
    pub fn chat_width(&self) -> Option<f32> {
        let text = std::fs::read_to_string(self.root.join(CHAT_WIDTH)).ok()?;
        text.trim()
            .parse()
            .ok()
            .filter(|width: &f32| width.is_finite())
    }

    pub fn set_chat_width(&self, width: f32) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        atomic::write(&self.root.join(CHAT_WIDTH), width.to_string().as_bytes())
    }

    /// Missing or invalid preferences keep the sidebar visible.
    pub fn sidebar_hidden(&self) -> bool {
        std::fs::read_to_string(self.root.join(SIDEBAR_HIDDEN))
            .is_ok_and(|text| text.trim() == "true")
    }

    pub fn set_sidebar_hidden(&self, hidden: bool) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        atomic::write(
            &self.root.join(SIDEBAR_HIDDEN),
            hidden.to_string().as_bytes(),
        )
    }

    fn draft_dir(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(id))
    }
}

/// The drafts list order: most recently active first, ties by id.
pub fn by_activity(a: &Draft, b: &Draft) -> Ordering {
    b.meta
        .updated_at
        .cmp(&a.meta.updated_at)
        .then_with(|| a.id.cmp(&b.id))
}

/// Overrides [`data_dir`], so a development build can run beside the
/// installed app without touching its drafts, settings or socket. Settings
/// follow it unless [`crate::CONFIG_DIR_VAR`] is set.
pub const DATA_DIR_VAR: &str = "DRAFTMUX_DATA_DIR";

/// The app's data directory: `$DRAFTMUX_DATA_DIR` when set, else
/// `~/Library/Application Support/Draftmux` on macOS or
/// `$XDG_DATA_HOME/Draftmux` (normally `~/.local/share/Draftmux`) on Linux.
pub fn data_dir() -> Result<PathBuf> {
    data_dir_from(std::env::var_os(DATA_DIR_VAR))
}

fn data_dir_from(var: Option<std::ffi::OsString>) -> Result<PathBuf> {
    match var.filter(|dir| !dir.is_empty()).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        Some(dir) => bail!("{DATA_DIR_VAR} must be an absolute path: {}", dir.display()),
        None => {
            let data = dirs::data_dir().context("no application data directory")?;
            Ok(data.join("Draftmux"))
        }
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
            source: Some(Source {
                kind: SourceKind::Pi,
                path: "/tmp/source session.jsonl".into(),
            }),
            cwd: "/tmp/repo".into(),
            pi_agent_dir: "/tmp/.pi/agent".into(),
            note: None,
            settings: Default::default(),
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
    fn data_dir_override_must_be_absolute() {
        assert_eq!(
            data_dir_from(Some("/tmp/dmx/data".into())).unwrap(),
            Path::new("/tmp/dmx/data")
        );
        assert!(data_dir_from(Some("data".into())).is_err());
        let default = data_dir_from(None).unwrap();
        assert_eq!(default, dirs::data_dir().unwrap().join("Draftmux"));
        assert_eq!(data_dir_from(Some("".into())).unwrap(), default);
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
    fn handed_off_drafts_start_with_the_first_draft_pending() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let handed_off = store.create(new_draft("a")).unwrap();
        assert!(handed_off.meta.source.is_some());
        assert!(handed_off.meta.first_draft_pending);

        let sample = store
            .create(NewDraft {
                source: None,
                ..new_draft("b")
            })
            .unwrap();
        assert!(!sample.meta.first_draft_pending);

        let cleared = store
            .update_meta(&handed_off.id, |meta| meta.first_draft_pending = false)
            .unwrap();
        assert_eq!(store.load(&handed_off.id).unwrap().meta, cleared);
    }

    #[test]
    fn create_with_session_records_the_file_before_meta() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = store
            .create_with_session(new_draft("a"), |draft| {
                assert!(draft.draft_path().is_file());
                assert!(!draft.dir.join(META_FILE).exists());
                let file = draft.session_dir().join("s.jsonl");
                std::fs::write(&file, "")?;
                Ok(file)
            })
            .unwrap();
        let file = draft.session_dir().join("s.jsonl");
        assert_eq!(draft.meta.pi_session_file.as_ref(), Some(&file));
        assert_eq!(store.load(&draft.id).unwrap().meta, draft.meta);
    }

    #[test]
    fn failed_session_step_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let err = store
            .create_with_session(new_draft("a"), |_| bail!("fork failed"))
            .unwrap_err();
        assert_eq!(err.to_string(), "fork failed");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
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
    fn new_drafts_record_their_syntax() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = store.create(new_draft("a")).unwrap();
        assert_eq!(
            store.load(&draft.id).unwrap().meta.syntax,
            Some(Syntax::Markdown)
        );
    }

    #[test]
    fn convert_text_keeps_a_backup_and_resumes_from_it() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        let draft = store.create(new_draft("a")).unwrap();
        store
            .update_meta(&draft.id, |meta| meta.syntax = None)
            .unwrap();
        store.save_text(&draft.id, "*old*").unwrap();
        set_updated(&store, &draft.id, 0);
        let before = store.load(&draft.id).unwrap().meta;

        // An interrupted conversion left the backup and a converted text.
        std::fs::write(draft.dir.join("draft.old.md"), "*old*").unwrap();
        store.save_text(&draft.id, "**old**").unwrap();
        set_updated(&store, &draft.id, 0);

        let (meta, text) = store
            .convert_text(&draft.id, "draft.old.md", Syntax::Markdown, |old| {
                format!("*{old}*")
            })
            .unwrap();
        assert_eq!(text, "**old**");
        assert_eq!(store.read_text(&draft.id).unwrap(), "**old**");
        assert_eq!(
            std::fs::read_to_string(draft.dir.join("draft.old.md")).unwrap(),
            "*old*"
        );
        assert_eq!(meta.syntax, Some(Syntax::Markdown));
        assert_eq!(meta.updated_at, before.updated_at);
        assert_eq!(store.load(&draft.id).unwrap().meta, meta);

        let fresh = store.create(new_draft("b")).unwrap();
        store.save_text(&fresh.id, "x").unwrap();
        store
            .convert_text(
                &fresh.id,
                "draft.old.md",
                Syntax::Markdown,
                str::to_uppercase,
            )
            .unwrap();
        assert_eq!(store.read_text(&fresh.id).unwrap(), "X");
        assert_eq!(
            std::fs::read_to_string(fresh.dir.join("draft.old.md")).unwrap(),
            "x"
        );
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
    fn sidebar_visibility_persists_and_is_not_a_draft() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("drafts");
        let store = Store::new(&path);
        assert!(!store.sidebar_hidden());
        store.set_sidebar_hidden(true).unwrap();
        assert!(Store::new(&path).sidebar_hidden());
        store.set_sidebar_hidden(false).unwrap();
        assert!(!Store::new(&path).sidebar_hidden());
        std::fs::write(path.join(SIDEBAR_HIDDEN), "invalid").unwrap();
        assert!(!store.sidebar_hidden());
        let scan = store.scan().unwrap();
        assert!(scan.drafts.is_empty() && scan.errors.is_empty());
    }

    #[test]
    fn chat_width_round_trips_and_is_not_a_draft() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path());
        assert_eq!(store.chat_width(), None);
        store.set_chat_width(412.5).unwrap();
        assert_eq!(store.chat_width(), Some(412.5));
        assert!(store.scan().unwrap().drafts.is_empty());
    }

    #[test]
    fn sample_marker_persists_and_is_not_a_draft() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("drafts"));
        assert!(!store.sample_created().unwrap());
        store.mark_sample_created().unwrap();
        assert!(store.sample_created().unwrap());

        let scan = store.scan().unwrap();
        assert!(scan.drafts.is_empty() && scan.errors.is_empty());
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
