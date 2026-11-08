//! A scripted stand-in for `pi --mode rpc`, used by the integration tests.
//!
//! `FAKE_PI_SCRIPT` names a script; each line is one step:
//!
//! - `>>> {json}`: read one stdin line and require it to equal `json`,
//!   ignoring `id` (except for `extension_ui_response`, whose id answers a
//!   request). The script's `id` is mapped to the received one.
//! - `{json}`: print it; a response's `id` is rewritten through that map.
//! - `### sleep <ms>`, `### stderr <text>`, `### exit <code>`.
//! - `### eof`: wait for stdin to close, then exit 0, as pi does. With
//!   `FAKE_PI_RECORD` set, `<record>.eof` is created first.
//! - `### hang`: sleep until killed.
//!
//! A mismatch prints to stderr and exits 99. When `FAKE_PI_RECORD` is set,
//! the pid, arguments, cwd and environment are written there as JSON first.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

fn main() {
    if let Some(path) = std::env::var_os("FAKE_PI_RECORD") {
        let record = json!({
            "pid": std::process::id(),
            "args": std::env::args().skip(1).collect::<Vec<_>>(),
            "cwd": std::env::current_dir().unwrap(),
            "env": std::env::vars().collect::<HashMap<_, _>>(),
        });
        std::fs::write(path, record.to_string()).unwrap();
    }
    let script = std::fs::read_to_string(std::env::var("FAKE_PI_SCRIPT").unwrap()).unwrap();
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut ids = HashMap::<String, Value>::new();

    for step in script.lines().filter(|line| !line.trim().is_empty()) {
        if let Some(expected) = step.strip_prefix(">>> ") {
            let mut expected: Value = serde_json::from_str(expected).unwrap();
            let mut line = String::new();
            if stdin.read_line(&mut line).unwrap() == 0 {
                fail(&format!("stdin closed, expected {expected}"));
            }
            let mut received: Value = serde_json::from_str(&line).unwrap();
            if expected["type"] == "extension_ui_response" {
                if received != expected {
                    fail(&format!("expected {expected}, received {received}"));
                }
                continue;
            }
            let received_id = received.as_object_mut().unwrap().remove("id");
            let expected_id = expected.as_object_mut().unwrap().remove("id");
            if received != expected {
                fail(&format!("expected {expected}, received {received}"));
            }
            if let (Some(Value::String(from)), Some(to)) = (expected_id, received_id) {
                ids.insert(from, to);
            }
        } else if let Some(directive) = step.strip_prefix("### ") {
            let (name, arg) = directive.split_once(' ').unwrap_or((directive, ""));
            match name {
                "sleep" => std::thread::sleep(Duration::from_millis(arg.parse().unwrap())),
                "stderr" => eprintln!("{arg}"),
                "exit" => std::process::exit(arg.parse().unwrap()),
                "eof" => {
                    let mut rest = String::new();
                    while stdin.read_line(&mut rest).unwrap() > 0 {
                        rest.clear();
                    }
                    if let Some(path) = std::env::var_os("FAKE_PI_RECORD") {
                        let mut path = path;
                        path.push(".eof");
                        std::fs::write(path, "").unwrap();
                    }
                    std::process::exit(0);
                }
                "hang" => loop {
                    std::thread::sleep(Duration::from_secs(60));
                },
                _ => fail(&format!("unknown directive {directive}")),
            }
        } else {
            let mut record: Value = serde_json::from_str(step).unwrap();
            if record["type"] == "response"
                && let Some(Value::String(id)) = record.get("id")
                && let Some(mapped) = ids.get(id)
            {
                record["id"] = mapped.clone();
            }
            writeln!(stdout, "{record}").unwrap();
            stdout.flush().unwrap();
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("fake-pi: {message}");
    std::process::exit(99);
}
