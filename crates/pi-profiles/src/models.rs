use std::fmt;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// A model as pi names it: provider and model ID, compared exactly. IDs
/// differ between profiles (`anthropic/…` and `portkey/…` for one model).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }
}

/// `provider/id`, as pi's `--model` takes it.
impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.id)
    }
}

/// Whether a profile has a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Missing,
    /// The profile's models aren't known: still loading, or listing failed.
    Unknown,
}

/// `models` is the profile's list, `None` until it has loaded.
pub fn availability(models: Option<&[ModelRef]>, model: &ModelRef) -> Availability {
    match models {
        None => Availability::Unknown,
        Some(models) if models.contains(model) => Availability::Available,
        Some(_) => Availability::Missing,
    }
}

/// How long `pi --list-models` may take. pi bounds its own availability
/// check at 15 s; startup and extension loading come on top.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for the output pipes to close once pi has exited.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// The models pi has credentials for in the profile `agent_dir`, from
/// `pi --list-models` run in `cwd`. `command` is the pi executable with the
/// environment to run it in.
///
/// Blocks for as long as pi takes, typically 2–3 s. `--list-models` loads
/// the profile's extensions, so providers they register are listed, but
/// exits before starting a session, so no extension session hooks run.
pub fn list_models(command: Command, agent_dir: &Path, cwd: &Path) -> Result<Vec<ModelRef>> {
    list_models_within(command, agent_dir, cwd, LIST_TIMEOUT)
}

fn list_models_within(
    mut command: Command,
    agent_dir: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Result<Vec<ModelRef>> {
    command
        .arg("--list-models")
        .current_dir(cwd)
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // The environment can carry values from a pi session the app was
    // launched from.
    for var in [
        "DRAFTMUX_MODE",
        "DRAFTMUX_DRAFT_PATH",
        "DRAFTMUX_INSTRUCTIONS",
    ] {
        command.env_remove(var);
    }
    let mut child = command.spawn().context("starting pi")?;
    let stdout = read_in_background(child.stdout.take().expect("stdout is piped"));
    let stderr = read_in_background(child.stderr.take().expect("stderr is piped"));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting for pi")? {
            break status;
        }
        if Instant::now() >= deadline {
            // SAFETY: the child leads its own process group.
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
            let _ = child.wait();
            bail!(
                "pi --list-models didn't finish within {} s",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    if !status.success() {
        let stderr = stderr.recv_timeout(DRAIN_GRACE).unwrap_or_default();
        let reason = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no error output");
        bail!("pi --list-models failed ({status}): {}", reason.trim());
    }
    parse_list(&stdout)
}

fn read_in_background(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    rx
}

/// Parses `pi --list-models` output (pi 1.5.0, `cli/list-models.ts`): a
/// header row `provider model context …` then one row per model with
/// space-padded columns, or a "No models available" message.
fn parse_list(stdout: &str) -> Result<Vec<ModelRef>> {
    let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        bail!("pi --list-models printed nothing");
    };
    if header.starts_with("No models available") {
        return Ok(Vec::new());
    }
    if !header.split_whitespace().take(2).eq(["provider", "model"]) {
        bail!("unexpected output from pi --list-models: {}", header.trim());
    }
    lines
        .map(|line| {
            let mut columns = line.split_whitespace();
            match (columns.next(), columns.next()) {
                (Some(provider), Some(id)) => Ok(ModelRef::new(provider, id)),
                _ => bail!("unexpected row from pi --list-models: {}", line.trim()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from pi 1.5.0.
    const TABLE: &str = "\
provider      model                                    context  max-out  thinking  images
deepseek      deepseek-flash                           1M       384K     yes       yes
google        gemini-3.5-pro                           1M       65.5K    yes       yes
openai-codex  gpt-5.5                                  400K     128K     yes       yes
";

    #[test]
    fn parses_the_table() {
        assert_eq!(
            parse_list(TABLE).unwrap(),
            [
                ModelRef::new("deepseek", "deepseek-flash"),
                ModelRef::new("google", "gemini-3.5-pro"),
                ModelRef::new("openai-codex", "gpt-5.5"),
            ]
        );
    }

    #[test]
    fn no_models_is_an_empty_list() {
        let output = "No models available. Use /login or set an API key environment variable.\n";
        assert!(parse_list(output).unwrap().is_empty());
    }

    #[test]
    fn other_output_is_an_error() {
        assert!(parse_list("").is_err());
        assert!(parse_list("Usage: pi [options]\n").is_err());
        assert!(parse_list("provider  model\nlonely\n").is_err());
    }

    #[test]
    fn availability_needs_a_loaded_list() {
        let opus = ModelRef::new("anthropic", "claude-opus-5-5");
        let list = [ModelRef::new("portkey", "claude-opus-5-5")];
        assert_eq!(availability(None, &opus), Availability::Unknown);
        assert_eq!(availability(Some(&list), &opus), Availability::Missing);
        assert_eq!(availability(Some(&list), &list[0]), Availability::Available);
    }

    /// A shell script standing in for pi. Run through `sh -c` rather than
    /// as a new executable, which macOS may take seconds to vet on first
    /// launch.
    fn fake_pi(body: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(body).arg("pi");
        command.env("DRAFTMUX_MODE", "selection");
        command
    }

    #[test]
    fn runs_pi_for_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!(
            r#"[ "$1" = --list-models ] || exit 3
[ -z "$DRAFTMUX_MODE" ] || exit 4
[ "$PWD" = "{cwd}" ] || exit 5
echo 'Warning: errors loading models.json' >&2
printf 'provider  model\n%s  m1\n' "$(basename "$PI_CODING_AGENT_DIR")""#,
            cwd = dir.path().canonicalize().unwrap().display()
        );
        let models =
            list_models(fake_pi(&script), Path::new("/profiles/work"), dir.path()).unwrap();
        assert_eq!(models, [ModelRef::new("work", "m1")]);
    }

    #[test]
    fn failure_reports_the_last_stderr_line() {
        let dir = tempfile::tempdir().unwrap();
        let command = fake_pi("echo first >&2; echo 'Error: bad models.json' >&2; exit 1");
        let err = list_models(command, dir.path(), dir.path()).unwrap_err();
        assert!(err.to_string().ends_with("Error: bad models.json"), "{err}");
    }

    #[test]
    fn timeout_kills_pi_and_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let command = fake_pi(&format!(
            "sleep 30 & echo $! > '{}'; wait",
            pid_file.display()
        ));
        let started = Instant::now();
        let err = list_models_within(command, dir.path(), dir.path(), Duration::from_secs(1))
            .unwrap_err();
        assert!(err.to_string().contains("didn't finish"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));

        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: signal 0 only checks that the process exists.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the background child survived"
        );
    }
}
