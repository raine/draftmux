//! Reading a Claude Code transcript (`<session-id>.jsonl`) and choosing the
//! lines that make up its conversation: the one Claude would resume, or the
//! one the chat shows.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead};

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

/// The fields of a transcript line that are read; the rest, including
/// attachment payloads, is skipped while parsing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub subtype: Option<String>,
    pub uuid: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    /// The line before a compact boundary, which has no `parentUuid`.
    #[serde(default)]
    pub logical_parent_uuid: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub is_meta: bool,
    #[serde(default)]
    pub is_compact_summary: bool,
    /// A `<synthetic>` assistant message reporting a failed model request.
    #[serde(default)]
    pub is_api_error_message: bool,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub message: Option<Message>,
    /// What a tool returned besides its `tool_result` text.
    #[serde(default)]
    pub tool_use_result: Option<ToolUseResult>,
    #[serde(default)]
    compact_metadata: Option<CompactMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub content: Value,
    /// What the model request that wrote an assistant message reported.
    #[serde(default)]
    usage: Option<Usage>,
}

/// An assistant message's token counts, as the API reports them.
#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl Usage {
    /// The request's input, then what it wrote: the conversation the next
    /// request starts from.
    pub fn context(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
            + self.output_tokens
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResult {
    /// An `Edit`'s or `Write`'s change.
    #[serde(rename_all = "camelCase")]
    Patch {
        structured_patch: Vec<Hunk>,
    },
    Other(IgnoredAny),
}

/// A hunk of a unified diff, as `structuredPatch` holds it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
    /// Lines with their ` `, `-` or `+` prefix.
    pub lines: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactMetadata {
    #[serde(default)]
    preserved_messages: Option<PreservedMessages>,
    /// The conversation's size in tokens after compacting.
    #[serde(default)]
    post_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PreservedMessages {
    #[serde(default)]
    uuids: Vec<String>,
}

impl Line {
    pub fn is_message(&self) -> bool {
        self.kind == "user" || self.kind == "assistant"
    }

    pub fn is_compact_boundary(&self) -> bool {
        self.kind == "system" && self.subtype.as_deref() == Some("compact_boundary")
    }

    pub fn blocks(&self) -> &[Value] {
        self.message
            .as_ref()
            .and_then(|m| m.content.as_array())
            .map_or(&[], Vec::as_slice)
    }

    /// The conversation's size in tokens after this line: what its
    /// assistant message's request reported, or what a compact boundary
    /// left, as far as known. `None` for a line that doesn't tell.
    pub fn context(&self) -> Option<Option<u64>> {
        if self.is_compact_boundary() {
            return Some(self.compact_metadata.as_ref().and_then(|m| m.post_tokens));
        }
        let usage = self.message.as_ref()?.usage.as_ref()?;
        let tokens = usage.context();
        (self.kind == "assistant" && tokens > 0).then_some(Some(tokens))
    }

    pub fn message_id(&self) -> Option<&str> {
        self.message.as_ref()?.id.as_deref()
    }

    fn tool_use_ids(&self) -> impl Iterator<Item = &str> {
        self.blocks()
            .iter()
            .filter(|b| is_tool_use(b))
            .filter_map(|b| b["id"].as_str())
    }

    fn tool_result_ids(&self) -> impl Iterator<Item = &str> {
        self.blocks()
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .filter_map(|b| b["tool_use_id"].as_str())
    }
}

pub fn is_tool_use(block: &Value) -> bool {
    matches!(block["type"].as_str(), Some("tool_use" | "server_tool_use"))
}

/// Parses every line with a `uuid`, skipping bookkeeping lines without one,
/// sidechain lines, and lines that don't parse (the transcript may be
/// mid-write, cut off inside a character).
pub fn parse(reader: impl BufRead) -> io::Result<Vec<Line>> {
    let mut lines = Vec::new();
    for line in reader.split(b'\n') {
        if let Ok(line) = serde_json::from_slice::<Line>(&line?)
            && !line.is_sidechain
        {
            lines.push(line);
        }
    }
    Ok(lines)
}

/// The lines of the conversation Claude would resume, in context order.
///
/// Rewinds leave abandoned branches in the file, so the conversation is the
/// `parentUuid` chain back from the newest message. The chain ends at the
/// latest compact boundary, which drops everything before it. Parallel tool
/// calls put blocks of one assistant message and their results on side
/// branches; those are added back by message and tool IDs. Lines a
/// compaction kept in context (`preservedMessages`) sit before the boundary
/// and go right after the compact summary.
pub fn conversation(lines: &[Line]) -> Result<Vec<&Line>, String> {
    let leaf = leaf(lines)?;
    let by_uuid = by_uuid(lines);
    let mut included = HashSet::new();
    let mut boundary = None;
    let mut current = Some(leaf);
    while let Some(i) = current {
        if !included.insert(i) {
            break;
        }
        let line = &lines[i];
        if line.is_compact_boundary() {
            boundary = Some(i);
            break;
        }
        current = parent(line, &by_uuid);
    }

    if let Some(b) = boundary {
        let preserved = lines[b]
            .compact_metadata
            .as_ref()
            .and_then(|m| m.preserved_messages.as_ref())
            .map_or(&[][..], |p| p.uuids.as_slice());
        included.extend(preserved.iter().filter_map(|u| by_uuid.get(u.as_str())));
    }
    add_branches(lines, &mut included);

    let mut order: Vec<usize> = included
        .into_iter()
        .filter(|&i| lines[i].is_message())
        .collect();
    order.sort_unstable();
    if let Some(b) = boundary {
        // Pre-boundary lines are the preserved segment and its siblings.
        let preserved: Vec<usize> = order.iter().copied().filter(|&i| i < b).collect();
        order.retain(|&i| i > b);
        let after_summary = order
            .iter()
            .position(|&i| lines[i].is_compact_summary)
            .map_or(0, |p| p + 1);
        order.splice(after_summary..after_summary, preserved);
    }
    Ok(order.into_iter().map(|i| &lines[i]).collect())
}

/// The index of the newest message, which a resumed conversation continues
/// from. Fails without messages or while a compaction is in progress.
pub fn leaf(lines: &[Line]) -> Result<usize, String> {
    let Some(leaf) = lines.iter().rposition(Line::is_message) else {
        return Err("the transcript has no messages".into());
    };
    if lines[leaf..].iter().any(Line::is_compact_boundary) {
        return Err("the transcript is being compacted; try again once compaction finishes".into());
    }
    Ok(leaf)
}

/// The messages of the whole conversation up to the newest message and the
/// compact boundaries between them, in file order: the chain
/// [`conversation`] follows, continued past each compaction to the lines
/// before it. Empty for a transcript without messages.
pub fn shown(lines: &[Line]) -> Vec<&Line> {
    let Some(leaf) = lines.iter().rposition(Line::is_message) else {
        return Vec::new();
    };
    let by_uuid = by_uuid(lines);
    let mut included = HashSet::new();
    let mut current = Some(leaf);
    while let Some(i) = current {
        if !included.insert(i) {
            break;
        }
        let line = &lines[i];
        current = parent(line, &by_uuid).or_else(|| {
            line.is_compact_boundary()
                .then_some(line.logical_parent_uuid.as_deref())
                .flatten()
                .and_then(|uuid| by_uuid.get(uuid).copied())
        });
    }
    add_branches(lines, &mut included);
    let mut order: Vec<usize> = included
        .into_iter()
        .filter(|&i| lines[i].is_message() || lines[i].is_compact_boundary())
        .collect();
    order.sort_unstable();
    order.into_iter().map(|i| &lines[i]).collect()
}

fn by_uuid(lines: &[Line]) -> HashMap<&str, usize> {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| (line.uuid.as_str(), i))
        .collect()
}

fn parent(line: &Line, by_uuid: &HashMap<&str, usize>) -> Option<usize> {
    line.parent_uuid
        .as_deref()
        .and_then(|parent| by_uuid.get(parent).copied())
}

/// Adds the other lines of included assistant messages, then the results of
/// their tool calls.
fn add_branches(lines: &[Line], included: &mut HashSet<usize>) {
    let message_ids: HashSet<&str> = included
        .iter()
        .filter_map(|&i| lines[i].message_id())
        .collect();
    included.extend(
        (0..lines.len())
            .filter(|&i| lines[i].kind == "assistant")
            .filter(|&i| {
                lines[i]
                    .message_id()
                    .is_some_and(|id| message_ids.contains(id))
            }),
    );
    let tool_ids: HashSet<&str> = included
        .iter()
        .flat_map(|&i| lines[i].tool_use_ids())
        .collect();
    included.extend(
        (0..lines.len())
            .filter(|&i| lines[i].kind == "user")
            .filter(|&i| lines[i].tool_result_ids().any(|id| tool_ids.contains(id))),
    );
}
