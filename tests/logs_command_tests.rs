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
fn raw_single_stream_snapshots_preserve_empty_and_unterminated_content() {
    let root = TestDir::new("logs-raw-partial");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);

    let (stdout_path, stderr_path) = gateway_log_paths(&root);
    fs::create_dir_all(stdout_path.parent().unwrap()).unwrap();
    for content in ["", "partial", "complete line\nfinal € fragment"] {
        for (stream, path) in [("info", &stdout_path), ("error", &stderr_path)] {
            fs::write(path, content).unwrap();
            let output = run_ocm(&cwd, &env, &["logs", "demo", "--raw", "--stream", stream]);
            assert!(output.status.success(), "{}", stderr(&output));
            assert_eq!(output.stdout, content.as_bytes());
            assert!(output.stderr.is_empty());
        }
    }
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

const CONFIG_INVALID: &str = "OpenClaw config is invalid";
const FIX_HINT: &str = "Run \"openclaw doctor --fix\" to repair the config, then retry.";
const CLEAN_SHUTDOWN: &str = "2026-04-20T00:13:47Z [shutdown] completed cleanly";

fn set_modified(path: &std::path::Path, rfc3339: &str) {
    let modified =
        time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
            .unwrap();
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(modified.into())).unwrap();
}

/// A healthy timestamped run, then three failed starts that print only undated
/// stderr, as a Gateway does when its config is invalid.
fn write_failed_restart_logs(root: &TestDir) {
    let (stdout_path, stderr_path) = gateway_log_paths(root);
    fs::create_dir_all(stdout_path.parent().unwrap()).unwrap();
    let mut healthy = String::new();
    for second in 0..59 {
        healthy.push_str(&format!(
            "2026-04-20T00:12:{second:02}Z [gateway] healthy {second}\n"
        ));
    }
    healthy.push_str(CLEAN_SHUTDOWN);
    healthy.push('\n');
    fs::write(&stdout_path, healthy).unwrap();

    let mut failed = String::from("2026-04-20T00:12:29.500Z [warn] old warning\n");
    for _attempt in 0..3 {
        for phase in 0..15 {
            failed.push_str(&format!("[gateway] startup phase {phase}\n"));
        }
        failed.push_str(CONFIG_INVALID);
        failed.push_str("\nProblem:\n  - gateway.port: Invalid input\n\n");
        failed.push_str(FIX_HINT);
        failed.push('\n');
    }
    fs::write(&stderr_path, failed).unwrap();
    set_modified(&stdout_path, "2026-04-20T00:13:47Z");
    set_modified(&stderr_path, "2026-04-20T00:14:30Z");
}

#[test]
fn logs_show_undated_stderr_written_after_older_timestamped_output() {
    let root = TestDir::new("logs-trailing-undated");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);
    write_failed_restart_logs(&root);

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--raw"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 50);
    assert!(lines.contains(&CONFIG_INVALID), "{text}");
    assert_eq!(lines.last(), Some(&FIX_HINT), "{text}");

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(body["content"], text);

    let error_only = run_ocm(&cwd, &env, &["logs", "demo", "--raw", "--stream", "error"]);
    assert!(error_only.status.success(), "{}", stderr(&error_only));
    assert_eq!(stdout(&error_only), text);

    let output = run_ocm(&cwd, &env, &["logs", "demo", "--raw", "--tail", "500"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 121);
    let shutdown = lines
        .iter()
        .position(|line| *line == CLEAN_SHUTDOWN)
        .unwrap();
    let warning = lines
        .iter()
        .position(|line| line.ends_with("[warn] old warning"))
        .unwrap();
    let first_failure = lines
        .iter()
        .position(|line| *line == CONFIG_INVALID)
        .unwrap();
    assert!(lines[warning - 1].ends_with("healthy 29"), "{text}");
    assert_eq!(shutdown, 60, "{text}");
    assert!(shutdown < first_failure, "{text}");
    assert_eq!(lines.last(), Some(&FIX_HINT), "{text}");
}

#[test]
fn logs_follow_starts_with_undated_stderr_written_after_older_timestamped_output() {
    let root = TestDir::new("logs-follow-trailing-undated");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);
    write_failed_restart_logs(&root);

    let mut child = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(&cwd)
        .args(["logs", "demo", "-f", "--raw"])
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
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let startup = (0..50)
        .map(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect::<Vec<_>>();
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    assert!(
        startup.iter().any(|line| line == CONFIG_INVALID),
        "{startup:?}"
    );
    assert_eq!(startup.last().map(String::as_str), Some(FIX_HINT));
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

fn write_service_logs(root: &TestDir, name: &str, label: &str) -> std::path::PathBuf {
    let logs_dir = root.child("ocm-home/supervisor/logs");
    fs::create_dir_all(&logs_dir).unwrap();
    for stream in ["stdout", "stderr"] {
        fs::write(
            logs_dir.join(format!("{name}.{stream}.log")),
            format!("{label} {stream}\n"),
        )
        .unwrap();
    }
    logs_dir
}

fn assert_no_logs(cwd: &std::path::Path, env: &std::collections::BTreeMap<String, String>) {
    let output = run_ocm(cwd, env, &["logs", "demo", "--json"]);
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("no logs exist for env \"demo\" across stdout or stderr"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_recreated_env_does_not_show_the_removed_envs_service_logs() {
    let cases: [(&[&str], &[&str]); 3] = [
        (
            &["env", "remove", "demo"],
            &["env", "create", "demo", "--launcher", "stable"],
        ),
        (
            &["env", "destroy", "demo", "--yes"],
            &["env", "clone", "keep", "demo"],
        ),
        (
            &["env", "remove", "demo"],
            &["env", "import", "keep.tar", "--name", "demo"],
        ),
    ];
    for (removal, creation) in cases {
        let root = TestDir::new("logs-recreated-env-service-logs");
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
        let exported = run_ocm(
            &cwd,
            &env,
            &["env", "export", "keep", "--output", "keep.tar"],
        );
        assert!(exported.status.success(), "{}", stderr(&exported));
        let logs_dir = write_service_logs(&root, "demo", "removed env");
        write_service_logs(&root, "keep", "sibling");

        let removed = run_ocm(&cwd, &env, removal);
        assert!(
            removed.status.success(),
            "{removal:?}: {}",
            stderr(&removed)
        );
        let recreated = run_ocm(&cwd, &env, creation);
        assert!(
            recreated.status.success(),
            "{creation:?}: {}",
            stderr(&recreated)
        );
        assert!(!logs_dir.join("demo.stdout.log").exists(), "{creation:?}");
        assert!(!logs_dir.join("demo.stderr.log").exists(), "{creation:?}");
        assert_no_logs(&cwd, &env);

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
fn a_new_env_does_not_inherit_service_logs_left_under_its_name() {
    let root = TestDir::new("logs-new-env-orphaned-service-logs");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    // Logs left by an environment removed before this cleanup existed.
    let logs_dir = write_service_logs(&root, "demo", "orphaned");

    setup_launcher_env(&cwd, &env);
    assert!(!logs_dir.join("demo.stdout.log").exists());
    assert!(!logs_dir.join("demo.stderr.log").exists());
    assert_no_logs(&cwd, &env);
}

#[test]
fn a_rejected_create_keeps_the_earlier_service_logs() {
    let root = TestDir::new("logs-rejected-create-keeps-logs");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let logs_dir = write_service_logs(&root, "demo", "removed env");

    let rejected = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "missing"],
    );
    assert!(!rejected.status.success());
    assert!(logs_dir.join("demo.stdout.log").exists());
    assert!(logs_dir.join("demo.stderr.log").exists());
}

#[test]
fn updating_a_registered_env_keeps_its_service_logs() {
    let root = TestDir::new("logs-update-keeps-service-logs");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);
    write_service_logs(&root, "demo", "current");

    let protected = run_ocm(&cwd, &env, &["env", "protect", "demo", "on"]);
    assert!(protected.status.success(), "{}", stderr(&protected));
    let output = run_ocm(&cwd, &env, &["logs", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let content = body["content"].as_str().unwrap();
    assert!(content.contains("current stdout"), "{content}");
    assert!(content.contains("current stderr"), "{content}");
}

#[test]
fn creating_a_case_variant_name_keeps_the_registered_envs_logs() {
    let root = TestDir::new("logs-case-variant-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    setup_launcher_env(&cwd, &env);
    let logs_dir = write_service_logs(&root, "demo", "current");
    // On a case-insensitive filesystem Demo.stdout.log is demo's own file; a
    // hard link gives the same aliasing here.
    if let Err(error) = fs::hard_link(
        logs_dir.join("demo.stdout.log"),
        logs_dir.join("Demo.stdout.log"),
    ) {
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    let other_root = root.child("other-root");
    let created = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "Demo",
            "--launcher",
            "stable",
            "--root",
            &path_string(&other_root),
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    assert!(logs_dir.join("Demo.stdout.log").exists());
    let output = run_ocm(&cwd, &env, &["logs", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let content = body["content"].as_str().unwrap();
    assert!(content.contains("current stdout"), "{content}");
    assert!(content.contains("current stderr"), "{content}");
}

#[cfg(unix)]
#[test]
fn case_distinct_envs_keep_separate_service_output() {
    use crate::support::{install_fake_service_manager, write_executable_script};

    let root = TestDir::new("logs-case-distinct-service-output");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let probe = root.child("case-probe");
    fs::write(&probe, "").unwrap();
    let case_insensitive = root.child("CASE-PROBE").exists();
    let gateway = root.child("gateway.sh");
    write_executable_script(
        &gateway,
        "#!/bin/sh\necho \"service-out $OCM_ACTIVE_ENV\"\necho \"service-err $OCM_ACTIVE_ENV\" >&2\nsleep 1\n",
    );
    let run = |args: &[&str]| {
        let output = run_ocm(&cwd, &env, args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    };
    let service_output = |name: &str| {
        let output = run_ocm(&cwd, &env, &["logs", name, "--json"]);
        if !output.status.success() {
            return String::new();
        }
        let body: Value = serde_json::from_str(&stdout(&output)).unwrap();
        body["content"].as_str().unwrap().to_string()
    };
    run(&["launcher", "add", "gw", "--command", &path_string(&gateway)]);
    run(&["env", "create", "alpha", "--launcher", "gw"]);
    let other_root = path_string(&root.child("other-root"));
    run(&[
        "env",
        "create",
        "Alpha",
        "--launcher",
        "gw",
        "--root",
        &other_root,
    ]);
    run(&["service", "start", "alpha"]);
    run(&["service", "start", "Alpha"]);
    run(&["__daemon", "run", "--once", "--json"]);

    for (name, other) in [("alpha", "Alpha"), ("Alpha", "alpha")] {
        let content = service_output(name);
        for stream in ["out", "err"] {
            assert!(
                content.contains(&format!("service-{stream} {name}\n")),
                "{name}: {content}"
            );
            assert!(
                !content.contains(&format!("service-{stream} {other}\n")),
                "{name}: {content}"
            );
        }
    }
    // Only case-insensitive storage needs the separate file.
    let logs_dir = root.child("ocm-home/supervisor/logs");
    assert_eq!(logs_dir.join("case-distinct").exists(), case_insensitive);

    // A recreated name starts without the removed environment's output.
    run(&["service", "stop", "Alpha"]);
    run(&["env", "remove", "Alpha"]);
    let third_root = path_string(&root.child("third-root"));
    run(&[
        "env",
        "create",
        "Alpha",
        "--launcher",
        "gw",
        "--root",
        &third_root,
    ]);
    assert!(!service_output("Alpha").contains("service-"));
    let content = service_output("alpha");
    assert!(content.contains("service-out alpha\n"), "{content}");
    assert!(!content.contains("service-out Alpha\n"), "{content}");
}

#[test]
fn env_create_fails_when_an_earlier_service_log_cannot_be_deleted() {
    let root = TestDir::new("logs-create-undeletable");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    // A directory in place of the log file makes its deletion fail.
    let blocked = root.child("ocm-home/supervisor/logs/demo.stdout.log");
    fs::create_dir_all(blocked.join("nested")).unwrap();

    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(!created.status.success());
    assert!(
        stderr(&created).contains("failed removing earlier service log"),
        "{}",
        stderr(&created)
    );
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(!show.status.success());
    assert!(stderr(&show).contains("environment \"demo\" does not exist"));
    assert!(!root.child("ocm-home/envs/demo").exists());
}

#[test]
fn an_env_named_daemon_keeps_only_the_daemons_own_logs() {
    for layout in ["default", "relocated", "alias", "case"] {
        if layout == "alias" && !cfg!(unix) {
            continue;
        }
        let name = if layout == "case" { "Daemon" } else { "daemon" };
        let root = TestDir::new("logs-env-named-daemon");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        let launcher = run_ocm(
            &cwd,
            &env,
            &["launcher", "add", "stable", "--command", "openclaw"],
        );
        assert!(launcher.status.success(), "{}", stderr(&launcher));
        let log = root.child("ocm-home/supervisor/logs/daemon.stdout.log");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "earlier output\n").unwrap();
        let daemon_logs = root.child("daemon-logs");
        match layout {
            "relocated" => fs::create_dir_all(&daemon_logs).unwrap(),
            // Another path to the same directory still holds the daemon's logs.
            #[cfg(unix)]
            "alias" => std::os::unix::fs::symlink(log.parent().unwrap(), &daemon_logs).unwrap(),
            _ => {}
        }
        if layout == "case" {
            // On a case-insensitive filesystem Daemon.stdout.log is the
            // daemon's own file; a hard link gives the same aliasing here.
            let alias = log.with_file_name("Daemon.stdout.log");
            if let Err(error) = fs::hard_link(&log, &alias) {
                assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
            }
        }
        if layout == "relocated" || layout == "alias" {
            fs::write(
                root.child("ocm-home/supervisor/daemon-log-config.json"),
                serde_json::json!({ "logDir": path_string(&daemon_logs) }).to_string(),
            )
            .unwrap();
        }

        let created = run_ocm(&cwd, &env, &["env", "create", name, "--launcher", "stable"]);
        assert!(created.status.success(), "{}", stderr(&created));
        // Legacy files may already contain mixed history. Keep them even
        // after relocation, but never attribute them to the new environment.
        assert!(log.exists(), "{layout}");
        let output = run_ocm(&cwd, &env, &["logs", name, "--json"]);
        assert!(!output.status.success(), "{layout}: {}", stdout(&output));
        assert!(!stdout(&output).contains("earlier output"));
        let child_log = log.with_file_name(format!(".{name}.stdout.log"));
        fs::write(&child_log, "child-only output\n").unwrap();
        let output = run_ocm(&cwd, &env, &["logs", name, "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("child-only output"));
        assert!(!stdout(&output).contains("earlier output"));
        assert!(layout != "case" || log.with_file_name("Daemon.stdout.log").exists());
        let removed = run_ocm(&cwd, &env, &["env", "remove", name]);
        assert!(removed.status.success(), "{}", stderr(&removed));
        let created = run_ocm(&cwd, &env, &["env", "create", name, "--launcher", "stable"]);
        assert!(created.status.success(), "{}", stderr(&created));
        assert!(!child_log.exists());
        assert_eq!(fs::read_to_string(&log).unwrap(), "earlier output\n");
    }
}

#[cfg(unix)]
#[test]
fn daemon_child_log_aliases_do_not_expose_supervisor_diagnostics() {
    for symlink in [false, true] {
        let root = TestDir::new("daemon-log-alias");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        let added = run_ocm(
            &cwd,
            &env,
            &["launcher", "add", "fixture", "--command", "openclaw"],
        );
        assert!(added.status.success(), "{}", stderr(&added));
        let created = run_ocm(
            &cwd,
            &env,
            &["env", "create", "daemon", "--launcher", "fixture"],
        );
        assert!(created.status.success(), "{}", stderr(&created));
        let log = root.child("ocm-home/supervisor/logs/daemon.stderr.log");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "shared supervisor diagnostic\n").unwrap();
        let child = log.with_file_name(".daemon.stdout.log");
        if symlink {
            std::os::unix::fs::symlink(&log, &child).unwrap();
        } else {
            fs::hard_link(&log, &child).unwrap();
        }
        for requested_name in ["daemon", " daemon "] {
            let output = run_ocm(&cwd, &env, &["logs", requested_name, "--json"]);
            assert!(!output.status.success());
            assert!(!stdout(&output).contains("shared supervisor diagnostic"));
        }
        let gateway = root.child("ocm-home/envs/daemon/.openclaw/logs/gateway.log");
        fs::create_dir_all(gateway.parent().unwrap()).unwrap();
        fs::write(&gateway, "gateway output\n").unwrap();
        let output = run_ocm(&cwd, &env, &["logs", "daemon", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(!stdout(&output).contains("shared supervisor diagnostic"));
        assert!(stdout(&output).contains("gateway output"));
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "shared supervisor diagnostic\n"
        );
        fs::write(
            root.child("ocm-home/supervisor/daemon-log-config.json"),
            "invalid json",
        )
        .unwrap();
        let output = run_ocm(&cwd, &env, &["logs", "daemon", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("gateway output"));
        assert!(!stdout(&output).contains("shared supervisor diagnostic"));
    }
}

#[cfg(unix)]
#[test]
fn followers_report_initial_native_child_log_read_errors() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use crate::support::{install_fake_systemd_tools, write_executable_script};

    for name in ["ordinary", "daemon", "Daemon"] {
        let root = TestDir::new("logs-follow-child-read-error");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        install_fake_systemd_tools(&root, &mut env);
        let launcher = root.child("child");
        write_executable_script(
            &launcher,
            "#!/bin/sh\nprintf 'child stdout\\n'\nprintf 'child stderr\\n' >&2\n",
        );
        for args in [
            vec![
                "launcher",
                "add",
                "fixture",
                "--command",
                launcher.to_str().unwrap(),
            ],
            vec!["env", "create", name, "--launcher", "fixture"],
            vec!["service", "start", name],
        ] {
            let output = run_ocm(&cwd, &env, &args);
            assert!(output.status.success(), "{}", stderr(&output));
        }
        let logs = root.child("ocm-home/supervisor/logs");
        let diagnostic = fs::File::create(logs.join("daemon.stderr.log")).unwrap();
        let produced = Command::new(env!("CARGO_BIN_EXE_ocm"))
            .current_dir(&cwd)
            .env_clear()
            .envs(&env)
            .args(["__daemon", "run", "--once"])
            .stderr(diagnostic)
            .output()
            .unwrap();
        assert!(produced.status.success());
        for (stream, file_stream) in [("info", "stdout"), ("error", "stderr")] {
            let prefix = if name.eq_ignore_ascii_case("daemon") {
                "."
            } else {
                ""
            };
            let path = logs.join(format!("{prefix}{name}.{file_stream}.log"));
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                format!("child {file_stream}\n")
            );
            for merged in [false, true] {
                for pretty in [false, true] {
                    let permissions = fs::metadata(&path).unwrap().permissions();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
                    let denied = fs::File::open(&path);
                    if denied.is_ok() {
                        fs::set_permissions(&path, permissions).unwrap();
                        continue; // Mode bits cannot stage denial for a privileged user.
                    }
                    let mut args = vec!["logs", name, "--follow"];
                    if !merged {
                        args.extend(["--stream", stream]);
                    }
                    if pretty {
                        args.extend(["--color", "always"]);
                    } else {
                        args.push("--raw");
                    }
                    let mut child = Command::new(env!("CARGO_BIN_EXE_ocm"))
                        .current_dir(&cwd)
                        .env_clear()
                        .envs(&env)
                        .args(&args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut timed_out = false;
                    while child.try_wait().unwrap().is_none() {
                        if Instant::now() >= deadline {
                            timed_out = true;
                            let _ = child.kill();
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    let output = child.wait_with_output().unwrap();
                    fs::set_permissions(&path, permissions).unwrap();
                    assert_eq!(
                        denied.unwrap_err().kind(),
                        std::io::ErrorKind::PermissionDenied
                    );
                    assert!(!timed_out, "reader stayed pending: {args:?}");
                    assert!(!output.status.success(), "{args:?}");
                    assert!(
                        stderr(&output).contains("Permission denied"),
                        "{args:?}: {}",
                        stderr(&output)
                    );
                    assert!(
                        output.stdout.is_empty(),
                        "must fail before header/output: {args:?}"
                    );
                }
            }
            // A child read error must not block a Gateway file that wins the
            // normal freshness comparison, including the equal-time rule.
            let gateway = root.child(&format!(
                "ocm-home/envs/{name}/.openclaw/logs/{}",
                if stream == "info" {
                    "gateway.log"
                } else {
                    "gateway.err.log"
                }
            ));
            fs::create_dir_all(gateway.parent().unwrap()).unwrap();
            fs::write(&gateway, "current gateway\n").unwrap();
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            let permissions = fs::metadata(&path).unwrap().permissions();
            fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
            let snapshots = [modified, modified + Duration::from_secs(1)].map(|time| {
                fs::File::open(&gateway)
                    .unwrap()
                    .set_modified(time)
                    .unwrap();
                run_ocm(&cwd, &env, &["logs", name, "--stream", stream, "--json"])
            });
            let (mut child, receiver) = spawn_follow(
                &cwd,
                &env,
                &["logs", name, "--stream", stream, "--follow", "--raw"],
            );
            let line = receiver.recv_timeout(Duration::from_secs(5));
            let _ = child.kill();
            let _ = child.wait();
            fs::set_permissions(&path, permissions).unwrap();
            for output in snapshots {
                assert!(output.status.success(), "{}", stderr(&output));
                let body: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(body["sourceKind"], "gateway");
                assert_eq!(body["content"], "current gateway\n");
            }
            assert_eq!(line.unwrap(), "current gateway");
        }
    }
}

#[cfg(unix)]
#[test]
fn existing_followers_exclude_native_diagnostics_after_directory_refresh() {
    use crate::support::{install_fake_launchctl, write_executable_script};

    let root = TestDir::new("logs-follow-refresh-diagnostics");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
    env.insert("OCM_SELF".into(), env!("CARGO_BIN_EXE_ocm").into());
    install_fake_launchctl(&root, &mut env);
    let launcher = root.child("child");
    write_executable_script(&launcher, "#!/bin/sh\nprintf 'child output\\n' >&2\n");
    for args in [
        vec![
            "launcher",
            "add",
            "fixture",
            "--command",
            launcher.to_str().unwrap(),
        ],
        vec!["env", "create", "daemon", "--launcher", "fixture"],
        vec!["service", "start", "daemon"],
    ] {
        let output = run_ocm(&cwd, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let daemon_once = |directory: &std::path::Path| {
        fs::create_dir_all(directory).unwrap();
        let diagnostic = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("daemon.stderr.log"))
            .unwrap();
        Command::new(env!("CARGO_BIN_EXE_ocm"))
            .current_dir(&cwd)
            .env_clear()
            .envs(&env)
            .args(["__daemon", "run", "--once"])
            .stdout(Stdio::null())
            .stderr(diagnostic)
            .status()
            .unwrap()
    };
    let original = root.child("ocm-home/supervisor/logs");
    assert!(daemon_once(&original).success());
    let child_path = original.join(".daemon.stderr.log");
    assert!(
        fs::read_to_string(&child_path)
            .unwrap()
            .contains("child output")
    );

    struct Followers(Vec<std::process::Child>);
    impl Drop for Followers {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut followers = Followers(Vec::new());
    let mut receivers = Vec::new();
    for stream_args in [vec![], vec!["--stream", "error"]] {
        let mut args = vec!["logs", "daemon", "--follow", "--raw"];
        args.extend(stream_args);
        let (child, receiver) = spawn_follow(&cwd, &env, &args);
        followers.0.push(child);
        receive_line_containing(&receiver, "child output");
        receivers.push(receiver);
    }

    // A supported refresh publishes each new directory while the same readers
    // stay attached. Before selection, the extra link is ordinary child history.
    for generation in ["first", "second"] {
        let next = root.child(generation);
        fs::create_dir_all(&next).unwrap();
        fs::hard_link(&child_path, next.join("daemon.stderr.log")).unwrap();
        let mut refresh_env = env.clone();
        refresh_env.insert("OCM_DAEMON_LOG_DIR".into(), path_string(&next));
        let output = run_ocm(
            &cwd,
            &refresh_env,
            &[
                "service",
                "refresh-daemon",
                "--acknowledge-gateway-restarts",
                "--json",
            ],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let summary: Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(
            summary["stderrPath"],
            path_string(&next.join("daemon.stderr.log"))
        );
        assert!(!daemon_once(&next).success());
        let diagnostics = fs::read_to_string(next.join("daemon.stderr.log")).unwrap();
        assert!(
            diagnostics.contains("ocm service: starting daemon"),
            "{diagnostics}"
        );
        assert!(
            diagnostics.contains("aliases the supervisor daemon's diagnostics"),
            "{diagnostics}"
        );
        // Keep the known current alias present for several polls; otherwise a
        // later replacement could hide a stale reader's failure by timing alone.
        thread::sleep(Duration::from_millis(750));
        let fresh = run_ocm(
            &cwd,
            &env,
            &["logs", "daemon", "--stream", "error", "--raw"],
        );
        assert!(!fresh.status.success());
        assert!(stdout(&fresh).is_empty());
        for receiver in &receivers {
            assert!(
                receiver.try_iter().collect::<Vec<_>>().is_empty(),
                "existing follower exposed diagnostic bytes"
            );
        }
        fs::remove_file(&child_path).unwrap();
        let history = format!("independent child history {generation}");
        fs::write(&child_path, format!("{history}\n")).unwrap();
        for receiver in &receivers {
            assert_eq!(receive_line_containing(receiver, &history), history);
        }
        let fresh = run_ocm(
            &cwd,
            &env,
            &["logs", "daemon", "--stream", "error", "--raw"],
        );
        assert!(fresh.status.success(), "{}", stderr(&fresh));
        assert_eq!(stdout(&fresh), format!("{history}\n"));
    }
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

#[cfg(unix)]
#[test]
fn logs_follow_delivers_continuous_foreground_output_before_the_writer_stops() {
    use crate::support::write_executable_script;

    let root = TestDir::new("logs-follow-foreground");
    let cwd = root.child("workspace");
    let source = root.child("source");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(source.join("scripts")).unwrap();
    fs::create_dir_all(source.join("extensions")).unwrap();
    fs::create_dir_all(source.join("node_modules/tsx")).unwrap();
    fs::write(
        source.join("package.json"),
        r#"{"name":"openclaw","version":"2026.4.19","type":"module","devDependencies":{"tsx":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        source.join("node_modules/tsx/package.json"),
        r#"{"name":"tsx","type":"module","exports":"./index.mjs"}"#,
    )
    .unwrap();
    fs::write(source.join("node_modules/tsx/index.mjs"), "export {};\n").unwrap();
    for entry in ["openclaw.mjs", "scripts/run-node.mjs"] {
        fs::write(source.join(entry), "console.log('fixture');\n").unwrap();
    }
    fs::write(
        source.join("scripts/watch-node.mjs"),
        format!(
            r#"import fs from 'node:fs';
const root = {};
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const released = () => fs.existsSync(root + '/release');
fs.writeSync(1, 'seed\n');
while (!fs.existsSync(root + '/start') && !released()) await sleep(10);
for (let i = 0; !released(); i++) {{
  fs.writeSync(1, `live-${{i}}\n`);
  await sleep(10);
}}
"#,
            serde_json::to_string(&path_string(root.path())).unwrap()
        ),
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        let output = Command::new("git")
            .current_dir(&source)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let env = ocm_env(&root);
    let stable = root.child("bin/stable");
    write_executable_script(&stable, "#!/bin/sh\nexit 0\n");
    for args in [
        vec![
            "runtime",
            "add",
            "stable",
            "--path",
            stable.to_str().unwrap(),
        ],
        vec!["env", "create", "demo", "--runtime", "stable"],
    ] {
        let output = run_ocm(&cwd, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }

    // Release the real producer even if a follower assertion fails. Reap the
    // foreground controller before stopping either follower or deleting state.
    struct Processes {
        children: Vec<std::process::Child>,
        release: std::path::PathBuf,
    }
    impl Drop for Processes {
        fn drop(&mut self) {
            let _ = fs::write(&self.release, "");
            if let Some(controller) = self.children.first_mut() {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while matches!(controller.try_wait(), Ok(None))
                    && std::time::Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            for child in &mut self.children {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut processes = Processes {
        children: Vec::new(),
        release: root.child("release"),
    };
    let (controller, foreground) = spawn_follow(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            source.to_str().unwrap(),
            "--watch",
            "--force",
            "--no-ui",
        ],
    );
    processes.children.push(controller);
    receive_line_containing(&foreground, "seed");
    let mut followers = Vec::new();
    for extra in [vec![], vec!["--stream", "info"]] {
        let mut args = vec!["logs", "demo", "--follow", "--raw", "--tail", "1"];
        args.extend(extra);
        let (child, lines) = spawn_follow(&cwd, &env, &args);
        processes.children.push(child);
        assert_eq!(receive_line_containing(&lines, "seed"), "seed");
        followers.push(lines);
    }
    fs::write(root.child("start"), "").unwrap();
    for sequence in 0..50 {
        let expected = format!("live-{sequence}");
        for lines in &followers {
            assert_eq!(
                lines.recv_timeout(Duration::from_secs(5)).unwrap(),
                expected
            );
        }
    }
    assert!(!root.child("release").exists());
    for child in &mut processes.children {
        assert!(child.try_wait().unwrap().is_none());
    }
    let (log, _) = gateway_log_paths(&root);
    assert!(fs::read_to_string(log).unwrap().contains("live-49\n"));
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
