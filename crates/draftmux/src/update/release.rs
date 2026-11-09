//! The latest GitHub release, read with the system's curl so the app needs
//! no TLS stack of its own.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use serde::Deserialize;

use super::{CURRENT_VERSION, Release};

const LATEST_RELEASE: &str = "https://api.github.com/repos/raine/draftmux/releases/latest";
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Larger responses are refused: the release JSON and checksum files are
/// small, and the app archive is tens of megabytes.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The latest published release; `None` before the first one.
pub fn fetch_latest() -> Result<Option<Release>> {
    let mut command = curl(MAX_RESPONSE_BYTES);
    command
        .args(["--header", "Accept: application/vnd.github+json"])
        .arg(LATEST_RELEASE);
    let body = match crate::process::run("curl", command, None, CHECK_TIMEOUT, None) {
        Ok(body) => body,
        Err(err) if is_not_found(&err) => return Ok(None),
        Err(err) => bail!(err),
    };
    let release: GithubRelease =
        serde_json::from_str(&body).context("reading the latest GitHub release")?;
    parse_release(release).map(Some)
}

/// Whether curl's `--fail` message reports a 404, which GitHub returns
/// while a repository has no releases.
fn is_not_found(curl_error: &str) -> bool {
    curl_error.trim_end().ends_with("returned error: 404")
}

/// The macOS app archive of `version`, which the release must include with
/// its checksum file.
pub fn archive_name(version: &Version) -> String {
    format!("Draftmux-{version}-arm64-app.tar.gz")
}

fn checksum_name(version: &Version) -> String {
    format!("Draftmux-{version}-arm64-app.sha256")
}

fn parse_release(release: GithubRelease) -> Result<Release> {
    let version = release
        .tag_name
        .strip_prefix('v')
        .and_then(|raw| Version::parse(raw).ok())
        .with_context(|| format!("release tag {} isn't vMAJOR.MINOR.PATCH", release.tag_name))?;
    let asset = |name: String| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
            .ok_or_else(|| anyhow!("release {} has no {name}", release.tag_name))
    };
    Ok(Release {
        archive_url: asset(archive_name(&version))?,
        checksum_url: asset(checksum_name(&version))?,
        page: release.html_url,
        version,
    })
}

/// A small HTTPS response body.
pub fn fetch_text(url: &str) -> Result<String> {
    let mut command = curl(MAX_RESPONSE_BYTES);
    command.arg(url);
    crate::process::run("curl", command, None, CHECK_TIMEOUT, None).map_err(|err| anyhow!(err))
}

/// Downloads `url` to `path`.
pub fn download(url: &str, path: &Path) -> Result<()> {
    let mut command = curl(MAX_DOWNLOAD_BYTES);
    command.arg("--output").arg(path).arg(url);
    crate::process::run("curl", command, None, DOWNLOAD_TIMEOUT, None)
        .map_err(|err| anyhow!(err))?;
    if !path.is_file() {
        bail!("the download of {url} wrote nothing");
    }
    Ok(())
}

fn curl(max_bytes: u64) -> Command {
    let mut command = Command::new(if cfg!(target_os = "macos") {
        "/usr/bin/curl"
    } else {
        "curl"
    });
    command
        .args(["--fail", "--silent", "--show-error", "--location"])
        // Redirects to the download host must stay on HTTPS too.
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--connect-timeout", "10"])
        .arg("--max-filesize")
        .arg(max_bytes.to_string())
        .arg("--user-agent")
        .arg(format!("draftmux/{CURRENT_VERSION}"));
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github_release(tag: &str, assets: &[&str]) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            html_url: format!("https://github.com/raine/draftmux/releases/tag/{tag}"),
            assets: assets
                .iter()
                .map(|name| GithubAsset {
                    name: (*name).into(),
                    browser_download_url: format!("https://example.com/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn release_names_its_app_archive_and_checksum() {
        let release = parse_release(github_release(
            "v1.2.3",
            &[
                "Draftmux-1.2.3-arm64.dmg",
                "Draftmux-1.2.3-arm64-app.tar.gz",
                "Draftmux-1.2.3-arm64-app.sha256",
            ],
        ))
        .unwrap();
        assert_eq!(release.version, Version::new(1, 2, 3));
        assert_eq!(
            release.archive_url,
            "https://example.com/Draftmux-1.2.3-arm64-app.tar.gz"
        );
        assert_eq!(
            release.checksum_url,
            "https://example.com/Draftmux-1.2.3-arm64-app.sha256"
        );
    }

    #[test]
    fn not_found_means_no_release() {
        assert!(is_not_found(
            "curl: (56) The requested URL returned error: 404"
        ));
        assert!(!is_not_found(
            "curl: (56) The requested URL returned error: 403"
        ));
        assert!(!is_not_found(
            "curl: (6) Could not resolve host: api.github.com"
        ));
    }

    #[test]
    fn release_without_app_archive_is_rejected() {
        let err = parse_release(github_release("v1.2.3", &["Draftmux-1.2.3-arm64.dmg"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Draftmux-1.2.3-arm64-app.tar.gz"), "{err}");
        assert!(parse_release(github_release("1.2.3", &[])).is_err());
        assert!(parse_release(github_release("vnext", &[])).is_err());
    }
}
