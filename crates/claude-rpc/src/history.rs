//! A transcript's conversation as the chat shows it ([`draftmux_agent`]'s
//! [`History`]), so Claude's line format stays in this crate.

use std::collections::{HashMap, HashSet};

use draftmux_agent::{History, Message, Stop, ToolCall, ToolKind, ToolResult};
use serde::Deserialize;
use serde_json::Value;

use crate::contract::is_context_block;
use crate::transcript::{self, Line, ToolUseResult};

/// What Claude Code writes as a user message when a run is interrupted,
/// with " for tool use]" or "]" after it.
const INTERRUPTED: &str = "[Request interrupted by user";
/// Starts of the user lines Claude Code writes for a slash command and its
/// output, such as the `/model` a model switch runs.
const COMMAND_LINES: &[&str] = &[
    "<command-name>",
    "<command-message>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<local-command-caveat>",
];

/// The conversation of a transcript's `lines`. With a `boundary`, the line
/// uuid a fork's source ended at, the messages up to it are the source
/// conversation, counted in [`History::earlier`]; without one, or when the
/// transcript doesn't hold it, every message is shown.
pub fn history(lines: &[Line], boundary: Option<&str>) -> History {
    let shown = transcript::shown(lines);
    let split = boundary
        .and_then(|uuid| shown.iter().position(|line| line.uuid == uuid))
        .map_or(0, |i| i + 1);
    let earlier = messages(&shown[..split])
        .iter()
        .filter(|message| matches!(message, Message::User(_) | Message::Assistant { .. }))
        .count();
    History {
        earlier,
        messages: messages(&shown[split..]),
        context: shown.iter().rev().find_map(|line| line.context()).flatten(),
    }
}

/// The messages of `lines`, which [`transcript::shown`] chose. An assistant
/// message written as several lines becomes one. Tool results whose call
/// isn't among them are dropped, such as the result Claude adds for a
/// source's unanswered call when it forks the session.
fn messages(lines: &[&Line]) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut assistants: HashMap<&str, usize> = HashMap::new();
    let mut calls = HashSet::new();
    for &line in lines {
        if line.is_compact_boundary() {
            messages.push(Message::Compacted);
            continue;
        }
        let Some(message) = &line.message else {
            continue;
        };
        if line.kind == "assistant" {
            let synthetic = message.model.as_deref() == Some("<synthetic>");
            // Claude Code's own placeholders, such as "No response
            // requested."; a failed model request is shown.
            if synthetic && !line.is_api_error_message {
                continue;
            }
            let index = match line.message_id().and_then(|id| assistants.get(id)) {
                Some(&index) => index,
                None => {
                    messages.push(Message::Assistant {
                        text: String::new(),
                        tool_calls: Vec::new(),
                        stop: Stop::Completed,
                    });
                    if let Some(id) = line.message_id() {
                        assistants.insert(id, messages.len() - 1);
                    }
                    messages.len() - 1
                }
            };
            let Message::Assistant {
                text,
                tool_calls,
                stop,
            } = &mut messages[index]
            else {
                unreachable!("message IDs index assistant messages");
            };
            for block in line.blocks() {
                if transcript::is_tool_use(block) {
                    let call = tool_call(block);
                    calls.insert(call.id.clone());
                    tool_calls.push(call);
                } else if block["type"] == "text"
                    && let Some(block) = block["text"].as_str().filter(|t| !t.trim().is_empty())
                {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(block);
                }
            }
            if synthetic {
                *stop = Stop::Error(Some(std::mem::take(text)));
            }
        } else if line.kind == "user" && !line.is_meta && !line.is_compact_summary {
            let results: Vec<&Value> = line
                .blocks()
                .iter()
                .filter(|block| block["type"] == "tool_result")
                .collect();
            for block in &results {
                let Some(id) = block["tool_use_id"]
                    .as_str()
                    .filter(|id| calls.contains(*id))
                else {
                    continue;
                };
                // `toolUseResult` belongs to the line's only result.
                let patch = match &line.tool_use_result {
                    Some(ToolUseResult::Patch { structured_patch }) if results.len() == 1 => {
                        patch(structured_patch)
                    }
                    _ => None,
                };
                messages.push(Message::ToolResult {
                    id: id.to_owned(),
                    result: ToolResult {
                        output: content_text(&block["content"]),
                        is_error: block["is_error"] == true,
                        duration: None,
                        patch,
                    },
                });
            }
            let text = prompt_text(&message.content);
            if text.starts_with(INTERRUPTED) {
                // The reply it interrupted, unless it came before any.
                if let Some(Message::Assistant { stop, .. }) = messages
                    .iter_mut()
                    .rev()
                    .find(|message| !matches!(message, Message::ToolResult { .. }))
                {
                    *stop = Stop::Aborted;
                }
            } else if !text.trim().is_empty()
                && !COMMAND_LINES.iter().any(|start| text.starts_with(start))
            {
                messages.push(Message::User(text));
            }
        }
    }
    // Messages that were only thinking.
    messages.retain(|message| {
        !matches!(message, Message::Assistant { text, tool_calls, stop: Stop::Completed }
            if text.is_empty() && tool_calls.is_empty())
    });
    messages
}

/// The text of a `content`: a string, or the text blocks of an array joined
/// by newlines.
pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The text of a user message's `content` without the context blocks
/// Draftmux puts ahead of a prompt.
fn prompt_text(content: &Value) -> String {
    match content {
        Value::Array(blocks) => {
            let blocks: Vec<Value> = blocks
                .iter()
                .filter(|block| !block["text"].as_str().is_some_and(is_context_block))
                .cloned()
                .collect();
            content_text(&Value::Array(blocks))
        }
        content => content_text(content),
    }
}

/// A `tool_use` block, with the arguments the chat shows for Claude Code's
/// built-in tools.
pub(crate) fn tool_call(block: &Value) -> ToolCall {
    let input = &block["input"];
    let arg = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
    let name = block["name"].as_str().unwrap_or_default();
    let kind = match name {
        "Bash" => ToolKind::Bash {
            command: arg("command"),
        },
        "Read" => ToolKind::Read {
            path: arg("file_path"),
        },
        "Edit" | "MultiEdit" => ToolKind::Edit {
            path: arg("file_path"),
        },
        "Write" => ToolKind::Write {
            path: arg("file_path"),
            content: arg("content"),
        },
        "Grep" => ToolKind::Grep {
            pattern: arg("pattern"),
        },
        "Glob" => ToolKind::Find {
            pattern: arg("pattern"),
        },
        name => ToolKind::Other {
            name: name.to_owned(),
        },
    };
    ToolCall {
        id: block["id"].as_str().unwrap_or_default().to_owned(),
        kind,
    }
}

/// The unified patch of an Edit's or Write's `tool_use_result`, as a
/// stream-json `user` frame carries it.
pub(crate) fn patch_of(tool_use_result: &Value) -> Option<String> {
    match ToolUseResult::deserialize(tool_use_result).ok()? {
        ToolUseResult::Patch { structured_patch } => patch(&structured_patch),
        ToolUseResult::Other(_) => None,
    }
}

/// `structuredPatch` hunks as a unified patch; none for a change without
/// hunks, such as a new file.
fn patch(hunks: &[transcript::Hunk]) -> Option<String> {
    if hunks.is_empty() {
        return None;
    }
    let mut patch = String::new();
    for hunk in hunks {
        patch.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        ));
        for line in &hunk.lines {
            patch.push_str(line);
            patch.push('\n');
        }
    }
    Some(patch)
}
