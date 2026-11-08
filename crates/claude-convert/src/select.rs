//! Reading a Claude Code transcript and choosing the lines that make up the
//! conversation Claude would resume.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

/// The fields of a transcript line the converter uses; the rest, including
/// attachment payloads, is skipped while parsing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Line {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub subtype: Option<String>,
    pub uuid: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub is_meta: bool,
    #[serde(default)]
    pub is_compact_summary: bool,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    compact_metadata: Option<CompactMetadata>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Message {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub content: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactMetadata {
    #[serde(default)]
    preserved_messages: Option<PreservedMessages>,
}

#[derive(Debug, Deserialize)]
struct PreservedMessages {
    #[serde(default)]
    uuids: Vec<String>,
}

impl Line {
    fn is_message(&self) -> bool {
        self.kind == "user" || self.kind == "assistant"
    }

    fn is_compact_boundary(&self) -> bool {
        self.kind == "system" && self.subtype.as_deref() == Some("compact_boundary")
    }

    pub fn blocks(&self) -> &[Value] {
        self.message
            .as_ref()
            .and_then(|m| m.content.as_array())
            .map_or(&[], Vec::as_slice)
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

pub(crate) fn is_tool_use(block: &Value) -> bool {
    matches!(block["type"].as_str(), Some("tool_use" | "server_tool_use"))
}

/// Parses every line with a `uuid`, skipping bookkeeping lines without one,
/// sidechain lines, and lines that don't parse (the transcript may be
/// mid-write).
pub(crate) fn parse(reader: impl BufRead) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    for line in reader.split(b'\n') {
        let line = line.context("reading the transcript")?;
        if let Ok(line) = serde_json::from_slice::<Line>(&line)
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
pub(crate) fn conversation(lines: &[Line]) -> Result<Vec<&Line>> {
    let Some(leaf) = lines.iter().rposition(Line::is_message) else {
        bail!("the transcript has no messages");
    };
    if lines[leaf..].iter().any(Line::is_compact_boundary) {
        bail!("the transcript is being compacted; try again once compaction finishes");
    }

    let by_uuid: HashMap<&str, usize> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| (line.uuid.as_str(), i))
        .collect();
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
        current = line
            .parent_uuid
            .as_deref()
            .and_then(|parent| by_uuid.get(parent).copied());
    }

    if let Some(b) = boundary {
        let preserved = lines[b]
            .compact_metadata
            .as_ref()
            .and_then(|m| m.preserved_messages.as_ref())
            .map_or(&[][..], |p| p.uuids.as_slice());
        included.extend(preserved.iter().filter_map(|u| by_uuid.get(u.as_str())));
    }

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
