//! Complete-option previews from the agent's one-shot JSON event stream:
//! pi's events, or Claude Code's stream-json frames.

use std::time::Instant;

use draftmux_claude_rpc::Reply;
use serde_json::Value;

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Agent {
    #[default]
    Pi,
    Claude,
}

impl Agent {
    /// The agent a draft on `backend` runs on.
    pub fn of(backend: &draftmux_storage::Backend) -> Self {
        match backend {
            draftmux_storage::Backend::Pi(_) => Agent::Pi,
            draftmux_storage::Backend::Claude(_) => Agent::Claude,
        }
    }

    /// The agent's name in messages.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Pi => "pi",
            Agent::Claude => "Claude Code",
        }
    }
}

/// What the reply holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    /// Up to this many options, as JSON.
    Options(usize),
    /// Plain text to insert at the cursor, given as the only option.
    Text,
}

impl Default for Format {
    fn default() -> Self {
        Format::Options(draftmux_settings::DEFAULT_ALTERNATIVES)
    }
}

#[derive(Default)]
pub(super) struct Stream {
    agent: Agent,
    format: Format,
    line: Vec<u8>,
    text: String,
    shown: Vec<String>,
    result: Option<Result<Vec<String>, String>>,
    settled: bool,
    /// When the first reply started, and when its first text arrived.
    replied: Option<Instant>,
    wrote: Option<Instant>,
    usage: Option<Usage>,
}

/// The tokens a reply took, as the agent reported them. Input that was
/// neither read from nor written to the prompt cache is `input`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
}

impl Usage {
    /// From `usage` with the agent's names for the counts: input, cache
    /// read, cache write and output. `None` when it reports no input.
    fn read(usage: &Value, [input, read, write, output]: [&str; 4]) -> Option<Self> {
        let count = |key: &str| usage[key].as_u64().unwrap_or(0);
        usage[input].is_u64().then(|| Self {
            input: count(input),
            cache_read: count(read),
            cache_write: count(write),
            output: count(output),
        })
    }
}

impl Stream {
    pub fn new(agent: Agent, format: Format) -> Self {
        Self {
            agent,
            format,
            ..Self::default()
        }
    }

    pub fn feed(
        &mut self,
        bytes: &[u8],
        preview: &mut impl FnMut(Vec<String>),
    ) -> Result<(), String> {
        let name = self.agent.name();
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            if self.line.len() + part.len() > MAX_BYTES {
                return Err(format!("{name}'s selection event was too large"));
            }
            self.line.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                let event: Value = serde_json::from_slice(&self.line)
                    .map_err(|_| format!("{name} printed an invalid selection event"))?;
                self.line.clear();
                match self.agent {
                    Agent::Pi => self.event(event, preview)?,
                    Agent::Claude => self.claude_event(&event, preview)?,
                }
            }
        }
        Ok(())
    }

    /// When the agent's first reply started and when its text started,
    /// which tells waiting for the model from its thinking.
    pub fn timings(&self) -> (Option<Instant>, Option<Instant>) {
        (self.replied, self.wrote)
    }

    /// The tokens of the last reply, once it has ended.
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    /// The error the agent reported, if any, which says more than its exit
    /// status. Claude Code exits non-zero after reporting a failed login.
    pub fn reported_error(&self) -> Option<String> {
        match (&self.result, self.agent) {
            (Some(Err(error)), Agent::Claude) => Some(error.clone()),
            _ => None,
        }
    }

    /// Appends streamed text and previews the options it completes, or the
    /// text so far.
    fn text(&mut self, text: &str, preview: &mut impl FnMut(Vec<String>)) -> Result<(), String> {
        if self.text.len() + text.len() > MAX_BYTES {
            return Err(format!(
                "{}'s selection reply was too large",
                self.agent.name()
            ));
        }
        self.text.push_str(text);
        self.wrote.get_or_insert_with(Instant::now);
        let shown = match self.format {
            Format::Options(count) => completed_options(&self.text, count),
            Format::Text => super::continuation(&self.text, false).into_iter().collect(),
        };
        self.publish(shown, preview);
        Ok(())
    }

    /// The reply's options, or its text as the only one.
    fn parse(&self, text: &str) -> Result<Vec<String>, String> {
        let name = self.agent.name();
        match self.format {
            Format::Options(count) => super::parse(name, text, count),
            Format::Text => super::continuation(text, true)
                .map(|text| vec![text])
                .ok_or_else(|| format!("{name}'s reply had no continuation")),
        }
    }

    fn claude_event(
        &mut self,
        frame: &Value,
        preview: &mut impl FnMut(Vec<String>),
    ) -> Result<(), String> {
        if self.settled {
            return Ok(());
        }
        if frame["type"] == "result" {
            self.usage = Usage::read(
                &frame["usage"],
                [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                    "output_tokens",
                ],
            );
        }
        match Reply::of(frame) {
            Reply::Message => self.reset(preview),
            Reply::TextBlock if !self.text.is_empty() => self.text("\n\n", preview)?,
            Reply::Text(text) => self.text(text, preview)?,
            Reply::Done(reply) => {
                let result = reply.and_then(|text| self.parse(text));
                self.publish(result.as_ref().cloned().unwrap_or_default(), preview);
                self.result = Some(result);
                self.settled = true;
            }
            Reply::TextBlock | Reply::Other => {}
        }
        Ok(())
    }

    fn publish(&mut self, options: Vec<String>, preview: &mut impl FnMut(Vec<String>)) {
        if self.shown != options {
            self.shown = options.clone();
            preview(options);
        }
    }

    /// A reply starts, replacing any before it.
    fn reset(&mut self, preview: &mut impl FnMut(Vec<String>)) {
        self.replied.get_or_insert_with(Instant::now);
        self.text.clear();
        self.result = None;
        self.settled = false;
        self.publish(Vec::new(), preview);
    }

    fn event(&mut self, event: Value, preview: &mut impl FnMut(Vec<String>)) -> Result<(), String> {
        match event["type"].as_str() {
            Some("message_start") if event["message"]["role"] == "assistant" => {
                self.reset(preview);
            }
            Some("auto_retry_start") => self.reset(preview),
            Some("message_update") => {
                let delta = &event["assistantMessageEvent"];
                match delta["type"].as_str() {
                    Some("text_start") if !self.text.is_empty() => {
                        self.text("\n\n", preview)?;
                    }
                    Some("text_delta") => {
                        let text = delta["delta"]
                            .as_str()
                            .ok_or("pi sent an invalid text delta")?;
                        self.text(text, preview)?;
                    }
                    _ => {}
                }
            }
            Some("message_end") if event["message"]["role"] == "assistant" => {
                let message = &event["message"];
                self.usage = Usage::read(
                    &message["usage"],
                    ["input", "cacheRead", "cacheWrite", "output"],
                );
                let result = if message["stopReason"] == "stop" {
                    let text = message["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|part| part["type"] == "text")
                        .filter_map(|part| part["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    self.parse(&text)
                } else {
                    Err(match message["stopReason"].as_str() {
                        Some("length") => "pi's selection reply was truncated".into(),
                        Some("aborted") => "pi's selection request was cancelled".into(),
                        _ => "pi couldn't finish the selection request".into(),
                    })
                };
                self.publish(result.as_ref().cloned().unwrap_or_default(), preview);
                self.result = Some(result);
            }
            Some("agent_settled") => {
                self.settled = event["aborted"] == false;
                if !self.settled {
                    self.publish(Vec::new(), preview);
                    self.result = Some(Err("pi's selection request was cancelled".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<String>, String> {
        let name = self.agent.name();
        if !self.line.is_empty() {
            return Err(format!("{name}'s selection event was incomplete"));
        }
        if let Some(Err(error)) = &self.result {
            return Err(error.clone());
        }
        if !self.settled {
            return Err(format!("{name} didn't finish the selection request"));
        }
        match self.result {
            Some(result) => result,
            None => self.parse(""),
        }
    }
}

/// Only canonical envelopes stream. Wrapped replies still use the tolerant
/// final parser. A delimiter after the string proves the whole value arrived.
fn completed_options(text: &str, count: usize) -> Vec<String> {
    let mut rest = text;
    for token in ["{", "\"alternatives\"", ":", "["] {
        let Some(tail) = rest.trim_start().strip_prefix(token) else {
            return Vec::new();
        };
        rest = tail;
    }
    let mut options = Vec::new();
    while options.len() < count {
        let mut values =
            serde_json::Deserializer::from_str(rest.trim_start()).into_iter::<String>();
        let Some(Ok(value)) = values.next() else {
            break;
        };
        rest = rest.trim_start()[values.byte_offset()..].trim_start();
        let Some(delimiter @ (b',' | b']')) = rest.as_bytes().first().copied() else {
            break;
        };
        let option = value.trim();
        if !option.is_empty() && !options.iter().any(|o| o == option) {
            options.push(option.to_string());
        }
        if delimiter == b']' {
            break;
        }
        rest = &rest[1..];
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn send(stream: &mut Stream, event: Value, updates: &mut Vec<Vec<String>>) {
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.push(b'\n');
        // Split inside Unicode, JSON strings, and event framing.
        for byte in bytes {
            stream
                .feed(&[byte], &mut |options| updates.push(options))
                .unwrap();
        }
    }

    fn delta(text: &str) -> Value {
        json!({"type":"message_update", "assistantMessageEvent":{"type":"text_delta","delta":text}})
    }

    fn end(text: &str, reason: &str) -> Value {
        json!({"type":"message_end","message":{"role":"assistant","stopReason":reason,"content":[{"type":"text","text":text}]}})
    }

    #[test]
    fn complete_strings_only_with_escapes_unicode_and_deduplication() {
        let text =
            r#"{"alternatives":["  "," café 🙂\n\"quote\" ","café 🙂\n\"quote\"","two","three"]}"#;
        let mut stream = Stream::default();
        let mut updates = Vec::new();
        for char in text.chars() {
            send(&mut stream, delta(&char.to_string()), &mut updates);
        }
        assert_eq!(updates.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(updates[2], super::super::parse("pi", text, 3).unwrap());
        assert_eq!(completed_options(text, 1).len(), 1);
        for text in [
            r#"{"alternatives":["unfinished"#,
            r#"{"alternatives":["closed"  "#,
            r#"{"alternatives":["\u12"#,
        ] {
            assert!(completed_options(text, 3).is_empty());
        }
        send(&mut stream, end(text, "stop"), &mut updates);
        send(
            &mut stream,
            json!({"type":"agent_settled","aborted":false}),
            &mut updates,
        );
        assert_eq!(stream.finish().unwrap(), updates[2]);
    }

    #[test]
    fn retry_clears_preview_and_final_message_is_authoritative() {
        let mut stream = Stream::default();
        let mut updates = Vec::new();
        send(
            &mut stream,
            delta(r#"{"alternatives":["old","#),
            &mut updates,
        );
        send(
            &mut stream,
            json!({"type":"auto_retry_start"}),
            &mut updates,
        );
        assert!(updates[1].is_empty());
        send(
            &mut stream,
            end("```json\n{\"alternatives\":[\"final\"]}\n```", "stop"),
            &mut updates,
        );
        send(
            &mut stream,
            json!({"type":"agent_settled","aborted":false}),
            &mut updates,
        );
        assert_eq!(stream.finish().unwrap(), ["final"]);
    }

    #[test]
    fn rejects_errors_truncation_abort_and_missing_settlement() {
        for reason in ["error", "length", "aborted", "stop"] {
            let mut stream = Stream::default();
            let mut updates = Vec::new();
            send(
                &mut stream,
                delta(r#"{"alternatives":["old","#),
                &mut updates,
            );
            send(
                &mut stream,
                end(r#"{"alternatives":["new"]}"#, reason),
                &mut updates,
            );
            assert!(stream.finish().is_err());
            if reason != "stop" {
                assert!(updates.last().unwrap().is_empty());
            }
        }
        let mut stream = Stream::default();
        send(
            &mut stream,
            end(r#"{"alternatives":["new"]}"#, "stop"),
            &mut Vec::new(),
        );
        send(
            &mut stream,
            json!({"type":"agent_settled","aborted":true}),
            &mut Vec::new(),
        );
        assert!(stream.finish().is_err());
    }

    #[test]
    fn thinking_and_wrapped_output_do_not_preview_and_input_is_bounded() {
        let mut stream = Stream::default();
        let mut updates = Vec::new();
        send(
            &mut stream,
            json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"thinking"}}),
            &mut updates,
        );
        send(
            &mut stream,
            delta("```json\n{\"alternatives\":[\"one\"]}"),
            &mut updates,
        );
        assert!(updates.is_empty());
        assert!(
            stream
                .feed(&vec![b'x'; MAX_BYTES + 1], &mut |_| {})
                .is_err()
        );
        assert!(Stream::default().feed(b"not json\n", &mut |_| {}).is_err());
        let mut stream = Stream {
            text: "x".repeat(MAX_BYTES),
            ..Stream::default()
        };
        let start = json!({"type":"message_update","assistantMessageEvent":{"type":"text_start"}});
        assert!(stream.event(start, &mut |_| {}).is_err());
        assert!(stream.event(delta("x"), &mut |_| {}).is_err());
        let mut stream = Stream::default();
        stream.feed(b"{", &mut |_| {}).unwrap();
        assert!(stream.finish().is_err());
    }

    #[test]
    fn text_mode_previews_the_text_so_far_and_retries_retract_it() {
        let mut stream = Stream::new(Agent::Pi, Format::Text);
        let mut updates = Vec::new();
        send(&mut stream, delta(" jumps"), &mut updates);
        send(&mut stream, delta(" over"), &mut updates);
        send(
            &mut stream,
            json!({"type":"auto_retry_start"}),
            &mut updates,
        );
        send(&mut stream, delta("leaps."), &mut updates);
        send(&mut stream, end("leaps.\n\nMore.", "stop"), &mut updates);
        send(
            &mut stream,
            json!({"type":"agent_settled","aborted":false}),
            &mut updates,
        );
        assert_eq!(
            updates,
            [
                vec!["jumps".to_string()],
                vec!["jumps over".to_string()],
                vec![],
                vec!["leaps.".to_string()],
            ]
        );
        assert_eq!(stream.finish().unwrap(), ["leaps."]);

        let mut stream = Stream::new(Agent::Claude, Format::Text);
        claude_send(&mut stream, claude_result("  "), &mut Vec::new());
        assert_eq!(
            stream.finish().unwrap_err(),
            "Claude Code's reply had no continuation"
        );
    }

    fn claude_send(stream: &mut Stream, frame: Value, updates: &mut Vec<Vec<String>>) {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        stream
            .feed(&bytes, &mut |options| updates.push(options))
            .unwrap();
    }

    fn claude_delta(text: &str) -> Value {
        json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":text}}})
    }

    fn claude_result(text: &str) -> Value {
        json!({"type":"result","subtype":"success","is_error":false,"result":text,"stop_reason":"end_turn"})
    }

    #[test]
    fn usage_comes_from_the_reply_s_end() {
        let mut updates = Vec::new();
        let mut pi = Stream::new(Agent::Pi, Format::default());
        assert_eq!(pi.usage(), None);
        let mut end = end(r#"{"alternatives":["a"]}"#, "stop");
        end["message"]["usage"] =
            json!({"input": 4, "output": 96, "cacheRead": 193973, "cacheWrite": 391});
        send(&mut pi, end, &mut updates);
        let usage = Usage {
            input: 4,
            cache_read: 193973,
            cache_write: 391,
            output: 96,
        };
        assert_eq!(pi.usage(), Some(usage));

        let mut claude = Stream::new(Agent::Claude, Format::default());
        let mut result = claude_result(r#"{"alternatives":["a"]}"#);
        result["usage"] = json!({"input_tokens": 4, "cache_read_input_tokens": 193973,
            "cache_creation_input_tokens": 391, "output_tokens": 96});
        claude_send(&mut claude, result, &mut updates);
        assert_eq!(claude.usage(), Some(usage));
    }

    #[test]
    fn claude_previews_reset_per_message_and_the_result_decides() {
        let mut stream = Stream::new(Agent::Claude, Format::default());
        let mut updates = Vec::new();
        let start = json!({"type":"stream_event","event":{"type":"message_start","message":{}}});
        claude_send(&mut stream, start.clone(), &mut updates);
        claude_send(
            &mut stream,
            claude_delta(r#"{"alternatives":["old","#),
            &mut updates,
        );
        // A message claude asks for after a reply it couldn't use.
        claude_send(&mut stream, start, &mut updates);
        assert_eq!(updates, [vec!["old".to_string()], Vec::new()]);
        claude_send(
            &mut stream,
            claude_delta(r#"{"alternatives":["new","#),
            &mut updates,
        );
        claude_send(
            &mut stream,
            claude_result("```json\n{\"alternatives\":[\"new\",\"two\"]}\n```"),
            &mut updates,
        );
        // Nothing after the result counts.
        claude_send(
            &mut stream,
            claude_result(r#"{"alternatives":["late"]}"#),
            &mut updates,
        );
        assert_eq!(updates.last().unwrap(), &["new", "two"]);
        assert_eq!(stream.reported_error(), None);
        assert_eq!(stream.finish().unwrap(), ["new", "two"]);
    }

    #[test]
    fn claude_errors_are_reported_and_retract_previews() {
        let mut stream = Stream::new(Agent::Claude, Format::default());
        let mut updates = Vec::new();
        claude_send(
            &mut stream,
            claude_delta(r#"{"alternatives":["old","#),
            &mut updates,
        );
        claude_send(
            &mut stream,
            json!({"type":"result","subtype":"success","is_error":false,"result":"{\"alternatives\":[\"cut","stop_reason":"max_tokens"}),
            &mut updates,
        );
        assert!(updates.last().unwrap().is_empty());
        let error = stream.reported_error().unwrap();
        assert!(error.contains("cut off"), "{error}");
        assert_eq!(stream.finish().unwrap_err(), error);

        // pi's errors stay with its exit status, as before.
        let mut stream = Stream::default();
        send(
            &mut stream,
            end(r#"{"alternatives":["x"]}"#, "error"),
            &mut Vec::new(),
        );
        assert_eq!(stream.reported_error(), None);

        assert_eq!(
            Stream::new(Agent::Claude, Format::default())
                .feed(b"nope\n", &mut |_| {})
                .unwrap_err(),
            "Claude Code printed an invalid selection event"
        );
    }
}
