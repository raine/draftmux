//! Images in tool results, such as screenshots the source agent read while
//! working, are replaced with a short note in a draft's session. A message
//! rarely needs them, yet every request carries them as base64: a couple of
//! screenshots add a megabyte or more to each chat turn and selection call.
//! The tool call before each result still names what was read, so the agent
//! can read it again. Images the user attached stay, since the message may be
//! about them.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

const NOTE: &str =
    "[Image left out of the draft's copy of the session. Read it again if the message needs it.]";

/// Rewrites the session file at `path` with images in tool results replaced
/// by a note. Lines without such an image are kept byte for byte, and the
/// file is left alone when there are none. Returns how many were replaced.
pub(crate) fn strip_tool_result_images(path: &Path) -> io::Result<usize> {
    let session = fs::read_to_string(path)?;
    let mut stripped = String::with_capacity(session.len());
    let mut count = 0;
    for line in session.split_inclusive('\n') {
        match strip_line(line) {
            Some((rewritten, replaced)) => {
                stripped.push_str(&rewritten);
                if line.ends_with('\n') {
                    stripped.push('\n');
                }
                count += replaced;
            }
            None => stripped.push_str(line),
        }
    }
    if count == 0 {
        return Ok(0);
    }
    // Not `.jsonl`, so nothing listing sessions picks up the partial file.
    let temp = path.with_extension("jsonl.tmp");
    fs::write(&temp, stripped)?;
    fs::rename(&temp, path)?;
    Ok(count)
}

/// The line rewritten without its tool result images and how many there
/// were, or `None` when it has none.
fn strip_line(line: &str) -> Option<(String, usize)> {
    // Cheap check before parsing: most lines have no image at all.
    if !line.contains("\"image\"") {
        return None;
    }
    let mut entry: Value = serde_json::from_str(line.trim_end()).ok()?;
    let message = entry.get_mut("message")?;
    if message.get("role")?.as_str()? != "toolResult" {
        return None;
    }
    let mut replaced = 0;
    for part in message.get_mut("content")?.as_array_mut()? {
        if part.get("type").and_then(Value::as_str) == Some("image") {
            *part = serde_json::json!({ "type": "text", "text": NOTE });
            replaced += 1;
        }
    }
    (replaced > 0).then(|| (entry.to_string(), replaced))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"type":"session","version":3,"id":"s","cwd":"/x"}"#;
    const CALL: &str = r#"{"type":"message","id":"a","message":{"role":"assistant","content":[{"type":"toolCall","id":"t","name":"read","arguments":{"path":"shot.png"}}]}}"#;
    const RESULT: &str = r#"{"type":"message","id":"r","message":{"role":"toolResult","toolCallId":"t","content":[{"type":"text","text":"Read image file [image/png]"},{"type":"image","data":"AAAA","mimeType":"image/png"}]}}"#;
    const USER: &str = r#"{"type":"message","id":"u","message":{"role":"user","content":[{"type":"text","text":"About this"},{"type":"image","data":"BBBB","mimeType":"image/png"}]}}"#;

    fn session(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        (dir, path)
    }

    #[test]
    fn replaces_tool_result_images_and_keeps_the_rest() {
        let (dir, path) = session(&[HEADER, CALL, RESULT, USER]);
        assert_eq!(strip_tool_result_images(&path).unwrap(), 1);
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(text.ends_with('\n'));
        assert_eq!(&lines[..2], &[HEADER, CALL]);
        assert_eq!(lines[3], USER);
        let result: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(
            result["message"]["content"],
            serde_json::json!([
                { "type": "text", "text": "Read image file [image/png]" },
                { "type": "text", "text": NOTE },
            ])
        );
        assert_eq!(result["message"]["toolCallId"], "t");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn leaves_a_session_without_tool_result_images_untouched() {
        let (_dir, path) = session(&[HEADER, CALL, USER]);
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(strip_tool_result_images(&path).unwrap(), 0);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            [HEADER, CALL, USER].join("\n") + "\n"
        );
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    }
}
