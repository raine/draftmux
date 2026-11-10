//! Selection actions as one-off pi calls (history/docs/design.md, Agent interaction).
//!
//! Each call forks the draft's session into a temporary session directory:
//! `pi -p <prompt> --fork <draft session> --session-dir <temp> -e <ext>`, with
//! the draft's profile and cwd, stdin closed and `DRAFTMUX_MODE=selection`,
//! so the extension blocks every tool call. The temporary directory is
//! deleted once pi has exited, so nothing reaches the draft's own session or
//! chat. The prompt carries the editor text and the selection, and pi answers
//! with replacement options as JSON.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use draftmux_pi_rpc::ThinkingLevel;
use draftmux_storage::{DraftSettings, Meta, Target};
use serde::Deserialize;

use crate::process;

/// A selection call waits for a full model turn over the whole session.
const TIMEOUT: Duration = Duration::from_secs(120);

/// The draft travels on the command line, and macOS limits arguments plus
/// environment to 1 MiB.
const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// Selection-call settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionConfig {
    /// `None` keeps the model pi restores from the forked session, which
    /// reuses the session's prompt cache.
    pub model: Option<String>,
    /// Sent with `model` only; pi restores the session's otherwise.
    pub thinking: Option<ThinkingLevel>,
    /// How many options each action asks for.
    pub alternatives: usize,
    /// Off runs pi with `--no-context-files`, as the draft's own pi.
    pub context_files: bool,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            model: None,
            thinking: None,
            alternatives: draftmux_settings::DEFAULT_ALTERNATIVES,
            context_files: true,
        }
    }
}

impl SelectionConfig {
    /// For a draft, from the settings it kept at creation, asking for
    /// `alternatives` options.
    pub fn for_draft(settings: &DraftSettings, alternatives: usize) -> Self {
        let model = settings.selection_model.as_ref();
        Self {
            model: model.map(|m| format!("{}/{}", m.provider, m.model_id)),
            thinking: model.and_then(|m| ThinkingLevel::parse(&m.thinking)),
            alternatives,
            context_files: settings.context_files,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Alternatives,
    /// The prompt of one of the user's selection actions.
    Instruction(String),
}

impl Action {
    fn instruction(&self) -> &str {
        match self {
            Action::Alternatives => {
                "Suggest alternative phrasings that preserve the meaning and fit the surrounding text."
            }
            Action::Instruction(text) => text,
        }
    }
}

/// What a call is about: the draft it forks and the selection in the
/// editor's current text.
#[derive(Debug, Clone)]
pub struct Request {
    /// The draft's pi session file (`Meta::pi_session_file`).
    pub session_file: PathBuf,
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub target: Target,
    pub text: String,
    pub range: Range<usize>,
    pub action: Action,
}

impl Request {
    /// `None` when the draft has no pi session yet.
    pub fn new(meta: &Meta, text: String, range: Range<usize>, action: Action) -> Option<Self> {
        Some(Self {
            session_file: meta.pi_session_file.clone()?,
            cwd: meta.cwd.clone(),
            agent_dir: meta.pi_agent_dir.clone(),
            target: meta.target,
            text,
            range,
            action,
        })
    }

    fn selected(&self) -> &str {
        &self.text[self.range.clone()]
    }
}

/// Runs the call with `pi` (a command for the pi executable) and returns up
/// to `config.alternatives` replacements for the selection, with the
/// selection's own leading and trailing whitespace kept. The temporary
/// session directory is created in `temp_parent` and is gone when this
/// returns.
pub fn run(
    pi: Command,
    extension: &Path,
    request: &Request,
    config: &SelectionConfig,
    temp_parent: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    let prompt = prompt(request, config.alternatives)?;
    let session_dir = tempfile::Builder::new()
        .prefix("draftmux-selection-")
        .tempdir_in(temp_parent)
        .map_err(|err| format!("Couldn't create a temporary session directory: {err}"))?;
    let command = command(pi, &prompt, request, extension, session_dir.path(), config);
    // `run` returns after pi has exited, so the directory is removed after.
    let output = process::run("pi", command, None, TIMEOUT, Some(cancel));
    drop(session_dir);
    let (leading, trailing) = surrounding_whitespace(request.selected());
    Ok(parse(&output?, config.alternatives)?
        .into_iter()
        .map(|option| format!("{leading}{option}{trailing}"))
        .collect())
}

fn command(
    mut pi: Command,
    prompt: &str,
    request: &Request,
    extension: &Path,
    session_dir: &Path,
    config: &SelectionConfig,
) -> Command {
    // `-p` takes the next argument as the prompt.
    pi.arg("-p")
        .arg(prompt)
        .arg("--fork")
        .arg(&request.session_file)
        .arg("--session-dir")
        .arg(session_dir)
        .arg("-e")
        .arg(extension);
    if !config.context_files {
        pi.arg("--no-context-files");
    }
    if let Some(model) = &config.model {
        pi.arg("--model").arg(model);
        if let Some(thinking) = config.thinking {
            pi.arg("--thinking").arg(thinking.as_str());
        }
    }
    pi.current_dir(&request.cwd)
        .env("PI_CODING_AGENT_DIR", &request.agent_dir)
        .env("DRAFTMUX_MODE", "selection")
        // The captured environment can carry values from a pi session the
        // app was launched from; selection mode reads none of them.
        .env_remove("DRAFTMUX_DRAFT_PATH")
        .env_remove("DRAFTMUX_INSTRUCTIONS");
    pi
}

const START: &str = "<<<SELECTION>>>";
const END: &str = "<<<END SELECTION>>>";

/// What the draft is written in. `pi-extension/draftmux.ts` describes the
/// targets with the same text.
fn syntax(target: Target) -> &'static str {
    match target {
        Target::Github => "GitHub-flavored Markdown",
        Target::Slack => {
            "Slack mrkdwn (*bold*, _italic_, ~strike~, `code`, <url|text> or [text](url) links; Markdown headings and tables are fine and paste into Slack as a bold line and a code block)"
        }
        Target::Rich => {
            "Markdown, pasted as rich text into an email or document (headings, bold, italic, links, lists, quotes, code and tables carry over; no raw HTML, alerts or @mentions; no subject line)"
        }
        Target::Plain => {
            "plain text, such as an email body (no Markdown syntax such as **bold**, # headings, backticks, [text](url) links or tables; write URLs in full; no subject line)"
        }
    }
}

/// Starts with a fixed line, so pi never reads it as a flag (`-…`) or a
/// file reference (`@…`).
fn prompt(request: &Request, count: usize) -> Result<String, String> {
    let selected = request.selected().trim();
    if selected.is_empty() {
        return Err("Select some text first".into());
    }
    let text = &request.text;
    let marked = format!(
        "{}{START}{}{END}{}",
        &text[..request.range.start],
        request.selected(),
        &text[request.range.end..]
    );
    let syntax = syntax(request.target);
    let prompt = format!(
        "[Draftmux selection request]\n\
         This is a one-off request about the draft, separate from editing the draft file. \
         Don't edit files or use tools; answer with JSON only.\n\
         \n\
         The current draft, written in {syntax}, with the selected passage between {START} and {END}:\n\
         \n\
         {marked}\n\
         \n\
         Selected passage:\n\
         \n\
         {selected}\n\
         \n\
         Task for the selected passage: {instruction}\n\
         \n\
         Reply with exactly this JSON shape and nothing else: {{\"alternatives\": [\"...\"]}}\n\
         - Give {count} distinct options, each a complete replacement for the selected passage only, \
         fitting the text around it.\n\
         - Write them in {syntax}, keeping the passage's formatting and line breaks where they still fit.\n\
         - No commentary, numbering or code fences.",
        instruction = request.action.instruction(),
    );
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("The draft is too long for a selection action".into());
    }
    Ok(prompt)
}

#[derive(Deserialize)]
struct Reply {
    alternatives: Vec<String>,
}

/// The options in pi's reply: the JSON object it contains, even when the
/// model wrapped it in a code fence or text. Empty and repeated options are
/// dropped.
fn parse(output: &str, count: usize) -> Result<Vec<String>, String> {
    let json = output
        .find('{')
        .zip(output.rfind('}'))
        .filter(|(start, end)| start < end)
        .map(|(start, end)| &output[start..=end]);
    let reply: Reply = json
        .and_then(|json| serde_json::from_str(json).ok())
        .ok_or("pi's reply had no alternatives")?;
    let mut options: Vec<String> = Vec::new();
    for option in reply.alternatives {
        let option = option.trim();
        if !option.is_empty() && !options.iter().any(|o| o == option) {
            options.push(option.to_string());
        }
    }
    options.truncate(count);
    if options.is_empty() {
        return Err("pi's reply had no alternatives".into());
    }
    Ok(options)
}

/// The whitespace before and after the selection's text, which options
/// replace without it.
fn surrounding_whitespace(selected: &str) -> (&str, &str) {
    let start = selected.len() - selected.trim_start().len();
    let end = selected.trim_end().len().max(start);
    (&selected[..start], &selected[end..])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn request(text: &str, selected: &str, action: Action) -> Request {
        let start = text.find(selected).unwrap();
        Request {
            session_file: "/drafts/x/session/s.jsonl".into(),
            cwd: "/repo".into(),
            agent_dir: "/profile".into(),
            target: Target::Github,
            text: text.to_string(),
            range: start..start + selected.len(),
            action,
        }
    }

    #[test]
    fn prompt_marks_the_selection_in_the_draft() {
        let request = request(
            "Hello there.\n\n-- a dash line\n",
            "-- a dash line\n",
            Action::Instruction("make it formal".into()),
        );
        let prompt = prompt(&request, 3).unwrap();
        assert!(prompt.starts_with("[Draftmux selection request]\n"));
        assert!(
            prompt.contains("Hello there.\n\n<<<SELECTION>>>-- a dash line\n<<<END SELECTION>>>\n")
        );
        assert!(prompt.contains("Selected passage:\n\n-- a dash line\n\nTask"));
        assert!(prompt.contains("Task for the selected passage: make it formal"));
        assert!(prompt.contains("Give 3 distinct options"));
        assert!(prompt.contains("GitHub-flavored Markdown"));
    }

    #[test]
    fn custom_actions_pass_the_prompt_unchanged() {
        let mut actions = draftmux_settings::default_selection_actions();
        actions.push(draftmux_settings::SelectionAction {
            name: "Custom".into(),
            prompt: "Keep the details.\n  Use a warmer tone.  ".into(),
        });
        for action in actions {
            let request = request(
                "Hi there",
                "there",
                Action::Instruction(action.prompt.clone()),
            );
            assert_eq!(request.action.instruction(), action.prompt);
            assert!(prompt(&request, 3).unwrap().contains(&format!(
                "Task for the selected passage: {}\n\n",
                action.prompt
            )));
        }
    }

    #[test]
    fn syntax_matches_the_draft_prompt() {
        let extension = include_str!("../../../pi-extension/draftmux.ts");
        for &target in Target::ALL {
            let literal = format!("\"{}\"", syntax(target));
            assert!(extension.contains(&literal), "{literal}");
        }
    }

    #[test]
    fn prompt_names_slack_syntax_and_refuses_blank_or_huge_selections() {
        let mut slack = request(
            "Ship it today",
            "today",
            Action::Instruction("Shorten it".into()),
        );
        slack.target = Target::Slack;
        assert!(prompt(&slack, 3).unwrap().contains("Slack mrkdwn"));
        slack.target = Target::Plain;
        assert!(
            prompt(&slack, 3)
                .unwrap()
                .contains("plain text, such as an email")
        );

        let blank = request("a \n b", " \n ", Action::Alternatives);
        assert!(prompt(&blank, 3).is_err());

        let long = "word ".repeat(60_000);
        let huge = request(&long, "word", Action::Alternatives);
        assert_eq!(
            prompt(&huge, 3).unwrap_err(),
            "The draft is too long for a selection action"
        );
    }

    #[test]
    fn parse_reads_json_inside_fences_and_text() {
        let output = "Sure!\n```json\n{\"alternatives\": [\"one\", \"two\\nlines\", \" one \", \"\", \"four\"]}\n```\n";
        assert_eq!(parse(output, 3).unwrap(), ["one", "two\nlines", "four"]);
        assert_eq!(parse(output, 1).unwrap(), ["one"]);
    }

    #[test]
    fn parse_rejects_replies_without_options() {
        for output in [
            "No JSON here",
            "{\"alternatives\": []}",
            "{\"other\": 1}",
            "} {",
        ] {
            assert_eq!(
                parse(output, 3).unwrap_err(),
                "pi's reply had no alternatives"
            );
        }
    }

    #[test]
    fn whitespace_around_the_selection_is_kept() {
        assert_eq!(surrounding_whitespace("  word \n"), ("  ", " \n"));
        assert_eq!(surrounding_whitespace("word"), ("", ""));
        assert_eq!(surrounding_whitespace("   "), ("   ", ""));
    }

    #[test]
    fn command_forks_into_the_temp_dir_with_selection_mode() {
        let mut config = SelectionConfig::default();
        let request = request("Hi there", "there", Action::Alternatives);
        let build = |config: &SelectionConfig| {
            let command = command(
                Command::new("pi"),
                "PROMPT",
                &request,
                Path::new("/ext/draftmux.ts"),
                Path::new("/tmp/s"),
                config,
            );
            let args: Vec<String> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            (command, args)
        };

        let (command, args) = build(&config);
        assert_eq!(
            args,
            [
                "-p",
                "PROMPT",
                "--fork",
                "/drafts/x/session/s.jsonl",
                "--session-dir",
                "/tmp/s",
                "-e",
                "/ext/draftmux.ts"
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/repo")));
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("PI_CODING_AGENT_DIR".as_ref(), Some("/profile".as_ref()))));
        assert!(envs.contains(&("DRAFTMUX_MODE".as_ref(), Some("selection".as_ref()))));
        assert!(envs.contains(&("DRAFTMUX_INSTRUCTIONS".as_ref(), None)));

        config.thinking = Some(ThinkingLevel::High);
        assert_eq!(build(&config).1.len(), 8, "thinking only with a model");
        config.model = Some("anthropic/claude-haiku-5-5".into());
        assert_eq!(
            build(&config).1[8..],
            [
                "--model",
                "anthropic/claude-haiku-5-5",
                "--thinking",
                "high"
            ]
        );

        config.context_files = false;
        assert_eq!(build(&config).1[8], "--no-context-files");
    }

    #[test]
    fn config_for_a_draft_comes_from_its_settings() {
        assert_eq!(
            SelectionConfig::for_draft(&DraftSettings::default(), 4),
            SelectionConfig {
                alternatives: 4,
                ..SelectionConfig::default()
            }
        );
        let settings = DraftSettings {
            instructions: None,
            context_files: false,
            selection_model: Some(draftmux_storage::ModelChoice {
                provider: "anthropic".into(),
                model_id: "claude-haiku-5-5".into(),
                thinking: "xhigh".into(),
            }),
        };
        assert_eq!(
            SelectionConfig::for_draft(&settings, 2),
            SelectionConfig {
                model: Some("anthropic/claude-haiku-5-5".into()),
                thinking: Some(ThinkingLevel::Xhigh),
                alternatives: 2,
                context_files: false,
            }
        );
    }

    /// A fake pi that records its arguments and checks the temp session dir
    /// exists while it runs.
    #[test]
    fn run_returns_options_and_removes_the_temp_dir() {
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("pi");
        fs::write(
            &script,
            r#"while [ "$1" ]; do [ "$1" = --session-dir ] && test -d "$2" && touch "$2/s.jsonl"; shift; done
echo '{"alternatives": ["over here", "here"]}'"#,
        )
        .unwrap();
        let mut pi = Command::new("/bin/sh");
        pi.arg(&script);
        let parent = tempfile::tempdir().unwrap();
        let mut request = request("Hi there\n", "there\n", Action::Alternatives);
        request.cwd = parent.path().to_path_buf();

        let options = run(
            pi,
            Path::new("/ext"),
            &request,
            &SelectionConfig::default(),
            parent.path(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(options, ["over here\n", "here\n"]);
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn run_reports_pi_errors() {
        let mut pi = Command::new("/bin/sh");
        pi.args(["-c", "echo 'No API key found for anthropic' >&2; exit 1"]);
        let parent = tempfile::tempdir().unwrap();
        let mut request = request(
            "Hi there",
            "there",
            Action::Instruction("Shorten it".into()),
        );
        request.cwd = parent.path().to_path_buf();
        let err = run(
            pi,
            Path::new("/ext"),
            &request,
            &SelectionConfig::default(),
            parent.path(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(err, "No API key found for anthropic");
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    /// Needs pi on `PATH` with a working default model in `~/.pi/agent`:
    /// `cargo test -j 2 -p draftmux --bin draftmux real_pi_selection -- --ignored`.
    #[test]
    #[ignore]
    fn real_pi_selection_call_leaves_the_draft_session_alone() {
        let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".pi/agent"));
        let cwd = tempfile::tempdir().unwrap();
        let draft_session = tempfile::tempdir().unwrap();
        let status = Command::new("pi")
            .args(["-p", "Reply with OK and nothing else.", "--session-dir"])
            .arg(draft_session.path())
            .current_dir(cwd.path())
            .env("PI_CODING_AGENT_DIR", &agent_dir)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "creating the source session");
        let session_file = fs::read_dir(draft_session.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let before = fs::read(&session_file).unwrap();

        let text = "Thanks for the detailed report! I'll take a look tomorrow.\n";
        let start = text.find("I'll").unwrap();
        let request = Request {
            session_file: session_file.clone(),
            cwd: cwd.path().to_path_buf(),
            agent_dir,
            target: Target::Github,
            text: text.to_string(),
            range: start..text.len(),
            // Tool calls are blocked; the model still answers in text.
            action: Action::Instruction(
                "First run `ls` with bash, then make it more casual.".into(),
            ),
        };
        let parent = tempfile::tempdir().unwrap();
        let extension = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../pi-extension/draftmux.ts"
        ));
        let options = run(
            Command::new("pi"),
            extension,
            &request,
            &SelectionConfig::default(),
            parent.path(),
            &AtomicBool::new(false),
        )
        .unwrap();
        eprintln!("options: {options:?}");
        assert!(!options.is_empty() && options.len() <= 3);
        assert!(options.iter().all(|o| o.ends_with('\n')));

        assert_eq!(
            fs::read(&session_file).unwrap(),
            before,
            "draft session unchanged"
        );
        assert_eq!(fs::read_dir(draft_session.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read_dir(parent.path()).unwrap().count(),
            0,
            "temp dir removed"
        );
    }
}
