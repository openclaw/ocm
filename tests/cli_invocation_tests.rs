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

#[cfg(unix)]
use crate::support::write_executable_script;
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

/// A fake OpenClaw that exits with a numeric argument or dies from the named
/// signal. Perl restores the signal's default action first, so a signal the
/// test harness ignores (as `nohup` or a background job may) still kills it.
#[cfg(unix)]
const SIGNAL_CHILD: &str = "#!/bin/sh\ncase \"$1\" in [0-9]*) exit \"$1\" ;; esac\nexec perl -e '$SIG{$ARGV[0]} = \"DEFAULT\"; kill $ARGV[0], $$; sleep 5; exit 99' \"$1\"\n";

#[cfg(unix)]
#[test]
fn env_exec_reports_a_signal_killed_child_as_128_plus_the_signal() {
    let root = TestDir::new("cli-exec-signal-exit");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let child = cwd.join("signal-child");
    write_executable_script(&child, SIGNAL_CHILD);
    let child = child.to_str().unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let exec = run_ocm(&cwd, &env, &["env", "exec", "demo", "--", child, "TERM"]);
    assert_eq!(exec.status.code(), Some(143), "{}", stderr(&exec));
    assert!(
        stderr(&exec).contains(&format!(
            "ocm: \"{child}\" was terminated by signal 15 (SIGTERM)"
        )),
        "{}",
        stderr(&exec)
    );
}

#[cfg(unix)]
#[test]
fn env_run_reports_a_signal_killed_child_as_128_plus_the_signal() {
    let root = TestDir::new("cli-run-signal-exit");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let child = cwd.join("openclaw");
    write_executable_script(&child, SIGNAL_CHILD);
    let child = child.to_str().unwrap();
    let env = ocm_env(&root);

    for args in [
        &["runtime", "add", "fake", "--path", child][..],
        &["env", "create", "demo", "--runtime", "fake"][..],
        &["launcher", "add", "fakel", "--command", child][..],
        &["env", "create", "launched", "--launcher", "fakel"][..],
    ] {
        let setup = run_ocm(&cwd, &env, args);
        assert!(setup.status.success(), "{args:?}: {}", stderr(&setup));
    }

    for (args, code, diagnostic) in [
        (
            &["@demo", "--", "TERM"][..],
            143,
            Some("signal 15 (SIGTERM)"),
        ),
        (
            &["env", "run", "demo", "--", "KILL"][..],
            137,
            Some("signal 9 (SIGKILL)"),
        ),
        (&["@demo", "--", "INT"][..], 130, None),
        (&["@demo", "--", "PIPE"][..], 141, None),
        (&["@demo", "--", "42"][..], 42, None),
    ] {
        let run = run_ocm(&cwd, &env, args);
        assert_eq!(run.status.code(), Some(code), "{args:?}: {}", stderr(&run));
        match diagnostic {
            Some(signal) => assert!(
                stderr(&run).lines().any(|line| line.starts_with("ocm: \"")
                    && line.ends_with(&format!("openclaw\" was terminated by {signal}"))),
                "{args:?}: {}",
                stderr(&run)
            ),
            None => assert!(
                !stderr(&run).contains("was terminated by signal"),
                "{args:?}: {}",
                stderr(&run)
            ),
        }
    }

    // Launcher-bound runs go through `sh -lc`, which either reports the death
    // itself or replaces itself with the command; both must give 128 + N.
    let launched = run_ocm(&cwd, &env, &["@launched", "--", "TERM"]);
    assert_eq!(launched.status.code(), Some(143), "{}", stderr(&launched));
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
