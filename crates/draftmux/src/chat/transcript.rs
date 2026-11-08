//! What the chat pane shows for one draft, built from pi's events and its
//! session history, independent of the UI.

use std::path::Path;
use std::time::{Duration, Instant};

use draftmux_pi_rpc::{AgentEvent, AssistantDelta, Event};
use markup::Document;
use serde_json::Value;

/// The extension's hidden context message, added after each Draftmux prompt
/// whose context changed (pi-extension/README.md).
const CONTEXT_TYPE: &str = "draftmux-context";
/// Lines of tool output kept for the expanded row.
const MAX_OUTPUT_LINES: usize = 400;

pub enum Item {
    User {
        text: String,
    },
    Assistant {
        text: String,
        doc: Document,
        streaming: bool,
    },
    Tool(Box<Tool>),
    /// A turn that failed, or that the user stopped (`error` is `None`).
    /// `retry` is the prompt to send again.
    Failure {
        error: Option<String>,
        retry: Option<String>,
    },
    Notice(String),
}

pub struct Tool {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub status: ToolStatus,
    /// The result's text, or the partial output while running.
    pub output: String,
    pub details: Value,
    /// Lines added and removed by an `edit` or `write`; removals of a
    /// `write` are known only when the draft's previous text is.
    pub stats: Option<(usize, Option<usize>)>,
    pub expanded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Running {
        since: Instant,
    },
    Done {
        is_error: bool,
        duration: Option<Duration>,
    },
    /// The run ended before the tool reported a result.
    Stopped,
}

/// One line of an `edit` row's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
}

pub struct Transcript {
    pub items: Vec<Item>,
    /// Between sending a prompt (or pi starting a run) and the run settling.
    pub busy: bool,
    /// The prompt of the latest turn, offered again by a failure's retry.
    last_prompt: Option<String>,
    /// An assistant error pi may still retry; shown once the run settles.
    pending_error: Option<String>,
    streaming: Option<usize>,
    /// The draft's text before the agent's next write.
    draft_text: Option<String>,
    /// The first item changed since [`Transcript::take_change`], for the
    /// list to remeasure from.
    changed_from: Option<usize>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            busy: false,
            last_prompt: None,
            pending_error: None,
            streaming: None,
            draft_text: None,
            changed_from: None,
        }
    }

    /// The first changed index and the item count before the changes, then
    /// forgets them.
    pub fn take_change(&mut self, old_len: usize) -> Option<(usize, usize)> {
        self.changed_from
            .take()
            .map(|from| (from.min(old_len), old_len))
    }

    fn touch(&mut self, index: usize) {
        self.changed_from = Some(self.changed_from.map_or(index, |from| from.min(index)));
    }

    fn push(&mut self, item: Item) {
        self.touch(self.items.len());
        self.items.push(item);
    }

    /// Puts the session's earlier messages above what is shown.
    pub fn prepend(&mut self, items: Vec<Item>) {
        if items.is_empty() {
            return;
        }
        self.streaming = self.streaming.map(|index| index + items.len());
        self.items.splice(0..0, items);
        self.touch(0);
    }

    /// The user sent `text`; `draft_text` is the draft as saved for pi.
    pub fn sent(&mut self, text: String, draft_text: String) {
        self.busy = true;
        self.last_prompt = Some(text.clone());
        self.draft_text = Some(draft_text);
        self.push(Item::User { text });
    }

    /// pi didn't take the prompt.
    pub fn send_failed(&mut self, error: String) {
        self.busy = false;
        self.push(Item::Failure {
            error: Some(error),
            retry: self.last_prompt.clone(),
        });
    }

    /// pi took the prompt without starting a run.
    pub fn handled(&mut self) {
        self.busy = false;
    }

    /// Whether the failure at `index` can be retried now: only the latest
    /// item, while nothing runs.
    pub fn retry_at(&self, index: usize) -> Option<&str> {
        match self.items.get(index) {
            Some(Item::Failure {
                retry: Some(retry), ..
            }) if !self.busy && index + 1 == self.items.len() => Some(retry),
            _ => None,
        }
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(Item::Tool(tool)) = self.items.get_mut(index) {
            tool.expanded = !tool.expanded;
            self.touch(index);
        }
    }

    /// Applies one agent event. `written` is the draft's new text when the
    /// event is a draft write ([`Event::is_draft_write`]).
    pub fn apply(&mut self, event: &AgentEvent, written: Option<&str>, now: Instant) {
        match event {
            AgentEvent::Pi(event) => self.apply_pi(event, written, now),
            AgentEvent::Ready(_) => {}
            AgentEvent::Crashed { error, failed_turn } => {
                self.end_run();
                self.push(Item::Failure {
                    error: Some(error.clone()),
                    retry: failed_turn.clone(),
                });
            }
        }
    }

    fn apply_pi(&mut self, event: &Event, written: Option<&str>, now: Instant) {
        match event {
            Event::AgentStart => self.busy = true,
            Event::AgentSettled { aborted } => {
                self.end_run();
                let error = self.pending_error.take();
                if *aborted || error.is_some() {
                    self.push(Item::Failure {
                        error: error.filter(|_| !aborted),
                        retry: self.last_prompt.clone(),
                    });
                }
            }
            Event::AutoRetryStart {
                attempt,
                max_attempts,
                error_message,
                ..
            } => {
                self.pending_error = None;
                self.push(Item::Notice(format!(
                    "{error_message} Retrying ({attempt} of {max_attempts})…"
                )));
            }
            Event::MessageStart { message } => match role(message) {
                "user" => {
                    let text = content_text(&message["content"], "\n");
                    let echoed = matches!(
                        self.items.last(),
                        Some(Item::User { text: last }) if *last == text
                    );
                    if !echoed {
                        // A run an extension started, or history from elsewhere.
                        self.last_prompt = Some(text.clone());
                        self.push(Item::User { text });
                    }
                }
                "assistant" => {
                    self.streaming = Some(self.items.len());
                    self.push(Item::Assistant {
                        text: String::new(),
                        doc: Document::default(),
                        streaming: true,
                    });
                }
                _ => {}
            },
            Event::MessageUpdate(delta) => {
                let Some(index) = self.streaming else { return };
                let Some(Item::Assistant { text, doc, .. }) = self.items.get_mut(index) else {
                    return;
                };
                match delta {
                    AssistantDelta::TextStart { .. } if !text.is_empty() => text.push_str("\n\n"),
                    AssistantDelta::TextDelta { delta, .. } => text.push_str(delta),
                    _ => return,
                }
                *doc = markup::github::parse(text);
                self.touch(index);
            }
            Event::MessageEnd { message } if role(message) == "assistant" => {
                if let Some(index) = self.streaming.take() {
                    let text = content_text(&message["content"], "\n\n");
                    if text.trim().is_empty() {
                        self.items.remove(index);
                    } else {
                        self.items[index] = Item::Assistant {
                            doc: markup::github::parse(&text),
                            text,
                            streaming: false,
                        };
                    }
                    self.touch(index);
                }
                if message["stopReason"] == "error" {
                    self.pending_error = Some(error_message(message));
                }
            }
            Event::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => self.push(Item::Tool(Box::new(Tool {
                id: tool_call_id.clone(),
                name: tool_name.clone(),
                args: args.clone(),
                status: ToolStatus::Running { since: now },
                output: String::new(),
                details: Value::Null,
                stats: None,
                expanded: false,
            }))),
            Event::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                if let Some(index) = self.tool_index(tool_call_id) {
                    let Item::Tool(tool) = &mut self.items[index] else {
                        unreachable!()
                    };
                    tool.output = truncate_lines(&content_text(&partial_result["content"], "\n"));
                    self.touch(index);
                }
            }
            Event::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                let Some(index) = self.tool_index(tool_call_id) else {
                    return;
                };
                let previous = written.and_then(|_| self.draft_text.as_deref().map(line_count));
                let Item::Tool(tool) = &mut self.items[index] else {
                    unreachable!()
                };
                let duration = duration_ms
                    .map(Duration::from_millis)
                    .or(match tool.status {
                        ToolStatus::Running { since } => Some(now.saturating_duration_since(since)),
                        _ => None,
                    });
                tool.finish(result, *is_error, duration);
                if tool.name == "write"
                    && let Some((_, removed)) = &mut tool.stats
                {
                    *removed = previous;
                }
                if let Some(written) = written {
                    self.draft_text = Some(written.to_owned());
                }
                self.touch(index);
            }
            _ => {}
        }
    }

    /// The run ended: nothing streams or runs any more.
    fn end_run(&mut self) {
        self.busy = false;
        if let Some(index) = self.streaming.take() {
            if let Some(Item::Assistant { streaming, .. }) = self.items.get_mut(index) {
                *streaming = false;
            }
            self.touch(index);
        }
        for index in 0..self.items.len() {
            if let Item::Tool(tool) = &mut self.items[index]
                && matches!(tool.status, ToolStatus::Running { .. })
            {
                tool.status = ToolStatus::Stopped;
                self.touch(index);
            }
        }
    }

    fn tool_index(&self, id: &str) -> Option<usize> {
        self.items
            .iter()
            .rposition(|item| matches!(item, Item::Tool(tool) if tool.id == id))
    }
}

impl Tool {
    fn finish(&mut self, result: &Value, is_error: bool, duration: Option<Duration>) {
        self.status = ToolStatus::Done { is_error, duration };
        self.output = truncate_lines(&content_text(&result["content"], "\n"));
        self.details = result.get("details").cloned().unwrap_or_default();
        if !is_error {
            self.stats = match self.name.as_str() {
                "edit" => self.patch().map(patch_stats),
                "write" => Some((line_count(self.str_arg("content")), None)),
                _ => None,
            };
        }
    }

    fn str_arg(&self, key: &str) -> &str {
        self.args[key].as_str().unwrap_or_default()
    }

    fn patch(&self) -> Option<&str> {
        self.details["patch"].as_str()
    }

    pub fn running(&self) -> bool {
        matches!(self.status, ToolStatus::Running { .. })
    }

    pub fn duration(&self) -> Option<Duration> {
        match self.status {
            ToolStatus::Done { duration, .. } => duration,
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self.status, ToolStatus::Done { is_error: true, .. })
    }

    /// The row's verb: present tense while running, past tense after.
    pub fn verb(&self) -> &'static str {
        let (running, done) = match self.name.as_str() {
            "bash" => ("Running", "Ran"),
            "read" => ("Reading", "Read"),
            "edit" => ("Editing", "Edited"),
            "write" => ("Writing", "Wrote"),
            "grep" => ("Searching", "Searched"),
            "find" => ("Finding", "Found"),
            "ls" => ("Listing", "Listed"),
            _ => ("Using", "Used"),
        };
        if self.running() { running } else { done }
    }

    /// The row's target: the command, or the path relative to `cwd`.
    pub fn target(&self, cwd: &Path, draft_path: &Path, home: Option<&Path>) -> String {
        match self.name.as_str() {
            "bash" => self
                .str_arg("command")
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
            "grep" | "find" => self.str_arg("pattern").to_owned(),
            "read" | "edit" | "write" | "ls" => {
                display_path(self.str_arg("path"), cwd, draft_path, home)
            }
            name => name.to_owned(),
        }
    }

    /// What the expanded `edit` row shows: removed lines before added ones,
    /// with one line of context.
    pub fn diff(&self) -> Option<Vec<DiffLine>> {
        (self.name == "edit" && !self.is_error()).then(|| self.patch().map(patch_lines))?
    }

    /// What the other expanded rows show, line by line.
    pub fn body(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.name == "bash" {
            lines.push(format!("$ {}", self.str_arg("command")));
        }
        let text = match self.name.as_str() {
            "write" if !self.is_error() => truncate_lines(self.str_arg("content")),
            _ => self.output.clone(),
        };
        lines.extend(text.trim_end_matches('\n').lines().map(str::to_owned));
        if lines.is_empty() && !self.running() {
            lines.push("No output".into());
        }
        lines
    }
}

/// The chat items for the session's messages from the first Draftmux prompt
/// on, and how many messages came before it (the source conversation).
pub fn history(messages: &[Value]) -> (Vec<Item>, usize) {
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

    let mut transcript = Transcript::new();
    for message in &messages[start..] {
        match role(message) {
            "user" => {
                let text = content_text(&message["content"], "\n");
                transcript.last_prompt = Some(text.clone());
                transcript.push(Item::User { text });
            }
            "assistant" => {
                let content = message["content"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let text = content_text(&message["content"], "\n\n");
                if !text.trim().is_empty() {
                    transcript.push(Item::Assistant {
                        doc: markup::github::parse(&text),
                        text,
                        streaming: false,
                    });
                }
                for call in content.iter().filter(|block| block["type"] == "toolCall") {
                    transcript.push(Item::Tool(Box::new(Tool {
                        id: call["id"].as_str().unwrap_or_default().to_owned(),
                        name: call["name"].as_str().unwrap_or_default().to_owned(),
                        args: call["arguments"].clone(),
                        status: ToolStatus::Stopped,
                        output: String::new(),
                        details: Value::Null,
                        stats: None,
                        expanded: false,
                    })));
                }
                let failure = match message["stopReason"].as_str() {
                    Some("error") => Some(Some(error_message(message))),
                    Some("aborted") => Some(None),
                    _ => None,
                };
                if let Some(error) = failure {
                    transcript.push(Item::Failure {
                        error,
                        retry: transcript.last_prompt.clone(),
                    });
                }
            }
            "toolResult" => {
                let id = message["toolCallId"].as_str().unwrap_or_default();
                if let Some(index) = transcript.tool_index(id) {
                    let Item::Tool(tool) = &mut transcript.items[index] else {
                        unreachable!()
                    };
                    let duration = message["durationMs"].as_u64().map(Duration::from_millis);
                    tool.finish(message, message["isError"] == true, duration);
                }
            }
            "compactionSummary" => {
                transcript.push(Item::Notice("Earlier messages were compacted".into()));
            }
            _ => {}
        }
    }
    (transcript.items, earlier)
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

fn error_message(message: &Value) -> String {
    message["errorMessage"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or("The model returned an error")
        .to_owned()
}

fn line_count(text: &str) -> usize {
    text.lines().count()
}

fn truncate_lines(text: &str) -> String {
    let mut lines = text.lines();
    let mut kept: Vec<&str> = lines.by_ref().take(MAX_OUTPUT_LINES).collect();
    if lines.next().is_some() {
        kept.push("…");
    }
    kept.join("\n")
}

/// Lines added and removed by a unified patch.
fn patch_stats(patch: &str) -> (usize, Option<usize>) {
    let lines = patch_lines(patch);
    let added = lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Added(_)))
        .count();
    let removed = lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Removed(_)))
        .count();
    (added, Some(removed))
}

/// The hunk lines of a unified patch, keeping one line of context on either
/// side of a change.
fn patch_lines(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut in_hunk = false;
    for line in patch.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (kind, rest) = line.split_at(line.len().min(1));
        lines.push(match kind {
            "+" => DiffLine::Added(rest.to_owned()),
            "-" => DiffLine::Removed(rest.to_owned()),
            " " | "" => DiffLine::Context(rest.to_owned()),
            // "\ No newline at end of file"
            _ => continue,
        });
    }
    let changed = |index: usize| {
        lines
            .get(index)
            .is_some_and(|line| !matches!(line, DiffLine::Context(_)))
    };
    (0..lines.len())
        .filter(|&i| changed(i) || (i > 0 && changed(i - 1)) || changed(i + 1))
        .map(|i| lines[i].clone())
        .collect()
}

/// `path` as the model wrote it, shown relative to `cwd`; the draft itself
/// is `draft.md`.
fn display_path(path: &str, cwd: &Path, draft_path: &Path, home: Option<&Path>) -> String {
    let absolute = match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => cwd.join(path),
    };
    if absolute == draft_path {
        return "draft.md".into();
    }
    match absolute.strip_prefix(cwd) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => crate::drafts::tilde(&absolute, home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent events pi printed in a recorded fake-pi script.
    fn recorded(script: &str) -> Vec<AgentEvent> {
        script
            .lines()
            .filter(|line| line.starts_with('{'))
            .filter_map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                match value["type"].as_str().unwrap() {
                    "response"
                    | "extension_ui_request"
                    | "message_update"
                    | "agent_end"
                    | "turn_end" => None,
                    _ => Some(AgentEvent::Pi(serde_json::from_value(value).unwrap())),
                }
            })
            .collect()
    }

    fn replay(transcript: &mut Transcript, events: &[AgentEvent], written: &str) {
        let now = Instant::now();
        for event in events {
            let write = matches!(event, AgentEvent::Pi(event) if event.is_draft_write());
            transcript.apply(event, write.then_some(written), now);
        }
    }

    fn tools(transcript: &Transcript) -> Vec<&Tool> {
        transcript
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Tool(tool) => Some(&**tool),
                _ => None,
            })
            .collect()
    }

    const CWD: &str = "/tmp/draft";
    const DRAFT: &str = "/tmp/draft/draft/draft.md";

    #[test]
    fn recorded_edit_run() {
        let mut transcript = Transcript::new();
        let prompt =
            "Use the edit tool to change 'world' to 'there' in the draft file. Then reply 'done'.";
        transcript.sent(prompt.into(), "Hello world draft.\n".into());
        replay(
            &mut transcript,
            &recorded(include_str!("../../../pi-rpc/tests/fixtures/tool.jsonl")),
            "Hello there draft.\n",
        );

        assert!(!transcript.busy);
        // The echoed prompt isn't shown twice.
        assert!(matches!(&transcript.items[0], Item::User { text } if text == prompt));
        let users = transcript
            .items
            .iter()
            .filter(|item| matches!(item, Item::User { .. }));
        assert_eq!(users.count(), 1);
        let tools = tools(&transcript);
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["read", "edit"]);
        let edit = tools[1];
        assert_eq!(edit.verb(), "Edited");
        assert_eq!(edit.stats, Some((1, Some(1))));
        assert_eq!(edit.duration(), Some(Duration::from_millis(3)));
        assert_eq!(
            edit.target(Path::new(CWD), Path::new(DRAFT), None),
            "draft.md"
        );
        assert_eq!(
            edit.diff().unwrap(),
            [
                DiffLine::Removed("Hello world draft.".into()),
                DiffLine::Added("Hello there draft.".into()),
            ]
        );
        assert!(matches!(
            transcript.items.last(),
            Some(Item::Assistant { streaming: false, text, .. }) if !text.is_empty()
        ));
    }

    #[test]
    fn recorded_aborted_write_run() {
        let mut transcript = Transcript::new();
        let prompt = "Count slowly from 1 to 200, one number per line.";
        transcript.sent(prompt.into(), "Hello world draft.\n".into());
        let numbers: String = (1..=200).map(|n| format!("{n}\n")).collect();
        replay(
            &mut transcript,
            &recorded(include_str!("../../../pi-rpc/tests/fixtures/abort.jsonl")),
            &numbers,
        );

        let write = tools(&transcript)[1];
        assert_eq!(write.verb(), "Wrote");
        assert_eq!(write.stats, Some((200, Some(1))));
        assert!(!transcript.busy);
        let last = transcript.items.len() - 1;
        assert!(matches!(
            &transcript.items[last],
            Item::Failure { error: None, retry: Some(retry) } if retry == prompt
        ));
        assert_eq!(transcript.retry_at(last), Some(prompt));
        assert_eq!(transcript.retry_at(0), None);
    }

    #[test]
    fn running_tool_and_crash() {
        let mut transcript = Transcript::new();
        transcript.sent("go".into(), String::new());
        let now = Instant::now();
        let start = AgentEvent::Pi(Event::ToolExecutionStart {
            tool_call_id: "1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!({"command": "git log\n--oneline"}),
        });
        transcript.apply(&start, None, now);
        let Some(Item::Tool(tool)) = transcript.items.last() else {
            panic!()
        };
        assert_eq!(tool.verb(), "Running");
        assert_eq!(
            tool.target(Path::new(CWD), Path::new(DRAFT), None),
            "git log"
        );
        assert_eq!(tool.duration(), None);

        transcript.apply(
            &AgentEvent::Crashed {
                error: "pi exited".into(),
                failed_turn: Some("go".into()),
            },
            None,
            now,
        );
        assert!(!transcript.busy);
        assert!(
            matches!(&transcript.items[1], Item::Tool(tool) if tool.status == ToolStatus::Stopped)
        );
        assert_eq!(transcript.retry_at(2), Some("go"));
    }

    #[test]
    fn assistant_error_waits_for_auto_retry() {
        let mut transcript = Transcript::new();
        transcript.sent("go".into(), String::new());
        let now = Instant::now();
        let error = serde_json::json!({
            "role": "assistant", "content": [], "stopReason": "error", "errorMessage": "overloaded"
        });
        for event in [
            Event::AgentStart,
            Event::MessageStart {
                message: error.clone(),
            },
            Event::MessageEnd {
                message: error.clone(),
            },
            Event::AutoRetryStart {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 10,
                error_message: "overloaded.".into(),
            },
            Event::MessageStart {
                message: error.clone(),
            },
            Event::MessageEnd { message: error },
            Event::AgentSettled { aborted: false },
        ] {
            transcript.apply(&AgentEvent::Pi(event), None, now);
        }
        let failures: Vec<_> = transcript
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Failure { error, .. } => error.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(failures, ["overloaded"]);
        assert_eq!(transcript.retry_at(transcript.items.len() - 1), Some("go"));
    }

    #[test]
    fn history_starts_at_the_first_draftmux_prompt() {
        let messages: Vec<Value> = serde_json::from_str(
            r#"[
            {"role":"user","content":"source question"},
            {"role":"assistant","content":[{"type":"text","text":"source answer"}],"stopReason":"stop"},
            {"role":"user","content":[{"type":"text","text":"shorter"}]},
            {"role":"custom","customType":"draftmux-context","content":"[Draftmux] …","display":false},
            {"role":"assistant","content":[{"type":"toolCall","id":"t1","name":"edit","arguments":{"path":"draft.md"}}],"stopReason":"toolUse"},
            {"role":"toolResult","toolCallId":"t1","toolName":"edit","content":[{"type":"text","text":"ok"}],"details":{"patch":"--- a\n+++ b\n@@ -1,3 +1,3 @@\n one\n two\n-three\n+3\n four\n"},"isError":false,"durationMs":4},
            {"role":"assistant","content":[{"type":"text","text":"Done."}],"stopReason":"stop"},
            {"role":"user","content":"again"},
            {"role":"assistant","content":[],"stopReason":"aborted"}
        ]"#,
        )
        .unwrap();
        let (items, earlier) = history(&messages);
        assert_eq!(earlier, 2);
        assert!(matches!(&items[0], Item::User { text } if text == "shorter"));
        let Item::Tool(edit) = &items[1] else {
            panic!()
        };
        assert_eq!(edit.stats, Some((1, Some(1))));
        assert_eq!(edit.duration(), Some(Duration::from_millis(4)));
        assert_eq!(
            edit.diff().unwrap(),
            [
                DiffLine::Context("two".into()),
                DiffLine::Removed("three".into()),
                DiffLine::Added("3".into()),
                DiffLine::Context("four".into()),
            ]
        );
        assert!(matches!(&items[2], Item::Assistant { text, .. } if text == "Done."));
        assert!(matches!(
            &items[4],
            Item::Failure { error: None, retry: Some(retry) } if retry == "again"
        ));

        let (items, earlier) = history(&messages[..2]);
        assert!(items.is_empty());
        assert_eq!(earlier, 2);
    }

    #[test]
    fn paths_are_shown_relative_to_the_cwd() {
        let cwd = Path::new("/repo");
        let draft = Path::new("/data/d1/draft.md");
        let home = Some(Path::new("/Users/me"));
        assert_eq!(display_path("src/a.rs", cwd, draft, home), "src/a.rs");
        assert_eq!(display_path("/repo/src/a.rs", cwd, draft, home), "src/a.rs");
        assert_eq!(
            display_path("/data/d1/draft.md", cwd, draft, home),
            "draft.md"
        );
        assert_eq!(display_path("~/notes.md", cwd, draft, home), "~/notes.md");
        assert_eq!(display_path("/etc/hosts", cwd, draft, home), "/etc/hosts");
    }
}
