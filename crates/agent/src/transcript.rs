//! What the chat pane shows for one draft, built from its agent's events
//! and history, independent of the UI.

use std::time::{Duration, Instant};

use markup::Document;

use crate::quote::{self, Quote};
use crate::{Event, Message, Stop, ToolCall, ToolKind, ToolResult};

/// Lines of tool output kept for the expanded row.
const MAX_OUTPUT_LINES: usize = 400;
/// The notice where earlier messages were replaced by a summary.
const COMPACTED: &str = "Earlier messages were compacted";

pub enum Item {
    /// The user's message, and the selections sent with it.
    User {
        text: String,
        quotes: Vec<Quote>,
    },
    Assistant {
        doc: Document,
    },
    Tool(Box<Tool>),
    /// A turn that failed, or that the user stopped (`error` is `None`),
    /// or the session's history that didn't load.
    Failure {
        error: Option<String>,
        retry: Option<Retry>,
    },
    Notice(String),
    /// The summary a draft's session started from instead of its source's
    /// history, collapsed until the user expands it.
    Summary {
        text: String,
        expanded: bool,
    },
}

/// What a failure's Retry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retry {
    /// Sends the prompt again.
    Prompt(String),
    /// Requests the session's history again.
    History,
}

pub struct Tool {
    pub id: String,
    pub kind: ToolKind,
    pub status: ToolStatus,
    /// The result's text, or the partial output while running.
    pub output: String,
    /// The unified patch of a finished `edit`.
    pub patch: Option<String>,
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

/// What a tool row names after its verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// A path as the model wrote it, for the UI to show relative to the
    /// draft's working directory.
    Path(&'a str),
    Text(&'a str),
}

/// One line of an `edit` row's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
}

/// The assistant message being streamed.
struct Stream {
    text: String,
    /// Its row, added once `text` has something to show.
    index: Option<usize>,
}

#[derive(Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    /// Between sending a prompt (or the agent starting a run) and the run
    /// settling.
    pub busy: bool,
    /// The prompt of the latest turn, offered again by a failure's retry.
    last_prompt: Option<String>,
    /// An assistant error the agent may still retry; shown once the run
    /// settles.
    pending_error: Option<String>,
    streaming: Option<Stream>,
    /// The draft's text before the agent's next write.
    draft_text: Option<String>,
    /// The first item changed since [`Transcript::take_change`], for the
    /// list to remeasure from.
    changed_from: Option<usize>,
    /// The conversation's size in tokens as the model last saw it; `None`
    /// until a history or model request reports it, and after a compaction
    /// that didn't say.
    pub context: Option<u64>,
    /// Earlier messages are being replaced with a summary, from the request
    /// until compaction ends.
    pub compacting: bool,
    /// The user asked for the compaction under way.
    compact_requested: bool,
}

impl Transcript {
    /// The first changed index, at most the item count before the changes
    /// (`old_len`), then forgets it.
    pub fn take_change(&mut self, old_len: usize) -> Option<usize> {
        self.changed_from.take().map(|from| from.min(old_len))
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
        if let Some(index) = self
            .streaming
            .as_mut()
            .and_then(|stream| stream.index.as_mut())
        {
            *index += items.len();
        }
        self.items.splice(0..0, items);
        self.touch(0);
    }

    /// The user sent `prompt`; `draft_text` is the draft as saved for the
    /// agent.
    pub fn sent(&mut self, prompt: String, draft_text: String) {
        self.busy = true;
        self.draft_text = Some(draft_text);
        self.push(Item::user(&prompt));
        self.last_prompt = Some(prompt);
    }

    /// The agent didn't take the prompt.
    pub fn send_failed(&mut self, error: String) {
        self.busy = false;
        self.push(Item::Failure {
            error: Some(error),
            retry: self.last_prompt.clone().map(Retry::Prompt),
        });
    }

    /// The session's history didn't load.
    pub fn history_failed(&mut self, error: &str) {
        self.push(Item::Failure {
            error: Some(format!("Couldn't load this conversation: {error}")),
            retry: Some(Retry::History),
        });
    }

    /// Removes a history failure before its history is requested again.
    pub fn retrying_history(&mut self) {
        if let Some(Item::Failure {
            retry: Some(Retry::History),
            ..
        }) = self.items.last()
        {
            self.items.pop();
            self.touch(self.items.len());
        }
    }

    /// The user asked to compact the conversation.
    pub fn compact_requested(&mut self) {
        self.compacting = true;
        self.compact_requested = true;
    }

    /// Compaction failed with `error` before the agent reported its end.
    /// Does nothing once it did, so the failure shows once.
    pub fn compact_failed(&mut self, error: String) {
        if self.compacting {
            self.compacting = false;
            self.compact_requested = false;
            self.push(Item::Failure {
                error: Some(format!("Couldn't compact the conversation: {error}")),
                retry: None,
            });
        }
    }

    /// The agent took the prompt without starting a run.
    pub fn handled(&mut self) {
        self.busy = false;
    }

    /// Whether the failure at `index` can be retried now: only the latest
    /// item, while nothing runs. The history can be requested again only
    /// while nothing else is shown, since the agent's history would also
    /// contain anything shown since and duplicate it.
    pub fn retry_at(&self, index: usize) -> Option<&Retry> {
        match self.items.get(index) {
            Some(Item::Failure {
                retry: Some(retry), ..
            }) if !self.busy && index + 1 == self.items.len() => match retry {
                Retry::History if index > 0 => None,
                retry => Some(retry),
            },
            _ => None,
        }
    }

    pub fn toggle(&mut self, index: usize) {
        match self.items.get_mut(index) {
            Some(Item::Tool(tool)) => tool.expanded = !tool.expanded,
            Some(Item::Summary { expanded, .. }) => *expanded = !*expanded,
            _ => return,
        }
        self.touch(index);
    }

    /// Applies one agent event. `written` is the draft's new text when the
    /// event is a draft write ([`Event::ToolFinished`] with `draft_write`).
    pub fn apply(&mut self, event: &Event, written: Option<&str>, now: Instant) {
        match event {
            Event::Ready(_) => {}
            Event::Crashed {
                error,
                failed_prompt,
            } => {
                self.end_run();
                self.compacting = false;
                self.compact_requested = false;
                self.push(Item::Failure {
                    error: Some(error.clone()),
                    retry: failed_prompt.clone().map(Retry::Prompt),
                });
            }
            Event::RunStarted => self.busy = true,
            Event::RunSettled { aborted } => {
                self.end_run();
                let error = self.pending_error.take();
                if *aborted || error.is_some() {
                    self.push(Item::Failure {
                        error: error.filter(|_| !aborted),
                        retry: self.last_prompt.clone().map(Retry::Prompt),
                    });
                }
            }
            Event::Retrying {
                attempt,
                max_attempts,
                error,
            } => {
                self.pending_error = None;
                self.push(Item::Notice(format!(
                    "{error} Retrying ({attempt} of {max_attempts})…"
                )));
            }
            Event::UserMessage(prompt) => {
                let echoed = matches!(
                    self.items.last(),
                    Some(Item::User { .. }) if self.last_prompt.as_ref() == Some(prompt)
                );
                if !echoed {
                    // A run the agent started, or history from elsewhere.
                    self.push(Item::user(prompt));
                    self.last_prompt = Some(prompt.clone());
                }
            }
            Event::AssistantStarted => {
                self.streaming = Some(Stream {
                    text: String::new(),
                    index: None,
                });
            }
            Event::AssistantBlock | Event::AssistantText(_) => {
                let Some(stream) = &mut self.streaming else {
                    return;
                };
                match event {
                    Event::AssistantText(delta) => stream.text.push_str(delta),
                    _ if !stream.text.is_empty() => stream.text.push_str("\n\n"),
                    _ => return,
                }
                if stream.text.trim().is_empty() {
                    return;
                }
                let item = Item::assistant(&stream.text);
                match stream.index {
                    Some(index) => {
                        self.items[index] = item;
                        self.touch(index);
                    }
                    None => {
                        stream.index = Some(self.items.len());
                        self.push(item);
                    }
                }
            }
            Event::AssistantEnded { text, stop } => {
                if let Some(stream) = self.streaming.take() {
                    match (stream.index, text.trim().is_empty()) {
                        (Some(index), true) => {
                            self.items.remove(index);
                            self.touch(index);
                        }
                        (Some(index), false) => {
                            self.items[index] = Item::assistant(text);
                            self.touch(index);
                        }
                        (None, false) => self.push(Item::assistant(text)),
                        (None, true) => {}
                    }
                }
                // An aborted message is reported by the run settling.
                if let Stop::Error(error) = stop {
                    self.pending_error = Some(error_message(error.as_deref()));
                }
            }
            Event::Context(tokens) => self.context = Some(*tokens),
            Event::CompactionStarted => self.compacting = true,
            Event::CompactionEnded {
                compacted,
                tokens,
                error,
            } => {
                if *compacted {
                    self.compacting = false;
                    self.compact_requested = false;
                    self.context = *tokens;
                    self.push(Item::Notice(COMPACTED.into()));
                } else if let Some(error) = error {
                    self.compact_failed(error.clone());
                } else if std::mem::take(&mut self.compact_requested) {
                    self.compacting = false;
                    self.push(Item::Notice("There was nothing to compact".into()));
                } else {
                    // The agent checked on its own and had nothing to do.
                    self.compacting = false;
                }
            }
            Event::ToolStarted(call) => {
                self.push(Tool::item(call.clone(), ToolStatus::Running { since: now }))
            }
            Event::ToolOutput { id, output } => {
                if let Some((index, tool)) = self.tool(id) {
                    tool.output = truncate_lines(output);
                    self.touch(index);
                }
            }
            Event::ToolFinished { id, result, .. } => {
                let previous = written.and_then(|_| self.draft_text.as_deref().map(line_count));
                let Some((index, tool)) = self.tool(id) else {
                    return;
                };
                let duration = result.duration.or(match tool.status {
                    ToolStatus::Running { since } => Some(now.saturating_duration_since(since)),
                    _ => None,
                });
                tool.finish(result, duration);
                if matches!(tool.kind, ToolKind::Write { .. })
                    && let Some((_, removed)) = &mut tool.stats
                {
                    *removed = previous;
                }
                if let Some(written) = written {
                    self.draft_text = Some(written.to_owned());
                }
                self.touch(index);
            }
        }
    }

    /// The run ended: nothing streams or runs any more.
    fn end_run(&mut self) {
        self.busy = false;
        self.streaming = None;
        for index in 0..self.items.len() {
            if let Item::Tool(tool) = &mut self.items[index]
                && matches!(tool.status, ToolStatus::Running { .. })
            {
                tool.status = ToolStatus::Stopped;
                self.touch(index);
            }
        }
    }

    /// The latest tool call with `id`, and its index.
    fn tool(&mut self, id: &str) -> Option<(usize, &mut Tool)> {
        self.items
            .iter_mut()
            .enumerate()
            .rev()
            .find_map(|(index, item)| match item {
                Item::Tool(tool) if tool.id == id => Some((index, &mut **tool)),
                _ => None,
            })
    }
}

impl Item {
    fn assistant(text: &str) -> Self {
        Item::Assistant {
            doc: markup::github::parse(text),
        }
    }

    /// The user's `prompt`, split into the message and its quotes.
    fn user(prompt: &str) -> Self {
        let (quotes, text) = quote::parse(prompt);
        Item::User {
            text: text.to_owned(),
            quotes,
        }
    }
}

impl Tool {
    fn item(call: ToolCall, status: ToolStatus) -> Item {
        Item::Tool(Box::new(Tool {
            id: call.id,
            kind: call.kind,
            status,
            output: String::new(),
            patch: None,
            stats: None,
            expanded: false,
        }))
    }

    fn finish(&mut self, result: &ToolResult, duration: Option<Duration>) {
        self.status = ToolStatus::Done {
            is_error: result.is_error,
            duration,
        };
        self.output = truncate_lines(&result.output);
        self.patch = result.patch.clone();
        if !result.is_error {
            self.stats = match &self.kind {
                ToolKind::Edit { .. } => self.patch.as_deref().map(patch_stats),
                ToolKind::Write { content, .. } => Some((line_count(content), None)),
                _ => None,
            };
        }
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
        let (running, done) = match self.kind {
            ToolKind::Bash { .. } => ("Running", "Ran"),
            ToolKind::Read { .. } => ("Reading", "Read"),
            ToolKind::Edit { .. } => ("Editing", "Edited"),
            ToolKind::Write { .. } => ("Writing", "Wrote"),
            ToolKind::Grep { .. } => ("Searching", "Searched"),
            ToolKind::Find { .. } => ("Finding", "Found"),
            ToolKind::Ls { .. } => ("Listing", "Listed"),
            ToolKind::Other { .. } => ("Using", "Used"),
        };
        if self.running() { running } else { done }
    }

    /// The row's target: the command's first line, the pattern, the path,
    /// or the tool's name.
    pub fn target(&self) -> Target<'_> {
        match &self.kind {
            ToolKind::Bash { command } => Target::Text(command.lines().next().unwrap_or_default()),
            ToolKind::Grep { pattern } | ToolKind::Find { pattern } => Target::Text(pattern),
            ToolKind::Read { path }
            | ToolKind::Edit { path }
            | ToolKind::Write { path, .. }
            | ToolKind::Ls { path } => Target::Path(path),
            ToolKind::Other { name } => Target::Text(name),
        }
    }

    /// What the expanded `edit` row shows: removed lines before added ones,
    /// with one line of context.
    pub fn diff(&self) -> Option<Vec<DiffLine>> {
        if !matches!(self.kind, ToolKind::Edit { .. }) || self.is_error() {
            return None;
        }
        self.patch.as_deref().map(patch_lines)
    }

    /// What the other expanded rows show, line by line. A `bash` row starts
    /// with its command.
    pub fn body(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let ToolKind::Bash { command } = &self.kind {
            lines.push(format!("$ {command}"));
        }
        let text = match &self.kind {
            ToolKind::Write { content, .. } if !self.is_error() => truncate_lines(content),
            _ => self.output.clone(),
        };
        lines.extend(text.trim_end_matches('\n').lines().map(str::to_owned));
        if lines.is_empty() && !self.running() {
            lines.push("No output".into());
        }
        lines
    }
}

/// The chat items for a conversation's messages.
pub fn history(messages: &[Message]) -> Vec<Item> {
    let mut transcript = Transcript::default();
    for message in messages {
        match message {
            Message::User(prompt) => {
                transcript.push(Item::user(prompt));
                transcript.last_prompt = Some(prompt.clone());
            }
            Message::Assistant {
                text,
                tool_calls,
                stop,
            } => {
                if !text.trim().is_empty() {
                    transcript.push(Item::assistant(text));
                }
                for call in tool_calls {
                    transcript.push(Tool::item(call.clone(), ToolStatus::Stopped));
                }
                let failure = match stop {
                    Stop::Completed => None,
                    Stop::Aborted => Some(None),
                    Stop::Error(error) => Some(Some(error_message(error.as_deref()))),
                };
                if let Some(error) = failure {
                    transcript.push(Item::Failure {
                        error,
                        retry: transcript.last_prompt.clone().map(Retry::Prompt),
                    });
                }
            }
            Message::ToolResult { id, result } => {
                if let Some((_, tool)) = transcript.tool(id) {
                    tool.finish(result, result.duration);
                }
            }
            Message::Compacted => transcript.push(Item::Notice(COMPACTED.into())),
        }
    }
    transcript.items
}

fn error_message(error: Option<&str>) -> String {
    error
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(id: &str, command: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: ToolKind::Bash {
                command: command.into(),
            },
        }
    }

    fn ended(text: &str, stop: Stop) -> Event {
        Event::AssistantEnded {
            text: text.into(),
            stop,
        }
    }

    #[test]
    fn the_summary_row_expands_and_collapses() {
        let mut transcript = Transcript::default();
        transcript.prepend(vec![Item::Summary {
            text: "The reviewer is Odile.".into(),
            expanded: false,
        }]);
        transcript.toggle(0);
        assert!(matches!(
            &transcript.items[0],
            Item::Summary { expanded: true, .. }
        ));
        transcript.toggle(0);
        assert!(matches!(
            &transcript.items[0],
            Item::Summary {
                expanded: false,
                ..
            }
        ));
    }

    #[test]
    fn compaction_shows_a_notice_and_takes_the_new_size() {
        let mut transcript = Transcript::default();
        let now = Instant::now();
        transcript.apply(&Event::Context(84_000), None, now);
        assert_eq!(transcript.context, Some(84_000));
        transcript.compact_requested();
        transcript.apply(&Event::CompactionStarted, None, now);
        assert!(transcript.compacting);
        transcript.apply(
            &Event::CompactionEnded {
                compacted: true,
                tokens: Some(9_000),
                error: None,
            },
            None,
            now,
        );
        assert!(!transcript.compacting);
        assert_eq!(transcript.context, Some(9_000));
        assert!(matches!(&transcript.items[..], [Item::Notice(text)] if text == COMPACTED));
    }

    #[test]
    fn a_failed_compaction_shows_once() {
        let mut transcript = Transcript::default();
        let now = Instant::now();
        transcript.compact_requested();
        transcript.apply(
            &Event::CompactionEnded {
                compacted: false,
                tokens: None,
                error: Some("Nothing to compact".into()),
            },
            None,
            now,
        );
        transcript.compact_failed("Nothing to compact".into());
        assert!(!transcript.compacting);
        assert!(matches!(
            &transcript.items[..],
            [Item::Failure { error: Some(error), retry: None }]
                if error == "Couldn't compact the conversation: Nothing to compact"
        ));
    }

    #[test]
    fn running_tool_and_crash() {
        let mut transcript = Transcript::default();
        transcript.sent("go".into(), String::new());
        let now = Instant::now();
        transcript.apply(
            &Event::ToolStarted(bash("1", "git log\n--oneline")),
            None,
            now,
        );
        let Some(Item::Tool(tool)) = transcript.items.last() else {
            panic!()
        };
        assert_eq!(tool.verb(), "Running");
        assert_eq!(tool.target(), Target::Text("git log"));
        assert_eq!(tool.duration(), None);

        transcript.apply(
            &Event::Crashed {
                error: "pi exited".into(),
                failed_prompt: Some("go".into()),
            },
            None,
            now,
        );
        assert!(!transcript.busy);
        assert!(
            matches!(&transcript.items[1], Item::Tool(tool) if tool.status == ToolStatus::Stopped)
        );
        assert_eq!(
            transcript.retry_at(2),
            Some(&Retry::Prompt("go".to_owned()))
        );
    }

    #[test]
    fn assistant_error_waits_for_auto_retry() {
        let mut transcript = Transcript::default();
        transcript.sent("go".into(), String::new());
        let now = Instant::now();
        let error = || ended("", Stop::Error(Some("overloaded".into())));
        for event in [
            Event::RunStarted,
            Event::AssistantStarted,
            error(),
            Event::Retrying {
                attempt: 1,
                max_attempts: 3,
                error: "overloaded.".into(),
            },
            Event::AssistantStarted,
            error(),
            Event::RunSettled { aborted: false },
        ] {
            transcript.apply(&event, None, now);
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
        assert_eq!(
            transcript.retry_at(transcript.items.len() - 1),
            Some(&Retry::Prompt("go".to_owned()))
        );
    }

    #[test]
    fn aborted_message_fails_once_when_the_run_settles() {
        let mut transcript = Transcript::default();
        transcript.sent("go".into(), String::new());
        let now = Instant::now();
        for event in [
            Event::RunStarted,
            Event::AssistantStarted,
            Event::AssistantText("Partial".into()),
            ended("Partial", Stop::Aborted),
        ] {
            transcript.apply(&event, None, now);
        }
        assert_eq!(transcript.items.len(), 2);
        transcript.apply(&Event::RunSettled { aborted: true }, None, now);
        assert!(matches!(
            &transcript.items[..],
            [_, Item::Assistant { .. }, Item::Failure { error: None, .. }]
        ));
    }

    #[test]
    fn assistant_row_waits_for_text() {
        let mut transcript = Transcript::default();
        transcript.sent("go".into(), String::new());
        transcript.take_change(0);
        let now = Instant::now();

        // A new block and blank text, then a tool call with no text.
        for event in [
            Event::RunStarted,
            Event::AssistantStarted,
            Event::AssistantBlock,
            Event::AssistantText("\n".into()),
            ended("", Stop::Completed),
        ] {
            transcript.apply(&event, None, now);
            assert_eq!(transcript.items.len(), 1);
            assert_eq!(transcript.take_change(1), None);
        }
        assert!(transcript.busy);

        // The reply's row appears with its first text.
        transcript.apply(&Event::AssistantStarted, None, now);
        assert_eq!(transcript.items.len(), 1);
        transcript.apply(&Event::AssistantText("Do".into()), None, now);
        transcript.apply(&Event::AssistantText("ne.".into()), None, now);
        assert_eq!(transcript.take_change(1), Some(1));
        assert!(
            matches!(&transcript.items[..], [Item::User { .. }, Item::Assistant { doc }] if *doc == markup::github::parse("Done."))
        );
    }

    #[test]
    fn write_stats_count_the_previous_draft_only_for_a_draft_write() {
        let write = |id: &str| ToolCall {
            id: id.into(),
            kind: ToolKind::Write {
                path: "draft.md".into(),
                content: "a\nb\nc\n".into(),
            },
        };
        let finished = |id: &str| Event::ToolFinished {
            id: id.into(),
            result: ToolResult {
                output: String::new(),
                is_error: false,
                duration: None,
                patch: None,
            },
            draft_write: true,
        };
        let mut transcript = Transcript::default();
        transcript.sent("go".into(), "one\n".into());
        let now = Instant::now();
        transcript.apply(&Event::ToolStarted(write("1")), None, now);
        transcript.apply(&finished("1"), Some("a\nb\nc\n"), now);
        transcript.apply(&Event::ToolStarted(write("2")), None, now);
        transcript.apply(&finished("2"), None, now);
        let stats: Vec<_> = transcript
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Tool(tool) => tool.stats,
                _ => None,
            })
            .collect();
        assert_eq!(stats, [(3, Some(1)), (3, None)]);
    }

    #[test]
    fn history_items() {
        let patch = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n one\n two\n-three\n+3\n four\n";
        let messages = [
            Message::User("shorter".into()),
            Message::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    kind: ToolKind::Edit {
                        path: "draft.md".into(),
                    },
                }],
                stop: Stop::Completed,
            },
            Message::ToolResult {
                id: "t1".into(),
                result: ToolResult {
                    output: "ok".into(),
                    is_error: false,
                    duration: Some(Duration::from_millis(4)),
                    patch: Some(patch.into()),
                },
            },
            Message::Assistant {
                text: "Done.".into(),
                tool_calls: Vec::new(),
                stop: Stop::Completed,
            },
            Message::Compacted,
            Message::User("again".into()),
            Message::Assistant {
                text: String::new(),
                tool_calls: Vec::new(),
                stop: Stop::Aborted,
            },
            Message::Assistant {
                text: String::new(),
                tool_calls: Vec::new(),
                stop: Stop::Error(None),
            },
        ];
        let items = history(&messages);
        assert!(
            matches!(&items[0], Item::User { text, quotes } if text == "shorter" && quotes.is_empty())
        );
        let Item::Tool(edit) = &items[1] else {
            panic!()
        };
        assert_eq!(edit.verb(), "Edited");
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
        assert!(
            matches!(&items[2], Item::Assistant { doc } if *doc == markup::github::parse("Done."))
        );
        assert!(
            matches!(&items[3], Item::Notice(text) if text == "Earlier messages were compacted")
        );
        assert!(matches!(
            &items[5],
            Item::Failure { error: None, retry: Some(Retry::Prompt(retry)) } if retry == "again"
        ));
        assert!(matches!(
            &items[6],
            Item::Failure { error: Some(error), .. } if error == "The model returned an error"
        ));
    }

    #[test]
    fn quoted_prompts_show_the_message_and_retry_the_whole_prompt() {
        let draft = "one\ntwo\nthree";
        let quote = Quote::new(draft, 4..7).unwrap();
        let prompt = quote::prompt("shorter", std::slice::from_ref(&quote));
        let is_quoted = |item: &Item| matches!(item, Item::User { text, quotes } if text == "shorter" && *quotes == [quote.clone()]);

        let mut transcript = Transcript::default();
        transcript.sent(prompt.clone(), draft.into());
        for event in [
            Event::RunStarted,
            Event::UserMessage(prompt.clone()),
            Event::RunSettled { aborted: true },
        ] {
            transcript.apply(&event, None, Instant::now());
        }
        // The echo of the prompt adds no second bubble.
        assert!(matches!(&transcript.items[..], [user, Item::Failure { .. }] if is_quoted(user)));
        assert_eq!(transcript.retry_at(1), Some(&Retry::Prompt(prompt.clone())));

        let items = history(&[Message::User(prompt)]);
        assert!(matches!(&items[..], [user] if is_quoted(user)));
    }

    #[test]
    fn history_failure_retries_while_nothing_else_is_shown() {
        let mut transcript = Transcript::default();
        transcript.history_failed("pi timed out");
        assert!(matches!(
            &transcript.items[..],
            [Item::Failure { error: Some(error), retry: Some(Retry::History) }]
                if error == "Couldn't load this conversation: pi timed out"
        ));
        assert_eq!(transcript.retry_at(0), Some(&Retry::History));

        transcript.retrying_history();
        assert!(transcript.items.is_empty());
        assert_eq!(transcript.take_change(1), Some(0));
        // Only a history failure is removed.
        transcript.sent("go".into(), String::new());
        transcript.send_failed("no reply".into());
        transcript.retrying_history();
        assert_eq!(transcript.items.len(), 2);
    }

    #[test]
    fn history_failure_after_a_prompt_has_no_retry() {
        // The prompt went out after the history request, so a new request
        // would return it too.
        let mut transcript = Transcript::default();
        transcript.sent("go".into(), String::new());
        transcript.history_failed("pi timed out");
        assert_eq!(transcript.retry_at(1), None);
        transcript.handled();
        assert_eq!(transcript.retry_at(1), None);

        let mut transcript = Transcript::default();
        transcript.history_failed("pi timed out");
        transcript.sent("go".into(), String::new());
        assert_eq!(transcript.retry_at(0), None);
    }
}
