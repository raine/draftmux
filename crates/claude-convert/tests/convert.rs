use std::path::Path;

use claude_convert::{Model, ModelSelection, Session, convert};
use serde_json::Value;

const COMPACTED: &[u8] = include_bytes!("fixtures/compacted.jsonl");

fn entries(session: &Session) -> Vec<Value> {
    session
        .jsonl
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn messages(entries: &[Value]) -> Vec<(&str, &str)> {
    entries
        .iter()
        .filter(|e| e["type"] == "message")
        .map(|e| {
            let message = &e["message"];
            let content = message["content"].as_array().unwrap();
            assert_eq!(content.len(), 1, "{message}");
            assert_eq!(content[0]["type"], "text");
            (
                message["role"].as_str().unwrap(),
                content[0]["text"].as_str().unwrap(),
            )
        })
        .collect()
}

fn sonnet() -> ModelSelection {
    ModelSelection {
        model: Some(Model {
            provider: "anthropic".into(),
            id: "claude-sonnet-5-5".into(),
        }),
        thinking_level: Some("high".into()),
    }
}

#[test]
fn keeps_the_resumed_conversation_after_the_latest_compaction() {
    let session = convert(COMPACTED, Path::new("/repo"), &sonnet()).unwrap();
    let entries = entries(&session);
    let long = format!("{}…", "Ä".repeat(240));
    assert_eq!(
        messages(&entries),
        [
            ("user", "SUMMARY-2: the release codename is BLUEBIRD."),
            ("assistant", "[ran: cargo test → test result: ok]"),
            ("user", "Which files changed?\n\n[image]"),
            (
                "assistant",
                format!(
                    "Let me check.\n\n[read src/lib.rs → {long}]\n\n\
                     [ran: git status … → error: fatal: not a git repository]"
                )
                .as_str()
            ),
            ("user", "Thanks, summarize."),
            ("assistant", "Done: lib.rs changed."),
        ]
    );
    // Pre-compaction lines, the abandoned branch, sidechains, meta lines,
    // thinking and Claude Code's synthetic messages are all gone.
    for dropped in [
        "OLD-",
        "SUMMARY-1",
        "ABANDONED",
        "SIDECHAIN",
        "Base directory",
        "THINKING",
        "SYNTHETIC",
    ] {
        assert!(!session.jsonl.contains(dropped), "{dropped} kept");
    }
    assert!(!session.jsonl.contains("toolResult"));
}

#[test]
fn writes_a_v3_session_chained_by_parent_id() {
    let session = convert(COMPACTED, Path::new("/repo"), &sonnet()).unwrap();
    let entries = entries(&session);

    let header = &entries[0];
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 3);
    assert_eq!(header["id"], session.id.as_str());
    assert_eq!(header["cwd"], "/repo");

    let mut parent = Value::Null;
    for entry in &entries[1..] {
        assert_eq!(entry["parentId"], parent, "{entry}");
        assert!(entry["timestamp"].is_string());
        parent = entry["id"].clone();
    }

    let user = &entries[1]["message"];
    assert_eq!(user["timestamp"], 1_794_132_660_000_i64);
    let assistant = &entries[2]["message"];
    assert_eq!(assistant["api"], "anthropic-messages");
    assert_eq!(assistant["provider"], "claude-code");
    assert_eq!(assistant["model"], "claude-opus-5-5");
    assert_eq!(assistant["stopReason"], "stop");
    assert_eq!(
        assistant["usage"],
        serde_json::json!({
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
        })
    );
    assert!(assistant["timestamp"].is_i64());

    let tail = &entries[entries.len() - 2..];
    assert_eq!(tail[0]["type"], "model_change");
    assert_eq!(tail[0]["provider"], "anthropic");
    assert_eq!(tail[0]["modelId"], "claude-sonnet-5-5");
    assert_eq!(tail[1]["type"], "thinking_level_change");
    assert_eq!(tail[1]["thinkingLevel"], "high");
}

#[test]
fn no_known_model_leaves_the_choice_to_pi() {
    let session = convert(COMPACTED, Path::new("/repo"), &ModelSelection::default()).unwrap();
    let last = entries(&session).pop().unwrap();
    assert_eq!(last["type"], "message");
}

#[test]
fn profile_default_reads_settings_and_fills_gaps() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        ModelSelection::profile_default(dir.path()).unwrap(),
        ModelSelection::default()
    );

    let settings = dir.path().join("settings.json");
    std::fs::write(
        &settings,
        "\u{feff}{\"defaultProvider\":\"openai\",\"defaultModel\":\"gpt-6\",\"defaultThinkingLevel\":\"low\"}",
    )
    .unwrap();
    let profile = ModelSelection::profile_default(dir.path()).unwrap();
    assert_eq!(
        profile,
        ModelSelection {
            model: Some(Model {
                provider: "openai".into(),
                id: "gpt-6".into()
            }),
            thinking_level: Some("low".into()),
        }
    );

    let explicit_model = ModelSelection {
        thinking_level: None,
        ..sonnet()
    };
    assert_eq!(
        explicit_model.or(profile.clone()),
        ModelSelection {
            thinking_level: Some("low".into()),
            ..sonnet()
        }
    );
    assert_eq!(ModelSelection::default().or(profile.clone()), profile);

    std::fs::write(&settings, "{\"defaultModel\":\"gpt-6\",}").unwrap();
    assert!(ModelSelection::profile_default(dir.path()).is_err());
}

#[test]
fn without_compaction_keeps_the_whole_chain() {
    let transcript = concat!(
        r#"{"type":"user","uuid":"1","parentUuid":null,"sessionId":"s","message":{"role":"user","content":"Hi"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"2","parentUuid":"1","sessionId":"s","message":{"id":"m","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t","name":"Mcp__x","input":{"q":"a"}}]}}"#,
        "\n",
        r#"{"type":"user","uuid":"3","parentUuid":"2","sessionId":"s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[]}]}}"#,
        "\n",
    );
    let session = convert(transcript.as_bytes(), Path::new("/repo"), &sonnet()).unwrap();
    assert_eq!(
        messages(&entries(&session)),
        [("user", "Hi"), ("assistant", r#"[Mcp__x: {"q":"a"}]"#)]
    );
}

#[test]
fn refuses_a_compaction_still_being_written() {
    let mut transcript = COMPACTED.to_vec();
    transcript.extend_from_slice(
        b"\n{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"uuid\":\"b3\",\"parentUuid\":null,\"sessionId\":\"s1\"}\n",
    );
    let err = convert(&transcript[..], Path::new("/repo"), &sonnet()).unwrap_err();
    assert!(err.to_string().contains("being compacted"), "{err}");
}

#[test]
fn refuses_a_transcript_without_messages() {
    let transcript = br#"{"type":"permission-mode","sessionId":"s"}"#;
    assert!(convert(&transcript[..], Path::new("/repo"), &sonnet()).is_err());
}

#[test]
fn writes_under_a_pi_style_name_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let session = convert(COMPACTED, Path::new("/repo"), &sonnet()).unwrap();
    let path = session.write_into(dir.path()).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.ends_with(&format!("_{}.jsonl", session.id)), "{name}");
    assert!(!name.contains(':') && !name[..name.len() - 6].contains('.'));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), session.jsonl);
    assert!(session.write_into(dir.path()).is_err());
}
