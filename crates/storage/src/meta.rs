use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Contents of a draft's `meta.json`. The draft ID is the directory name and
/// is not stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub target: Target,
    /// The markup the text is written in. A Slack draft without it is in
    /// Slack's mrkdwn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax: Option<Syntax>,
    pub status: Status,
    #[serde(default)]
    pub title: String,
    /// The session the draft was handed off from; none for the sample draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
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
    /// Whether the app still has to ask pi for the first draft. Set for a
    /// handed-off draft and cleared once the app sends that request, so it
    /// is sent once even if the app is restarted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first_draft_pending: bool,
    /// Settings resolved for the draft's directory when it was created.
    /// Later settings changes leave them alone.
    #[serde(default)]
    pub settings: DraftSettings,
    pub created_at: DateTime<Utc>,
    /// Last activity; the drafts list is sorted by this, newest first.
    pub updated_at: DateTime<Utc>,
}

impl Meta {
    /// The sample draft is the only draft without a source session.
    pub fn is_sample(&self) -> bool {
        self.source.is_none()
    }

    /// Whether the text is in Slack's mrkdwn rather than Markdown.
    pub fn is_mrkdwn(&self) -> bool {
        self.target == Target::Slack && self.syntax.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Github,
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
    pub const ALL: &[Target] = &[Target::Github, Target::Slack, Target::Rich, Target::Plain];

    /// The name settings, `meta.json` and `draftmux open --target` use.
    pub fn name(self) -> &'static str {
        match self {
            Target::Github => "github",
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
        assert_eq!(meta.pi_session_file, None);
        assert_eq!(meta.note, None);
        assert!(!meta.first_draft_pending);
        assert_eq!(meta.settings, DraftSettings::default());
        assert!(meta.settings.context_files);
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
}
