//! Which directory rules apply to a working directory.
//!
//! Both sides are compared as real paths: `~` expanded, then symlinks
//! resolved by `canonicalize`, which also gives the case stored on disk, so a
//! case-insensitive volume matches regardless of how a path was typed and a
//! case-sensitive one stays exact. A path that doesn't exist keeps its
//! expanded form. Matching is by whole components, so `~/code/app` doesn't
//! match `~/code/app2`.
//!
//! A linked git worktree also matches the rules of its main worktree, with
//! its path inside the worktree carried over: a cwd of
//! `~/code/app__worktrees/fix/src` matches a rule on `~/code/app/src`.
//!
//! The nearest rule ranks first: the fewest components between the rule and
//! the cwd, or its counterpart in the main worktree. At the same distance the
//! rule on the real path wins.

use std::path::{Path, PathBuf};

use crate::Settings;

/// `path` with a leading `~` replaced by `home`.
pub fn expand_home(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_owned(),
    }
}

/// `path` with a leading `home` shown as `~`.
pub fn tilde_path(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => Path::new("~").join(rest),
        _ => path.to_owned(),
    }
}

/// The path compared when matching.
pub(crate) fn real_path(path: &Path, home: &Path) -> PathBuf {
    let path = expand_home(path, home);
    let path = std::path::absolute(&path).unwrap_or(path);
    path.canonicalize().unwrap_or(path)
}

impl Settings {
    /// Indices of the rules that apply to `cwd`, most specific first.
    pub fn matching_rules(&self, cwd: &Path, home: &Path) -> Vec<usize> {
        let cwd = real_path(cwd, home);
        let rules: Vec<PathBuf> = self
            .directories
            .iter()
            .map(|rule| real_path(&rule.path, home))
            .collect();
        let candidates = std::iter::once(cwd.clone()).chain(main_worktree_path(&cwd));
        let mut found: Vec<(usize, usize, usize)> = Vec::new();
        for (mapped, candidate) in candidates.enumerate() {
            for (ix, rule) in rules.iter().enumerate() {
                if candidate.starts_with(rule) {
                    let distance = candidate.components().count() - rule.components().count();
                    found.push((distance, mapped, ix));
                }
            }
        }
        found.sort();
        let mut matches = Vec::new();
        for (_, _, ix) in found {
            if !matches.contains(&ix) {
                matches.push(ix);
            }
        }
        matches
    }

    /// The rule whose directory is `dir`, compared as real paths.
    pub fn rule_for(&self, dir: &Path, home: &Path) -> Option<usize> {
        let dir = real_path(dir, home);
        self.directories
            .iter()
            .position(|rule| real_path(&rule.path, home) == dir)
    }
}

/// For `cwd` inside a linked worktree, the same place in the main worktree.
/// A linked worktree's `.git` is a file naming its git dir, whose
/// `commondir` leads to the main repository's `.git` folder.
fn main_worktree_path(cwd: &Path) -> Option<PathBuf> {
    let root = cwd.ancestors().find(|dir| dir.join(".git").exists())?;
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return None;
    }
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir = root.join(contents.strip_prefix("gitdir:")?.trim());
    let common = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let common = git_dir.join(common.trim()).canonicalize().ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let main = common.parent()?;
    let mapped = main.join(cwd.strip_prefix(root).ok()?);
    (mapped != cwd).then_some(mapped)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::Rule;

    fn settings(paths: &[&Path]) -> Settings {
        Settings {
            directories: paths.iter().map(|p| Rule::new(p.to_path_buf())).collect(),
            ..Settings::default()
        }
    }

    /// A temp dir resolved like `canonicalize` reports it (`/var` is a
    /// symlink to `/private/var` on macOS).
    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }

    #[test]
    fn matches_by_component_most_specific_first() {
        let (_t, root) = tmp();
        for dir in ["code/app/src", "code/app2"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let s = settings(&[
            &root.join("code"),
            &root.join("code/app"),
            &root.join("other"),
        ]);
        assert_eq!(s.matching_rules(&root.join("code/app/src"), &root), [1, 0]);
        assert_eq!(s.matching_rules(&root.join("code/app2"), &root), [0]);
        assert_eq!(s.matching_rules(&root.join("code/app"), &root), [1, 0]);
        assert!(s.matching_rules(&root.join("elsewhere"), &root).is_empty());
    }

    #[test]
    fn expands_tilde_in_rules() {
        let (_t, home) = tmp();
        fs::create_dir_all(home.join("code/app")).unwrap();
        let s = settings(&[Path::new("~/code")]);
        assert_eq!(s.matching_rules(&home.join("code/app"), &home), [0]);
        assert_eq!(s.matching_rules(Path::new("~/code/app"), &home), [0]);
    }

    #[test]
    fn resolves_symlinks_on_both_sides() {
        let (_t, root) = tmp();
        fs::create_dir_all(root.join("real/app")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let s = settings(&[&root.join("link")]);
        assert_eq!(s.matching_rules(&root.join("real/app"), &root), [0]);
        let s = settings(&[&root.join("real")]);
        assert_eq!(s.matching_rules(&root.join("link/app"), &root), [0]);
    }

    #[test]
    fn follows_the_volumes_case_rules() {
        let (_t, root) = tmp();
        fs::create_dir_all(root.join("Code/App")).unwrap();
        let s = settings(&[&root.join("code")]);
        let insensitive = root.join("CODE").exists();
        let matched = !s.matching_rules(&root.join("Code/App"), &root).is_empty();
        assert_eq!(matched, insensitive);
    }

    #[test]
    fn missing_paths_match_as_written() {
        let s = settings(&[Path::new("/gone/repo")]);
        assert_eq!(
            s.matching_rules(Path::new("/gone/repo/sub"), Path::new("/h")),
            [0]
        );
    }

    /// A main repo at `app` with a linked worktree at `app__worktrees/fix`,
    /// laid out as `git worktree add` writes it.
    fn worktree(root: &Path) -> (PathBuf, PathBuf) {
        let main = root.join("app");
        let wt = root.join("app__worktrees/fix");
        let git_dir = main.join(".git/worktrees/fix");
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(main.join("src")).unwrap();
        fs::create_dir_all(wt.join("src/deep")).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", git_dir.display())).unwrap();
        fs::write(git_dir.join("commondir"), "../..\n").unwrap();
        (main, wt)
    }

    #[test]
    fn linked_worktrees_match_main_worktree_rules_nearest_first() {
        let (_t, root) = tmp();
        let (main, wt) = worktree(&root);
        let s = settings(&[&main, &main.join("src"), &wt, &root]);
        // main/src is 1 away, wt and main 2 (wt on the real path wins), root 4.
        assert_eq!(s.matching_rules(&wt.join("src/deep"), &root), [1, 2, 0, 3]);
        assert_eq!(s.matching_rules(&wt, &root), [2, 0, 3]);
        // The main worktree itself isn't mapped anywhere.
        assert_eq!(s.matching_rules(&main.join("src"), &root), [1, 0, 3]);
    }

    #[test]
    fn relative_gitdir_is_read_from_the_worktree() {
        let (_t, root) = tmp();
        let (main, wt) = worktree(&root);
        fs::write(wt.join(".git"), "gitdir: ../../app/.git/worktrees/fix\n").unwrap();
        let s = settings(&[&main]);
        assert_eq!(s.matching_rules(&wt.join("src"), &root), [0]);
    }

    #[test]
    fn rule_for_compares_real_paths() {
        let (_t, home) = tmp();
        fs::create_dir_all(home.join("code")).unwrap();
        let s = settings(&[Path::new("~/code")]);
        assert_eq!(s.rule_for(&home.join("code"), &home), Some(0));
        assert_eq!(s.rule_for(&home, &home), None);
    }

    #[test]
    fn tilde_round_trip() {
        let home = Path::new("/Users/me");
        let shown = tilde_path(Path::new("/Users/me/code"), home);
        assert_eq!(shown, Path::new("~/code"));
        assert_eq!(expand_home(&shown, home), Path::new("/Users/me/code"));
        assert_eq!(tilde_path(Path::new("/opt/x"), home), Path::new("/opt/x"));
    }
}
