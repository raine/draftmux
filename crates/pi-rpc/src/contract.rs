//! Pi behind the shared agent contract ([`draftmux_agent`]): [`PiAgent`]
//! wraps [`Agent`], and its events and `get_messages` history are read
//! into the contract's types here, so pi's message JSON stays in this
//! crate.

use std::time::Duration;

use draftmux_agent as contract;
use futures::{FutureExt, StreamExt, future};
use serde_json::Value;

use crate::{
    Agent, AgentEvent, AssistantDelta, Disposition, Error, Event, LaunchSpec, State, ThinkingLevel,
};

/// The extension's hidden context message, added after each Draftmux prompt
/// whose context changed (pi-extension/README.md). The Draftmux part of a
/// conversation starts with the prompt before the first one.
const CONTEXT_TYPE: &str = "draftmux-context";

/// A draft's pi [`Agent`] as a [`contract::Agent`].
pub struct PiAgent(Agent);

impl PiAgent {
    /// Like [`Agent::new`], with the events read into the contract's.
    pub fn new(
        spec: LaunchSpec,
        idle_timeout: Duration,
        request_timeout: Duration,
    ) -> (Self, contract::Events) {
        let (agent, events) = Agent::new(spec, idle_timeout, request_timeout);
        let events = events
            .flat_map(|event| futures::stream::iter(events_from(event)))
            .boxed();
        (Self(agent), events)
    }
}

impl contract::Agent for PiAgent {
    fn capabilities(&self) -> contract::Capabilities {
        contract::Capabilities {
            model_switching: true,
            default_thinking: false,
        }
    }

    fn focus(&self) -> Result<(), contract::Error> {
        self.0.focus().map_err(error)
    }

    fn blur(&self) {
        self.0.blur();
    }

    fn prompt(
        &self,
        text: String,
    ) -> future::BoxFuture<'static, Result<contract::Prompted, contract::Error>> {
        self.0
            .prompt(text)
            .map(|reply| {
                reply.map_err(error).map(|disposition| match disposition {
                    Disposition::Started | Disposition::Queued => contract::Prompted::Running,
                    Disposition::Handled => contract::Prompted::Handled,
                })
            })
            .boxed()
    }

    fn abort(&self) -> future::BoxFuture<'static, Result<(), contract::Error>> {
        self.0.abort().map(|reply| reply.map_err(error)).boxed()
    }

    fn history(&self) -> future::BoxFuture<'static, Result<contract::History, contract::Error>> {
        self.0
            .get_messages()
            .map(|messages| messages.map(|messages| history(&messages)).map_err(error))
            .boxed()
    }

    fn available_models(
        &self,
    ) -> future::BoxFuture<'static, Result<Vec<contract::Model>, contract::Error>> {
        self.0
            .get_available_models()
            .map(|models| {
                models
                    .map_err(error)
                    .map(|models| models.into_iter().map(model_from).collect())
            })
            .boxed()
    }

    fn switch_model(
        &self,
        model: contract::Model,
    ) -> future::BoxFuture<'static, Result<contract::State, contract::Error>> {
        self.0
            .switch_model(model.provider, model.id)
            .map(|state| state.map(state_from).map_err(error))
            .boxed()
    }

    fn thinking_levels(&self) -> future::BoxFuture<'static, Result<Vec<String>, contract::Error>> {
        self.0
            .get_available_thinking_levels()
            .map(|levels| {
                levels
                    .map(|levels| levels.iter().map(|l| l.as_str().to_owned()).collect())
                    .map_err(error)
            })
            .boxed()
    }

    fn set_thinking(
        &self,
        level: Option<String>,
    ) -> future::BoxFuture<'static, Result<contract::State, contract::Error>> {
        let Some(level) = level.as_deref().and_then(ThinkingLevel::parse) else {
            return future::ready(Err(contract::Error {
                kind: contract::ErrorKind::Other,
                message: format!("pi has no thinking level {level:?}"),
            }))
            .boxed();
        };
        self.0
            .change_thinking_level(level)
            .map(|state| state.map(state_from).map_err(error))
            .boxed()
    }

    fn compact(
        &self,
        instructions: String,
    ) -> future::BoxFuture<'static, Result<(), contract::Error>> {
        self.0
            .compact(instructions)
            .map(|reply| reply.map_err(error))
            .boxed()
    }
}

fn error(error: Error) -> contract::Error {
    contract::Error {
        kind: match error {
            Error::Stopped => contract::ErrorKind::Stopped,
            _ => contract::ErrorKind::Other,
        },
        message: error.to_string(),
    }
}

fn state_from(state: State) -> contract::State {
    contract::State {
        model: state.model.map(model_from),
        thinking: Some(state.thinking_level.as_str().to_owned()),
        running: None,
    }
}

fn model_from(model: crate::Model) -> contract::Model {
    contract::Model {
        provider: model.provider,
        id: model.id,
    }
}

/// The contract's events for one of the agent's: none when the chat
/// doesn't show it.
fn events_from(event: AgentEvent) -> Vec<contract::Event> {
    use contract::Event as E;
    if let AgentEvent::Pi(Event::MessageEnd { message }) = &event
        && role(message) == "assistant"
    {
        let ended = E::AssistantEnded {
            text: content_text(&message["content"], "\n\n"),
            stop: stop(message),
        };
        return match context_tokens(message) {
            Some(tokens) => vec![E::Context(tokens), ended],
            None => vec![ended],
        };
    }
    let event = match event {
        AgentEvent::Ready(state) => E::Ready(state_from(state)),
        AgentEvent::Crashed { error, failed_turn } => E::Crashed {
            error,
            failed_prompt: failed_turn,
        },
        AgentEvent::Pi(event) => {
            let draft_write = event.is_draft_write();
            match event {
                Event::AgentStart => E::RunStarted,
                Event::AgentSettled { aborted } => E::RunSettled { aborted },
                Event::AutoRetryStart {
                    attempt,
                    max_attempts,
                    error_message,
                    ..
                } => E::Retrying {
                    attempt,
                    max_attempts,
                    error: error_message,
                },
                Event::MessageStart { message } => match role(&message) {
                    "user" => E::UserMessage(content_text(&message["content"], "\n")),
                    "assistant" => E::AssistantStarted,
                    _ => return Vec::new(),
                },
                Event::MessageUpdate(AssistantDelta::TextStart { .. }) => E::AssistantBlock,
                Event::MessageUpdate(AssistantDelta::TextDelta { delta, .. }) => {
                    E::AssistantText(delta)
                }
                Event::CompactionStart { .. } => E::CompactionStarted,
                // pi compacts again once a retried request settles.
                Event::CompactionEnd {
                    will_retry: true,
                    result: None,
                    ..
                } => return Vec::new(),
                Event::CompactionEnd {
                    result,
                    aborted,
                    error_message,
                    ..
                } => E::CompactionEnded {
                    compacted: result.is_some(),
                    // pi's `estimatedTokensAfter` counts characters, not
                    // what the model reports; like pi, the size is unknown
                    // until the next reply.
                    tokens: None,
                    error: error_message.filter(|_| !aborted).map(|error| {
                        error
                            .strip_prefix("Compaction failed: ")
                            .map_or(error.clone(), str::to_owned)
                    }),
                },
                Event::ToolExecutionStart {
                    tool_call_id,
                    tool_name,
                    args,
                } => E::ToolStarted(tool_call(tool_call_id, &tool_name, &args)),
                Event::ToolExecutionUpdate {
                    tool_call_id,
                    partial_result,
                    ..
                } => E::ToolOutput {
                    id: tool_call_id,
                    output: content_text(&partial_result["content"], "\n"),
                },
                Event::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    is_error,
                    duration_ms,
                    ..
                } => E::ToolFinished {
                    id: tool_call_id,
                    result: tool_result(&result, is_error, duration_ms),
                    draft_write,
                },
                _ => return Vec::new(),
            }
        }
    };
    vec![event]
}

/// The conversation's size an assistant message's request reported, as pi
/// counts it; none for a failed or stopped request, which reports none.
fn context_tokens(message: &Value) -> Option<u64> {
    let usage = &message["usage"];
    let total = usage["totalTokens"].as_u64().filter(|&total| total > 0);
    let tokens = total.unwrap_or_else(|| {
        ["input", "output", "cacheRead", "cacheWrite"]
            .iter()
            .filter_map(|key| usage[key].as_u64())
            .sum()
    });
    (tokens > 0).then_some(tokens)
}

/// The Draftmux part of a session's messages (pi's `AgentMessage` JSON,
/// as `get_messages` returns them): from the prompt before the first
/// context message on. Without one, it is all source conversation.
fn history(messages: &[Value]) -> contract::History {
    let context = history_context(messages);
    let start = messages
        .iter()
        .position(|message| role(message) == "custom" && message["customType"] == CONTEXT_TYPE)
        .map_or(messages.len(), |context| {
            messages[..context]
                .iter()
                .rposition(|message| role(message) == "user")
                .unwrap_or(context)
        });
    let earlier = messages[..start]
        .iter()
        .filter(|message| matches!(role(message), "user" | "assistant"))
        .count();
    let messages = messages[start..]
        .iter()
        .filter_map(|message| {
            Some(match role(message) {
                "user" => contract::Message::User(content_text(&message["content"], "\n")),
                "assistant" => contract::Message::Assistant {
                    text: content_text(&message["content"], "\n\n"),
                    tool_calls: message["content"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .filter(|block| block["type"] == "toolCall")
                        .map(|call| {
                            tool_call(
                                call["id"].as_str().unwrap_or_default().to_owned(),
                                call["name"].as_str().unwrap_or_default(),
                                &call["arguments"],
                            )
                        })
                        .collect(),
                    stop: stop(message),
                },
                "toolResult" => contract::Message::ToolResult {
                    id: message["toolCallId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    result: tool_result(
                        message,
                        message["isError"] == true,
                        message["durationMs"].as_u64(),
                    ),
                },
                "compactionSummary" => contract::Message::Compacted,
                _ => return None,
            })
        })
        .collect();
    contract::History {
        earlier,
        messages,
        context,
    }
}

/// The size the latest assistant message's request reported, unless a
/// compaction summary follows it: what the model sees changed since.
fn history_context(messages: &[Value]) -> Option<u64> {
    messages
        .iter()
        .rev()
        .find_map(|message| match role(message) {
            "compactionSummary" => Some(None),
            "assistant" => context_tokens(message).map(Some),
            _ => None,
        })?
}

fn role(message: &Value) -> &str {
    message["role"].as_str().unwrap_or_default()
}

/// The text blocks of a message's `content` (a string or an array of
/// blocks), joined by `separator`.
fn content_text(content: &Value, separator: &str) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join(separator),
        _ => String::new(),
    }
}

/// Why an assistant message ended, from its `stopReason`.
fn stop(message: &Value) -> contract::Stop {
    match message["stopReason"].as_str() {
        Some("error") => contract::Stop::Error(message["errorMessage"].as_str().map(str::to_owned)),
        Some("aborted") => contract::Stop::Aborted,
        _ => contract::Stop::Completed,
    }
}

/// A call of one of pi's built-in tools, with the arguments its row shows.
fn tool_call(id: String, name: &str, args: &Value) -> contract::ToolCall {
    use contract::ToolKind as K;
    let arg = |key: &str| args[key].as_str().unwrap_or_default().to_owned();
    let kind = match name {
        "bash" => K::Bash {
            command: arg("command"),
        },
        "read" => K::Read { path: arg("path") },
        "edit" => K::Edit { path: arg("path") },
        "write" => K::Write {
            path: arg("path"),
            content: arg("content"),
        },
        "grep" => K::Grep {
            pattern: arg("pattern"),
        },
        "find" => K::Find {
            pattern: arg("pattern"),
        },
        "ls" => K::Ls { path: arg("path") },
        name => K::Other {
            name: name.to_owned(),
        },
    };
    contract::ToolCall { id, kind }
}

/// A tool result: its `content` text and the patch an `edit` reports in
/// `details`.
fn tool_result(result: &Value, is_error: bool, duration_ms: Option<u64>) -> contract::ToolResult {
    contract::ToolResult {
        output: content_text(&result["content"], "\n"),
        is_error,
        duration: duration_ms.map(Duration::from_millis),
        patch: result["details"]["patch"].as_str().map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::path::Path;
    use std::time::Instant;

    use draftmux_agent::transcript::{self, Item, Target, ToolStatus, Transcript};

    use super::*;
    use crate::protocol::{Incoming, parse_line};

    /// The chat's path display (`chat::display_path`), so snapshots show
    /// targets as the chat does.
    fn display_path(path: &str, cwd: &Path, draft_path: &Path, home: &Path) -> String {
        let absolute = match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => cwd.join(path),
        };
        if absolute == draft_path {
            return "draft.md".into();
        }
        match absolute.strip_prefix(cwd) {
            Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
            _ => match absolute.strip_prefix(home) {
                Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => absolute.display().to_string(),
            },
        }
    }

    fn tool_name(kind: &contract::ToolKind) -> &str {
        use contract::ToolKind as K;
        match kind {
            K::Bash { .. } => "bash",
            K::Read { .. } => "read",
            K::Edit { .. } => "edit",
            K::Write { .. } => "write",
            K::Grep { .. } => "grep",
            K::Find { .. } => "find",
            K::Ls { .. } => "ls",
            K::Other { name } => name,
        }
    }

    /// Everything the chat shows of `transcript`, one item per line.
    fn snapshot(t: &Transcript) -> String {
        let mut s = String::new();
        writeln!(s, "busy={}", t.busy).unwrap();
        for (i, item) in t.items.iter().enumerate() {
            match item {
                Item::User { text, quotes } => writeln!(s, "{i} user {text:?} {quotes:?}"),
                Item::Assistant { doc } => writeln!(s, "{i} assistant {doc:?}"),
                Item::Failure { error, retry } => writeln!(s, "{i} failure {error:?} {retry:?}"),
                Item::Notice(text) => writeln!(s, "{i} notice {text:?}"),
                Item::Tool(tool) => {
                    let status = match tool.status {
                        ToolStatus::Running { .. } => "running".to_owned(),
                        ToolStatus::Stopped => "stopped".to_owned(),
                        ToolStatus::Done { is_error, .. } => format!("done error={is_error}"),
                    };
                    let target = match tool.target() {
                        Target::Path(path) => display_path(
                            path,
                            Path::new("/tmp/draft"),
                            Path::new("/tmp/draft/draft/draft.md"),
                            Path::new("/home/me"),
                        ),
                        Target::Text(text) => text.to_owned(),
                    };
                    writeln!(
                        s,
                        "{i} tool {} {:?} {} {target:?} [{status}] dur={:?} err={} running={} stats={:?} out={:?} diff={:?} body={:?} expanded={}",
                        tool.id,
                        tool_name(&tool.kind),
                        tool.verb(),
                        tool.duration(),
                        tool.is_error(),
                        tool.running(),
                        tool.stats,
                        tool.output,
                        tool.diff(),
                        tool.body(),
                        tool.expanded,
                    )
                }
            }
            .unwrap();
        }
        for i in 0..t.items.len() {
            if let Some(retry) = t.retry_at(i) {
                writeln!(s, "retry_at {i} {retry:?}").unwrap();
            }
        }
        s
    }

    /// The transcript after the user sent `prompt` and after each event
    /// of `script` that changed it. Draft writes leave `written`.
    fn live(name: &str, prompt: &str, draft: &str, written: &str, script: &str) -> String {
        let mut t = Transcript::default();
        let now = Instant::now();
        let mut out = format!("# live {name}\n");
        t.sent(prompt.into(), draft.into());
        let mut last = snapshot(&t);
        out.push_str("## sent\n");
        out.push_str(&last);
        // The pi script's stdout lines, as the agent reports them.
        let events = script
            .lines()
            .filter(|line| line.starts_with('{'))
            .flat_map(|line| match parse_line(line).unwrap() {
                Incoming::Event(event) => events_from(AgentEvent::Pi(event)),
                _ => Vec::new(),
            });
        for event in events {
            let write = matches!(
                event,
                contract::Event::ToolFinished {
                    draft_write: true,
                    ..
                }
            );
            let old_len = t.items.len();
            t.apply(
                &event,
                write.then_some(written),
                now + Duration::from_millis(7),
            );
            let change = t.take_change(old_len);
            let snap = snapshot(&t);
            if snap != last || change.is_some() {
                writeln!(out, "## change {change:?}").unwrap();
                out.push_str(&snap);
                last = snap;
            }
        }
        out
    }

    fn crash() -> String {
        let mut t = Transcript::default();
        let now = Instant::now();
        t.sent("go".into(), String::new());
        let events = [
            AgentEvent::Pi(Event::ToolExecutionStart {
                tool_call_id: "1".into(),
                tool_name: "bash".into(),
                args: serde_json::json!({"command": "sleep 9"}),
            }),
            AgentEvent::Crashed {
                error: "pi exited".into(),
                failed_turn: Some("go".into()),
            },
        ];
        for event in events.into_iter().flat_map(events_from) {
            t.apply(&event, None, now);
        }
        format!("# crash\n{}", snapshot(&t))
    }

    fn shown_history(name: &str, messages: &[Value]) -> String {
        let history = history(messages);
        let mut t = Transcript::default();
        t.prepend(transcript::history(&history.messages));
        format!(
            "# history {name} earlier={}\n{}",
            history.earlier,
            snapshot(&t)
        )
    }

    /// The whole conversation, as pi printed it at the end of a run.
    fn agent_end(script: &str) -> Vec<Value> {
        let line = script
            .lines()
            .find(|line| line.starts_with(r#"{"type":"agent_end""#))
            .unwrap();
        let value: Value = serde_json::from_str(line).unwrap();
        value["messages"].as_array().unwrap().clone()
    }

    const TEXT: &str = include_str!("../tests/fixtures/text.jsonl");
    const TOOL: &str = include_str!("../tests/fixtures/tool.jsonl");
    const ABORT: &str = include_str!("../tests/fixtures/abort.jsonl");

    /// The recorded and written runs and histories show what the chat
    /// showed when it read pi's events itself (`transcript/expected.txt`).
    #[test]
    fn transcripts_match_the_pi_chat() {
        let events = include_str!("../tests/fixtures/transcript/events.jsonl");
        let messages: Vec<Value> =
            serde_json::from_str(include_str!("../tests/fixtures/transcript/history.json"))
                .unwrap();
        let numbers: String = (1..=200).map(|n| format!("{n}\n")).collect();
        let quoted = "<draftmux-selection lines=\"2-2\" bytes=\"4-7\">\ntwo\n</draftmux-selection>\nThe user selected the text above in the draft file (line numbers from 1, byte offsets from 0). When their message says \"this\", \"the selection\" or \"the selected part\", it means that text: change only that part of the draft unless they ask otherwise, and if the same text appears elsewhere, change the one at these lines.\n\nshorter";
        let actual = [
            live(
                "text",
                "Reply with exactly the word: pong. Do not use tools.",
                "",
                "",
                TEXT,
            ),
            live(
                "tool",
                "Use the edit tool to change 'world' to 'there' in the draft file. Then reply 'done'.",
                "Hello world draft.\n",
                "Hello there draft.\n",
                TOOL,
            ),
            live(
                "abort",
                "Count slowly from 1 to 200, one number per line.",
                "Hello world draft.\n",
                &numbers,
                ABORT,
            ),
            live("events", "go", "a\nb\n", "one\n2\nthree\nfour", events),
            live(
                "quoted",
                quoted,
                "one\ntwo\nthree",
                "",
                &format!(
                    "{{\"type\":\"agent_start\"}}\n{}\n{{\"type\":\"agent_settled\",\"aborted\":true}}",
                    serde_json::json!({"type": "message_start", "message": {"role": "user", "content": [{"type": "text", "text": quoted}]}})
                ),
            ),
            crash(),
            shown_history("sample", &messages),
            shown_history("sample-before-marker", &messages[..5]),
            shown_history("empty", &[]),
            shown_history("text", &agent_end(TEXT)),
            shown_history("tool", &agent_end(TOOL)),
            shown_history("abort", &agent_end(ABORT)),
        ]
        .join("\n");
        let expected = include_str!("../tests/fixtures/transcript/expected.txt");
        if actual != expected {
            let diff = similar_lines(expected, &actual);
            panic!("transcripts differ:\n{diff}");
        }
    }

    #[test]
    fn errors_keep_pis_message() {
        let stopped = error(Error::Stopped);
        assert_eq!(stopped.kind, contract::ErrorKind::Stopped);
        assert_eq!(stopped.to_string(), "pi was stopped before answering");
        let rejected = error(Error::Rejected("No API key for anthropic".into()));
        assert_eq!(rejected.kind, contract::ErrorKind::Other);
        assert_eq!(rejected.to_string(), "No API key for anthropic");
    }

    /// The first differing lines of two snapshots.
    fn similar_lines(expected: &str, actual: &str) -> String {
        expected
            .lines()
            .zip(actual.lines())
            .enumerate()
            .filter(|(_, (e, a))| e != a)
            .take(5)
            .map(|(n, (e, a))| format!("line {}:\n- {e}\n+ {a}\n", n + 1))
            .collect::<String>()
            + &format!(
                "({} expected lines, {} actual)",
                expected.lines().count(),
                actual.lines().count()
            )
    }

    #[test]
    fn assistant_usage_reports_the_context() {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "Done."}],
            "usage": {"input": 3, "output": 40, "cacheRead": 80000, "cacheWrite": 2000, "totalTokens": 82043},
            "stopReason": "stop",
        });
        let events = events_from(AgentEvent::Pi(Event::MessageEnd {
            message: message.clone(),
        }));
        assert_eq!(events[0], contract::Event::Context(82043));
        assert!(matches!(events[1], contract::Event::AssistantEnded { .. }));

        let summary =
            serde_json::json!({"role": "compactionSummary", "summary": "s", "tokensBefore": 82043});
        assert_eq!(history(std::slice::from_ref(&message)).context, Some(82043));
        assert_eq!(history(&[message.clone(), summary.clone()]).context, None);
        assert_eq!(history(&[summary, message]).context, Some(82043));
    }

    #[test]
    fn compaction_end_reports_success_or_the_error() {
        let ended = |result: Option<Value>, error: Option<&str>| {
            events_from(AgentEvent::Pi(Event::CompactionEnd {
                reason: "manual".into(),
                result,
                aborted: false,
                will_retry: false,
                error_message: error.map(Into::into),
            }))
        };
        assert_eq!(
            ended(
                Some(
                    serde_json::json!({"summary": "s", "tokensBefore": 90000, "estimatedTokensAfter": 21000})
                ),
                None
            ),
            vec![contract::Event::CompactionEnded {
                compacted: true,
                tokens: None,
                error: None,
            }]
        );
        assert_eq!(
            ended(None, Some("Compaction failed: Nothing to compact")),
            vec![contract::Event::CompactionEnded {
                compacted: false,
                tokens: None,
                error: Some("Nothing to compact".into()),
            }]
        );
    }
}
