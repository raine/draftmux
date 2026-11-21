//! Images in tool results, such as screenshots the source agent read while
//! working, are replaced with a short note in a draft's snapshot of its
//! source. A message rarely needs them, yet every request carries them as
//! base64. The `tool_use` before each result still names what was read, so
//! the agent can read it again. Images the user attached stay, since the
//! message may be about them. Claude Code's own copy in a line's
//! `toolUseResult` isn't sent to the model and is left alone.

use serde_json::Value;

const NOTE: &str =
    "[Image left out of the draft's copy of the session. Read it again if the message needs it.]";

/// `transcript` with images in tool results replaced by a note, and how
/// many were replaced. Lines without such an image are kept byte for byte.
pub(crate) fn strip_tool_result_images(transcript: &str) -> (String, usize) {
    let mut stripped = String::with_capacity(transcript.len());
    let mut count = 0;
    for line in transcript.split_inclusive('\n') {
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
    (stripped, count)
}

/// The line rewritten without its tool result images and how many there
/// were, or `None` when it has none.
fn strip_line(line: &str) -> Option<(String, usize)> {
    // Cheap check before parsing: most lines have no image at all.
    if !line.contains("\"image\"") {
        return None;
    }
    let mut entry: Value = serde_json::from_str(line.trim_end()).ok()?;
    let mut replaced = 0;
    let blocks = entry.pointer_mut("/message/content")?.as_array_mut()?;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(parts) = block.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("image") {
                *part = serde_json::json!({ "type": "text", "text": NOTE });
                replaced += 1;
            }
        }
    }
    (replaced > 0).then(|| (entry.to_string(), replaced))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USE: &str = r#"{"type":"assistant","uuid":"a","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Read","input":{"file_path":"shot.png"}}]}}"#;
    const RESULT: &str = r#"{"type":"user","uuid":"r","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"image","source":{"type":"base64","data":"AAAA","media_type":"image/png"}}]}]},"toolUseResult":{"type":"image","file":{"base64":"AAAA"}}}"#;
    const USER: &str = r#"{"type":"user","uuid":"u","message":{"role":"user","content":[{"type":"text","text":"About this"},{"type":"image","source":{"type":"base64","data":"BBBB","media_type":"image/png"}}]}}"#;

    #[test]
    fn replaces_tool_result_images_and_keeps_the_rest() {
        let transcript = [USE, RESULT, USER].join("\n") + "\n";
        let (stripped, count) = strip_tool_result_images(&transcript);
        assert_eq!(count, 1);
        assert!(stripped.ends_with('\n'));
        let lines: Vec<&str> = stripped.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], USE);
        assert_eq!(lines[2], USER);
        let result: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            result["message"]["content"][0]["content"],
            serde_json::json!([{ "type": "text", "text": NOTE }])
        );
        assert_eq!(result["message"]["content"][0]["tool_use_id"], "t");
        assert_eq!(result["uuid"], "r");
        assert_eq!(result["toolUseResult"]["file"]["base64"], "AAAA");
    }

    #[test]
    fn keeps_a_transcript_without_tool_result_images() {
        let transcript = [USE, USER].join("\n") + "\n";
        assert_eq!(strip_tool_result_images(&transcript), (transcript, 0));
    }
}
