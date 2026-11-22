//! Which tool calls a draft's agent may make: reading is open, writing is
//! limited to the draft file.
//!
//! The agent gets Read, Glob, Grep, Bash, Edit and Write. Edit and Write are
//! confined to the draft; Bash isn't, so this keeps the agent on the draft
//! rather than sandboxing it. Bash may not run in the background, since a
//! process outliving the call escapes the agent's process group.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// What a tool call may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// A call that doesn't write.
    Allow,
    /// An Edit or Write of the draft file.
    DraftWrite,
    /// Refused, with the reason the model is shown.
    Deny(String),
}

/// The draft file of one agent, as located when the agent was made.
#[derive(Debug)]
pub(crate) struct DraftPolicy {
    /// The path the draft is known by, for messages.
    draft: PathBuf,
    /// The draft's directory resolved, with the file name. Storage replaces
    /// `draft.md` by renaming over it, so the draft is always the regular
    /// file at this path; a symlink there isn't the draft.
    pinned: PathBuf,
}

impl DraftPolicy {
    pub(crate) fn new(draft: &Path) -> io::Result<Self> {
        let (Some(dir), Some(name)) = (draft.parent(), draft.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} isn't a file path", draft.display()),
            ));
        };
        Ok(Self {
            draft: draft.to_path_buf(),
            pinned: dir.canonicalize()?.join(name),
        })
    }

    pub(crate) fn draft(&self) -> &Path {
        &self.draft
    }

    /// Decides a call of `tool` with `input` (its `tool_input`).
    pub(crate) fn decide(&self, tool: &str, input: &Value) -> Decision {
        match tool {
            "Read" | "Glob" | "Grep" => Decision::Allow,
            "Bash" if input["run_in_background"] == true => Decision::Deny(
                "Draftmux: Bash can't run commands in the background in this draft; \
                 run them in the foreground."
                    .into(),
            ),
            "Bash" => Decision::Allow,
            "Edit" | "Write" => match input["file_path"].as_str() {
                Some(path) if self.is_draft(path) => Decision::DraftWrite,
                Some(path) => Decision::Deny(format!(
                    "Draftmux: {tool} is limited to the draft file {}; \"{path}\" is outside it. \
                     Give the draft's absolute path, and use Read, Glob, Grep or Bash to \
                     inspect other files.",
                    self.draft.display()
                )),
                None => Decision::Deny(format!(
                    "Draftmux: {tool} needs the absolute path of the draft file {}.",
                    self.draft.display()
                )),
            },
            _ => Decision::Deny(format!(
                "Draftmux: {tool} isn't available in this draft. Use Read, Glob, Grep or \
                 Bash, and Edit or Write on the draft file."
            )),
        }
    }

    /// Whether `path`, as a tool was given it, names the draft file: an
    /// absolute path that resolves to it, through symlinked directories,
    /// `..` or a different letter case where the file system ignores case.
    pub(crate) fn is_draft(&self, path: &str) -> bool {
        let path = Path::new(path);
        if !path.is_absolute() {
            return false;
        }
        let resolved = match path.canonicalize() {
            Ok(resolved) => resolved,
            // Something is there that doesn't resolve, such as a dangling
            // symlink.
            Err(_) if path.symlink_metadata().is_ok() => return false,
            // A draft that doesn't exist yet: its directory must.
            Err(_) => match (path.parent(), path.file_name()) {
                (Some(dir), Some(name)) => match dir.canonicalize() {
                    Ok(dir) => dir.join(name),
                    Err(_) => return false,
                },
                _ => return false,
            },
        };
        resolved == self.pinned
    }
}

/// What the draft holds after the Edit or Write `input` applied to
/// `before`, or `None` when that can't be told.
pub(crate) fn expected(tool: &str, input: &Value, before: Option<&str>) -> Option<String> {
    match tool {
        "Write" => input["content"].as_str().map(str::to_owned),
        "Edit" => {
            let before = before?;
            let old = input["old_string"].as_str().filter(|old| !old.is_empty())?;
            let new = input["new_string"].as_str()?;
            if !before.contains(old) {
                return None;
            }
            // Claude Code's Edit deletes a removed line's newline with it.
            let line = format!("{old}\n");
            let old = if new.is_empty() && !old.ends_with('\n') && before.contains(&line) {
                line.as_str()
            } else {
                old
            };
            Some(if input["replace_all"] == true {
                before.replace(old, new)
            } else {
                before.replacen(old, new, 1)
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    struct Fixture {
        _tmp: tempfile::TempDir,
        /// The temporary directory as given, which on macOS is under a
        /// symlinked `/var`.
        root: PathBuf,
        policy: DraftPolicy,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let dir = root.join("drafts/one");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("draft.md"), "Hi\n").unwrap();
        fs::write(dir.join("meta.json"), "{}").unwrap();
        let policy = DraftPolicy::new(&dir.join("draft.md")).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            policy,
        }
    }

    #[cfg(unix)]
    fn write(path: &Path) -> Value {
        json!({"file_path": path.to_str().unwrap(), "content": "x"})
    }

    #[test]
    fn reading_and_foreground_bash_are_open_and_other_tools_are_refused() {
        let f = fixture();
        for tool in ["Read", "Glob", "Grep"] {
            assert_eq!(
                f.policy.decide(tool, &json!({"file_path": "/etc/hosts"})),
                Decision::Allow
            );
        }
        assert_eq!(
            f.policy.decide("Bash", &json!({"command": "ls"})),
            Decision::Allow
        );
        let background = json!({"command": "sleep 9", "run_in_background": true});
        let Decision::Deny(reason) = f.policy.decide("Bash", &background) else {
            panic!("background Bash allowed");
        };
        assert!(reason.contains("background"), "{reason}");
        for tool in ["MultiEdit", "NotebookEdit", "Task", "mcp__x__y"] {
            let Decision::Deny(reason) = f.policy.decide(tool, &json!({})) else {
                panic!("{tool} allowed");
            };
            assert!(reason.contains(tool), "{reason}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn writes_reach_the_draft_by_any_alias() {
        let f = fixture();
        let draft = f.root.join("drafts/one/draft.md");
        let link = f.root.join("link");
        symlink(f.root.join("drafts/one"), &link).unwrap();
        let aliases = [
            draft.clone(),
            draft.canonicalize().unwrap(),
            f.root.join("drafts/one/../one/./draft.md"),
            link.join("draft.md"),
        ];
        for path in &aliases {
            for tool in ["Edit", "Write"] {
                assert_eq!(
                    f.policy.decide(tool, &write(path)),
                    Decision::DraftWrite,
                    "{tool} {}",
                    path.display()
                );
            }
        }
        // The file system decides whether case matters.
        let upper = f.root.join("drafts/one/DRAFT.md");
        let insensitive = upper.exists();
        assert_eq!(f.policy.is_draft(upper.to_str().unwrap()), insensitive);
        // A draft that was deleted can be written again.
        fs::remove_file(&draft).unwrap();
        assert_eq!(
            f.policy.decide("Write", &write(&draft)),
            Decision::DraftWrite
        );
    }

    #[test]
    fn writes_elsewhere_are_refused() {
        let f = fixture();
        let dir = f.root.join("drafts/one");
        let outside = [
            json!({"file_path": dir.join("meta.json").to_str().unwrap()}),
            json!({"file_path": dir.join("draft.md.bak").to_str().unwrap()}),
            json!({"file_path": f.root.join("missing/draft.md").to_str().unwrap()}),
            // Relative paths depend on claude's working directory.
            json!({"file_path": "draft.md"}),
            json!({"file_path": "drafts/one/draft.md"}),
            json!({"file_path": "~/draft.md"}),
            json!({"file_path": 3}),
            json!({}),
        ];
        for input in &outside {
            for tool in ["Edit", "Write"] {
                let Decision::Deny(reason) = f.policy.decide(tool, input) else {
                    panic!("{tool} {input} allowed");
                };
                assert!(reason.contains("draft file"), "{reason}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_draft_path_is_not_the_draft() {
        let f = fixture();
        let dir = f.root.join("drafts/one");
        let elsewhere = f.root.join("elsewhere.md");
        fs::write(&elsewhere, "other\n").unwrap();
        fs::remove_file(dir.join("draft.md")).unwrap();
        symlink(&elsewhere, dir.join("draft.md")).unwrap();
        for path in [dir.join("draft.md"), elsewhere] {
            assert!(matches!(
                f.policy.decide("Write", &write(&path)),
                Decision::Deny(_)
            ));
        }
        // Nor is one whose target is gone, which a write would create.
        fs::remove_file(dir.join("draft.md")).unwrap();
        symlink(f.root.join("gone.md"), dir.join("draft.md")).unwrap();
        assert!(matches!(
            f.policy.decide("Write", &write(&dir.join("draft.md"))),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_draft_whose_directory_is_gone_is_an_error() {
        assert!(DraftPolicy::new(Path::new("/nonexistent/one/draft.md")).is_err());
        assert!(DraftPolicy::new(Path::new("/")).is_err());
    }

    #[test]
    fn expected_text_of_writes_and_edits() {
        let edit = |old: &str, new: &str, all: bool| json!({"file_path": "/d", "old_string": old, "new_string": new, "replace_all": all});
        assert_eq!(
            expected("Write", &json!({"content": "new"}), None).as_deref(),
            Some("new")
        );
        assert_eq!(
            expected("Edit", &edit("a", "b", false), Some("a a")).as_deref(),
            Some("b a")
        );
        assert_eq!(
            expected("Edit", &edit("a", "b", true), Some("a a")).as_deref(),
            Some("b b")
        );
        assert_eq!(
            expected("Edit", &edit("Hello", "", false), Some("Hello\nWorld\n")).as_deref(),
            Some("World\n")
        );
        assert_eq!(
            expected("Edit", &edit("a", "", true), Some("a\nba\na")).as_deref(),
            Some("ba")
        );
        assert_eq!(
            expected("Edit", &edit("end", "", false), Some("the end")).as_deref(),
            Some("the ")
        );
        assert_eq!(expected("Edit", &edit("z", "b", false), Some("a")), None);
        assert_eq!(expected("Edit", &edit("", "b", false), Some("a")), None);
        assert_eq!(expected("Edit", &edit("a", "b", false), None), None);
        assert_eq!(expected("Read", &json!({}), Some("a")), None);
    }
}
