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

/// Send SIGTERM to the OCM process alone, as process managers and harnesses
/// do, and require OCM to relay it and outlive the child's shutdown.
#[cfg(unix)]
fn assert_foreground_child_owns_termination(
    root: &TestDir,
    cwd: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
    args: &[&str],
    direct_child: bool,
) {
    let marker = root.child(format!("signals-{}.log", args.join("-").replace('/', "_")));
    let mut process = Command::new(crate::support::ocm_test_binary_path())
        .current_dir(cwd)
        .args(args)
        .env_clear()
        .envs(env)
        .env("OCM_TEST_SIGNAL_MARKER", &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = wait_for_marker(&marker, "ready");
    let fixture_pid: libc::pid_t = ready
        .split_whitespace()
        .nth(1)
        .and_then(|pid| pid.parse().ok())
        .unwrap();

    assert_eq!(
        unsafe { libc::kill(process.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = process.kill();
            panic!("OCM did not exit after its child stopped");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let log = fs::read_to_string(&marker).unwrap();
    // An orphan that exited may wait briefly for init to reap it.
    let fixture_alive = Command::new("ps")
        .args(["-o", "stat=", "-p", &fixture_pid.to_string()])
        .output()
        .map(|output| {
            let state = String::from_utf8_lossy(&output.stdout);
            !state.trim().is_empty() && !state.trim_start().starts_with('Z')
        })
        .unwrap();
    if fixture_alive {
        unsafe {
            libc::kill(fixture_pid, libc::SIGKILL);
        }
    }
    assert!(
        log.contains("signal TERM") && log.ends_with("stopped\n"),
        "OCM exited before its child received and finished handling SIGTERM ({status:?}): {log:?}"
    );
    assert!(!fixture_alive, "the foreground child outlived OCM");
    // A launcher's shell may itself die from the signal; only a direct child's
    // own exit status is OCM's to report.
    if direct_child {
        assert_eq!(status.code(), Some(0), "{log:?}");
    }
}

#[cfg(unix)]
#[test]
fn foreground_runs_relay_termination_to_their_child_and_wait_for_it() {
    let root = TestDir::new("cli-foreground-termination");
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

    assert_foreground_child_owns_termination(
        &root,
        &cwd,
        &env,
        &["@bound", "--", "gateway", "run"],
        true,
    );
    // The launcher's shell stays between OCM and the fixture on Linux.
    assert_foreground_child_owns_termination(
        &root,
        &cwd,
        &env,
        &["env", "run", "launched", "--", "gateway", "run"],
        false,
    );
    assert_foreground_child_owns_termination(
        &root,
        &cwd,
        &env,
        &[
            "env",
            "exec",
            "bound",
            "--",
            openclaw.as_str(),
            "gateway",
            "run",
        ],
        true,
    );
}
