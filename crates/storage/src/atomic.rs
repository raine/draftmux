use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Replaces `path` with `contents` by writing a dot-prefixed temp file in the
/// same directory and renaming it over the target.
///
/// Readers see the old or the new file, never a partial one. There is no
/// fsync: on macOS that is a full device flush, and drafts don't need
/// power-loss durability.
pub(crate) fn write(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let mut file = tempfile::Builder::new()
        .prefix(".")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("creating temp file in {}", dir.display()))?;
    file.write_all(contents)
        .with_context(|| format!("writing {}", file.path().display()))?;
    file.persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_file_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("draft.md");
        write(&path, b"first").unwrap();
        write(&path, b"second").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["draft.md"]);
    }
}
