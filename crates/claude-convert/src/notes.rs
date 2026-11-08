//! Compact text notes standing in for Claude tool calls and their results.

use std::path::Path;

use serde_json::Value;

const CALL_CHARS: usize = 160;
const RESULT_CHARS: usize = 240;

/// `[ran: git status]`-style description of a `tool_use` block, without the
/// brackets.
pub(crate) fn call(block: &Value, cwd: &Path) -> String {
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    let field = |key: &str| input[key].as_str().map(|s| s.trim());
    let path = |key: &str| field(key).map(|p| relative(p, cwd));
    let described = match name {
        "Bash" => field("command").map(|c| format!("ran: {}", first_line(c))),
        "Read" => path("file_path").map(|p| format!("read {p}")),
        "Edit" | "MultiEdit" => path("file_path").map(|p| format!("edited {p}")),
        "Write" => path("file_path").map(|p| format!("wrote {p}")),
        "NotebookEdit" => path("notebook_path").map(|p| format!("edited {p}")),
        "Grep" => field("pattern").map(|p| format!("grep {p}")),
        "Glob" => field("pattern").map(|p| format!("glob {p}")),
        "Task" | "Agent" => field("description").map(|d| format!("subagent: {d}")),
        "WebFetch" => field("url").map(|u| format!("fetched {u}")),
        "WebSearch" => field("query").map(|q| format!("searched: {q}")),
        _ => None,
    };
    let described = described.unwrap_or_else(|| match input {
        Value::Object(map) if map.is_empty() => format!("used {name}"),
        Value::Null => format!("used {name}"),
        _ => format!("{name}: {input}"),
    });
    truncate(&collapse(&described), CALL_CHARS)
}

/// A short excerpt of a `tool_result` block, prefixed `error:` for failed
/// calls. `None` when the result has no text.
pub(crate) fn result(block: &Value) -> Option<String> {
    let text = match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part["type"].as_str() {
                Some("text") => part["text"].as_str().map(str::to_owned),
                Some("image") => Some("[image]".to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    let text = collapse(&text);
    let error = block["is_error"].as_bool().unwrap_or(false);
    match (text.is_empty(), error) {
        (true, false) => None,
        (true, true) => Some("error".to_owned()),
        (false, false) => Some(truncate(&text, RESULT_CHARS)),
        (false, true) => Some(format!("error: {}", truncate(&text, RESULT_CHARS))),
    }
}

/// Paths under the session's cwd are shown relative to it.
fn relative(path: &str, cwd: &Path) -> String {
    match Path::new(path).strip_prefix(cwd) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => path.to_owned(),
    }
}

fn first_line(text: &str) -> String {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or_default().trim();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cuts on a character boundary and marks the cut.
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}
