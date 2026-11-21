use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Contents of a draft's `meta.json`. The draft ID is the directory name and
/// is not stored here.
///
/// Read and written through [`MetaFile`], which keeps the Pi layout older
/// builds read and rejects any backend descriptor that isn't valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MetaFile", into = "MetaFile")]
pub struct Meta {
    pub target: Target,
    /// The markup the text is written in. A Slack draft without it is in
    /// Slack's mrkdwn.
    pub syntax: Option<Syntax>,
    pub status: Status,
    pub title: String,
    /// The session the draft was handed off from; none for the sample draft
    /// and a file opened with `draftmux <FILE>`.
    /// Independent of [`Meta::backend`]: a Claude Code transcript converted
    /// into a pi session is a Claude source on the Pi backend.
    pub source: Option<Source>,
    /// Working directory the agent runs in.
    pub cwd: PathBuf,
    /// The agent the draft runs on, fixed when the draft is created.
    pub backend: Backend,
    /// Free-form note from the agent that handed the draft off
    /// (`draftmux open --note`).
    pub note: Option<String>,
    /// The summary of the source session the draft started from instead of
    /// its history (`draftmux open --summary`). The draft's session starts
    /// fresh, and the source's transcript stays available for details the
    /// summary leaves out.
    pub summary: Option<String>,
    /// Whether the app still has to ask the agent for the first draft. Set
    /// for a handed-off draft and cleared once the app sends that request,
    /// so it is sent once even if the app is restarted.
    pub first_draft_pending: bool,
    /// Settings resolved for the draft's directory when it was created.
    /// Later settings changes leave them alone.
    pub settings: DraftSettings,
    /// The file the draft was opened from (`draftmux open --draft-file`),
    /// which saving writes the draft back to.
    pub file: Option<LinkedFile>,
    pub created_at: DateTime<Utc>,
    /// Last activity; the drafts list is sorted by this, newest first.
    pub updated_at: DateTime<Utc>,
}

impl Meta {
    /// The sample draft is the only draft with neither a source session nor
    /// a linked file.
    pub fn is_sample(&self) -> bool {
        self.source.is_none() && self.file.is_none()
    }

    /// Whether the text is in Slack's mrkdwn rather than Markdown.
    pub fn is_mrkdwn(&self) -> bool {
        self.target == Target::Slack && self.syntax.is_none()
    }

    /// The draft's pi session; none for a draft on another backend.
    pub fn pi(&self) -> Option<&PiSession> {
        match &self.backend {
            Backend::Pi(pi) => Some(pi),
            Backend::Claude(_) => None,
        }
    }

    /// Errors for a descriptor [`MetaFile`] would reject, so the store
    /// never writes one it couldn't read back.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let Backend::Claude(claude) = &self.backend else {
            return Ok(());
        };
        if self.settings.selection_model.is_some() {
            return Err("a Claude draft has a pi selection_model".into());
        }
        if let Some(dir) = &claude.config_dir
            && !dir.is_absolute()
        {
            return Err(format!(
                "the Claude config_dir {} is not absolute",
                dir.display()
            ));
        }
        if claude
            .fork
            .as_ref()
            .is_some_and(|fork| fork.source_session_id == claude.session_id)
        {
            return Err("a Claude draft's session is its fork source".into());
        }
        if let Some(transcript) = &claude.transcript {
            if !transcript.is_absolute() {
                return Err(format!(
                    "the Claude transcript {} is not absolute",
                    transcript.display()
                ));
            }
            // Claude Code resumes a transcript in place only under this
            // name, and writes to a new file beside it otherwise.
            let name = format!("{}.jsonl", claude.session_id);
            if transcript.file_name() != Some(name.as_ref()) {
                return Err(format!(
                    "the Claude transcript {} is not named {name}",
                    transcript.display()
                ));
            }
        }
        Ok(())
    }
}

/// The agent a draft runs on (see [`Meta::backend`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Pi(PiSession),
    Claude(ClaudeSession),
}

impl Backend {
    /// A Pi backend pinned to the profile `agent_dir`, before its session
    /// exists.
    pub fn pi(agent_dir: impl Into<PathBuf>) -> Self {
        Backend::Pi(PiSession {
            agent_dir: agent_dir.into(),
            session_file: None,
        })
    }
}

/// A draft's pi session. Stored as the top-level `pi_session_file` and
/// `pi_agent_dir`, without a `backend` key, as before there were other
/// backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiSession {
    /// pi profile the draft is pinned to (`PI_CODING_AGENT_DIR`).
    pub agent_dir: PathBuf,
    /// The draft's pi session file, known once the session has been created.
    pub session_file: Option<PathBuf>,
}

/// A draft's Claude Code session. Stored under `backend` with no `pi_*`
/// keys, so a build that predates other backends fails to read the draft
/// rather than run it on pi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSession {
    /// The native session ID (`--session-id`), known before the session
    /// has a transcript.
    pub session_id: Uuid,
    /// Where the session's transcript, `<session_id>.jsonl`, was last
    /// recorded: in Claude Code's storage after the first run, then in the
    /// draft's `session/` once moved there. None until it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` for the session; none leaves it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<PathBuf>,
    /// The Claude Code session the draft's session forks, from a snapshot
    /// in the draft's `session/`; none for a session of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<ClaudeFork>,
    /// The model the user chose for the draft, as Claude Code's `--model`
    /// takes it, such as `sonnet`; passed to every process. None continues
    /// on the session's own model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The effort the user chose for the draft, as Claude Code's
    /// `--effort` takes it, such as `high`; passed to every process. None
    /// leaves it to the model's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The model selection actions run on, as `--model` takes it, kept from
    /// the settings at creation. None uses `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_model: Option<String>,
}

/// Where a draft's Claude Code session forked its source (see
/// [`ClaudeSession::fork`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeFork {
    /// The source's native session ID.
    pub source_session_id: Uuid,
    /// The uuid of the source's newest message when the draft was created.
    /// The conversation up to it is the source's, after it the draft's.
    pub boundary: Uuid,
}

/// The `backend` value of `meta.json`. Pi has no variant: a draft without
/// `backend` is a Pi draft.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum BackendFile {
    Claude(ClaudeSession),
}

/// `meta.json` as stored. The `pi_*` and `backend` keys read as present even
/// when `null`, so a key that doesn't belong is an error whatever its value;
/// only `pi_session_file` may be `null`.
#[derive(Serialize, Deserialize)]
struct MetaFile {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    syntax: Option<Syntax>,
    status: Status,
    #[serde(default)]
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<Source>,
    cwd: PathBuf,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    backend: Option<BackendFile>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pi_session_file: Option<Option<PathBuf>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pi_agent_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    first_draft_pending: bool,
    #[serde(default)]
    settings: DraftSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<LinkedFile>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// `Some` for a key that is present, so `null` reaches `T`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl TryFrom<MetaFile> for Meta {
    type Error = String;

    fn try_from(file: MetaFile) -> Result<Self, String> {
        let backend = match (file.backend, file.pi_session_file, file.pi_agent_dir) {
            (None, session_file, Some(agent_dir)) => Backend::Pi(PiSession {
                agent_dir,
                session_file: session_file.flatten(),
            }),
            (None, _, None) => return Err("missing field `pi_agent_dir`".into()),
            (Some(BackendFile::Claude(claude)), None, None) => Backend::Claude(claude),
            (Some(BackendFile::Claude(_)), _, _) => {
                return Err("a Claude draft has pi_session_file or pi_agent_dir".into());
            }
        };
        let meta = Meta {
            target: file.target,
            syntax: file.syntax,
            status: file.status,
            title: file.title,
            source: file.source,
            cwd: file.cwd,
            backend,
            note: file.note,
            summary: file.summary,
            first_draft_pending: file.first_draft_pending,
            settings: file.settings,
            file: file.file,
            created_at: file.created_at,
            updated_at: file.updated_at,
        };
        meta.validate()?;
        Ok(meta)
    }
}

impl From<Meta> for MetaFile {
    fn from(meta: Meta) -> Self {
        let (backend, pi_session_file, pi_agent_dir) = match meta.backend {
            Backend::Pi(pi) => (None, Some(pi.session_file), Some(pi.agent_dir)),
            Backend::Claude(claude) => (Some(BackendFile::Claude(claude)), None, None),
        };
        MetaFile {
            target: meta.target,
            syntax: meta.syntax,
            status: meta.status,
            title: meta.title,
            source: meta.source,
            cwd: meta.cwd,
            backend,
            pi_session_file,
            pi_agent_dir,
            note: meta.note,
            summary: meta.summary,
            first_draft_pending: meta.first_draft_pending,
            settings: meta.settings,
            file: meta.file,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// GitHub Markdown as a comment, issue or PR shows it.
    Github,
    /// GitHub Markdown as a file in a repository, such as README.md, shows
    /// it: a single newline inside a paragraph is a space.
    #[serde(rename = "github-file")]
    GithubFile,
    Slack,
    /// Markdown pasted as rich text, such as into an email or a document.
    Rich,
    /// Plain text, such as an email body. `email` also reads as this
    /// target.
    #[serde(alias = "email")]
    Plain,
}

impl Target {
    /// Every target, in the order the app offers them.
    pub const ALL: &[Target] = &[
        Target::Github,
        Target::GithubFile,
        Target::Slack,
        Target::Rich,
        Target::Plain,
    ];

    /// The name settings, `meta.json` and `draftmux open --target` use.
    pub fn name(self) -> &'static str {
        match self {
            Target::Github => "github",
            Target::GithubFile => "github-file",
            Target::Slack => "slack",
            Target::Rich => "rich",
            Target::Plain => "plain",
        }
    }

    /// Other names that read as this target.
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Target::Plain => &["email"],
            _ => &[],
        }
    }

    /// The target `name` or one of its aliases names.
    pub fn from_name(name: &str) -> Option<Target> {
        Target::ALL
            .iter()
            .copied()
            .find(|target| target.name() == name || target.aliases().contains(&name))
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Github => "GitHub",
            Target::GithubFile => "GitHub file",
            Target::Slack => "Slack",
            Target::Rich => "Rich text",
            Target::Plain => "Plain text",
        }
    }
}

/// The markup of a draft's text (see [`Meta::syntax`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Syntax {
    /// Markdown, read with Slack's line and block rules in a Slack draft,
    /// or plain text in a plain-text draft.
    Markdown,
}

/// The settings a draft keeps from its creation (see [`Meta::settings`]).
/// A `meta.json` without `settings` loads with these defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftSettings {
    /// Sent to the drafting agent by the bundled pi extension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Whether pi loads `AGENTS.md`/`CLAUDE.md` context files; off runs pi
    /// with `--no-context-files`.
    pub context_files: bool,
    /// The model selection actions run on; none keeps the draft session's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_model: Option<ModelChoice>,
}

impl Default for DraftSettings {
    fn default() -> Self {
        Self {
            instructions: None,
            context_files: true,
            selection_model: None,
        }
    }
}

/// A model with the thinking level it runs at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model_id: String,
    /// A pi thinking level such as `high`.
    pub thinking: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Archived,
}

/// A file a draft was opened from (see [`Meta::file`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedFile {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// SHA-256, in hex, of the file's contents when the draft last read or
    /// wrote it. A file that hashes otherwise was changed outside the
    /// draft.
    pub sha256: String,
}

/// The session the draft was handed off from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Pi,
    Claude,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const FIXTURE: &str = r#"{
  "target": "slack",
  "status": "archived",
  "title": "Release notes",
  "source": {
    "kind": "claude",
    "path": "/Users/me/.claude/projects/x/abc.jsonl"
  },
  "cwd": "/Users/me/code/app",
  "pi_session_file": "/Users/me/Library/Application Support/Draftmux/drafts/x/session/s.jsonl",
  "pi_agent_dir": "/Users/me/.pi/agent",
  "settings": {
    "instructions": "Short paragraphs.",
    "context_files": false,
    "selection_model": {
      "provider": "anthropic",
      "model_id": "claude-haiku-5-5",
      "thinking": "low"
    }
  },
  "created_at": "2026-11-08T10:00:00Z",
  "updated_at": "2026-11-08T11:30:00Z"
}"#;

    #[test]
    fn round_trips_through_json() {
        let meta: Meta = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(meta.status, Status::Archived);
        assert_eq!(meta.source.as_ref().unwrap().kind, SourceKind::Claude);
        // A converted Claude Code transcript runs on Pi.
        assert_eq!(
            meta.pi().unwrap().agent_dir,
            Path::new("/Users/me/.pi/agent")
        );
        assert_eq!(meta.updated_at.to_rfc3339(), "2026-11-08T11:30:00+00:00");

        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert_eq!(json, FIXTURE);
    }

    #[test]
    fn optional_fields_default_when_missing() {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("title");
        object.remove("pi_session_file");
        object.remove("settings");

        let meta: Meta = serde_json::from_value(value).unwrap();
        assert_eq!(meta.title, "");
        assert_eq!(meta.syntax, None);
        assert_eq!(meta.backend, Backend::pi("/Users/me/.pi/agent"));
        assert_eq!(meta.note, None);
        assert!(!meta.first_draft_pending);
        assert_eq!(meta.settings, DraftSettings::default());
        assert!(meta.settings.context_files);

        // Written as before there were other backends.
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["pi_session_file"], serde_json::Value::Null);
        assert!(json.get("backend").is_none());
    }

    #[test]
    fn sample_draft_has_no_source_key() {
        let mut meta: Meta = serde_json::from_str(FIXTURE).unwrap();
        meta.source = None;
        let json = serde_json::to_value(&meta).unwrap();
        assert!(json.get("source").is_none());
        let back: Meta = serde_json::from_value(json).unwrap();
        assert!(back.is_sample());
    }

    #[test]
    fn linked_file_is_stored_only_when_known() {
        let mut meta: Meta = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(meta.file, None);
        assert!(serde_json::to_value(&meta).unwrap().get("file").is_none());
        meta.file = Some(LinkedFile {
            path: "/Users/me/code/app/README.md".into(),
            sha256: "ab12".into(),
        });
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(
            json["file"],
            serde_json::json!({"path": "/Users/me/code/app/README.md", "sha256": "ab12"})
        );
        assert_eq!(serde_json::from_value::<Meta>(json).unwrap(), meta);
    }

    #[test]
    fn syntax_is_stored_by_name_when_known() {
        let mut meta: Meta = serde_json::from_str(FIXTURE).unwrap();
        assert!(serde_json::to_value(&meta).unwrap().get("syntax").is_none());
        meta.syntax = Some(Syntax::Markdown);
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["syntax"], "markdown");
        assert_eq!(serde_json::from_value::<Meta>(json).unwrap(), meta);
    }

    #[test]
    fn targets_are_stored_by_name() {
        for (target, json) in [
            (Target::Github, r#""github""#),
            (Target::GithubFile, r#""github-file""#),
            (Target::Slack, r#""slack""#),
            (Target::Rich, r#""rich""#),
            (Target::Plain, r#""plain""#),
        ] {
            assert_eq!(serde_json::to_string(&target).unwrap(), json);
            assert_eq!(serde_json::from_str::<Target>(json).unwrap(), target);
        }
        assert_eq!(
            serde_json::from_str::<Target>(r#""email""#).unwrap(),
            Target::Plain
        );
    }

    #[test]
    fn target_names_match_serde() {
        for &target in Target::ALL {
            let json = format!("\"{}\"", target.name());
            assert_eq!(serde_json::to_string(&target).unwrap(), json);
            assert_eq!(Target::from_name(target.name()), Some(target));
            for alias in target.aliases() {
                let json = format!("\"{alias}\"");
                assert_eq!(serde_json::from_str::<Target>(&json).unwrap(), target);
                assert_eq!(Target::from_name(alias), Some(target));
            }
        }
        assert_eq!(Target::from_name("Github"), None);
    }

    const CLAUDE_FIXTURE: &str = r#"{
  "target": "github",
  "syntax": "markdown",
  "status": "active",
  "title": "PR body",
  "source": {
    "kind": "claude",
    "path": "/Users/me/.claude/projects/x/abc.jsonl"
  },
  "cwd": "/Users/me/code/app",
  "backend": {
    "kind": "claude",
    "session_id": "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60",
    "transcript": "/Users/me/.claude/projects/-Users-me-code-app/0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60.jsonl",
    "config_dir": "/Users/me/.claude-work",
    "fork": {
      "source_session_id": "6c1d1b9e-2f4a-4b8e-9d3c-7a5e1f0b2c4d",
      "boundary": "0cf5a4d1-79c4-4901-a3ef-8d3611211105"
    },
    "model": "sonnet",
    "selection_model": "haiku"
  },
  "first_draft_pending": true,
  "settings": {
    "context_files": true
  },
  "created_at": "2026-11-08T10:00:00Z",
  "updated_at": "2026-11-08T11:30:00Z"
}"#;

    fn claude_value() -> serde_json::Value {
        serde_json::from_str(CLAUDE_FIXTURE).unwrap()
    }

    fn parse(value: serde_json::Value) -> Result<Meta, String> {
        serde_json::from_value(value).map_err(|err| err.to_string())
    }

    // Its fixture holds Unix absolute paths.
    #[cfg(unix)]
    #[test]
    fn claude_session_round_trips_without_pi_keys() {
        let meta: Meta = serde_json::from_str(CLAUDE_FIXTURE).unwrap();
        let Backend::Claude(claude) = &meta.backend else {
            panic!("not Claude: {:?}", meta.backend);
        };
        assert_eq!(
            claude.session_id.to_string(),
            "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60"
        );
        assert!(meta.pi().is_none());
        assert_eq!(serde_json::to_string_pretty(&meta).unwrap(), CLAUDE_FIXTURE);

        // Before the session has a transcript, and without a config dir.
        let mut value = claude_value();
        let backend = value["backend"].as_object_mut().unwrap();
        backend.remove("transcript");
        backend.remove("config_dir");
        backend.remove("fork");
        backend.remove("model");
        backend.remove("selection_model");
        let meta = parse(value.clone()).unwrap();
        let Backend::Claude(claude) = &meta.backend else {
            panic!("not Claude");
        };
        assert_eq!((&claude.transcript, &claude.config_dir), (&None, &None));
        assert_eq!((&claude.fork, &claude.model), (&None, &None));
        assert_eq!(claude.selection_model, None);
        assert_eq!(serde_json::to_value(&meta).unwrap(), value);
    }

    #[test]
    fn rejects_unknown_and_inconsistent_descriptors() {
        type Edit = fn(&mut serde_json::Value);
        let cases: &[(&str, Edit)] = &[
            ("unknown backend", |v| v["backend"]["kind"] = "codex".into()),
            // Pi is stored without a backend, so `pi` has no second form.
            ("explicit pi", |v| {
                v["backend"] = serde_json::json!({"kind": "pi"})
            }),
            ("backend without kind", |v| {
                v["backend"].as_object_mut().unwrap().remove("kind");
            }),
            ("null backend", |v| v["backend"] = serde_json::Value::Null),
            ("unknown Claude field", |v| {
                v["backend"]["temperature"] = "high".into()
            }),
            ("Claude with a pi profile", |v| {
                v["pi_agent_dir"] = "/Users/me/.pi/agent".into();
            }),
            ("Claude with a null pi session", |v| {
                v["pi_session_file"] = serde_json::Value::Null;
            }),
            ("Claude with a null pi profile", |v| {
                v["pi_agent_dir"] = serde_json::Value::Null;
            }),
            ("Claude with a pi selection model", |v| {
                v["settings"]["selection_model"] = serde_json::json!({
                    "provider": "anthropic",
                    "model_id": "claude-haiku-5-5",
                    "thinking": "low"
                });
            }),
            ("bad session id", |v| {
                v["backend"]["session_id"] = "abc".into()
            }),
            ("missing session id", |v| {
                v["backend"].as_object_mut().unwrap().remove("session_id");
            }),
            ("relative transcript", |v| {
                v["backend"]["transcript"] = "0196f1d2-7c1e-7a40-9b8e-3d1c2b4a5f60.jsonl".into();
            }),
            ("transcript named for another session", |v| {
                v["backend"]["transcript"] = "/tmp/copy.jsonl".into();
            }),
            ("fork of itself", |v| {
                v["backend"]["fork"]["source_session_id"] = v["backend"]["session_id"].clone()
            }),
            ("fork without boundary", |v| {
                v["backend"]["fork"]
                    .as_object_mut()
                    .unwrap()
                    .remove("boundary");
            }),
            ("bad fork boundary", |v| {
                v["backend"]["fork"]["boundary"] = "leaf".into()
            }),
            ("unknown fork field", |v| {
                v["backend"]["fork"]["snapshot"] = "/tmp/source.jsonl".into()
            }),
            ("relative config dir", |v| {
                v["backend"]["config_dir"] = ".claude".into()
            }),
            ("Claude draft without backend", |v| {
                v.as_object_mut().unwrap().remove("backend");
            }),
        ];
        for (name, edit) in cases {
            let mut value = claude_value();
            edit(&mut value);
            assert!(parse(value).is_err(), "{name} accepted");
        }

        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        value["pi_agent_dir"] = serde_json::Value::Null;
        assert!(parse(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("pi_agent_dir");
        assert_eq!(parse(value).unwrap_err(), "missing field `pi_agent_dir`");
    }

    /// `Meta` as builds before other backends read it: `pi_agent_dir` is
    /// required and unknown keys are ignored.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PiOnlyMeta {
        target: Target,
        #[serde(default)]
        syntax: Option<Syntax>,
        status: Status,
        #[serde(default)]
        title: String,
        #[serde(default)]
        source: Option<Source>,
        cwd: PathBuf,
        #[serde(default)]
        pi_session_file: Option<PathBuf>,
        pi_agent_dir: PathBuf,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        first_draft_pending: bool,
        #[serde(default)]
        settings: DraftSettings,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    }

    #[test]
    fn older_builds_reject_claude_drafts_and_read_pi_ones() {
        let err = serde_json::from_str::<PiOnlyMeta>(CLAUDE_FIXTURE)
            .err()
            .expect("an older build read a Claude draft");
        assert!(
            err.to_string().contains("missing field `pi_agent_dir`"),
            "{err}"
        );

        let meta: Meta = serde_json::from_str(FIXTURE).unwrap();
        let old: PiOnlyMeta = serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        let pi = meta.pi().unwrap();
        assert_eq!(old.pi_agent_dir, pi.agent_dir);
        assert_eq!(old.pi_session_file, pi.session_file);
    }
}
