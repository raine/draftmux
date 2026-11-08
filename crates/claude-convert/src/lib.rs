//! Converts a Claude Code transcript into a pi session file (format version
//! 3), so a draft handed off from Claude Code continues with its context.
//!
//! The result is text only: user messages, one assistant message per Claude
//! API response with thinking dropped, and tool calls and results as short
//! bracketed notes in the assistant text rather than pi tool calls. Assistant
//! messages name a placeholder provider that pi can't resolve; the session
//! ends with a `model_change` and `thinking_level_change` for the model to
//! continue on, so pi uses that model instead.

mod notes;
mod select;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use select::Line;

/// pi's `KnownApi` value for Anthropic's Messages API, the API Claude Code
/// talks to.
const API: &str = "anthropic-messages";
/// Not a pi provider, so pi never restores the converted messages' model.
const PROVIDER: &str = "claude-code";

/// The model and thinking level a converted session continues on. Either
/// part may be unknown; pi then uses its own default for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: Option<Model>,
    /// A pi thinking level such as `medium`; pi clamps it to the model.
    pub thinking_level: Option<String>,
}

pub use draftmux_pi_profiles::ModelRef as Model;

impl ModelSelection {
    /// The profile's defaults from `<agent_dir>/settings.json`
    /// (`defaultProvider`, `defaultModel`, `defaultThinkingLevel`). A missing
    /// file or key is unknown; a file that doesn't parse is an error.
    pub fn profile_default(agent_dir: &Path) -> Result<Self> {
        let defaults = draftmux_pi_profiles::Defaults::read(agent_dir)?;
        Ok(Self {
            model: defaults.model,
            thinking_level: defaults.thinking_level,
        })
    }

    /// Each part from `self`, else from `fallback`.
    pub fn or(self, fallback: Self) -> Self {
        Self {
            model: self.model.or(fallback.model),
            thinking_level: self.thinking_level.or(fallback.thinking_level),
        }
    }
}

/// A converted session, ready to be written.
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub created_at: DateTime<Utc>,
    /// The session file's contents, one JSON entry per line.
    pub jsonl: String,
}

impl Session {
    /// Writes the session into `dir` under a pi-style name
    /// (`<timestamp>_<id>.jsonl`) and returns its path. Never overwrites.
    pub fn write_into(&self, dir: &Path) -> Result<PathBuf> {
        let stamp = iso(self.created_at).replace([':', '.'], "-");
        let path = dir.join(format!("{stamp}_{}.jsonl", self.id));
        let mut file = std::fs::File::create_new(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(self.jsonl.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

/// Converts the transcript read from `transcript`. `cwd` becomes the pi
/// session's working directory and shortens paths in tool notes.
pub fn convert(transcript: impl BufRead, cwd: &Path, model: &ModelSelection) -> Result<Session> {
    let lines = select::parse(transcript)?;
    let conversation = select::conversation(&lines)?;
    let messages = messages(&conversation, cwd);
    Ok(emit(
        &messages,
        cwd,
        model,
        uuid::Uuid::now_v7().to_string(),
        Utc::now(),
    ))
}

enum Turn<'a> {
    User { text: String, line: &'a Line },
    Assistant { parts: Vec<Part>, line: &'a Line },
}

enum Part {
    Text(String),
    Tool {
        call: String,
        result: Option<String>,
    },
}

fn messages<'a>(conversation: &[&'a Line], cwd: &Path) -> Vec<Turn<'a>> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut by_message_id: HashMap<&str, usize> = HashMap::new();
    let mut tool_slots: HashMap<&str, (usize, usize)> = HashMap::new();

    for &line in conversation {
        let Some(message) = &line.message else {
            continue;
        };
        if line.kind == "assistant" {
            // Claude Code's own placeholders, such as API error notices.
            if message.model.as_deref() == Some("<synthetic>") {
                continue;
            }
            let index = match line.message_id().and_then(|id| by_message_id.get(id)) {
                Some(&index) => index,
                None => {
                    turns.push(Turn::Assistant {
                        parts: Vec::new(),
                        line,
                    });
                    if let Some(id) = line.message_id() {
                        by_message_id.insert(id, turns.len() - 1);
                    }
                    turns.len() - 1
                }
            };
            let Turn::Assistant { parts, .. } = &mut turns[index] else {
                unreachable!("message IDs index assistant turns");
            };
            for block in line.blocks() {
                if select::is_tool_use(block) {
                    if let Some(id) = block["id"].as_str() {
                        tool_slots.insert(id, (index, parts.len()));
                    }
                    parts.push(Part::Tool {
                        call: notes::call(block, cwd),
                        result: None,
                    });
                } else if block["type"] == "text"
                    && let Some(text) = block["text"].as_str().filter(|t| !t.trim().is_empty())
                {
                    parts.push(Part::Text(text.to_owned()));
                }
            }
        } else if !line.is_meta {
            let mut texts = Vec::new();
            match &message.content {
                Value::String(text) => texts.push(text.clone()),
                Value::Array(blocks) => {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("text") => {
                                texts.extend(block["text"].as_str().map(str::to_owned));
                            }
                            Some("image") => texts.push("[image]".to_owned()),
                            Some("tool_result") => {
                                let slot = block["tool_use_id"]
                                    .as_str()
                                    .and_then(|id| tool_slots.get(id));
                                if let Some(&(turn, part)) = slot
                                    && let Turn::Assistant { parts, .. } = &mut turns[turn]
                                    && let Part::Tool { result, .. } = &mut parts[part]
                                {
                                    *result = notes::result(block);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            let text = texts.join("\n\n");
            if !text.trim().is_empty() {
                turns.push(Turn::User { text, line });
            }
        }
    }
    turns
}

fn emit(
    turns: &[Turn],
    cwd: &Path,
    model: &ModelSelection,
    session_id: String,
    now: DateTime<Utc>,
) -> Session {
    let mut entries = vec![json!({
        "type": "session",
        "version": 3,
        "id": session_id,
        "timestamp": iso(now),
        "cwd": cwd,
    })];
    let mut parent: Option<String> = None;
    let mut push = |entries: &mut Vec<Value>, at: DateTime<Utc>, mut entry: Value| {
        let id = format!("{:08x}", entries.len());
        entry["id"] = json!(id);
        entry["parentId"] = json!(parent);
        entry["timestamp"] = json!(iso(at));
        entries.push(entry);
        parent = Some(id);
    };

    for turn in turns {
        let (line, message) = match turn {
            Turn::User { text, line } => (
                line,
                json!({
                    "role": "user",
                    "content": [{ "type": "text", "text": text }],
                }),
            ),
            Turn::Assistant { parts, line } => {
                if parts.is_empty() {
                    continue;
                }
                let model = line
                    .message
                    .as_ref()
                    .and_then(|m| m.model.as_deref())
                    .unwrap_or("claude");
                (
                    line,
                    json!({
                        "role": "assistant",
                        "content": [{ "type": "text", "text": assistant_text(parts) }],
                        "api": API,
                        "provider": PROVIDER,
                        "model": model,
                        "usage": {
                            "input": 0,
                            "output": 0,
                            "cacheRead": 0,
                            "cacheWrite": 0,
                            "totalTokens": 0,
                            "cost": {
                                "input": 0,
                                "output": 0,
                                "cacheRead": 0,
                                "cacheWrite": 0,
                                "total": 0,
                            },
                        },
                        "stopReason": "stop",
                    }),
                )
            }
        };
        let at = line
            .timestamp
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map_or(now, |t| t.with_timezone(&Utc));
        let mut message = message;
        message["timestamp"] = at.timestamp_millis().into();
        push(
            &mut entries,
            at,
            json!({ "type": "message", "message": message }),
        );
    }

    if let Some(Model { provider, id }) = &model.model {
        push(
            &mut entries,
            now,
            json!({ "type": "model_change", "provider": provider, "modelId": id }),
        );
    }
    if let Some(level) = &model.thinking_level {
        push(
            &mut entries,
            now,
            json!({ "type": "thinking_level_change", "thinkingLevel": level }),
        );
    }

    let mut jsonl = String::new();
    for entry in &entries {
        jsonl.push_str(&entry.to_string());
        jsonl.push('\n');
    }
    Session {
        id: session_id,
        created_at: now,
        jsonl,
    }
}

/// Text blocks as written; each tool call as a bracketed note with its result.
fn assistant_text(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => text.clone(),
            Part::Tool { call, result: None } => format!("[{call}]"),
            Part::Tool {
                call,
                result: Some(result),
            } => format!("[{call} → {result}]"),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}
