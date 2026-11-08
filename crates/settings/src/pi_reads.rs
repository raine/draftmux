//! What pi reads in a directory, predicted from files, for the Settings
//! window's read-only panel. Follows pi 1.5.0: project settings are
//! `<dir>/.pi/settings.json`, loaded only when the project is trusted;
//! context files load regardless of trust. A profile extension can still
//! decide trust at runtime, so this is a preview; a running draft's own pi
//! reports the real status.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Entries of `<dir>/.pi/` that make pi ask whether to trust the project.
const TRUST_REQUIRING: [&str; 8] = [
    "settings.json",
    "mcp.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

/// The context file names pi looks for in each folder; the first found
/// is used.
const CONTEXT_NAMES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiReads {
    pub project_settings: Option<ProjectSettings>,
    pub trust: Trust,
    /// In the order pi loads them: the profile's own, then from the root
    /// down to the directory.
    pub context_files: Vec<ContextFile>,
}

/// `<dir>/.pi/settings.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSettings {
    pub path: PathBuf,
    /// `provider/model`.
    pub model: Option<String>,
    pub thinking: Option<String>,
    /// Why the file couldn't be read.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Nothing in the project needs trust.
    NotNeeded,
    /// Saved in the profile's `trust.json` for this folder or a parent.
    Trusted { saved_for: PathBuf },
    /// The profile sets `defaultProjectTrust` to `always`.
    TrustedByDefault,
    /// Saved as not trusted for this folder or a parent.
    Untrusted { saved_for: PathBuf },
    /// No decision; pi in RPC mode can't ask, so it skips the project.
    NotDecided,
    /// The profile's `trust.json` couldn't be read; pi fails to start.
    Unreadable(String),
}

impl Trust {
    /// Whether pi loads the project's `.pi/` resources.
    pub fn loads_project(&self) -> bool {
        matches!(self, Trust::Trusted { .. } | Trust::TrustedByDefault)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: PathBuf,
    /// The profile's own file rather than one up the directory tree.
    pub from_profile: bool,
}

/// What pi started in `dir` with `PI_CODING_AGENT_DIR=profile` reads.
pub fn pi_reads(dir: &Path, profile: &Path, home: &Path) -> PiReads {
    let real = dir.canonicalize().unwrap_or_else(|_| dir.to_owned());
    let settings_path = dir.join(".pi/settings.json");
    PiReads {
        project_settings: settings_path
            .is_file()
            .then(|| project_settings(settings_path)),
        trust: trust(&real, profile, home),
        context_files: context_files(dir, profile),
    }
}

/// JSON as pi reads its files, after an optional byte order mark.
fn parse_json(bytes: &[u8]) -> serde_json::Result<Value> {
    serde_json::from_slice(bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes))
}

fn project_settings(path: PathBuf) -> ProjectSettings {
    let parsed = std::fs::read(&path)
        .map_err(|err| err.to_string())
        .and_then(|bytes| parse_json(&bytes).map_err(|err| err.to_string()));
    match parsed {
        Ok(settings) => {
            let string = |key: &str| settings[key].as_str().map(str::to_owned);
            ProjectSettings {
                model: string("defaultModel").map(|model| match string("defaultProvider") {
                    Some(provider) => format!("{provider}/{model}"),
                    None => model,
                }),
                thinking: string("defaultThinkingLevel"),
                path,
                error: None,
            }
        }
        Err(err) => ProjectSettings {
            path,
            model: None,
            thinking: None,
            error: Some(err),
        },
    }
}

fn needs_trust(real: &Path, home: &Path) -> bool {
    let config = real.join(".pi");
    if TRUST_REQUIRING
        .iter()
        .any(|entry| config.join(entry).exists())
    {
        return true;
    }
    let home = home.canonicalize().unwrap_or_else(|_| home.to_owned());
    let user_skills = home.join(".agents/skills");
    real.ancestors().any(|dir| {
        let skills = dir.join(".agents/skills");
        skills != user_skills && skills.exists()
    })
}

fn trust(real: &Path, profile: &Path, home: &Path) -> Trust {
    if !needs_trust(real, home) {
        return Trust::NotNeeded;
    }
    let path = profile.join("trust.json");
    let saved = match std::fs::read(&path) {
        Ok(bytes) => match parse_json(&bytes) {
            Ok(Value::Object(map)) => map,
            Ok(_) => return Trust::Unreadable(format!("{} isn't an object", path.display())),
            Err(err) => return Trust::Unreadable(format!("{}: {err}", path.display())),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(err) => return Trust::Unreadable(format!("{}: {err}", path.display())),
    };
    // The nearest folder with a saved decision; keys are compared exactly.
    for dir in real.ancestors() {
        let key = dir.to_string_lossy();
        match saved.get(key.as_ref()) {
            Some(Value::Bool(true)) => {
                return Trust::Trusted {
                    saved_for: dir.into(),
                };
            }
            Some(Value::Bool(false)) => {
                return Trust::Untrusted {
                    saved_for: dir.into(),
                };
            }
            _ => {}
        }
    }
    let default = std::fs::read(profile.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|settings| settings["defaultProjectTrust"].as_str().map(str::to_owned));
    match default.as_deref() {
        Some("always") => Trust::TrustedByDefault,
        _ => Trust::NotDecided,
    }
}

fn first_context_file(dir: &Path) -> Option<PathBuf> {
    CONTEXT_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn context_files(dir: &Path, profile: &Path) -> Vec<ContextFile> {
    let mut files: Vec<ContextFile> = first_context_file(profile)
        .map(|path| ContextFile {
            path,
            from_profile: true,
        })
        .into_iter()
        .collect();
    let mut tree: Vec<PathBuf> = dir.ancestors().filter_map(first_context_file).collect();
    tree.reverse();
    for path in tree {
        if !files.iter().any(|file| file.path == path) {
            files.push(ContextFile {
                path,
                from_profile: false,
            });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        profile: PathBuf,
        project: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let profile = home.join(".pi/agent");
        let project = home.join("code/app");
        fs::create_dir_all(&profile).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            home,
            profile,
            project,
        }
    }

    fn reads(f: &Fixture) -> PiReads {
        pi_reads(&f.project, &f.profile, &f.home)
    }

    #[test]
    fn project_without_trust_requiring_files_needs_no_trust() {
        let f = fixture();
        let r = reads(&f);
        assert_eq!(r.trust, Trust::NotNeeded);
        assert_eq!(r.project_settings, None);
        // The user's own skills folder doesn't count.
        fs::create_dir_all(f.home.join(".agents/skills")).unwrap();
        assert_eq!(reads(&f).trust, Trust::NotNeeded);
    }

    #[test]
    fn project_settings_follow_the_nearest_saved_decision() {
        let f = fixture();
        fs::write(
            f.project.join(".pi/settings.json"),
            r#"{"defaultProvider":"anthropic","defaultModel":"sonnet","defaultThinkingLevel":"high"}"#,
        )
        .unwrap();
        let r = reads(&f);
        assert_eq!(r.trust, Trust::NotDecided);
        assert!(!r.trust.loads_project());
        let settings = r.project_settings.unwrap();
        assert_eq!(settings.model.as_deref(), Some("anthropic/sonnet"));
        assert_eq!(settings.thinking.as_deref(), Some("high"));

        let code = f.home.join("code");
        let trust = |json: String| fs::write(f.profile.join("trust.json"), json).unwrap();
        trust(format!(r#"{{"{}": true}}"#, code.display()));
        assert_eq!(
            reads(&f).trust,
            Trust::Trusted {
                saved_for: code.clone()
            }
        );

        // A nearer decision wins; null entries are skipped.
        trust(format!(
            r#"{{"{}": true, "{}": false}}"#,
            code.display(),
            f.project.display()
        ));
        assert_eq!(
            reads(&f).trust,
            Trust::Untrusted {
                saved_for: f.project.clone()
            }
        );
        trust(format!(
            r#"{{"{}": true, "{}": null}}"#,
            code.display(),
            f.project.display()
        ));
        assert!(reads(&f).trust.loads_project());

        trust("[]".into());
        assert!(matches!(reads(&f).trust, Trust::Unreadable(_)));
    }

    #[test]
    fn default_project_trust_always_trusts_undecided_projects() {
        let f = fixture();
        fs::create_dir_all(f.project.join(".pi/extensions")).unwrap();
        fs::write(
            f.profile.join("settings.json"),
            r#"{"defaultProjectTrust":"always"}"#,
        )
        .unwrap();
        assert_eq!(reads(&f).trust, Trust::TrustedByDefault);
    }

    #[test]
    fn project_skills_up_the_tree_need_trust() {
        let f = fixture();
        fs::create_dir_all(f.home.join("code/.agents/skills")).unwrap();
        assert_eq!(reads(&f).trust, Trust::NotDecided);
    }

    #[test]
    fn unparseable_project_settings_are_reported() {
        let f = fixture();
        fs::write(f.project.join(".pi/settings.json"), "{").unwrap();
        assert!(reads(&f).project_settings.unwrap().error.is_some());
    }

    #[test]
    fn context_files_profile_first_then_root_down_one_per_folder() {
        let f = fixture();
        fs::write(f.profile.join("AGENTS.md"), "").unwrap();
        fs::write(f.root.join("CLAUDE.md"), "").unwrap();
        fs::write(f.project.join("CLAUDE.md"), "").unwrap();
        fs::write(f.project.join("AGENTS.md"), "").unwrap();
        let paths: Vec<(PathBuf, bool)> = reads(&f)
            .context_files
            .into_iter()
            .map(|c| (c.path, c.from_profile))
            .collect();
        assert_eq!(
            paths,
            [
                (f.profile.join("AGENTS.md"), true),
                (f.root.join("CLAUDE.md"), false),
                (f.project.join("AGENTS.md"), false),
            ]
        );
    }
}
