//! Reading a handed-off session file: its format, working directory and the
//! pi profile it belongs to.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use draftmux_storage::SourceKind;
use serde_json::Value;

/// What `draftmux open` records about the source session.
#[derive(Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub kind: SourceKind,
    pub cwd: PathBuf,
}

/// pi files start with a `{"type":"session","version":…}` header holding the
/// cwd. Claude Code lines carry `uuid` and `sessionId`, often after
/// bookkeeping lines that don't; the latest `cwd` wins. Unparseable lines are
/// skipped, since the source session may be mid-write.
pub fn inspect(path: &Path) -> Result<SessionInfo> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut claude = false;
    let mut cwd = None;
    let mut first = true;
    // Byte lines, so a final line cut off mid-character is skipped like any
    // other unparseable line.
    for line in BufReader::new(file).split(b'\n') {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        if line.trim_ascii().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            first = false;
            continue;
        };
        if std::mem::take(&mut first) && value["type"] == "session" {
            return pi_header(&value, path);
        }
        claude |= value.get("uuid").is_some() && value.get("sessionId").is_some();
        if let Some(dir) = value["cwd"].as_str() {
            cwd = Some(PathBuf::from(dir));
        }
    }
    if !claude {
        bail!(
            "{} is neither a pi session nor a Claude Code transcript",
            path.display()
        );
    }
    let cwd = cwd.with_context(|| format!("{} records no working directory", path.display()))?;
    Ok(SessionInfo {
        kind: SourceKind::Claude,
        cwd,
    })
}

/// pi's `--fork` copies entries unmigrated while stamping v3, so older
/// sessions are refused.
fn pi_header(header: &Value, path: &Path) -> Result<SessionInfo> {
    let version = header["version"].as_u64().unwrap_or(1);
    if version < 3 {
        bail!(
            "{} is a version {version} pi session; Draftmux needs version 3 or later \
             (open it once in a current pi to migrate it)",
            path.display()
        );
    }
    let cwd = header["cwd"]
        .as_str()
        .with_context(|| format!("{} has no cwd in its session header", path.display()))?;
    Ok(SessionInfo {
        kind: SourceKind::Pi,
        cwd: cwd.into(),
    })
}

/// Profile pinning: the `--pi-agent-dir` flag, else the profile a pi session
/// file lives in (`<profile>/sessions/<cwd slug>/<file>`), else the profile
/// the directory settings give (`settings`) if it is a folder, else the
/// default profile.
/// `source` is absolute; paths are made absolute without resolving symlinks,
/// like the recorded source path.
pub fn resolve_profile(
    flag: Option<&Path>,
    source: &Path,
    kind: SourceKind,
    settings: Option<&Path>,
    home: &Path,
) -> Result<PathBuf> {
    if let Some(dir) = flag {
        // pi expands `~` in PI_CODING_AGENT_DIR, so the flag may carry it.
        let dir = draftmux_settings::expand_home(dir, home);
        let dir = std::path::absolute(&dir)
            .with_context(|| format!("resolving pi agent dir {}", dir.display()))?;
        if !dir.is_dir() {
            bail!("pi agent dir {} is not a directory", dir.display());
        }
        return Ok(dir);
    }
    if kind == SourceKind::Pi
        && let Some(sessions) = source.parent().and_then(Path::parent)
        && sessions.file_name().is_some_and(|name| name == "sessions")
        && let Some(profile) = sessions.parent()
    {
        return Ok(profile.to_owned());
    }
    Ok(settings_profile(settings, home))
}

/// The profile the directory settings give (`settings`) if it is a folder,
/// else the default profile.
pub fn settings_profile(settings: Option<&Path>, home: &Path) -> PathBuf {
    if let Some(dir) = settings {
        if dir.is_dir() {
            return dir.to_owned();
        }
        eprintln!(
            "draftmux: profile {} from settings is not a directory; using the default",
            dir.display()
        );
    }
    draftmux_settings::default_profile(home)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(dir: &Path, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn detects_pi_header_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "s.jsonl",
            concat!(
                r#"{"type":"session","version":3,"id":"x","timestamp":"t","cwd":"/repo"}"#,
                "\n",
                r#"{"type":"message","id":"a","parentId":null,"cwd":"/elsewhere"}"#,
                "\n",
            ),
        );
        let info = inspect(&path).unwrap();
        assert_eq!(info.kind, SourceKind::Pi);
        assert_eq!(info.cwd, Path::new("/repo"));
    }

    #[test]
    fn rejects_pre_v3_pi_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let v2 = write(
            dir.path(),
            "v2.jsonl",
            r#"{"type":"session","version":2,"cwd":"/r"}"#,
        );
        let v1 = write(dir.path(), "v1.jsonl", r#"{"type":"session","cwd":"/r"}"#);
        for path in [v2, v1] {
            let err = inspect(&path).unwrap_err().to_string();
            assert!(err.contains("needs version 3"), "{err}");
        }
    }

    #[test]
    fn detects_claude_after_bookkeeping_with_latest_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "c.jsonl",
            concat!(
                r#"{"type":"last-prompt","sessionId":"s"}"#,
                "\n",
                r#"{"type":"user","uuid":"1","parentUuid":null,"sessionId":"s","cwd":"/a"}"#,
                "\n\n",
                r#"{"type":"assistant","uuid":"2","parentUuid":"1","sessionId":"s","cwd":"/b"}"#,
                "\n",
                r#"{"type":"assistant","uuid":"3","par"#,
            ),
        );
        let info = inspect(&path).unwrap();
        assert_eq!(info.kind, SourceKind::Claude);
        assert_eq!(info.cwd, Path::new("/b"));
    }

    #[test]
    fn skips_tail_cut_off_mid_character() {
        let dir = tempfile::tempdir().unwrap();
        let mut contents = br#"{"type":"user","uuid":"1","sessionId":"s","cwd":"/a"}"#.to_vec();
        contents.extend_from_slice(b"\n{\"type\":\"assistant\",\"text\":\"\xe2\x82");
        let path = write(dir.path(), "c.jsonl", contents);
        assert_eq!(inspect(&path).unwrap().cwd, Path::new("/a"));
    }

    #[test]
    fn rejects_unknown_files() {
        let dir = tempfile::tempdir().unwrap();
        let empty = write(dir.path(), "empty.jsonl", "");
        let other = write(dir.path(), "other.jsonl", "{\"a\":1}\nnot json\n");
        let no_cwd = write(
            dir.path(),
            "nocwd.jsonl",
            r#"{"type":"user","uuid":"1","sessionId":"s"}"#,
        );
        for path in [empty, other, no_cwd] {
            assert!(inspect(&path).is_err(), "{} accepted", path.display());
        }
    }

    #[test]
    fn profile_flag_expands_tilde_and_must_exist() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".pi-work/agent")).unwrap();
        let source = Path::new("/x/sessions/slug/s.jsonl");

        let flag = Path::new("~/.pi-work/agent");
        let settings = Some(Path::new("/settings/agent"));
        let profile = resolve_profile(Some(flag), source, SourceKind::Pi, settings, home).unwrap();
        assert_eq!(profile, home.join(".pi-work/agent"));

        let missing = home.join("missing");
        assert!(resolve_profile(Some(&missing), source, SourceKind::Pi, None, home).is_err());
    }

    #[test]
    fn profile_falls_back_to_session_location_then_settings_then_default() {
        let dir = tempfile::tempdir().unwrap();
        let settings_profile = dir.path().join("settings/agent");
        fs::create_dir_all(&settings_profile).unwrap();
        let home = Path::new("/home/me");
        let settings = Some(settings_profile.as_path());
        let pi = Path::new("/p/.pi-work/agent/sessions/--repo--/s.jsonl");
        assert_eq!(
            resolve_profile(None, pi, SourceKind::Pi, settings, home).unwrap(),
            Path::new("/p/.pi-work/agent")
        );

        let custom = Path::new("/tmp/elsewhere/s.jsonl");
        let claude = Path::new("/home/me/.claude/projects/sessions/s.jsonl");
        for (source, kind) in [(custom, SourceKind::Pi), (claude, SourceKind::Claude)] {
            assert_eq!(
                resolve_profile(None, source, kind, settings, home).unwrap(),
                settings_profile
            );
            assert_eq!(
                resolve_profile(None, source, kind, None, home).unwrap(),
                Path::new("/home/me/.pi/agent")
            );
            // A profile folder that's gone falls back to the default.
            let missing = Some(Path::new("/settings/missing"));
            assert_eq!(
                resolve_profile(None, source, kind, missing, home).unwrap(),
                Path::new("/home/me/.pi/agent")
            );
        }
    }
}
