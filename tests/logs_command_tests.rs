mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::support::{TestDir, ocm_env, path_string, run_ocm, stderr, stdout};

fn setup_launcher_env(cwd: &std::path::Path, env: &std::collections::BTreeMap<String, String>) {
    let launcher = run_ocm(
        cwd,
        env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let created = run_ocm(cwd, env, &["env", "create", "demo", "--launcher", "stable"]);
    assert!(created.status.success(), "{}", stderr(&created));
}

fn gateway_log_paths(root: &TestDir) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        root.child("ocm-home/envs/demo/.openclaw/logs/gateway.log"),
        root.child("ocm-home/envs/demo/.openclaw/logs/gateway.err.log"),
    )
}

#[test]
fn logs_reads_gateway_logs_from_the_env_root() {
    let root = TestDir::new("logs-gateway-root");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (stdout_path, stderr_path) = gateway_log_paths(&root);
    fs::create_dir_all(stdout_path.parent().unwrap()).unwrap();
    fs::write(&stdout_path, "hello from gateway stdout\n").unwrap();
    fs::write(&stderr_path, "hello from gateway stderr\n").unwrap();

    let stdout_log = run_ocm(&cwd, &env, &["logs", "demo"]);
    assert!(stdout_log.status.success(), "{}", stderr(&stdout_log));
    assert!(stdout(&stdout_log).contains("hello from gateway stdout"));

    let stderr_log = run_ocm(&cwd, &env, &["logs", "demo", "--stream", "error", "--json"]);
    assert!(stderr_log.status.success(), "{}", stderr(&stderr_log));
    let body: Value = serde_json::from_str(&stdout(&stderr_log)).unwrap();
    assert_eq!(body["sourceKind"], "gateway");
    assert_eq!(body["path"], path_string(&stderr_path));
    assert_eq!(body["content"], "hello from gateway stderr\n");
}

#[test]
fn logs_can_merge_stdout_and_stderr_in_one_snapshot() {
    let root = TestDir::new("logs-all-streams");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (stdout_path, stderr_path) = gateway_log_paths(&root);
    fs::create_dir_all(stdout_path.parent().unwrap()).unwrap();
    fs::write(
        &stdout_path,
        concat!(
            "2026-04-20T00:13:45.497+01:00 [gateway] one\n",
            "2026-04-20T00:13:47.497+01:00 [gateway] three\n"
        ),
    )
    .unwrap();
    fs::write(
        &stderr_path,
        "2026-04-20T00:13:46.497+01:00 error gateway two\n",
    )
    .unwrap();

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--raw"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        concat!(
            "2026-04-20T00:13:45.497+01:00 [gateway] one\n",
            "2026-04-20T00:13:46.497+01:00 error gateway two\n",
            "2026-04-20T00:13:47.497+01:00 [gateway] three\n"
        )
    );
}

#[test]
fn logs_rejects_unknown_stream_level() {
    let root = TestDir::new("logs-invalid-stream");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--stream", "warn"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unsupported log stream level: warn"));
    assert!(stderr(&output).contains("--stream info or --stream error"));
}

#[test]
fn logs_rejects_json_with_follow_alias() {
    let root = TestDir::new("logs-json-follow-short");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--json", "-f"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("logs cannot combine --json with --follow"));
}

#[test]
fn logs_fall_back_to_supervisor_logs_when_gateway_logs_are_missing() {
    let root = TestDir::new("logs-service-fallback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let fallback = root.child("ocm-home/supervisor/logs/demo.stdout.log");
    fs::create_dir_all(fallback.parent().unwrap()).unwrap();
    fs::write(&fallback, "hello from supervisor stdout\n").unwrap();

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--stream", "info", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(body["sourceKind"], "service");
    assert_eq!(body["path"], path_string(&fallback));
    assert_eq!(body["content"], "hello from supervisor stdout\n");
}

#[test]
fn env_remove_and_destroy_delete_service_logs_so_a_recreated_env_starts_without_them() {
    for removal in [
        &["env", "remove", "demo"][..],
        &["env", "destroy", "demo", "--yes"],
    ] {
        let root = TestDir::new("logs-removed-env-service-logs");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        setup_launcher_env(&cwd, &env);
        let sibling = run_ocm(
            &cwd,
            &env,
            &["env", "create", "keep", "--launcher", "stable"],
        );
        assert!(sibling.status.success(), "{}", stderr(&sibling));

        let logs_dir = root.child("ocm-home/supervisor/logs");
        fs::create_dir_all(&logs_dir).unwrap();
        for stream in ["stdout", "stderr"] {
            fs::write(
                logs_dir.join(format!("demo.{stream}.log")),
                format!("removed env {stream}\n"),
            )
            .unwrap();
            fs::write(
                logs_dir.join(format!("keep.{stream}.log")),
                format!("sibling {stream}\n"),
            )
            .unwrap();
        }

        let removed = run_ocm(&cwd, &env, removal);
        assert!(
            removed.status.success(),
            "{removal:?}: {}",
            stderr(&removed)
        );
        assert!(!logs_dir.join("demo.stdout.log").exists(), "{removal:?}");
        assert!(!logs_dir.join("demo.stderr.log").exists(), "{removal:?}");

        let recreated = run_ocm(
            &cwd,
            &env,
            &["env", "create", "demo", "--launcher", "stable"],
        );
        assert!(recreated.status.success(), "{}", stderr(&recreated));
        let output = run_ocm(&cwd, &env, &["logs", "demo", "--json"]);
        assert!(!output.status.success(), "{removal:?}: {}", stdout(&output));
        assert!(
            stderr(&output).contains("no logs exist for env \"demo\" across stdout or stderr"),
            "{removal:?}: {}",
            stderr(&output)
        );

        let sibling_logs = run_ocm(&cwd, &env, &["logs", "keep", "--json"]);
        assert!(sibling_logs.status.success(), "{}", stderr(&sibling_logs));
        let body: Value = serde_json::from_str(&stdout(&sibling_logs)).unwrap();
        assert_eq!(body["sourceKind"], "service");
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("sibling stdout"), "{content}");
        assert!(content.contains("sibling stderr"), "{content}");
    }
}

#[test]
fn env_remove_keeps_the_env_when_its_service_log_cannot_be_deleted() {
    let root = TestDir::new("logs-remove-undeletable");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    // A directory in place of the log file makes its deletion fail.
    let blocked = root.child("ocm-home/supervisor/logs/demo.stdout.log");
    fs::create_dir_all(blocked.join("nested")).unwrap();

    let removed = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(!removed.status.success());
    assert!(
        stderr(&removed).contains("failed removing service log"),
        "{}",
        stderr(&removed)
    );
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(root.child("ocm-home/envs/demo").exists());
}

#[test]
fn removing_an_env_named_daemon_keeps_the_supervisor_daemon_logs() {
    let root = TestDir::new("logs-remove-daemon-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "daemon", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    let daemon_log = root.child("ocm-home/supervisor/logs/daemon.stdout.log");
    fs::create_dir_all(daemon_log.parent().unwrap()).unwrap();
    fs::write(&daemon_log, "supervisor daemon output\n").unwrap();

    let removed = run_ocm(&cwd, &env, &["env", "remove", "daemon"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert_eq!(
        fs::read_to_string(&daemon_log).unwrap(),
        "supervisor daemon output\n"
    );
}

#[test]
fn logs_prefer_the_newer_service_log_when_gateway_log_is_stale() {
    let root = TestDir::new("logs-prefer-newer-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (gateway_path, _) = gateway_log_paths(&root);
    fs::create_dir_all(gateway_path.parent().unwrap()).unwrap();
    fs::write(&gateway_path, "old gateway output\n").unwrap();
    thread::sleep(Duration::from_millis(1100));

    let service_path = root.child("ocm-home/supervisor/logs/demo.stdout.log");
    fs::create_dir_all(service_path.parent().unwrap()).unwrap();
    fs::write(&service_path, "new service output\n").unwrap();

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--stream", "info", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(body["sourceKind"], "service");
    assert_eq!(body["path"], path_string(&service_path));
    assert_eq!(body["content"], "new service output\n");
}

#[test]
fn logs_follow_streams_new_lines() {
    let root = TestDir::new("logs-follow");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (stdout_path, _) = gateway_log_paths(&root);
    fs::create_dir_all(stdout_path.parent().unwrap()).unwrap();
    fs::write(&stdout_path, "first line\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(&cwd)
        .args(["logs", "demo", "-f", "--tail", "1", "--raw"])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            lines.push(line.clone());
            let _ = sender.send(line);
        }
        lines
    });

    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
        "first line"
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&stdout_path)
        .unwrap()
        .write_all(b"second line\n")
        .unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
        "second line"
    );

    let _ = child.kill();
    let _ = child.wait();
    let lines = reader.join().unwrap();
    assert!(lines.iter().any(|line| line == "first line"));
    assert!(lines.iter().any(|line| line == "second line"));
}

fn spawn_follow(
    cwd: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
    args: &[&str],
) -> (std::process::Child, mpsc::Receiver<String>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(cwd)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    (child, receiver)
}

fn strip_ansi(line: &str) -> String {
    let mut plain = String::new();
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

fn receive_line_containing(receiver: &mpsc::Receiver<String>, needle: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let line = receiver
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("follow did not print a line containing {needle:?}"));
        if line.contains(needle) {
            return line;
        }
    }
}

#[test]
fn logs_follow_started_before_any_log_attaches_to_the_service_log() {
    let root = TestDir::new("logs-follow-pending-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (mut child, receiver) = spawn_follow(
        &cwd,
        &env,
        &["logs", "demo", "--stream", "error", "--follow", "--raw"],
    );
    // Let the follower start and poll while neither log file exists.
    thread::sleep(Duration::from_millis(600));
    let service_path = root.child("ocm-home/supervisor/logs/demo.stderr.log");
    fs::create_dir_all(service_path.parent().unwrap()).unwrap();
    fs::write(&service_path, "service error one\n").unwrap();
    receive_line_containing(&receiver, "service error one");
    fs::OpenOptions::new()
        .append(true)
        .open(&service_path)
        .unwrap()
        .write_all(b"service error two\n")
        .unwrap();
    receive_line_containing(&receiver, "service error two");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn logs_follow_header_reports_a_pending_source_before_any_log_exists() {
    let root = TestDir::new("logs-follow-pending-header");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (mut child, receiver) = spawn_follow(
        &cwd,
        &env,
        &[
            "logs", "demo", "--stream", "error", "--follow", "--color", "always",
        ],
    );
    receive_line_containing(&receiver, "pending");
    let path_line = strip_ansi(&receive_line_containing(&receiver, "gateway.err.log"));
    let (_, gateway_path) = gateway_log_paths(&root);
    let service_path = root.child("ocm-home/supervisor/logs/demo.stderr.log");
    assert!(
        path_line.contains(&format!(
            "{} or {}",
            path_string(&gateway_path),
            path_string(&service_path)
        )),
        "{path_line}"
    );

    let _ = child.kill();
    let _ = child.wait();
}
