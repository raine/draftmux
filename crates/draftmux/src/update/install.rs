//! Replaces the installed `Draftmux.app` with a release's. The new bundle is
//! downloaded and checked in a folder beside the installed one, so the swap
//! is one atomic rename on the same volume. The running app keeps its old
//! executable until it restarts.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use sha2::{Digest, Sha256};

use super::{BUNDLE_ID, Ineligible, Release, release};

const BUNDLE_NAME: &str = "Draftmux.app";
const TOOL_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// The running `Draftmux.app`, which updates replace.
pub struct Installation {
    app: PathBuf,
    /// The Developer ID team that signed it; a release must be signed by the
    /// same team.
    team: String,
}

/// Whether this copy can update itself: a Developer ID signed bundle in a
/// folder the user can write to.
pub fn installation() -> Result<Installation, Ineligible> {
    let exe = crate::resources::exe().map_err(|_| Ineligible::Development)?;
    let app = crate::resources::bundle(&exe)
        .ok_or(Ineligible::Development)?
        .to_owned();
    let team = signing_team(&app).ok_or(Ineligible::Development)?;
    let dir = app.parent().ok_or(Ineligible::Development)?;
    let writable = tempfile::Builder::new()
        .prefix(".Draftmux-write-test-")
        .tempfile_in(dir)
        .is_ok();
    if !writable {
        return Err(Ineligible::Unwritable(dir.display().to_string()));
    }
    Ok(Installation { app, team })
}

pub fn install(release: &Release, installation: &Installation) -> Result<()> {
    let dir = installation
        .app
        .parent()
        .context("Draftmux.app has no parent folder")?;
    // Removed with whatever it holds when this returns, including the old
    // bundle after a successful swap.
    let staging = tempfile::Builder::new()
        .prefix(".Draftmux-update-")
        .tempdir_in(dir)
        .with_context(|| format!("creating a staging folder in {}", dir.display()))?;
    let archive = staging.path().join(release::archive_name(&release.version));
    release::download(&release.archive_url, &archive)?;
    let checksum = release::fetch_text(&release.checksum_url)?;
    let bytes = std::fs::read(&archive).context("reading the downloaded archive")?;
    verify_checksum(&bytes, &checksum)?;

    let extracted = staging.path().join("extracted");
    std::fs::create_dir(&extracted).context("creating the extraction folder")?;
    let mut tar = Command::new("/usr/bin/tar");
    tar.arg("-xzf").arg(&archive).arg("-C").arg(&extracted);
    run("tar", tar)?;
    let new_app = only_bundle(&extracted)?;
    let version = bundle_version(&new_app)?;
    if version != release.version {
        bail!(
            "the archive holds Draftmux {version}, not {}",
            release.version
        );
    }
    verify_signature(&new_app, &installation.team)?;
    swap(&new_app, &installation.app)
}

/// Checks `archive` against a `shasum -a 256` line.
fn verify_checksum(archive: &[u8], checksum_file: &str) -> Result<()> {
    let expected = checksum_file
        .split_whitespace()
        .next()
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("the checksum file has no SHA-256")?;
    let actual: String = Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("the downloaded archive doesn't match its checksum");
    }
    Ok(())
}

/// The archive must hold `Draftmux.app` and nothing else.
fn only_bundle(extracted: &Path) -> Result<PathBuf> {
    let names: Vec<_> = std::fs::read_dir(extracted)
        .context("reading the extracted archive")?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()
        .context("reading the extracted archive")?;
    if names != [BUNDLE_NAME] {
        bail!("the archive doesn't hold just {BUNDLE_NAME}");
    }
    let app = extracted.join(BUNDLE_NAME);
    let metadata = std::fs::symlink_metadata(&app).context("reading the extracted bundle")?;
    if !metadata.is_dir() {
        bail!("{BUNDLE_NAME} in the archive isn't a folder");
    }
    Ok(app)
}

fn bundle_version(app: &Path) -> Result<Version> {
    let mut plutil = Command::new("/usr/bin/plutil");
    plutil
        .args(["-extract", "CFBundleShortVersionString", "raw"])
        .arg(app.join("Contents/Info.plist"));
    let raw = run("plutil", plutil)?;
    Version::parse(raw.trim()).with_context(|| format!("the bundle's version {raw:?} isn't valid"))
}

/// Requires a valid, complete signature by `team` for Draftmux's identifier.
/// The signature seals every file in the bundle.
fn verify_signature(app: &Path, team: &str) -> Result<()> {
    let requirement = format!(
        "=anchor apple generic and identifier \"{BUNDLE_ID}\" \
         and certificate leaf[subject.OU] = \"{team}\""
    );
    let mut codesign = Command::new("/usr/bin/codesign");
    codesign
        .args(["--verify", "--deep", "--strict"])
        .arg(format!("-R{requirement}"))
        .arg(app);
    run("codesign", codesign)
        .context("the new Draftmux.app isn't signed by Draftmux's developer")?;
    Ok(())
}

/// The Developer ID team of a bundle signed with Draftmux's identifier;
/// `None` for ad-hoc signed development builds.
fn signing_team(app: &Path) -> Option<String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=2"])
        .arg(app)
        .output()
        .ok()?;
    // codesign prints the details on stderr.
    parse_signing_team(&String::from_utf8_lossy(&output.stderr))
}

fn parse_signing_team(details: &str) -> Option<String> {
    let field = |name: &str| {
        details
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
    };
    let team = field("TeamIdentifier")?;
    let developer_id = details
        .lines()
        .any(|line| line.starts_with("Authority=Developer ID Application:"));
    (field("Identifier") == Some(BUNDLE_ID)
        && developer_id
        && !team.is_empty()
        && team.bytes().all(|b| b.is_ascii_alphanumeric()))
    .then(|| team.to_owned())
}

/// Atomically exchanges the two bundles: `installed` becomes the new one and
/// `new` the old.
fn swap(new: &Path, installed: &Path) -> Result<()> {
    let from = CString::new(new.as_os_str().as_bytes())?;
    let to = CString::new(installed.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths for the call's duration.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("replacing {}", installed.display()));
    }
    Ok(())
}

fn run(name: &str, command: Command) -> Result<String> {
    crate::process::run(name, command, None, TOOL_TIMEOUT, None).map_err(|err| anyhow!(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE_DETAILS: &str = "Executable=/Applications/Draftmux.app/Contents/MacOS/draftmux
Identifier=fi.zendit.Draftmux
Format=app bundle with Mach-O thin (arm64)
Authority=Developer ID Application: Example Person (ABCDE12345)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
TeamIdentifier=ABCDE12345
";

    #[test]
    fn developer_id_builds_have_a_team() {
        assert_eq!(
            parse_signing_team(RELEASE_DETAILS).as_deref(),
            Some("ABCDE12345")
        );
    }

    #[test]
    fn development_and_foreign_builds_have_no_team() {
        let adhoc = "Identifier=fi.zendit.Draftmux\nSignature=adhoc\nTeamIdentifier=not set\n";
        assert_eq!(parse_signing_team(adhoc), None);
        let other = RELEASE_DETAILS.replace("fi.zendit.Draftmux", "com.example.Other");
        assert_eq!(parse_signing_team(&other), None);
        assert_eq!(parse_signing_team(""), None);
    }

    #[test]
    fn checksum_must_match() {
        let archive = b"archive";
        let sha = "4a0e6a6d6e6b0a2f1d7c7a4e0d3a36a5c1d2b3d0e4b6f0b0c9e5e6f2f5a8b9c1";
        assert!(verify_checksum(archive, &format!("{sha}  a.tar.gz")).is_err());
        let actual: String = Sha256::digest(archive)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        verify_checksum(
            archive,
            &format!("{actual}  Draftmux-1.0.0-arm64-app.tar.gz\n"),
        )
        .unwrap();
        assert!(verify_checksum(archive, "").is_err());
        assert!(verify_checksum(archive, "abc  file").is_err());
    }

    #[test]
    fn archive_must_hold_only_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(BUNDLE_NAME)).unwrap();
        assert_eq!(
            only_bundle(dir.path()).unwrap(),
            dir.path().join(BUNDLE_NAME)
        );
        std::fs::write(dir.path().join("extra"), "").unwrap();
        assert!(only_bundle(dir.path()).is_err());
    }

    #[test]
    fn swap_exchanges_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let (new, installed) = (dir.path().join("new"), dir.path().join("installed"));
        std::fs::create_dir(&new).unwrap();
        std::fs::write(new.join("version"), "2").unwrap();
        std::fs::create_dir(&installed).unwrap();
        std::fs::write(installed.join("version"), "1").unwrap();
        swap(&new, &installed).unwrap();
        assert_eq!(
            std::fs::read_to_string(installed.join("version")).unwrap(),
            "2"
        );
        assert_eq!(std::fs::read_to_string(new.join("version")).unwrap(), "1");
    }
}
