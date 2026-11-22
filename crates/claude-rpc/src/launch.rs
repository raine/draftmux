//! The `claude` command line and environment of a draft's process.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use crate::instructions;
use crate::session::{DraftSession, Start};

/// Variables passed through from [`LaunchSpec::env`]. Everything else is
/// dropped: an app started from a Claude Code session inherits variables
/// such as `CLAUDECODE` and `CLAUDE_CODE_SESSION_ID` that change how the CLI
/// behaves.
const ENV_ALLOWLIST: &[&str] = &[
    "HOME", "PATH", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG", "TERM",
];
/// What Windows programs need on top: Node's runtime, the user's folders,
/// the temporary directory and the Git Bash that Claude Code runs commands
/// in. Names there are case-insensitive.
#[cfg(windows)]
const WINDOWS_ENV_ALLOWLIST: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "USERPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "CommonProgramFiles",
    "TEMP",
    "TMP",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "CLAUDE_CODE_GIT_BASH_PATH",
];

/// The oldest Claude Code whose protocol this crate was checked against.
const MIN_VERSION: [u32; 3] = [2, 1, 293];
/// `program --version` with the variables of `env` a `claude` process
/// gets. Its output goes to [`check_version`].
pub fn version_command(program: &Path, env: &[(OsString, OsString)]) -> Command {
    let mut command = clean_command(program, env);
    command.arg("--version");
    command
}

/// `program` with only the variables of `env` a `claude` process gets.
pub(crate) fn clean_command(program: &Path, env: &[(OsString, OsString)]) -> Command {
    let mut command = Command::new(program);
    command.env_clear().envs(allowed(env).map(|(k, v)| (k, v)));
    command
}

/// The version `program` printed for `--version` when this crate supports
/// it, else a message for the user.
pub fn check_version(program: &Path, output: &str) -> Result<String, String> {
    supported(output.trim()).map_err(|found| {
        format!(
            "Draftmux needs Claude Code {} or later; {} reports {found}. Update Claude Code, \
             then restart Draftmux.",
            MIN_VERSION.map(|n| n.to_string()).join("."),
            program.display(),
        )
    })
}

/// The version in `claude --version` output such as `2.1.293 (Claude
/// Code)` when it is supported, else what was found.
fn supported(output: &str) -> Result<String, String> {
    let version = output.split_whitespace().next().unwrap_or_default();
    let parts: Option<Vec<u32>> = version.split('.').map(|n| n.parse().ok()).collect();
    match parts {
        Some(parts) if parts.len() == 3 && parts[..] >= MIN_VERSION[..] => Ok(version.into()),
        _ if output.is_empty() => Err("nothing".into()),
        _ => Err(format!("\"{output}\"")),
    }
}

/// The home directory a `claude` process gets, found as Node's
/// `os.homedir()` finds it: the first `HOME` in `vars`, else on Windows the
/// first `USERPROFILE`, else this process's.
pub(crate) fn home<'a>(vars: impl Iterator<Item = &'a (OsString, OsString)> + Clone) -> PathBuf {
    let find = |name| {
        vars.clone()
            .find(|(key, _)| is_var(key, name))
            .map(|(_, home)| PathBuf::from(home))
    };
    find("HOME")
        .or_else(|| {
            if cfg!(windows) {
                find("USERPROFILE")
            } else {
                None
            }
        })
        .or_else(std::env::home_dir)
        .unwrap_or_default()
}

/// The variables of `env` a `claude` process gets.
fn allowed(env: &[(OsString, OsString)]) -> impl Iterator<Item = &(OsString, OsString)> {
    env.iter().filter(|(key, _)| {
        #[cfg(windows)]
        if WINDOWS_ENV_ALLOWLIST.iter().any(|name| is_var(key, name)) {
            return true;
        }
        ENV_ALLOWLIST.iter().any(|name| is_var(key, name))
            || key.to_string_lossy().starts_with("LC_")
    })
}

/// Whether `key` names the variable `name`, ignoring case on Windows.
fn is_var(key: &OsString, name: &str) -> bool {
    if cfg!(windows) {
        key.eq_ignore_ascii_case(name)
    } else {
        key == name
    }
}

/// The user settings that say how Claude Code reaches and authenticates with
/// its API, besides `env`: commands that print a key or refresh cloud
/// credentials.
const LOGIN_SETTINGS: &[&str] = &["apiKeyHelper", "awsAuthRefresh", "awsCredentialExport"];

/// Gives `command` the login parts of the user settings in `config_dir`
/// (`settings.json`): their `env` as variables and [`LOGIN_SETTINGS`] as
/// `--settings`. A `claude` that loads no setting sources skips that file,
/// and with it a gateway's `ANTHROPIC_BASE_URL` and key helper, and then
/// isn't logged in. Hooks, permissions and the rest stay unloaded.
/// Variables set on `command` later override these. A missing or unreadable
/// file adds nothing.
pub(crate) fn apply_login(command: &mut Command, config_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(config_dir.join("settings.json")) else {
        return;
    };
    let Ok(Value::Object(settings)) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    if let Some(Value::Object(env)) = settings.get("env") {
        for (key, value) in env {
            let value = match value {
                Value::String(value) => value.clone(),
                Value::Number(_) | Value::Bool(_) => value.to_string(),
                _ => continue,
            };
            command.env(key, value);
        }
    }
    let login: Map<String, Value> = settings
        .into_iter()
        .filter(|(key, _)| LOGIN_SETTINGS.contains(&key.as_str()))
        .collect();
    if !login.is_empty() {
        command
            .arg("--settings")
            .arg(Value::Object(login).to_string());
    }
}

/// Everything needed to start a draft's `claude` process.
pub struct LaunchSpec {
    /// The `claude` executable itself. A shell alias can add flags such as
    /// `--dangerously-skip-permissions`, so resolve the binary's path.
    pub program: PathBuf,
    pub cwd: PathBuf,
    /// The environment to take allowlisted variables from, such as the
    /// login-shell environment. `LANG` and `LC_*` are kept with the basics.
    pub env: Vec<(OsString, OsString)>,
    /// Variables set as given. A [`Session::Draft`] sets or unsets
    /// `CLAUDE_CONFIG_DIR` from its own `config_dir`.
    pub extra_env: Vec<(OsString, OsString)>,
    /// The model of the first process. A successful [`Agent::set_model`]
    /// replaces it for later processes, since an explicit `--model`
    /// overrides the model a resumed session last used.
    ///
    /// [`Agent::set_model`]: crate::Agent::set_model
    pub model: Option<String>,
    /// The effort of the first process (`--effort`), such as `high`; none
    /// for the model's default. A successful [`Agent::set_effort`] replaces
    /// it for later processes.
    ///
    /// [`Agent::set_effort`]: crate::Agent::set_effort
    pub effort: Option<String>,
    /// The built-in tools the model can use; empty disables them all.
    pub tools: Vec<Tool>,
    /// The settings files to load; empty loads none, which also skips
    /// `CLAUDE.md`, though a resumed or forked session gets back the
    /// instruction files its transcript recorded (see [`instructions`]).
    /// Settings can enable executable hooks. The user settings' login parts
    /// apply either way.
    ///
    /// [`instructions`]: crate::instructions
    pub setting_sources: Vec<SettingSource>,
    pub session: Option<Session>,
    /// Registered with `initialize`; each call arrives as
    /// [`AgentEvent::Hook`](crate::AgentEvent::Hook). Unlike permission
    /// prompts, these also run for tools the settings already allow.
    pub hooks: Vec<PreToolUseHook>,
    /// Ask for tool permission through
    /// [`AgentEvent::Permission`](crate::AgentEvent::Permission)
    /// (`--permission-prompt-tool stdio`).
    pub permission_prompt: bool,
    /// Text added to Claude Code's system prompt
    /// (`--append-system-prompt`). Claude Code 2.1.293 ignores it when it
    /// resumes or forks a session.
    pub system_prompt: Option<String>,
}

/// The tools a draft's agent may get. Task and background shells are left
/// out: a process outliving the call escapes the process group, so stopping
/// the agent couldn't end it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Read,
    Edit,
    Write,
    Glob,
    Grep,
    Bash,
}

impl Tool {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Tool::Read => "Read",
            Tool::Edit => "Edit",
            Tool::Write => "Write",
            Tool::Glob => "Glob",
            Tool::Grep => "Grep",
            Tool::Bash => "Bash",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    User,
    Project,
    Local,
}

impl SettingSource {
    fn name(self) -> &'static str {
        match self {
            SettingSource::User => "user",
            SettingSource::Project => "project",
            SettingSource::Local => "local",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// Start a session with this id (`--session-id`, a UUID).
    New(String),
    /// Continue a session by id or transcript path (`--resume`). A missing
    /// session makes the process exit with "No conversation found".
    Resume(OsString),
    /// A draft's session, which each process starts, forks or resumes
    /// depending on what exists then.
    Draft(DraftSession),
}

/// A `PreToolUse` hook for the tools `matcher` matches, such as
/// `"Edit|Write"`, or for every tool call without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreToolUseHook {
    pub matcher: Option<String>,
    /// Seconds the CLI waits for the answer. A hook that times out blocks
    /// the tool call.
    pub timeout: Option<u32>,
}

impl LaunchSpec {
    /// `claude` in stream-json mode with an allowlisted environment,
    /// continuing the session as `start` says.
    pub(crate) fn command(
        &self,
        model: Option<&str>,
        effort: Option<&str>,
        tools: &[Tool],
        start: Option<&Start>,
    ) -> Command {
        let mut command = clean_command(&self.program, &self.env);
        command
            .args([
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--strict-mcp-config",
            ])
            .arg("--setting-sources")
            .arg(join(self.setting_sources.iter().map(|s| s.name())))
            .arg("--tools")
            .arg(join(tools.iter().map(|t| t.name())));
        if self.permission_prompt {
            command.args(["--permission-prompt-tool", "stdio"]);
        }
        if let Some(prompt) = &self.system_prompt {
            command.arg("--append-system-prompt").arg(prompt);
        }
        if let Some(model) = model {
            command.arg("--model").arg(model);
        }
        if let Some(effort) = effort {
            command.arg("--effort").arg(effort);
        }
        match start {
            Some(Start::New(id)) => {
                command.arg("--session-id").arg(id);
            }
            Some(Start::Fork { snapshot, id }) => {
                instructions::add_dirs(&mut command, snapshot);
                command
                    .arg("--resume")
                    .arg(snapshot)
                    .args(["--fork-session", "--session-id"])
                    .arg(id);
            }
            Some(Start::Resume(target)) => {
                instructions::add_dirs(&mut command, Path::new(target));
                command.arg("--resume").arg(target);
            }
            None => {}
        }
        command.current_dir(&self.cwd);
        apply_login(&mut command, &self.config_dir());
        command.envs(self.extra_env.iter().map(|(k, v)| (k, v)));
        // Claude must write where the agent looks for the transcript.
        if let Some(Session::Draft(draft)) = &self.session {
            match &draft.config_dir {
                Some(dir) => command.env("CLAUDE_CONFIG_DIR", dir),
                None => command.env_remove("CLAUDE_CONFIG_DIR"),
            };
        }
        command
            // Reports `session_state_changed`, which tells when a run is
            // over.
            .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1");
        command
    }
}

impl LaunchSpec {
    /// The directory the process keeps its settings and transcripts in, as
    /// [`LaunchSpec::command`] sets `CLAUDE_CONFIG_DIR`.
    fn config_dir(&self) -> PathBuf {
        let home = || home(self.env.iter()).join(".claude");
        if let Some(Session::Draft(draft)) = &self.session {
            return draft.config_dir.clone().unwrap_or_else(home);
        }
        self.extra_env
            .iter()
            .rev()
            .find(|(key, _)| is_var(key, "CLAUDE_CONFIG_DIR"))
            .map_or_else(home, |(_, dir)| PathBuf::from(dir))
    }
}

fn join<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            program: PathBuf::from("/opt/claude/bin/claude"),
            cwd: PathBuf::from("/drafts/one"),
            env: [
                ("HOME", "/home/u"),
                ("PATH", "/usr/bin"),
                ("LC_ALL", "C"),
                ("CLAUDECODE", "1"),
                ("CLAUDE_CODE_SESSION_ID", "parent"),
                ("ANTHROPIC_API_KEY", "inherited"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
            extra_env: vec![("CLAUDE_CONFIG_DIR".into(), "/cfg".into())],
            model: Some("haiku".into()),
            effort: None,
            tools: vec![Tool::Read, Tool::Edit],
            setting_sources: vec![],
            session: Some(Session::New("0b5c".into())),
            hooks: vec![],
            permission_prompt: true,
            system_prompt: None,
        }
    }

    #[test]
    fn a_draft_is_logged_in_from_its_config_dir_s_settings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("settings.json"),
            r#"{"env": {"ANTHROPIC_BASE_URL": "https://gateway"}, "apiKeyHelper": "/bin/key", "hooks": {}}"#,
        )
        .unwrap();
        let mut spec = spec();
        spec.extra_env = vec![("CLAUDE_CONFIG_DIR".into(), tmp.path().into())];
        let command = spec.command(None, None, &[], None);
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
        let at = args.iter().position(|arg| arg == "--settings").unwrap();
        assert_eq!(args[at + 1], r#"{"apiKeyHelper":"/bin/key"}"#);
        assert!(command.get_envs().any(|env| env
            == (
                "ANTHROPIC_BASE_URL".as_ref(),
                Some("https://gateway".as_ref())
            )));

        // No settings file, as for the default spec's /cfg.
        let args: Vec<_> = super::tests::spec()
            .command(None, None, &[], None)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.contains(&"--settings".to_string()));
    }

    #[test]
    fn versions_older_than_the_checked_one_are_refused() {
        assert_eq!(supported("2.1.293 (Claude Code)"), Ok("2.1.293".into()));
        assert_eq!(supported("2.2.0 (Claude Code)"), Ok("2.2.0".into()));
        assert_eq!(supported("3.0.1"), Ok("3.0.1".into()));
        assert_eq!(
            supported("2.1.292 (Claude Code)"),
            Err("\"2.1.292 (Claude Code)\"".into())
        );
        assert_eq!(supported("1.0"), Err("\"1.0\"".into()));
        assert_eq!(supported(""), Err("nothing".into()));
    }

    #[test]
    fn the_version_check_gets_only_the_allowlisted_variables() {
        let env = [
            ("PATH".into(), "/usr/bin".into()),
            ("ANTHROPIC_API_KEY".into(), "secret".into()),
        ];
        let command = version_command(Path::new("/opt/claude"), &env);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--version"]);
        let vars: Vec<_> = command.get_envs().map(|(k, _)| k).collect();
        assert_eq!(vars, ["PATH"]);
        let program = Path::new("/opt/claude");
        assert_eq!(
            check_version(program, "2.1.300 (Claude Code)\n"),
            Ok("2.1.300".into())
        );
        let err = check_version(program, "2.0.1 (Claude Code)").unwrap_err();
        assert!(err.contains("needs Claude Code 2.1.293 or later"), "{err}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_variables_pass_in_any_case() {
        let env = [
            ("Path".into(), r"C:\Windows".into()),
            (
                "claude_code_git_bash_path".into(),
                r"D:\Git\bin\bash.exe".into(),
            ),
            ("CLAUDECODE".into(), "1".into()),
        ];
        let command = version_command(Path::new(r"C:\claude.exe"), &env);
        let mut vars: Vec<_> = command
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        vars.sort();
        assert_eq!(vars, ["Path", "claude_code_git_bash_path"]);
    }

    #[test]
    fn command_line_and_environment() {
        let spec = spec();
        let command = spec.command(
            Some("sonnet"),
            Some("high"),
            &spec.tools,
            Some(&Start::New("0b5c".into())),
        );
        assert_eq!(command.get_program(), "/opt/claude/bin/claude");
        assert_eq!(command.get_current_dir().unwrap(), spec.cwd);
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--tools",
                "Read,Edit",
                "--permission-prompt-tool",
                "stdio",
                "--model",
                "sonnet",
                "--effort",
                "high",
                "--session-id",
                "0b5c",
            ]
        );
        let mut env: Vec<_> = command
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?, v?.to_str()?)))
            .collect();
        env.sort();
        assert_eq!(
            env,
            [
                ("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1"),
                ("CLAUDE_CONFIG_DIR", "/cfg"),
                ("HOME", "/home/u"),
                ("LC_ALL", "C"),
                ("PATH", "/usr/bin"),
            ]
        );
    }

    #[test]
    fn a_fork_gets_back_the_instruction_files_of_its_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshot = tmp.path().join("source.jsonl");
        let line = serde_json::json!({
            "type": "attachment",
            "attachment": {"type": "instructions", "files": [
                {"path": tmp.path().join("CLAUDE.md"), "type": "Project", "content": "x"},
            ]},
        });
        fs::write(&snapshot, format!("{line}\n")).unwrap();
        let spec = spec();
        let command = spec.command(
            None,
            None,
            &[],
            Some(&Start::Fork {
                snapshot: snapshot.clone(),
                id: "0b5c".into(),
            }),
        );
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        let dir = tmp.path().to_str().unwrap();
        let resume = args.iter().position(|a| *a == "--resume").unwrap();
        assert_eq!(&args[resume - 2..resume], ["--add-dir", dir]);
        assert!(command.get_envs().any(|(k, v)| {
            k == "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD" && v == Some("1".as_ref())
        }));
    }

    #[test]
    fn resume_without_tools_or_model() {
        let mut spec = spec();
        spec.setting_sources = vec![SettingSource::Project, SettingSource::Local];
        spec.permission_prompt = false;
        spec.system_prompt = Some("Be brief.".into());
        let command = spec.command(
            None,
            None,
            &[],
            Some(&Start::Resume("/drafts/one/s.jsonl".into())),
        );
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            &args[7..],
            [
                "--setting-sources",
                "project,local",
                "--tools",
                "",
                "--append-system-prompt",
                "Be brief.",
                "--resume",
                "/drafts/one/s.jsonl",
            ]
        );
    }
}
