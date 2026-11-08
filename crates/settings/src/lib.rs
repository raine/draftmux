//! Draftmux's own settings: defaults for every directory, directory rules
//! that override them, and how they resolve for a draft's working directory.
//!
//! The Settings window writes `settings.json` in Draftmux's data directory;
//! `draftmux open` reads it, resolves it for the source session's cwd and
//! keeps the result in the new draft, so changes apply to new drafts only.
//! The alternatives count is an app preference read on every selection call.

mod matching;
mod pi_reads;
mod resolve;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use draftmux_pi_rpc::ModelOverride;
use draftmux_storage::Target;
use serde::{Deserialize, Serialize};

pub use matching::{expand_home, tilde_path};
pub use pi_reads::{ContextFile, PiReads, ProjectSettings, Trust, pi_reads};
pub use resolve::{Resolved, Source, Sourced};

/// Alternatives a selection action asks for unless set.
pub const DEFAULT_ALTERNATIVES: usize = 3;
/// The alternatives counts offered.
pub const ALTERNATIVES: std::ops::RangeInclusive<usize> = 1..=5;

/// The profile pi uses when nothing names one.
pub fn default_profile(home: &Path) -> PathBuf {
    home.join(".pi/agent")
}

/// Contents of `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The All directories scope.
    pub defaults: Scope,
    /// In the order the user added them; matching doesn't depend on it.
    pub directories: Vec<Rule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<usize>,
}

/// One scope's values; an unset field inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    /// pi profile folder (`PI_CODING_AGENT_DIR`), `~` collapsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drafting_model: Option<DraftingModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_model: Option<SelectionModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// Never empty; clearing the text unsets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_files: Option<bool>,
}

/// Settings for drafts whose working directory is `path` or below it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// As shown, with `~` for the home directory.
    pub path: PathBuf,
    #[serde(flatten)]
    pub scope: Scope,
    /// The rule's instructions replace the inherited ones instead of
    /// following them; with no instructions of its own, it clears them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_instructions: bool,
}

impl Rule {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            scope: Scope::default(),
            replace_instructions: false,
        }
    }

    /// How many form rows the rule overrides, for the sidebar.
    pub fn override_count(&self) -> usize {
        let s = &self.scope;
        [
            s.profile.is_some(),
            s.drafting_model.is_some(),
            s.selection_model.is_some(),
            s.target.is_some(),
            s.instructions.is_some() || self.replace_instructions,
            s.context_files.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }
}

/// The model a new draft starts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftingModel {
    /// Continue on the source session's model, keeping its prompt cache.
    Session,
    Model(ModelOverride),
}

impl DraftingModel {
    pub fn model(&self) -> Option<&ModelOverride> {
        match self {
            DraftingModel::Session => None,
            DraftingModel::Model(model) => Some(model),
        }
    }
}

/// The model selection actions run on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionModel {
    /// Whatever the draft's session runs on, keeping its prompt cache.
    Drafting,
    Model(ModelOverride),
}

impl SelectionModel {
    pub fn model(&self) -> Option<&ModelOverride> {
        match self {
            SelectionModel::Drafting => None,
            SelectionModel::Model(model) => Some(model),
        }
    }
}

impl Settings {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    /// The settings in `data_dir`; the built-in defaults when there's no
    /// file yet. A file that doesn't parse is an error.
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let mut json = serde_json::to_vec_pretty(self).expect("settings serialize");
        json.push(b'\n');
        draftmux_storage::write_atomic(&Self::path(data_dir), &json)
    }

    pub fn alternatives(&self) -> usize {
        self.alternatives.unwrap_or(DEFAULT_ALTERNATIVES)
    }
}

#[cfg(test)]
mod tests {
    use draftmux_pi_rpc::ThinkingLevel;

    use super::*;

    #[test]
    fn missing_file_is_the_defaults_and_saves_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Settings::default());

        let mut settings = Settings::default();
        settings.defaults.drafting_model = Some(DraftingModel::Session);
        settings.defaults.selection_model = Some(SelectionModel::Model(ModelOverride {
            provider: "anthropic".into(),
            model_id: "claude-haiku-5-5".into(),
            thinking: ThinkingLevel::Low,
        }));
        let mut rule = Rule::new("~/work".into());
        rule.scope.target = Some(Target::Slack);
        rule.replace_instructions = true;
        settings.directories.push(rule);
        settings.alternatives = Some(4);
        settings.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), settings);

        let json = std::fs::read_to_string(Settings::path(dir.path())).unwrap();
        assert!(json.contains(r#""drafting_model": "session""#), "{json}");
        assert!(json.contains(r#""path": "~/work""#), "{json}");
    }

    #[test]
    fn unparseable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Settings::path(dir.path()), "{\"defaults\":").unwrap();
        assert!(Settings::load(dir.path()).is_err());
    }

    #[test]
    fn override_count_counts_rows() {
        let mut rule = Rule::new("/r".into());
        assert_eq!(rule.override_count(), 0);
        rule.replace_instructions = true;
        rule.scope.target = Some(Target::Github);
        assert_eq!(rule.override_count(), 2);
        rule.scope.instructions = Some("x".into());
        assert_eq!(rule.override_count(), 2);
    }
}
