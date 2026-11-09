use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::capture::{fallback_env, get, parse_env};

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn var<'a>(env: &'a ShellEnv, name: &str) -> Option<&'a OsString> {
    get(&env.vars, name)
}

fn base_env(home: &Path) -> Vars {
    vec![
        ("HOME".into(), home.into()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
    ]
}

const TIMEOUT: Duration = Duration::from_secs(5);

fn capture(shell: impl Into<PathBuf>, base_env: Vars, timeout: Duration) -> ShellEnv {
    ShellEnv::capture_with(&CaptureOptions {
        shell: shell.into(),
        base_env,
        timeout,
    })
}

#[test]
fn parse_env_keeps_values_verbatim() {
    let mut output = b"MULTI=a\nb\0EQ=x=y\0EMPTY=\0SHLVL=2\0PWD=/tmp\0_=/usr/bin/env\0".to_vec();
    output.extend_from_slice(b"RAW=\xff\xfe\0=bad\0noequals\0");
    let vars = parse_env(&output);
    let expected: Vars = vec![
        ("MULTI".into(), "a\nb".into()),
        ("EQ".into(), "x=y".into()),
        ("EMPTY".into(), "".into()),
        ("RAW".into(), OsString::from_vec(vec![0xff, 0xfe])),
    ];
    assert_eq!(vars, expected);
}

#[test]
fn find_executable_takes_first_absolute_executable_file() {
    let dir = tempfile::tempdir().unwrap();
    let (not_exec, is_dir, first, second) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
        dir.path().join("d"),
    );
    for d in [&not_exec, &is_dir, &first, &second] {
        fs::create_dir(d).unwrap();
    }
    fs::write(not_exec.join("pi"), "").unwrap();
    fs::create_dir(is_dir.join("pi")).unwrap();
    write_executable(&first.join("pi"), "");
    write_executable(&second.join("pi"), "");
    let link_dir = dir.path().join("links");
    fs::create_dir(&link_dir).unwrap();
    std::os::unix::fs::symlink(second.join("pi"), link_dir.join("pi")).unwrap();

    let path = std::env::join_paths([&not_exec, &is_dir, &first, &second]).unwrap();
    assert_eq!(find_executable("pi", Some(&path)), Some(first.join("pi")));
    let path = std::env::join_paths([&link_dir, &first]).unwrap();
    assert_eq!(
        find_executable("pi", Some(&path)),
        Some(link_dir.join("pi"))
    );
    // Relative entries would resolve against whatever the cwd is.
    let path = OsString::from(format!("c:{}", second.display()));
    assert_eq!(find_executable("pi", Some(&path)), Some(second.join("pi")));
    assert_eq!(find_executable("pi", None), None);
}

#[test]
fn fallback_env_appends_common_install_dirs() {
    let env = fallback_env(base_env(Path::new("/Users/someone")));
    let path = get(&env, "PATH").unwrap();
    assert_eq!(
        path,
        if cfg!(target_os = "macos") {
            "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin:/Users/someone/.local/bin"
        } else {
            "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/Users/someone/.local/bin"
        }
    );
}

#[test]
fn captures_despite_rc_noise_and_failing_exit() {
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("noisy-sh");
    // Ignores its flags, prints around the real script, then fails.
    write_executable(
        &shell,
        "#!/bin/sh\necho banner\nprintf 'garbage\\0X=1\\0'\nexport FROM_RC=yes\nsh -c \"$4\"\necho after\nexit 1\n",
    );
    let env = capture(shell, base_env(dir.path()), TIMEOUT);
    assert_eq!(env.capture_error, None);
    assert_eq!(var(&env, "FROM_RC").unwrap(), "yes");
    assert_eq!(var(&env, "X"), None);
}

#[test]
fn hanging_shell_falls_back_within_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("hang-sh");
    write_executable(&shell, "#!/bin/sh\nsleep 30 &\nsleep 30\n");
    let started = Instant::now();
    let env = capture(shell, base_env(dir.path()), Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        env.capture_error
            .as_deref()
            .unwrap()
            .contains("didn't finish")
    );
    let path = var(&env, "PATH").unwrap().to_str().unwrap();
    assert!(path.contains("/usr/local/bin"), "{path}");
}

#[test]
fn shell_hanging_after_printing_env_is_not_waited_for() {
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("logout-sh");
    // Like a slow `.zlogout` or an exit trap.
    write_executable(&shell, "#!/bin/sh\nsh -c \"$4\"\nsleep 30\n");
    let started = Instant::now();
    let env = capture(shell, base_env(dir.path()), TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(env.capture_error, None);
}

#[test]
fn missing_shell_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let env = capture(
        dir.path().join("no-such-shell"),
        base_env(dir.path()),
        TIMEOUT,
    );
    assert!(
        env.capture_error
            .as_deref()
            .unwrap()
            .contains("couldn't start")
    );
    let missing = env.pi().unwrap_err().to_string();
    assert!(missing.contains("pi wasn't found"), "{missing}");
    assert!(missing.contains("couldn't start"), "{missing}");
}

/// The real zsh reads `.zshrc` (where API keys usually live), its PATH
/// additions resolve tools, and spawned tools get exactly that environment.
#[test]
fn zsh_login_env_reaches_spawned_tools() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(
        &bin.join("pi"),
        "#!/bin/sh\nprintf '%s|%s' \"$TEST_API_KEY\" \"${INHERITED-unset}\"\n",
    );
    fs::write(
        dir.path().join(".zshrc"),
        format!(
            "export TEST_API_KEY=secret-from-zshrc\nexport PATH={}:$PATH\nunset INHERITED\n",
            bin.display()
        ),
    )
    .unwrap();
    let mut base = base_env(dir.path());
    base.push(("ZDOTDIR".into(), dir.path().into()));
    base.push(("INHERITED".into(), "from-app".into()));

    let env = capture("/bin/zsh", base, TIMEOUT);
    assert_eq!(env.capture_error, None);
    assert_eq!(env.pi().unwrap(), bin.join("pi"));
    assert_eq!(env.oxfmt(), None);

    let output = env.command(env.pi().unwrap()).output().unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "secret-from-zshrc|unset"
    );
}

#[test]
fn tcsh_is_started_as_login_shell() {
    let shell = Path::new("/bin/tcsh");
    if !shell.exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".login"), "setenv FROM_LOGIN yes\n").unwrap();
    let env = capture(shell, base_env(dir.path()), TIMEOUT);
    assert_eq!(env.capture_error, None);
    assert_eq!(var(&env, "FROM_LOGIN").unwrap(), "yes");
}

#[test]
fn debug_omits_values() {
    let env = ShellEnv::new(vec![("TEST_API_KEY".into(), "secret".into())], None);
    let debug = format!("{env:?}");
    assert!(debug.contains("TEST_API_KEY"));
    assert!(!debug.contains("secret"));
}

#[test]
fn shell_exiting_without_env_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("broken-sh");
    write_executable(&shell, "#!/bin/sh\necho 'rc error'\nexit 1\n");
    let env = capture(shell, base_env(dir.path()), TIMEOUT);
    assert!(
        env.capture_error
            .as_deref()
            .unwrap()
            .contains("exited without printing")
    );
}

#[test]
fn flooding_shell_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("flood-sh");
    write_executable(&shell, "#!/bin/sh\nexec /usr/bin/yes\n");
    let started = Instant::now();
    let env = capture(shell, base_env(dir.path()), Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        env.capture_error
            .as_deref()
            .unwrap()
            .contains("didn't finish")
    );
}
