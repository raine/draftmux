//! The instruction files (`CLAUDE.md` and rules) a conversation was given,
//! loaded again without the user's settings.
//!
//! Draftmux runs Claude Code with `--setting-sources ""`, which loads no
//! instruction files. Claude Code 2.1.325 re-reads them when a process
//! resumes or forks a session and tells the model that the ones the
//! transcript recorded are "no longer present", so it stops following them.
//! An additional directory (`--add-dir`) has its `CLAUDE.md`, `.claude/CLAUDE.md`
//! and `.claude/rules` loaded under `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD`
//! whatever the setting sources, so the process gets the directories of the
//! files the transcript recorded. `CLAUDE.local.md` loads only with the
//! `local` setting source and stays out.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Adds `--add-dir` for each directory of [`dirs`] of `transcript`, and the
/// variable that makes Claude Code load their instruction files.
pub(crate) fn add_dirs(command: &mut Command, transcript: &Path) {
    let dirs = dirs(transcript);
    if dirs.is_empty() {
        return;
    }
    for dir in dirs {
        command.arg("--add-dir").arg(dir);
    }
    command.env("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD", "1");
}

/// The existing directories that load the project and user instruction
/// files `transcript` records as loaded at any point, in first-seen order.
/// An unreadable transcript has none.
pub(crate) fn dirs(transcript: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(transcript) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    text.lines()
        .filter(|line| line.contains("\"instructions\""))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["type"] == "attachment" && line["attachment"]["type"] == "instructions")
        .flat_map(|line| {
            let files = line["attachment"]["files"].as_array().cloned();
            files.unwrap_or_default()
        })
        .filter(|file| matches!(file["type"].as_str(), Some("Project" | "User")))
        .filter_map(|file| file["path"].as_str().and_then(|path| dir(Path::new(path))))
        .filter(|dir| seen.insert(dir.clone()) && dir.is_dir())
        .collect()
}

/// The directory that, added, loads the instruction file `path`: its own
/// for a `CLAUDE.md`, else the one holding its `.claude` directory.
fn dir(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if path.file_name() == Some(OsStr::new("CLAUDE.md")) {
        return Some(parent.to_path_buf());
    }
    path.ancestors()
        .find(|dir| dir.file_name() == Some(OsStr::new(".claude")))
        .and_then(Path::parent)
        .or(Some(parent))
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(files: &[(&str, &str)], removed: &[&str]) -> String {
        let files: Vec<Value> = files
            .iter()
            .map(|(path, kind)| serde_json::json!({"path": path, "type": kind, "content": "x"}))
            .collect();
        serde_json::json!({
            "type": "attachment",
            "uuid": "u",
            "attachment": {"type": "instructions", "files": files, "removed": removed},
        })
        .to_string()
    }

    #[test]
    fn files_map_to_the_directories_that_load_them() {
        assert_eq!(dir(Path::new("/r/CLAUDE.md")), Some("/r".into()));
        assert_eq!(
            dir(Path::new("/r/a/.claude/CLAUDE.md")),
            Some("/r/a/.claude".into())
        );
        assert_eq!(dir(Path::new("/r/.claude/rules/x/y.md")), Some("/r".into()));
        assert_eq!(dir(Path::new("/h/.claude/rules/y.md")), Some("/h".into()));
        assert_eq!(dir(Path::new("/r/notes.md")), Some("/r".into()));
    }

    #[test]
    fn every_project_and_user_file_ever_loaded_counts_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in ["repo/sub", "repo/.claude/rules", "home/.claude"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let p = |rel: &str| root.join(rel).to_str().unwrap().to_owned();
        let lines = [
            r#"{"type":"user","uuid":"a","message":{"content":"\"instructions\""}}"#.to_owned(),
            attachment(
                &[
                    (&p("home/.claude/CLAUDE.md"), "User"),
                    (&p("repo/CLAUDE.md"), "Project"),
                    (&p("repo/.claude/rules/r.md"), "Project"),
                    (&p("repo/CLAUDE.local.md"), "Local"),
                    (&p("gone/CLAUDE.md"), "Project"),
                ],
                &[],
            ),
            // A later process that loaded none still leaves the earlier ones.
            attachment(&[], &[&p("repo/CLAUDE.md")]),
            attachment(&[(&p("repo/sub/CLAUDE.md"), "Project")], &[]),
            "not json \"instructions\"".to_owned(),
        ];
        let transcript = root.join("t.jsonl");
        fs::write(&transcript, lines.join("\n")).unwrap();
        assert_eq!(
            dirs(&transcript),
            [
                root.join("home/.claude"),
                root.join("repo"),
                root.join("repo/sub")
            ]
        );
        assert!(dirs(&root.join("missing.jsonl")).is_empty());
    }
}
