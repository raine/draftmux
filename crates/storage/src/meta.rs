use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Contents of a draft's `meta.json`. The draft ID is the directory name and
/// is not stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub target: Target,
    pub status: Status,
    #[serde(default)]
    pub title: String,
    pub source: Source,
    /// Working directory pi runs in.
    pub cwd: PathBuf,
    /// The draft's pi session file, known once the session has been created.
    #[serde(default)]
    pub pi_session_file: Option<PathBuf>,
    /// pi profile the draft is pinned to (`PI_CODING_AGENT_DIR`).
    pub pi_agent_dir: PathBuf,
    /// Free-form note from the agent that handed the draft off
    /// (`draftmux open --note`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Last activity; the drafts list is sorted by this, newest first.
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Github,
    Slack,
    /// A plain-text email body.
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Archived,
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
  "created_at": "2026-11-08T10:00:00Z",
  "updated_at": "2026-11-08T11:30:00Z"
}"#;

    #[test]
    fn round_trips_through_json() {
        let meta: Meta = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(meta.target, Target::Slack);
        assert_eq!(meta.status, Status::Archived);
        assert_eq!(meta.source.kind, SourceKind::Claude);
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

        let meta: Meta = serde_json::from_value(value).unwrap();
        assert_eq!(meta.title, "");
        assert_eq!(meta.pi_session_file, None);
        assert_eq!(meta.note, None);
    }

    #[test]
    fn email_target_is_stored_as_email() {
        let json = FIXTURE.replace(r#""target": "slack""#, r#""target": "email""#);
        let meta: Meta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta.target, Target::Email);
        assert_eq!(serde_json::to_string_pretty(&meta).unwrap(), json);
    }
}
