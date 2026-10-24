mod support;

#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::process::{Command, Stdio};

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};

#[test]
fn env_exec_propagates_the_child_exit_code() {
    let root = TestDir::new("cli-exec-exit-code");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let exec = run_ocm(
        &cwd,
        &env,
        &["env", "exec", "demo", "--", "sh", "-lc", "exit 7"],
    );
    assert_eq!(exec.status.code(), Some(7), "{}", stderr(&exec));
}

#[test]
fn env_exec_requires_double_dash_before_the_command() {
    let root = TestDir::new("cli-exec-missing-separator");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let exec = run_ocm(
        &cwd,
        &env,
        &["env", "exec", "demo", "sh", "-lc", "printf ok"],
    );
    assert_eq!(exec.status.code(), Some(1));
    assert!(stderr(&exec).contains("env exec requires -- before the command"));
}

#[test]
fn env_exec_mentions_the_command_when_process_launch_fails() {
    let root = TestDir::new("cli-exec-missing-command");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let exec = run_ocm(
        &cwd,
        &env,
        &["env", "exec", "demo", "--", "definitely-not-a-real-command"],
    );
    assert_eq!(exec.status.code(), Some(1));
    assert!(stderr(&exec).contains("failed to run \"definitely-not-a-real-command\""));
}

#[test]
fn env_run_requires_double_dash_before_openclaw_arguments() {
    let root = TestDir::new("cli-run-missing-separator");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(&cwd, &env, &["env", "run", "demo", "-lc", "printf ok"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("env run requires -- before OpenClaw arguments"));
}

#[test]
fn version_rejects_trailing_arguments() {
    let root = TestDir::new("cli-version-trailing");
    let env = ocm_env(&root);

    let version = run_ocm(root.path(), &env, &["--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert!(!stdout(&version).trim().is_empty());

    for flag in ["--version", "-v"] {
        let invalid = run_ocm(root.path(), &env, &[flag, "extra"]);
        assert_eq!(invalid.status.code(), Some(1));
        assert!(stderr(&invalid).contains("unexpected arguments: extra"));
    }
}

#[cfg(unix)]
#[test]
fn startup_rejects_non_utf8_environment_values_without_panicking() {
    let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .arg("--version")
        .env("OCM_INVALID_UTF8_TEST", OsString::from_vec(vec![0xff]))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OCM_INVALID_UTF8_TEST contains a non-UTF-8 value"));
    assert!(!stderr.contains("panicked"));
}

#[cfg(unix)]
#[test]
fn startup_rejects_non_utf8_arguments_without_panicking() {
    let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .arg(OsString::from_vec(vec![0xff]))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("command arguments must be valid UTF-8"));
    assert!(!stderr.contains("panicked"));
}

#[cfg(unix)]
#[test]
fn closed_stdout_terminates_quietly() {
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .arg("--version")
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
}

#[cfg(unix)]
#[test]
fn closed_stderr_preserves_command_failure() {
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .args(["--version", "extra"])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(OwnedFd::from(writer)))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
}

#[cfg(unix)]
const SIGNAL_RECORDING_OPENCLAW: &str = r#"#!/bin/sh
marker="$OCM_TEST_SIGNAL_MARKER"
stop() {
  echo "signal $1" >> "$marker"
  sleep 1
  echo stopped >> "$marker"
  exit 0
}
trap 'stop TERM' TERM
trap 'stop INT' INT
trap 'stop HUP' HUP
echo "ready $$" >> "$marker"
# Bounded so a failed run cannot leave the fixture behind.
i=0
while [ "$i" -lt 300 ]; do
  sleep 0.1
  i=$((i + 1))
done
echo expired >> "$marker"
"#;

/// Environments `bound` (runtime) and `launched` (launcher, through the
/// platform shell) whose OpenClaw records the signals it receives.
#[cfg(unix)]
struct SignalFixture {
    root: TestDir,
    cwd: std::path::PathBuf,
    env: std::collections::BTreeMap<String, String>,
    openclaw: String,
}

#[cfg(unix)]
impl SignalFixture {
    fn new(name: &str) -> Self {
        let root = TestDir::new(name);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let openclaw = root.child("bin/openclaw");
        crate::support::write_executable_script(&openclaw, SIGNAL_RECORDING_OPENCLAW);
        let openclaw = crate::support::path_string(&openclaw);
        let env = ocm_env(&root);
        for args in [
            vec!["runtime", "add", "fake", "--path", openclaw.as_str()],
            vec!["launcher", "add", "fake", "--command", openclaw.as_str()],
            vec!["env", "create", "bound", "--runtime", "fake"],
            vec!["env", "create", "launched", "--launcher", "fake"],
        ] {
            let output = run_ocm(&cwd, &env, &args);
            assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        }
        Self {
            root,
            cwd,
            env,
            openclaw,
        }
    }

    fn command(&self, args: &[&str], marker: &std::path::Path) -> Command {
        let mut command = Command::new(crate::support::ocm_test_binary_path());
        command
            .current_dir(&self.cwd)
            .args(args)
            .env_clear()
            .envs(&self.env)
            .env("OCM_TEST_SIGNAL_MARKER", marker);
        command
    }

    fn marker(&self, label: &str) -> std::path::PathBuf {
        self.root.child(format!("signals-{label}.log"))
    }
}

#[cfg(unix)]
fn wait_for_marker(marker: &std::path::Path, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let contents = fs::read_to_string(marker).unwrap_or_default();
        if contents.contains(needle) {
            return contents;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture never wrote {needle:?}: {contents:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn ready_pid(marker: &std::path::Path) -> libc::pid_t {
    wait_for_marker(marker, "ready")
        .split_whitespace()
        .nth(1)
        .and_then(|pid| pid.parse().ok())
        .unwrap()
}

#[cfg(unix)]
fn wait_with_deadline(process: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = process.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = process.kill();
            panic!("OCM did not exit after its command stopped");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Whether `pid` still runs; an exited orphan can wait briefly for init.
#[cfg(unix)]
fn reap_if_running(pid: libc::pid_t) -> bool {
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&state.stdout);
    let running = !state.trim().is_empty() && !state.trim_start().starts_with('Z');
    if running {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    running
}

/// Send SIGTERM to the OCM process alone, as process managers and harnesses
/// do, and require OCM to pass it on and outlive the command's shutdown.
#[cfg(unix)]
fn assert_command_receives_termination(fixture: &SignalFixture, label: &str, args: &[&str]) {
    use std::os::unix::process::ExitStatusExt;

    let marker = fixture.marker(label);
    let mut process = fixture
        .command(args, &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let command_pid = ready_pid(&marker);
    assert_eq!(
        unsafe { libc::kill(process.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = wait_with_deadline(&mut process);
    let log = fs::read_to_string(&marker).unwrap();
    let survived = reap_if_running(command_pid);
    assert!(
        log.contains("signal TERM") && log.ends_with("stopped\n"),
        "{label}: OCM exited before its command received and finished handling SIGTERM ({status:?}): {log:?}"
    );
    assert!(!survived, "{label}: the command outlived OCM");
    if label == "launcher" {
        // dash runs the recipe under a shell that SIGTERM ends, so OCM ends
        // by the same signal; shells that exec the recipe report its exit.
        assert!(
            status.code() == Some(0) || status.signal() == Some(libc::SIGTERM),
            "{label}: {status:?}"
        );
    } else {
        assert_eq!(status.code(), Some(0), "{label}: {log:?}");
    }
}

#[cfg(unix)]
#[test]
fn foreground_runs_relay_termination_to_their_child_and_wait_for_it() {
    let fixture = SignalFixture::new("cli-foreground-termination");
    let openclaw = fixture.openclaw.clone();
    assert_command_receives_termination(&fixture, "runtime", &["@bound", "--", "gateway", "run"]);
    assert_command_receives_termination(
        &fixture,
        "launcher",
        &["env", "run", "launched", "--", "gateway", "run"],
    );
    assert_command_receives_termination(
        &fixture,
        "exec",
        &["env", "exec", "bound", "--", &openclaw, "gateway", "run"],
    );
}

#[cfg(unix)]
#[test]
fn foreground_runs_end_by_the_stop_signal_that_ended_their_command() {
    use std::os::unix::process::ExitStatusExt;

    let fixture = SignalFixture::new("cli-foreground-stop-signal");
    let marker = fixture.marker("default");
    let mut process = fixture
        .command(
            &[
                "env",
                "exec",
                "bound",
                "--",
                "sh",
                "-c",
                r#"echo "ready $$" >> "$OCM_TEST_SIGNAL_MARKER"; exec sleep 30"#,
            ],
            &marker,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let command_pid = ready_pid(&marker);
    unsafe {
        libc::kill(process.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = wait_with_deadline(&mut process);
    assert!(!reap_if_running(command_pid), "the command outlived OCM");
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
}

/// Run OCM as the session leader of a new pseudo-terminal, as a terminal
/// emulator runs a shell, and collect what it writes there.
#[cfg(unix)]
struct OnTerminal {
    process: std::process::Child,
    master: fs::File,
    output: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

#[cfg(unix)]
impl OnTerminal {
    fn spawn(mut command: Command) -> Self {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        use std::os::unix::process::CommandExt;

        let (mut master, mut slave) = (-1, -1);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut::<libc::c_char>(),
                std::ptr::null_mut::<libc::termios>(),
                std::ptr::null_mut::<libc::winsize>(),
            )
        };
        assert_eq!(opened, 0, "{}", std::io::Error::last_os_error());
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1
                    || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command.spawn().unwrap();
        // Close the parent's copies so reads end when the session does.
        drop(command);
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = std::sync::Arc::clone(&output);
        let mut reader = master.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                collected
                    .lock()
                    .unwrap()
                    .extend_from_slice(&buffer[..count]);
            }
        });
        Self {
            process,
            master,
            output,
        }
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }
}

#[cfg(unix)]
#[test]
fn terminal_interrupt_reaches_the_command_once_and_ocm_waits_for_it() {
    use std::io::Write;

    let fixture = SignalFixture::new("cli-foreground-terminal-interrupt");
    let marker = fixture.marker("terminal");
    let mut terminal =
        OnTerminal::spawn(fixture.command(&["@bound", "--", "gateway", "run"], &marker));
    let command_pid = ready_pid(&marker);
    terminal.master.write_all(b"\x03").unwrap();
    let status = wait_with_deadline(&mut terminal.process);
    let log = fs::read_to_string(&marker).unwrap();
    let survived = reap_if_running(command_pid);
    assert_eq!(
        log.matches("signal ").count(),
        1,
        "Ctrl-C must reach the command exactly once ({status:?}): {log:?}"
    );
    assert!(
        log.contains("signal INT") && log.ends_with("stopped\n"),
        "OCM exited before its command finished handling Ctrl-C ({status:?}): {log:?}"
    );
    assert!(!survived, "the command outlived OCM");
    assert_eq!(status.code(), Some(0), "{}", terminal.output());
}

#[cfg(unix)]
#[test]
fn failed_launch_returns_the_terminal_to_the_caller() {
    let fixture = SignalFixture::new("cli-foreground-terminal-launch-failure");
    // A non-job-control shell shares OCM's process group, so the terminal's
    // foreground group must be its own again once OCM returns.
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            r#""$0" env exec bound -- definitely-not-a-real-command; echo "groups $(ps -o tpgid= -o pgid= -p $$)""#,
        ])
        .arg(crate::support::ocm_test_binary_path())
        .current_dir(&fixture.cwd)
        .env_clear()
        .envs(&fixture.env);
    let mut terminal = OnTerminal::spawn(command);
    let status = wait_with_deadline(&mut terminal.process);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let groups = loop {
        let output = terminal.output();
        if let Some(line) = output.lines().find(|line| line.starts_with("groups ")) {
            break line
                .split_whitespace()
                .skip(1)
                .map(str::to_string)
                .collect::<Vec<_>>();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{status:?}: {output:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(
        terminal
            .output()
            .contains("failed to run \"definitely-not-a-real-command\""),
        "{}",
        terminal.output()
    );
    assert_eq!(groups.len(), 2, "{}", terminal.output());
    assert_eq!(
        groups[0],
        groups[1],
        "the terminal's foreground group was not handed back: {}",
        terminal.output()
    );
}
