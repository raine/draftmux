//! Images compiled into the binary, served to `svg()` and `img()` by path.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub const GITHUB_ICON: &str = "icons/github.svg";
/// 48px, drawn at 16px: `img()` rasterizes SVGs at 1x, which blurs on Retina.
pub const SLACK_ICON: &str = "icons/slack.png";
pub const ARCHIVE_ICON: &str = "icons/archive.svg";
pub const PREVIEW_ICON: &str = "icons/preview.svg";
pub const COPY_ICON: &str = "icons/copy.svg";

const ASSETS: &[(&str, &[u8])] = &[
    (GITHUB_ICON, include_bytes!("../assets/icons/github.svg")),
    (SLACK_ICON, include_bytes!("../assets/icons/slack.png")),
    (ARCHIVE_ICON, include_bytes!("../assets/icons/archive.svg")),
    (PREVIEW_ICON, include_bytes!("../assets/icons/preview.svg")),
    (COPY_ICON, include_bytes!("../assets/icons/copy.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::new_static(name))
            .collect())
    }
}
