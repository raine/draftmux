//! Images compiled into the binary, served to `svg()` and `img()` by path.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub const PI_ICON: &str = "icons/pi.svg";
pub const ARCHIVE_ICON: &str = "icons/archive.svg";
pub const PREVIEW_ICON: &str = "icons/preview.svg";
pub const EDITOR_PANE_ICON: &str = "icons/editor-pane.svg";
pub const SIDEBAR_ICON: &str = "icons/sidebar.svg";
pub const COPY_ICON: &str = "icons/copy.svg";
pub const CHECK_ICON: &str = "icons/check.svg";
/// Slack's orientation of the copy icon, for its code block header.
pub const SLACK_COPY_ICON: &str = "icons/copy-slack.svg";
pub const TOOL_BASH_ICON: &str = "icons/tool-bash.svg";
pub const TOOL_READ_ICON: &str = "icons/tool-read.svg";
pub const TOOL_WRITE_ICON: &str = "icons/tool-write.svg";
pub const TOOL_EDIT_ICON: &str = "icons/tool-edit.svg";
pub const TOOL_OTHER_ICON: &str = "icons/tool-other.svg";
pub const SPINNER_TRACK_ICON: &str = "icons/spinner-track.svg";
pub const SPINNER_ARC_ICON: &str = "icons/spinner-arc.svg";
pub const CHEVRON_ICON: &str = "icons/chevron.svg";
pub const SEND_ICON: &str = "icons/send.svg";
pub const STOP_ICON: &str = "icons/stop.svg";
pub const QUOTE_ICON: &str = "icons/quote.svg";
pub const CLOSE_ICON: &str = "icons/close.svg";
pub const TOAST_DONE_ICON: &str = "icons/toast-done.svg";
pub const TOAST_ERROR_ICON: &str = "icons/toast-error.svg";
pub const GEAR_ICON: &str = "icons/gear.svg";
pub const SPARK_ICON: &str = "icons/spark.svg";
pub const GLOBE_ICON: &str = "icons/globe.svg";
pub const FOLDER_ICON: &str = "icons/folder.svg";
pub const PLUS_ICON: &str = "icons/plus.svg";
pub const RESET_ICON: &str = "icons/reset.svg";
pub const UPDOWN_ICON: &str = "icons/updown.svg";
pub const LINK_ICON: &str = "icons/link.svg";
/// GitHub's alert octicons (Primer, MIT).
pub const ALERT_NOTE_ICON: &str = "icons/alert-note.svg";
pub const ALERT_TIP_ICON: &str = "icons/alert-tip.svg";
pub const ALERT_IMPORTANT_ICON: &str = "icons/alert-important.svg";
pub const ALERT_WARNING_ICON: &str = "icons/alert-warning.svg";
pub const ALERT_CAUTION_ICON: &str = "icons/alert-caution.svg";

const ASSETS: &[(&str, &[u8])] = &[
    (PI_ICON, include_bytes!("../assets/icons/pi.svg")),
    (SIDEBAR_ICON, include_bytes!("../assets/icons/sidebar.svg")),
    (ARCHIVE_ICON, include_bytes!("../assets/icons/archive.svg")),
    (PREVIEW_ICON, include_bytes!("../assets/icons/preview.svg")),
    (
        EDITOR_PANE_ICON,
        include_bytes!("../assets/icons/editor-pane.svg"),
    ),
    (COPY_ICON, include_bytes!("../assets/icons/copy.svg")),
    (CHECK_ICON, include_bytes!("../assets/icons/check.svg")),
    (
        SLACK_COPY_ICON,
        include_bytes!("../assets/icons/copy-slack.svg"),
    ),
    (LINK_ICON, include_bytes!("../assets/icons/link.svg")),
    (
        ALERT_NOTE_ICON,
        include_bytes!("../assets/icons/alert-note.svg"),
    ),
    (
        ALERT_TIP_ICON,
        include_bytes!("../assets/icons/alert-tip.svg"),
    ),
    (
        ALERT_IMPORTANT_ICON,
        include_bytes!("../assets/icons/alert-important.svg"),
    ),
    (
        ALERT_WARNING_ICON,
        include_bytes!("../assets/icons/alert-warning.svg"),
    ),
    (
        ALERT_CAUTION_ICON,
        include_bytes!("../assets/icons/alert-caution.svg"),
    ),
    (
        TOOL_BASH_ICON,
        include_bytes!("../assets/icons/tool-bash.svg"),
    ),
    (
        TOOL_READ_ICON,
        include_bytes!("../assets/icons/tool-read.svg"),
    ),
    (
        TOOL_WRITE_ICON,
        include_bytes!("../assets/icons/tool-write.svg"),
    ),
    (
        TOOL_EDIT_ICON,
        include_bytes!("../assets/icons/tool-edit.svg"),
    ),
    (
        TOOL_OTHER_ICON,
        include_bytes!("../assets/icons/tool-other.svg"),
    ),
    (
        SPINNER_TRACK_ICON,
        include_bytes!("../assets/icons/spinner-track.svg"),
    ),
    (
        SPINNER_ARC_ICON,
        include_bytes!("../assets/icons/spinner-arc.svg"),
    ),
    (CHEVRON_ICON, include_bytes!("../assets/icons/chevron.svg")),
    (SEND_ICON, include_bytes!("../assets/icons/send.svg")),
    (STOP_ICON, include_bytes!("../assets/icons/stop.svg")),
    (QUOTE_ICON, include_bytes!("../assets/icons/quote.svg")),
    (CLOSE_ICON, include_bytes!("../assets/icons/close.svg")),
    (
        TOAST_DONE_ICON,
        include_bytes!("../assets/icons/toast-done.svg"),
    ),
    (
        TOAST_ERROR_ICON,
        include_bytes!("../assets/icons/toast-error.svg"),
    ),
    (GEAR_ICON, include_bytes!("../assets/icons/gear.svg")),
    (SPARK_ICON, include_bytes!("../assets/icons/spark.svg")),
    (GLOBE_ICON, include_bytes!("../assets/icons/globe.svg")),
    (FOLDER_ICON, include_bytes!("../assets/icons/folder.svg")),
    (PLUS_ICON, include_bytes!("../assets/icons/plus.svg")),
    (RESET_ICON, include_bytes!("../assets/icons/reset.svg")),
    (UPDOWN_ICON, include_bytes!("../assets/icons/updown.svg")),
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
