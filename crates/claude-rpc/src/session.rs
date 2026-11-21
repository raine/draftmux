//! A draft's own Claude Code session: forking it from a snapshot of the
//! source transcript, keeping its transcript in the draft, and reading the
//! conversation back.
//!
//! Claude Code 2.1.293 has no option for where a session's transcript goes.
//! A new session or a fork writes it to `<config>/projects/<cwd>/<id>.jsonl`
//! with the run of its first prompt, and keeps writing there by path while
//! the process lives. Once no process has the session, the draft adopts the
//! file as `<dir>/<id>.jsonl`; resuming that path appends to it in place.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};

use draftmux_agent::History;

use crate::history;
use crate::transcript;

/// The name of a fork's snapshot of its source transcript in
/// [`DraftSession::dir`].
const SNAPSHOT: &str = "source.jsonl";

/// A draft's session, from its descriptor. The process the agent starts
/// depends on what exists: the draft's transcript is resumed; a transcript
/// still in Claude's storage is adopted first; otherwise the session starts,
/// as a fork of the snapshot when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSession {
    /// The session ID, a UUID, distinct from a fork's source.
    pub id: String,
    /// The draft's session directory. It holds the transcript `<id>.jsonl`
    /// once adopted, and a fork's snapshot of its source, `source.jsonl`.
    pub dir: PathBuf,
    /// `CLAUDE_CONFIG_DIR`; none leaves it unset, so Claude uses
    /// `~/.claude`.
    pub config_dir: Option<PathBuf>,
    /// For a fork, the uuid of the line its snapshot ends at: the history
    /// before it is the source conversation.
    pub fork_boundary: Option<String>,
    /// Where the transcript was last recorded
    /// ([`AgentEvent::Transcript`](crate::AgentEvent::Transcript)). Once
    /// set, a transcript that can't be found is an error rather than a
    /// reason to start over.
    pub transcript: Option<PathBuf>,
}

/// How a process continues the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Start {
    /// `--session-id <id>`
    New(String),
    /// `--resume <snapshot> --fork-session --session-id <id>`
    Fork { snapshot: PathBuf, id: String },
    /// `--resume <transcript>`
    Resume(OsString),
}

/// What a session can't start without.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// The recorded transcript is in neither place it can be.
    Transcript(PathBuf),
    /// A fork that hasn't started lost its snapshot.
    Snapshot(PathBuf),
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Missing::Transcript(path) => write!(
                f,
                "the conversation's transcript {} is missing; restore it there to \
                 continue the conversation (the draft's text is unaffected)",
                path.display()
            ),
            Missing::Snapshot(path) => write!(
                f,
                "the copy of the source session {} is missing, so the conversation \
                 can't start (the draft's text is unaffected)",
                path.display()
            ),
        }
    }
}

impl DraftSession {
    /// The transcript once the draft has adopted it.
    pub fn owned(&self) -> PathBuf {
        self.dir.join(format!("{}.jsonl", self.id))
    }

    pub fn snapshot(&self) -> PathBuf {
        self.dir.join(SNAPSHOT)
    }

    /// Where Claude keeps transcripts: `config_dir`, else `.claude` in
    /// `home`.
    pub(crate) fn storage(&self, home: &Path) -> PathBuf {
        self.config_dir
            .clone()
            .unwrap_or_else(|| home.join(".claude"))
    }

    /// The transcript wherever it is now: the draft's, the recorded one, or
    /// the one in `storage`.
    pub(crate) fn locate(&self, storage: &Path) -> io::Result<Option<PathBuf>> {
        let owned = self.owned();
        if owned.is_file() {
            return Ok(Some(owned));
        }
        if let Some(recorded) = self.transcript.as_ref().filter(|path| path.is_file()) {
            return Ok(Some(recorded.clone()));
        }
        // An adoption that finished since the first check moved it here.
        Ok(native(storage, &self.id)?.or_else(|| owned.is_file().then_some(owned)))
    }

    /// Moves a transcript found outside the draft to [`DraftSession::owned`]
    /// and returns its new path. Call only while no process has the session:
    /// claude reopens the transcript by path, so lines written after a move
    /// would go to a new file. Never replaces an existing file.
    pub(crate) fn adopt(&self, storage: &Path) -> io::Result<Option<PathBuf>> {
        let owned = self.owned();
        let Some(found) = self.locate(storage)? else {
            return Ok(None);
        };
        if found == owned {
            return Ok(None);
        }
        move_no_clobber(&found, &owned)?;
        Ok(Some(owned))
    }

    /// How the next process starts, once a found transcript was adopted.
    pub(crate) fn start(&self) -> Result<Start, Missing> {
        let owned = self.owned();
        if owned.is_file() {
            return Ok(Start::Resume(owned.into()));
        }
        if let Some(recorded) = &self.transcript {
            return Err(Missing::Transcript(recorded.clone()));
        }
        if self.fork_boundary.is_some() {
            let snapshot = self.snapshot();
            if !snapshot.is_file() {
                return Err(Missing::Snapshot(snapshot));
            }
            return Ok(Start::Fork {
                snapshot,
                id: self.id.clone(),
            });
        }
        Ok(Start::New(self.id.clone()))
    }

    /// The file holding the conversation so far, left where it is: the
    /// session's transcript, or before a fork's first run its snapshot.
    /// None before a session of its own has started.
    pub(crate) fn conversation(&self, storage: &Path) -> Result<Option<PathBuf>, String> {
        if let Some(path) = self.locate(storage).map_err(|err| err.to_string())? {
            return Ok(Some(path));
        }
        if let Some(recorded) = &self.transcript {
            return Err(Missing::Transcript(recorded.clone()).to_string());
        }
        if self.fork_boundary.is_none() {
            return Ok(None);
        }
        let snapshot = self.snapshot();
        if !snapshot.is_file() {
            return Err(Missing::Snapshot(snapshot).to_string());
        }
        Ok(Some(snapshot))
    }

    /// The conversation so far. Before a fork's first run it is the
    /// snapshot's, all of it earlier messages.
    pub fn history(&self, storage: &Path) -> Result<History, String> {
        let Some(path) = self.conversation(storage)? else {
            return Ok(History {
                earlier: 0,
                messages: Vec::new(),
                context: None,
            });
        };
        let file =
            fs::File::open(&path).map_err(|err| format!("opening {}: {err}", path.display()))?;
        let lines = transcript::parse(BufReader::new(file))
            .map_err(|err| format!("reading {}: {err}", path.display()))?;
        Ok(history::history(&lines, self.fork_boundary.as_deref()))
    }
}

/// The session `id`'s transcript in Claude's `storage`
/// (`projects/<cwd>/<id>.jsonl`). The project directory's name is
/// derived from the cwd in ways that aren't documented, so every one is
/// checked.
pub(crate) fn native(storage: &Path, id: &str) -> io::Result<Option<PathBuf>> {
    let name = format!("{id}.jsonl");
    let projects = match fs::read_dir(storage.join("projects")) {
        Ok(projects) => projects,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    for project in projects {
        let path = project?.path().join(&name);
        if path.is_file() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Moves `from` to `to` unless `to` exists, as a hard link and an unlink,
/// or across file systems a synced copy linked into place.
fn move_no_clobber(from: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::CrossesDevices => {
            let temp = to.with_extension("jsonl.adopting");
            let copied = (|| {
                let mut file = fs::File::create(&temp)?;
                io::copy(&mut fs::File::open(from)?, &mut file)?;
                file.sync_all()?;
                fs::hard_link(&temp, to)
            })();
            let _ = fs::remove_file(&temp);
            copied?;
        }
        Err(err) => return Err(err),
    }
    fs::remove_file(from)
}

/// A consistent copy of a source transcript in a draft's session
/// directory, which the draft's session forks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    /// The source's native session ID.
    pub session_id: String,
    /// The uuid of the source's newest message, where the fork's own
    /// conversation starts after.
    pub boundary: String,
    /// The working directory the source last ran in.
    pub cwd: PathBuf,
}

impl Snapshot {
    /// Reads `source` once, up to its last complete line, and writes that
    /// to `source.jsonl` in `dir`. Only reads `source`, which Claude Code may
    /// be appending to. Fails if the copy wouldn't fork: no messages, or a
    /// compaction in progress.
    pub fn take(source: &Path, dir: &Path) -> Result<Snapshot, String> {
        let mut bytes =
            fs::read(source).map_err(|err| format!("reading {}: {err}", source.display()))?;
        bytes.truncate(bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1));
        let lines = transcript::parse(&bytes[..]).map_err(|err| err.to_string())?;
        let leaf = transcript::leaf(&lines)
            .map_err(|err| format!("{} can't be continued: {err}", source.display()))?;
        let leaf = &lines[leaf];
        let session_id = leaf
            .session_id
            .clone()
            .ok_or_else(|| format!("{} records no session ID", source.display()))?;
        let cwd = lines
            .iter()
            .rev()
            .find_map(|line| line.cwd.as_deref())
            .ok_or_else(|| format!("{} records no working directory", source.display()))?;
        let path = dir.join(SNAPSHOT);
        let write = || -> io::Result<()> {
            let mut file = fs::File::create_new(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()
        };
        write().map_err(|err| format!("writing {}: {err}", path.display()))?;
        Ok(Snapshot {
            path,
            session_id,
            boundary: leaf.uuid.clone(),
            cwd: cwd.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/transcripts/fork-source.jsonl"
    );

    fn draft(dir: &Path) -> DraftSession {
        DraftSession {
            id: "11111111-1111-4111-8111-111111111111".into(),
            dir: dir.to_path_buf(),
            config_dir: None,
            fork_boundary: None,
            transcript: None,
        }
    }

    #[test]
    fn a_snapshot_copies_complete_lines_and_leaves_the_source_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("src.jsonl");
        let mut bytes = fs::read(SOURCE).unwrap();
        let complete = bytes.len();
        bytes.extend_from_slice(br#"{"type":"user","uuid":"cut"#);
        fs::write(&source, &bytes).unwrap();

        let snapshot = Snapshot::take(&source, tmp.path()).unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes, "the source is unchanged");
        assert_eq!(fs::read(&snapshot.path).unwrap(), bytes[..complete]);
        assert_eq!(snapshot.path, tmp.path().join(SNAPSHOT));
        assert_eq!(snapshot.session_id, "cad8e265-e847-4eb0-b351-fc8f2bbca9b5");
        assert_eq!(snapshot.cwd, Path::new("/tmp/c4/w4"));
        let lines = transcript::parse(&bytes[..]).unwrap();
        let leaf = lines.iter().rev().find(|line| line.is_message()).unwrap();
        assert_eq!(snapshot.boundary, leaf.uuid);

        let again = Snapshot::take(&source, tmp.path()).unwrap_err();
        assert!(
            again.contains("writing"),
            "never replaces a snapshot: {again}"
        );
    }

    #[test]
    fn a_snapshot_needs_a_conversation_that_can_be_continued() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty.jsonl");
        fs::write(&empty, "{\"type\":\"summary\"}\n").unwrap();
        assert!(Snapshot::take(&empty, tmp.path()).is_err());

        let compacting = tmp.path().join("compacting.jsonl");
        let mut bytes = fs::read(SOURCE).unwrap();
        bytes.extend_from_slice(
            br#"{"type":"system","subtype":"compact_boundary","uuid":"cb","parentUuid":null}
"#,
        );
        fs::write(&compacting, bytes).unwrap();
        let err = Snapshot::take(&compacting, tmp.path()).unwrap_err();
        assert!(err.contains("compacted"), "{err}");
        assert!(!tmp.path().join(SNAPSHOT).exists());
    }

    #[test]
    fn a_fork_shows_its_snapshot_until_it_has_a_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let mut session = draft(tmp.path());
        assert_eq!(session.history(&storage).unwrap().messages, []);
        session.fork_boundary = Some("b".into());
        let missing = session.history(&storage).unwrap_err();
        assert!(missing.contains("text is unaffected"), "{missing}");
        let snapshot = Snapshot::take(Path::new(SOURCE), tmp.path()).unwrap();
        session.fork_boundary = Some(snapshot.boundary);
        let history = session.history(&storage).unwrap();
        assert!(history.earlier > 0);
        assert_eq!(history.messages, []);
        assert_eq!(
            session.start(),
            Ok(Start::Fork {
                snapshot: snapshot.path,
                id: session.id.clone(),
            })
        );
    }

    #[test]
    fn adopting_moves_the_transcript_from_storage_once() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        let session = draft(&tmp.path().join("draft"));
        fs::create_dir_all(&session.dir).unwrap();
        assert_eq!(session.adopt(&storage).unwrap(), None);
        assert_eq!(session.start(), Ok(Start::New(session.id.clone())));

        let native = storage.join(format!("projects/-tmp-x/{}.jsonl", session.id));
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        fs::write(&native, "line\n").unwrap();
        assert_eq!(session.locate(&storage).unwrap(), Some(native.clone()));
        assert_eq!(session.adopt(&storage).unwrap(), Some(session.owned()));
        assert!(!native.exists());
        assert_eq!(fs::read_to_string(session.owned()).unwrap(), "line\n");
        assert_eq!(session.adopt(&storage).unwrap(), None);
        assert_eq!(session.start(), Ok(Start::Resume(session.owned().into())));
    }

    #[test]
    fn a_move_never_replaces_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("from"), tmp.path().join("to"));
        fs::write(&from, "new").unwrap();
        fs::write(&to, "old").unwrap();
        let err = move_no_clobber(&from, &to).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&from).unwrap(), "new");
        assert_eq!(fs::read_to_string(&to).unwrap(), "old");
    }
}
