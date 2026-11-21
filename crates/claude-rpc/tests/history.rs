//! History projection over Claude Code 2.1.293 transcripts: the sanitized
//! probe transcripts in `fixtures/transcripts` and the converter's synthetic
//! one with branches, sidechains and two compactions.

use std::fmt::Write;

use draftmux_agent::{History, Message, Stop, ToolKind};
use draftmux_claude_rpc::{history, transcript};

fn read(bytes: &[u8], boundary: Option<&str>) -> History {
    history(&transcript::parse(bytes).unwrap(), boundary)
}

/// One line per message, with the parts the chat shows.
fn render(history: &History) -> String {
    let mut out = format!("earlier {}\n", history.earlier);
    for message in &history.messages {
        match message {
            Message::User(text) => writeln!(out, "user: {}", first_line(text)),
            Message::Assistant {
                text,
                tool_calls,
                stop,
            } => {
                let tools: Vec<String> = tool_calls
                    .iter()
                    .map(|call| match &call.kind {
                        ToolKind::Read { path } => format!("Read {path}"),
                        ToolKind::Edit { path } => format!("Edit {path}"),
                        other => format!("{other:?}"),
                    })
                    .collect();
                let stop = match stop {
                    Stop::Completed => String::new(),
                    Stop::Aborted => " (aborted)".into(),
                    Stop::Error(error) => format!(" (error: {})", error.as_deref().unwrap_or("")),
                };
                writeln!(out, "assistant{stop}: {} {tools:?}", first_line(text))
            }
            Message::ToolResult { id, result } => writeln!(
                out,
                "result {}{}{}: {}",
                &id[id.len().saturating_sub(4)..],
                if result.is_error { " error" } else { "" },
                match &result.patch {
                    Some(patch) => format!(" patch {patch:?}"),
                    None => String::new(),
                },
                first_line(&result.output)
            ),
            Message::Compacted => writeln!(out, "compacted"),
        }
        .unwrap();
    }
    out
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    line.chars().take(60).collect()
}

const SESSION: &[u8] = include_bytes!("fixtures/transcripts/session.jsonl");
const FORK: &[u8] = include_bytes!("fixtures/transcripts/fork.jsonl");
/// The uuid of the newest message of `FORK`'s source
/// (`fork-source.jsonl`), an unanswered `tool_use`.
const FORK_BOUNDARY: &str = "0cf5a4d1-79c4-4901-a3ef-8d3611211105";

#[test]
fn parallel_tools_edit_compaction_and_interrupt() {
    assert_eq!(
        render(&read(SESSION, None)),
        "earlier 0
user: In a single response, call the Read tool twice in parallel: 
assistant:  [\"Read /tmp/c4/w4/a.txt\", \"Read /tmp/c4/w4/b.txt\"]
result qsFh: 1\tapple
result Wnab: 1\tcherry
assistant:  [\"Edit /tmp/c4/w4/a.txt\"]
result zmwU patch \"@@ -1,2 +1,2 @@\\n apple\\n-banana\\n+blueberry\\n\": The file /tmp/c4/w4/a.txt has been updated successfully. (fi
assistant: done. []
compacted
user: Reply with exactly: after compact
assistant: after compact []
user: Write a 400 word story about a lighthouse.
assistant (aborted): The keeper's name was Tomas Reyne, and he had tended the lig []
user: Reply with exactly: final
assistant: final []
"
    );
}

#[test]
fn skips_the_lines_of_a_model_switch() {
    assert_eq!(
        render(&read(
            include_bytes!("fixtures/transcripts/model-switch.jsonl"),
            None
        )),
        "earlier 0
user: Reply with exactly: alpha one
assistant: alpha one []
user: Reply with exactly: gamma three
assistant: gamma three []
"
    );
}

#[test]
fn branches_sidechains_and_two_compactions() {
    // The last line is cut off mid-write.
    assert_eq!(
        render(&read(
            include_bytes!("../../claude-convert/tests/fixtures/compacted.jsonl"),
            None
        )),
        "earlier 0
user: OLD-1 before the first compaction
assistant: old answer []
compacted
user: OLD-2 between compactions
assistant:  [\"Bash { command: \\\"cargo test\\\" }\"]
result t2: test result: ok
compacted
user: Which files changed?
assistant: Let me check. [\"Read /repo/src/lib.rs\", \"Bash { command: \\\"git status\\\\ngit diff\\\" }\"]
result t4a: ÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄ
result t4b error: fatal: not a git repository
user: Thanks, summarize.
assistant: Done: lib.rs changed. []
"
    );
}

#[test]
fn a_fork_shows_its_own_turns_after_the_source() {
    // Claude answered the source's unanswered call and added "No response
    // requested." when it forked; neither is the draft's.
    assert_eq!(
        render(&read(FORK, Some(FORK_BOUNDARY))),
        "earlier 2
user: Ignore any pending tool call. Reply with exactly: forked ok
assistant: forked ok []
"
    );
    // A boundary the transcript doesn't hold shows everything.
    let unknown = read(FORK, Some("00000000-0000-4000-8000-000000000000"));
    assert_eq!(unknown.earlier, 0);
    assert!(render(&unknown).contains("user: In a single response"));
}

#[test]
fn a_cut_off_last_line_is_left_out() {
    let full = read(SESSION, None);
    let last = SESSION[..SESSION.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap();
    // The last line is the final answer; cut it inside the line, and
    // inside a character of the interrupted story's.
    let cut = read(&SESSION[..last + 40], None);
    assert_eq!(cut.messages, full.messages[..full.messages.len() - 1]);
    let story = SESSION.windows(6).position(|w| w == b"keeper").unwrap();
    let mut torn = SESSION[..story].to_vec();
    torn.extend_from_slice("’s".as_bytes().split_at(1).0);
    let torn = read(&torn, None);
    assert_eq!(
        render(&torn).lines().last(),
        Some("user: Write a 400 word story about a lighthouse.")
    );
}

#[test]
fn the_context_ahead_of_a_prompt_is_left_out() {
    let user = serde_json::json!({"parentUuid": null, "isSidechain": false, "type": "user",
        "uuid": "u1", "sessionId": "s", "message": {"role": "user", "content": [
            {"type": "text", "text": "<draftmux-instructions>\nDraft file: /d/draft.md\n</draftmux-instructions>"},
            {"type": "text", "text": "<draftmux-context>\nTarget platform: Slack\n</draftmux-context>"},
            {"type": "text", "text": "<draftmux-draft>\nHello\n</draftmux-draft>"},
            {"type": "text", "text": "Shorten it"}]}});
    let assistant = serde_json::json!({"parentUuid": "u1", "isSidechain": false,
        "type": "assistant", "uuid": "a1", "sessionId": "s", "message": {"id": "m1",
            "role": "assistant", "content": [{"type": "text", "text": "Done."}],
            "stop_reason": "end_turn"}});
    let bytes = format!("{user}\n{assistant}\n");
    assert_eq!(
        render(&read(bytes.as_bytes(), None)),
        "earlier 0
user: Shorten it
assistant: Done. []
"
    );
}

#[test]
fn context_is_the_latest_usage_or_what_compaction_left() {
    let user =
        r#"{"type":"user","uuid":"u1","parentUuid":null,"message":{"role":"user","content":"hi"}}"#;
    let assistant = r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Hello"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":15790,"cache_read_input_tokens":8376,"output_tokens":9}}}"#;
    let boundary = r#"{"type":"system","subtype":"compact_boundary","uuid":"b1","parentUuid":null,"logicalParentUuid":"a1","compactMetadata":{"trigger":"manual","preTokens":24177,"postTokens":10563}}"#;
    let summary = r#"{"type":"user","uuid":"s1","parentUuid":"b1","isCompactSummary":true,"message":{"role":"user","content":"Summary"}}"#;
    let before = [user, assistant].join("\n");
    assert_eq!(read(before.as_bytes(), None).context, Some(24177));
    let after = [user, assistant, boundary, summary].join("\n");
    assert_eq!(read(after.as_bytes(), None).context, Some(10563));
}
