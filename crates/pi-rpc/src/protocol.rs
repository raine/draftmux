//! Wire types for `pi --mode rpc`, as emitted by pi 1.5.0
//! (`packages/coding-agent/src/modes/rpc/rpc-types.ts`, `json-event.ts`).
//!
//! Only the parts Draftmux uses are typed; unknown fields are ignored and
//! unknown event types decode to [`Event::Other`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The commands Draftmux sends. Commands that write pi's global settings
/// (`set_steering_mode`, `set_follow_up_mode`, `set_auto_compaction`,
/// `set_auto_retry`) are deliberately absent, so they can't be sent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Command {
    Prompt { message: String },
    Abort,
    GetState,
    SetModel { provider: String, model_id: String },
    SetThinkingLevel { level: ThinkingLevel },
    GetAvailableModels,
    GetMessages,
}

impl Command {
    /// One JSONL record with the given correlation id.
    pub(crate) fn to_line(&self, id: &str) -> String {
        let mut value = serde_json::to_value(self).expect("commands serialize");
        value["id"] = Value::from(id);
        let mut line = value.to_string();
        line.push('\n');
        line
    }
}

/// The reply that cancels an extension dialog.
pub(crate) fn cancel_dialog_line(id: &str) -> String {
    let mut line =
        serde_json::json!({"type": "extension_ui_response", "id": id, "cancelled": true})
            .to_string();
    line.push('\n');
    line
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    /// Every level, lowest first.
    pub const ALL: [ThinkingLevel; 7] = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];

    /// The level as pi writes it, such as `xhigh`.
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }

    /// The level pi writes as `name`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == name)
    }
}

/// What pi did with a prompt. `Handled` means an extension or command
/// consumed it and no run follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Started,
    Queued,
    Handled,
}

/// `get_state`: what pi actually resolved for the session.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub model: Option<Model>,
    pub thinking_level: ThinkingLevel,
    pub is_streaming: bool,
    pub session_file: Option<PathBuf>,
}

/// A model from pi's catalog.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub provider: String,
    pub api: String,
    #[serde(default)]
    pub reasoning: bool,
}

#[derive(Deserialize)]
pub(crate) struct PromptData {
    pub disposition: Disposition,
}

#[derive(Deserialize)]
pub(crate) struct ModelsData {
    pub models: Vec<Model>,
}

#[derive(Deserialize)]
pub(crate) struct MessagesData {
    pub messages: Vec<Value>,
}

/// One record pi wrote on stdout.
#[derive(Debug)]
pub(crate) enum Incoming {
    Response(Response),
    /// An extension dialog (`select`, `confirm`, `input`, `editor`) waiting
    /// for a reply.
    Dialog {
        id: String,
        method: String,
        title: String,
    },
    Event(Event),
}

#[derive(Debug, Deserialize)]
pub(crate) struct Response {
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// An event from pi's stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    AgentStart,
    /// The agent loop ended; with `will_retry` it continues before settling.
    AgentEnd {
        #[serde(default)]
        will_retry: bool,
    },
    /// The run is over, including retries and compaction.
    AgentSettled {
        aborted: bool,
    },
    TurnStart,
    TurnEnd,
    /// A message as pi's `AgentMessage` JSON. Messages with `display: false`
    /// (such as the extension's `draftmux-context`) aren't for the chat.
    MessageStart {
        message: Value,
    },
    #[serde(skip)]
    MessageUpdate(AssistantDelta),
    /// The final, authoritative message.
    MessageEnd {
        message: Value,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        #[serde(default)]
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        #[serde(default)]
        partial_result: Value,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        #[serde(default)]
        result: Value,
        is_error: bool,
        duration_ms: Option<u64>,
    },
    ThinkingLevelChanged {
        level: ThinkingLevel,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        final_error: Option<String>,
    },
    CompactionStart {
        reason: String,
    },
    CompactionEnd {
        reason: String,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    },
    ExtensionError {
        extension_path: String,
        event: String,
        error: String,
    },
    /// An extension's `setStatus`. The bundled extension reports project
    /// trust as key `draftmux.trust`, text `trusted` or `untrusted`.
    #[serde(skip)]
    Status {
        key: String,
        text: Option<String>,
    },
    /// An extension's `notify`.
    #[serde(skip)]
    Notify {
        message: String,
        kind: Option<String>,
    },
    /// An extension asked for input. There is no UI for it, so it was
    /// answered as cancelled.
    #[serde(skip)]
    DialogCancelled {
        method: String,
        title: String,
    },
    /// A record Draftmux doesn't use, by its `type` (or extension UI method).
    #[serde(skip)]
    Other(String),
    /// Decoding placeholder for unknown types; never delivered, they
    /// arrive as [`Event::Other`].
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl Event {
    /// A successful `edit` or `write`. The bundled extension blocks every
    /// other target, so this changed `draft.md` and the editor should
    /// reload it.
    pub fn is_draft_write(&self) -> bool {
        matches!(
            self,
            Event::ToolExecutionEnd { tool_name, is_error: false, .. }
                if tool_name == "edit" || tool_name == "write"
        )
    }
}

/// A streaming delta of the assistant message (`message_update`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AssistantDelta {
    TextStart {
        content_index: usize,
    },
    TextDelta {
        content_index: usize,
        delta: String,
    },
    TextEnd {
        content_index: usize,
        content: String,
    },
    ThinkingStart {
        content_index: usize,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        content_index: usize,
        content: String,
    },
    ToolcallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    ToolcallDelta {
        content_index: usize,
        delta: String,
    },
    ToolcallEnd {
        content_index: usize,
        tool_call: Value,
    },
    Done {
        reason: String,
    },
    Error {
        reason: String,
    },
    #[serde(other)]
    Other,
}

/// Parses one stdout line. The `type` is read first so `agent_end`, whose
/// `messages` is the whole conversation, is decoded without building them.
pub(crate) fn parse_line(line: &str) -> serde_json::Result<Incoming> {
    #[derive(Deserialize)]
    struct Tag {
        #[serde(rename = "type")]
        kind: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct UiRequest {
        id: String,
        method: String,
        #[serde(default)]
        title: String,
        status_key: Option<String>,
        status_text: Option<String>,
        message: Option<String>,
        notify_type: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AgentEnd {
        #[serde(default)]
        will_retry: bool,
    }

    let kind = serde_json::from_str::<Tag>(line)?.kind;
    let event = match kind.as_str() {
        "response" => return Ok(Incoming::Response(serde_json::from_str(line)?)),
        "extension_ui_request" => {
            let req: UiRequest = serde_json::from_str(line)?;
            match req.method.as_str() {
                "select" | "confirm" | "input" | "editor" => {
                    return Ok(Incoming::Dialog {
                        id: req.id,
                        method: req.method,
                        title: req.title,
                    });
                }
                "setStatus" => Event::Status {
                    key: req.status_key.unwrap_or_default(),
                    text: req.status_text,
                },
                "notify" => Event::Notify {
                    message: req.message.unwrap_or_default(),
                    kind: req.notify_type,
                },
                _ => Event::Other(req.method),
            }
        }
        "agent_end" => Event::AgentEnd {
            will_retry: serde_json::from_str::<AgentEnd>(line)?.will_retry,
        },
        // Carries the turn's messages, which `message_end` already gave.
        "turn_end" => Event::TurnEnd,
        "message_update" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Wire {
                assistant_message_event: AssistantDelta,
            }
            Event::MessageUpdate(serde_json::from_str::<Wire>(line)?.assistant_message_event)
        }
        _ => match serde_json::from_str(line)? {
            Event::Unknown => Event::Other(kind),
            event => event,
        },
    };
    Ok(Incoming::Event(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_serialize_to_pi_shapes() {
        let cases = [
            (
                Command::Prompt {
                    message: "hi".into(),
                },
                r#"{"id":"7","message":"hi","type":"prompt"}"#,
            ),
            (Command::Abort, r#"{"id":"7","type":"abort"}"#),
            (Command::GetState, r#"{"id":"7","type":"get_state"}"#),
            (
                Command::SetModel {
                    provider: "anthropic".into(),
                    model_id: "claude-opus-5-5".into(),
                },
                r#"{"id":"7","modelId":"claude-opus-5-5","provider":"anthropic","type":"set_model"}"#,
            ),
            (
                Command::SetThinkingLevel {
                    level: ThinkingLevel::Xhigh,
                },
                r#"{"id":"7","level":"xhigh","type":"set_thinking_level"}"#,
            ),
            (
                Command::GetAvailableModels,
                r#"{"id":"7","type":"get_available_models"}"#,
            ),
            (Command::GetMessages, r#"{"id":"7","type":"get_messages"}"#),
        ];
        for (command, expected) in cases {
            let line = command.to_line("7");
            let json = line.strip_suffix('\n').expect("line ends with a newline");
            assert!(
                !json.contains('\n'),
                "line is a single JSON object: {line:?}"
            );
            let actual: Value = serde_json::from_str(json).unwrap();
            let expected: Value = serde_json::from_str(expected).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn agent_end_ignores_messages() {
        let big = "x".repeat(1 << 20);
        let line = format!(
            r#"{{"type":"agent_end","messages":[{{"role":"user","content":"{big}"}}],"willRetry":true}}"#
        );
        let Incoming::Event(event) = parse_line(&line).unwrap() else {
            panic!("not an event");
        };
        assert_eq!(event, Event::AgentEnd { will_retry: true });
    }

    #[test]
    fn extension_ui_requests() {
        let status = r#"{"type":"extension_ui_request","id":"a","method":"setStatus","statusKey":"draftmux.trust","statusText":"trusted"}"#;
        assert!(matches!(
            parse_line(status).unwrap(),
            Incoming::Event(Event::Status { key, text: Some(text) })
                if key == "draftmux.trust" && text == "trusted"
        ));
        let confirm = r#"{"type":"extension_ui_request","id":"b","method":"confirm","title":"Sure?","message":"x"}"#;
        assert!(matches!(
            parse_line(confirm).unwrap(),
            Incoming::Dialog { id, method, title }
                if id == "b" && method == "confirm" && title == "Sure?"
        ));
        let widget =
            r#"{"type":"extension_ui_request","id":"c","method":"setWidget","widgetKey":"w"}"#;
        assert!(matches!(
            parse_line(widget).unwrap(),
            Incoming::Event(Event::Other(method)) if method == "setWidget"
        ));
    }

    /// The events pi printed in a recorded fake-pi script.
    fn recorded_events(script: &str) -> Vec<Event> {
        script
            .lines()
            .filter(|line| line.starts_with('{'))
            .filter_map(|line| match parse_line(line).unwrap() {
                Incoming::Event(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    fn draft_writes(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter(|event| event.is_draft_write())
            .map(|event| match event {
                Event::ToolExecutionEnd { tool_name, .. } => tool_name.as_str(),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn draft_writes_in_recorded_runs() {
        // A read, then an edit of the draft.
        let tool = recorded_events(include_str!("../tests/fixtures/tool.jsonl"));
        assert_eq!(draft_writes(&tool), ["edit"]);
        // A read, then a write of the draft, then an aborted reply.
        let abort = recorded_events(include_str!("../tests/fixtures/abort.jsonl"));
        assert_eq!(draft_writes(&abort), ["write"]);
    }

    #[test]
    fn blocked_write_is_not_a_draft_write() {
        // The bundled extension's guard text (pi-extension/README.md).
        let line = r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"write","result":{"content":[{"type":"text","text":"Draftmux: write is limited to the draft file /d/draft.md; \"notes.txt\" (/repo/notes.txt) is outside it. Use read or bash to inspect other files."}]},"isError":true,"durationMs":1}"#;
        assert!(draft_writes(&recorded_events(line)).is_empty());
    }

    #[test]
    fn draft_write_detection() {
        let end = |tool: &str, is_error| Event::ToolExecutionEnd {
            tool_call_id: "1".into(),
            tool_name: tool.into(),
            result: Value::Null,
            is_error,
            duration_ms: None,
        };
        assert!(end("edit", false).is_draft_write());
        assert!(end("write", false).is_draft_write());
        assert!(!end("edit", true).is_draft_write());
        assert!(!end("bash", false).is_draft_write());
    }

    #[test]
    fn unknown_records() {
        assert!(matches!(
            parse_line(r#"{"type":"queue_update","steering":[],"followUp":[]}"#).unwrap(),
            Incoming::Event(Event::Other(kind)) if kind == "queue_update"
        ));
        let delta = r#"{"type":"message_update","usage":{},"assistantMessageEvent":{"type":"new_kind","x":1}}"#;
        assert!(matches!(
            parse_line(delta).unwrap(),
            Incoming::Event(Event::MessageUpdate(AssistantDelta::Other))
        ));
    }

    #[test]
    fn thinking_names_match_the_wire_format() {
        for level in ThinkingLevel::ALL {
            assert_eq!(serde_json::to_value(level).unwrap(), level.as_str());
            assert_eq!(ThinkingLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(ThinkingLevel::parse("extreme"), None);
    }
}
