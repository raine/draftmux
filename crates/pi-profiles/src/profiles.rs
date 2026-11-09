use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ModelRef;

/// One pi config folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The folder pi runs with as `PI_CODING_AGENT_DIR`.
    pub dir: PathBuf,
    /// A short name from the folder: `pi` for `~/.pi/agent`, `work` for
    /// `~/.pi-work/agent`.
    pub name: String,
}

/// The profiles to offer: every `<home>/.pi*/agent` folder, `~/.pi/agent`
/// first and the rest by name, then `added` in order. A folder reached
/// through two paths (a symlink) is listed once, under the first.
pub fn detect(home: &Path, added: &[PathBuf]) -> Vec<Profile> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let dir = entry.path().join("agent");
            (name.starts_with(".pi") && dir.is_dir()).then_some((name, dir))
        })
        .collect();
    found.sort_by(|(a, _), (b, _)| (a != ".pi", a).cmp(&(b != ".pi", b)));

    let mut profiles: Vec<Profile> = Vec::new();
    let mut seen = Vec::new();
    let candidates = found
        .into_iter()
        .map(|(folder, dir)| (detected_name(&folder), dir))
        .chain(added.iter().map(|dir| (added_name(dir), dir.clone())));
    for (name, dir) in candidates {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if !seen.contains(&key) {
            seen.push(key);
            profiles.push(Profile { dir, name });
        }
    }
    profiles
}

/// `.pi` → `pi`, `.pi-work` → `work`.
fn detected_name(folder: &str) -> String {
    let rest = folder[".pi".len()..].trim_start_matches(['-', '_', '.']);
    if rest.is_empty() { "pi" } else { rest }.to_owned()
}

/// The folder's name, or its parent's for pi's usual `<folder>/agent`.
fn added_name(dir: &Path) -> String {
    let named = match dir.file_name() {
        Some(name) if name == "agent" => dir.parent().unwrap_or(dir),
        _ => dir,
    };
    named
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The folders the user added, in `profiles.json` in Draftmux's
/// configuration directory. Stateless like the drafts store: every call reads the file.
#[derive(Debug, Clone)]
pub struct AddedProfiles {
    path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct AddedFile {
    added: Vec<PathBuf>,
}

impl AddedProfiles {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("profiles.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The added folders; none when the file doesn't exist.
    pub fn load(&self) -> Result<Vec<PathBuf>> {
        Ok(self.read()?.added)
    }

    /// Adds a folder and returns it as stored: a leading `~` expanded and
    /// made absolute without resolving symlinks, as pi does with
    /// `PI_CODING_AGENT_DIR`. Adding a folder twice keeps one entry.
    pub fn add(&self, dir: &Path, home: &Path) -> Result<PathBuf> {
        let dir = match dir.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => dir.to_owned(),
        };
        let dir =
            std::path::absolute(&dir).with_context(|| format!("resolving {}", dir.display()))?;
        if !dir.is_dir() {
            bail!("{} is not a folder", dir.display());
        }
        let mut file = self.read()?;
        if !file.added.contains(&dir) {
            file.added.push(dir.clone());
            let dir_of_file = self.path.parent().expect("profiles.json has a parent");
            std::fs::create_dir_all(dir_of_file)
                .with_context(|| format!("creating {}", dir_of_file.display()))?;
            let json = serde_json::to_vec_pretty(&file).expect("profiles serialize");
            draftmux_storage::write_atomic(&self.path, &json)?;
        }
        Ok(dir)
    }

    fn read(&self) -> Result<AddedFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AddedFile::default()),
            Err(err) => Err(err).with_context(|| format!("reading {}", self.path.display())),
        }
    }
}

/// A profile's defaults from its `settings.json`. pi uses them when a
/// session names no model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Defaults {
    pub model: Option<ModelRef>,
    /// A pi thinking level such as `medium`, as written.
    pub thinking_level: Option<String>,
}

impl Defaults {
    /// `defaultProvider` + `defaultModel` and `defaultThinkingLevel` from
    /// `<agent_dir>/settings.json`. A missing file or key is unknown; a file
    /// that doesn't parse is an error.
    pub fn read(agent_dir: &Path) -> Result<Self> {
        let path = agent_dir.join("settings.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
        let settings: Value =
            serde_json::from_slice(bytes).with_context(|| format!("parsing {}", path.display()))?;
        let string = |key: &str| settings[key].as_str().map(str::to_owned);
        Ok(Self {
            model: string("defaultProvider")
                .zip(string("defaultModel"))
                .map(|(provider, id)| ModelRef { provider, id }),
            thinking_level: string("defaultThinkingLevel"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn names(profiles: &[Profile]) -> Vec<&str> {
        profiles.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn detects_pi_folders_with_an_agent_dir() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        for dir in [
            ".pi-work/agent",
            ".pi/agent",
            ".pi-alt/agent",
            ".pinned",
            ".pip",
        ] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        fs::create_dir_all(home.join(".config/agent")).unwrap();
        fs::write(home.join(".pi-file"), "").unwrap();

        let profiles = detect(home, &[]);
        assert_eq!(names(&profiles), ["pi", "alt", "work"]);
        assert_eq!(profiles[0].dir, home.join(".pi/agent"));
    }

    #[test]
    fn added_folders_follow_and_duplicates_are_dropped() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".pi/agent")).unwrap();
        fs::create_dir_all(home.join("profiles/team/agent")).unwrap();
        fs::create_dir_all(home.join("custom")).unwrap();
        std::os::unix::fs::symlink(home.join(".pi/agent"), home.join("link")).unwrap();

        let added = [
            home.join("profiles/team/agent"),
            home.join("link"),
            home.join("custom"),
            home.join("custom"),
        ];
        let profiles = detect(home, &added);
        assert_eq!(names(&profiles), ["pi", "team", "custom"]);
    }

    #[test]
    fn added_profiles_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join("work/agent")).unwrap();
        let store = AddedProfiles::new(&home.join("data"));
        assert!(store.load().unwrap().is_empty());

        let dir = store.add(Path::new("~/work/agent"), home).unwrap();
        assert_eq!(dir, home.join("work/agent"));
        store.add(&home.join("work/agent"), home).unwrap();
        assert_eq!(store.load().unwrap(), [home.join("work/agent")]);

        assert!(store.add(&home.join("missing"), home).is_err());
        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn defaults_from_settings() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Defaults::read(dir.path()).unwrap(), Defaults::default());

        let settings = dir.path().join("settings.json");
        fs::write(
            &settings,
            "\u{feff}{\"defaultProvider\":\"openai\",\"defaultModel\":\"gpt-6\",\"defaultThinkingLevel\":\"low\"}",
        )
        .unwrap();
        assert_eq!(
            Defaults::read(dir.path()).unwrap(),
            Defaults {
                model: Some(ModelRef::new("openai", "gpt-6")),
                thinking_level: Some("low".into()),
            }
        );

        fs::write(&settings, "{\"defaultModel\":\"gpt-6\"}").unwrap();
        assert_eq!(Defaults::read(dir.path()).unwrap(), Defaults::default());

        fs::write(&settings, "{\"defaultModel\":").unwrap();
        assert!(Defaults::read(dir.path()).is_err());
    }
}
