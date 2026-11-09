//! Fonts and shortcuts shared by the editor and its host.
#[macro_export]
macro_rules! secondary {
    ($keys:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $keys)
        } else {
            concat!("ctrl-", $keys)
        }
    };
}
#[macro_export]
macro_rules! by_platform {
    ($macos:expr, $linux:expr) => {
        if cfg!(target_os = "macos") {
            $macos
        } else {
            $linux
        }
    };
}
pub const MONO: &str = by_platform!("Menlo", "DejaVu Sans Mono");
pub const RICH_SANS: &str = by_platform!("Helvetica", "Liberation Sans");

pub fn ui_font() -> gpui::SharedString {
    #[cfg(target_os = "macos")]
    {
        ".SystemUIFont".into()
    }
    #[cfg(not(target_os = "macos"))]
    {
        static FAMILY: std::sync::OnceLock<gpui::SharedString> = std::sync::OnceLock::new();
        FAMILY
            .get_or_init(|| {
                std::process::Command::new("fc-match")
                    .args(["--format=%{family[0]}", "sans-serif"])
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8(output.stdout).ok())
                    .filter(|family| !family.trim().is_empty())
                    .map_or_else(
                        || "DejaVu Sans".into(),
                        |family| family.trim().to_owned().into(),
                    )
            })
            .clone()
    }
}
