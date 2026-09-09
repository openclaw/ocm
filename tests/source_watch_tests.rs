mod support;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use ocm::env::EnvironmentService;
use ocm::supervisor::SupervisorService;
use serde_json::{Value, json};

use crate::support::{
    TestDir, install_fake_service_manager, ocm_env, path_string, run_ocm, stderr, stdout,
    write_executable_script,
};

fn install_fake_node(root: &TestDir, env: &mut BTreeMap<String, String>) -> PathBuf {
    let bin_dir = root.child("fake-node-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let node_log = root.child("node.log");
    let node = format!(
        "#!/bin/sh\nprintf 'pwd=%s|args=%s|bundled=%s|devroot=%s\\n' \"$PWD\" \"$*\" \"$OPENCLAW_BUNDLED_PLUGINS_DIR\" \"$OPENCLAW_DEV_SOURCE_ROOT\" >> \"{}\"\nprintf 'node-ok\\n'\n",
        path_string(&node_log)
    );
    write_executable_script(&bin_dir.join("node"), &node);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    env.insert(
        "PATH".to_string(),
        if existing_path.is_empty() {
            path_string(&bin_dir)
        } else {
            format!("{}:{existing_path}", path_string(&bin_dir))
        },
    );
    node_log
}

fn create_source_repo(root: &TestDir) -> PathBuf {
    let repo = root.child("source/openclaw");
    fs::create_dir_all(repo.join("extensions/codex")).unwrap();
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::write(repo.join("package.json"), r#"{"name":"openclaw"}"#).unwrap();
    fs::write(repo.join("openclaw.mjs"), "console.log('openclaw');\n").unwrap();
    fs::write(repo.join("scripts/run-node.mjs"), "").unwrap();
    fs::write(repo.join("scripts/watch-node.mjs"), "").unwrap();
    fs::write(
        repo.join("extensions/codex/openclaw.plugin.json"),
        r#"{"id":"codex"}"#,
    )
    .unwrap();
    repo
}

fn create_runtime_backed_env(
    root: &TestDir,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> PathBuf {
    let runtime_path = root.child("bin/stable-openclaw");
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    write_executable_script(&runtime_path, "#!/bin/sh\nprintf 'runtime-ok\\n'\n");

    let runtime = run_ocm(
        cwd,
        env,
        &[
            "runtime",
            "add",
            "stable",
            "--path",
            &path_string(&runtime_path),
        ],
    );
    assert!(runtime.status.success(), "{}", stderr(&runtime));

    let created = run_ocm(
        cwd,
        env,
        &[
            "env",
            "create",
            "demo",
            "--runtime",
            "stable",
            "--port",
            "21901",
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    runtime_path
}

fn write_active_source_watch_override(root: &TestDir, source_repo: &Path) {
    let path = root.child("ocm-home/source-watch/demo.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        json!({
            "kind": "ocm-source-watch-override",
            "envName": "demo",
            "repoRoot": path_string(source_repo),
            "watchPid": std::process::id(),
            "token": "test-source-watch",
            "startedAt": "2026-06-17T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
}

struct SourceWatchFixture {
    lock_file: File,
}

impl Drop for SourceWatchFixture {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

fn lock_source_watch(root: &TestDir) -> SourceWatchFixture {
    lock_source_watch_with_id(root, "")
}

fn lock_source_watch_with_id(root: &TestDir, lease_id: &str) -> SourceWatchFixture {
    let lock_path = root.child("ocm-home/source-watch/demo.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let mut lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    FileExt::lock_exclusive(&lock_file).unwrap();
    lock_file.set_len(0).unwrap();
    use std::io::Write;
    writeln!(lock_file, "{lease_id}").unwrap();
    SourceWatchFixture { lock_file }
}

fn share_source_watch_lock_with_id(root: &TestDir, lease_id: &str) -> SourceWatchFixture {
    let lock_path = root.child("ocm-home/source-watch/demo.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let mut lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    FileExt::lock_shared(&lock_file).unwrap();
    lock_file.set_len(0).unwrap();
    use std::io::Write;
    writeln!(lock_file, "{lease_id}").unwrap();
    SourceWatchFixture { lock_file }
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

#[test]
fn source_watch_override_takes_precedence_for_resolve_and_run() {
    let root = TestDir::new("source-watch-resolve-run");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let mut env = ocm_env(&root);
    let node_log = install_fake_node(&root, &mut env);
    let runtime_path = create_runtime_backed_env(&root, &cwd, &env);
    let _source_watch = lock_source_watch(&root);
    write_active_source_watch_override(&root, &source_repo);

    let entry = source_repo.join("openclaw.mjs");
    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );
    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["bindingKind"], "source-watch");
    assert_eq!(resolved["bindingName"], "source-watch");
    assert_eq!(
        resolved["command"],
        format!("node {} status", path_string(&entry))
    );
    assert_eq!(resolved["binaryPath"], path_string(&entry));
    assert_eq!(resolved["runDir"], path_string(&source_repo));
    assert_eq!(
        resolved["forwardedArgs"],
        Value::Array(vec![Value::String("status".to_string())])
    );

    let explicit = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "resolve",
            "demo",
            "--runtime",
            "stable",
            "--json",
            "--",
            "status",
        ],
    );
    assert!(explicit.status.success(), "{}", stderr(&explicit));
    let explicit: Value = serde_json::from_str(&stdout(&explicit)).unwrap();
    assert_eq!(explicit["bindingKind"], "runtime");
    assert_eq!(explicit["bindingName"], "stable");
    assert_eq!(explicit["binaryPath"], path_string(&runtime_path));

    let run = run_ocm(&cwd, &env, &["@demo", "--", "status"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "node-ok\n");

    let node_log = fs::read_to_string(node_log).unwrap();
    assert!(node_log.contains(&format!(
        "args={} status|bundled={}|devroot={}",
        path_string(&entry),
        path_string(&source_repo.join("extensions")),
        path_string(&source_repo)
    )));
}

#[test]
fn source_watch_blocks_direct_service_policy_changes() {
    let root = TestDir::new("source-watch-service-policy");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    create_runtime_backed_env(&root, &cwd, &env);
    let service = EnvironmentService::new(&env, &cwd);
    let before = service.get("demo").unwrap();
    let _source_watch = lock_source_watch(&root);
    write_active_source_watch_override(&root, &source_repo);

    let error = service
        .set_service_policy("demo", Some(true), Some(true))
        .unwrap_err();
    assert!(error.contains("source watch is active"), "{error}");
    let mut changed = before.clone();
    changed.service_enabled = true;
    changed.service_running = true;
    let error = ocm::store::save_environment(changed, &env, &cwd).unwrap_err();
    assert!(error.contains("source watch is active"), "{error}");
    let after = service.get("demo").unwrap();
    assert_eq!(after.service_enabled, before.service_enabled);
    assert_eq!(after.service_running, before.service_running);
}

#[test]
fn service_policy_admission_rechecks_a_watch_claimed_while_waiting() {
    let root = TestDir::new("source-watch-service-admission");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    create_runtime_backed_env(&root, &cwd, &env);
    let admission_path = root.child("ocm-home/source-watch/demo.admission.lock");
    fs::create_dir_all(admission_path.parent().unwrap()).unwrap();
    let admission = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&admission_path)
        .unwrap();
    FileExt::lock_exclusive(&admission).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let worker_env = env.clone();
    let worker_cwd = cwd.clone();
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = EnvironmentService::new(&worker_env, &worker_cwd).set_service_policy(
            "demo",
            Some(true),
            Some(true),
        );
        result_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early_result = result_rx.recv_timeout(Duration::from_millis(100));
    let _source_watch = lock_source_watch_with_id(&root, "starting-lease");
    drop(admission);
    let result = match early_result {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            result_rx.recv_timeout(Duration::from_secs(5)).unwrap()
        }
        Err(error) => panic!("service policy worker disconnected: {error}"),
    };
    worker.join().unwrap();

    let error = result.unwrap_err();
    assert!(error.contains("source watch"), "{error}");
    let meta = EnvironmentService::new(&env, &cwd).get("demo").unwrap();
    assert!(!meta.service_enabled);
    assert!(!meta.service_running);
}

#[test]
fn source_watch_blocks_planning_and_execution_of_a_saved_service_plan() {
    let root = TestDir::new("source-watch-service-plan");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    create_runtime_backed_env(&root, &cwd, &env);
    EnvironmentService::new(&env, &cwd)
        .set_service_policy("demo", Some(true), Some(true))
        .unwrap();
    let supervisor = SupervisorService::new(&env, &cwd);
    supervisor.sync().unwrap();
    let source_watch = lock_source_watch(&root);
    write_active_source_watch_override(&root, &source_repo);

    let plan = serde_json::to_value(supervisor.plan().unwrap()).unwrap();
    assert!(plan["children"].as_array().unwrap().is_empty());
    assert!(plan["skippedEnvs"].as_array().unwrap().iter().any(|entry| {
        entry["envName"] == "demo" && entry["reason"].as_str().unwrap().contains("source watch")
    }));
    let error = supervisor.run(true).unwrap_err();
    assert!(error.contains("source watch is active"), "{error}");

    drop(source_watch);
    let run = supervisor.run(true).unwrap();
    assert_eq!(run.child_results.len(), 1);
    assert!(run.child_results[0].success);

    // Older versions could persist the temporary source resolution as a service
    // plan. Ending its lease must not make that temporary binding runnable.
    let state_path = root.child("ocm-home/supervisor/state.json");
    let mut saved: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    saved["children"][0]["bindingKind"] = json!("source-watch");
    fs::write(&state_path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let error = supervisor.run(true).unwrap_err();
    assert!(error.contains("saved service plan belongs to a source-watch session"));
}

#[test]
fn stale_source_watch_without_a_lock_is_cleaned_under_a_new_lock() {
    let root = TestDir::new("source-watch-stale-pid");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    let runtime_path = create_runtime_backed_env(&root, &cwd, &env);
    let override_path = root.child("ocm-home/source-watch/demo.json");
    let lock_path = root.child("ocm-home/source-watch/demo.lock");
    fs::create_dir_all(override_path.parent().unwrap()).unwrap();
    fs::write(
        &override_path,
        json!({
            "kind": "ocm-source-watch-override",
            "envName": "demo",
            "repoRoot": path_string(&source_repo),
            "watchPid": std::process::id(),
            "token": "<redacted>",
            "startedAt": "2026-06-17T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );
    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["bindingKind"], "runtime");
    assert_eq!(resolved["binaryPath"], path_string(&runtime_path));
    assert!(!override_path.exists());
    assert!(lock_path.exists());
}

#[cfg(unix)]
#[test]
fn source_watch_lookup_without_state_keeps_a_read_only_home_unchanged() {
    let root = TestDir::new("source-watch-read-only-home");
    let cwd = root.child("workspace");
    let ocm_home = root.child("ocm-home");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&ocm_home).unwrap();
    let env = ocm_env(&root);
    fs::set_permissions(&ocm_home, fs::Permissions::from_mode(0o500)).unwrap();

    let result = EnvironmentService::new(&env, &cwd).active_source_watch_override("demo");

    fs::set_permissions(&ocm_home, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.unwrap().is_none());
    assert!(!ocm_home.join("source-watch").exists());
}

#[test]
fn concurrent_stale_lock_readers_do_not_revive_source_watch_metadata() {
    let root = TestDir::new("source-watch-stale-reader");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    let runtime_path = create_runtime_backed_env(&root, &cwd, &env);
    let lease_id = "stale-reader";
    let _stale_reader = share_source_watch_lock_with_id(&root, lease_id);
    let override_path = root.child("ocm-home/source-watch/demo.json");
    fs::write(
        &override_path,
        json!({
            "kind": "ocm-source-watch-override",
            "envName": "demo",
            "repoRoot": path_string(&source_repo),
            "watchPid": std::process::id(),
            "token": format!("lease:{lease_id}:{}", "<redacted>"),
            "startedAt": "2026-06-17T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );

    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["bindingKind"], "runtime");
    assert_eq!(resolved["binaryPath"], path_string(&runtime_path));
    assert!(!override_path.exists());
}

#[test]
fn held_source_watch_lease_without_metadata_blocks_runtime_fallback() {
    let root = TestDir::new("source-watch-starting");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    create_runtime_backed_env(&root, &cwd, &env);
    let _source_watch = lock_source_watch_with_id(&root, "starting-lease");

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );

    assert!(!resolve.status.success());
    assert!(
        stderr(&resolve).contains(
            "source watch for env \"demo\" is active or starting, but its metadata is unavailable"
        ),
        "{}",
        stderr(&resolve)
    );
}

#[test]
fn restoring_source_watch_lease_allows_runtime_fallback() {
    let root = TestDir::new("source-watch-restoring");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    let runtime_path = create_runtime_backed_env(&root, &cwd, &env);
    let _source_watch = lock_source_watch_with_id(&root, "restoring:fixture-lease");

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );

    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["bindingKind"], "runtime");
    assert_eq!(resolved["binaryPath"], path_string(&runtime_path));

    let overlapping_watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&source_repo),
            "--watch",
            "--force",
        ],
    );
    assert!(!overlapping_watch.status.success());
    assert!(
        stderr(&overlapping_watch).contains("already active or starting"),
        "{}",
        stderr(&overlapping_watch)
    );
}

#[test]
fn leased_source_watch_remains_active_after_its_wrapper_pid_exits() {
    let root = TestDir::new("source-watch-descendant-lease");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    create_runtime_backed_env(&root, &cwd, &env);
    let lease_id = "fixture-lease";
    let _source_watch = lock_source_watch_with_id(&root, lease_id);
    let override_path = root.child("ocm-home/source-watch/demo.json");
    let lease_token = format!("lease:{lease_id}:{}", "<redacted>");
    fs::write(
        &override_path,
        json!({
            "kind": "ocm-source-watch-override",
            "envName": "demo",
            "repoRoot": path_string(&source_repo),
            "watchPid": u32::MAX,
            "token": lease_token,
            "startedAt": "2026-06-17T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );
    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["bindingKind"], "source-watch");
    assert!(override_path.exists());
}

#[test]
fn leased_source_watch_blocks_fallback_for_metadata_from_an_older_lease() {
    let root = TestDir::new("source-watch-stale-generation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let env = ocm_env(&root);
    create_runtime_backed_env(&root, &cwd, &env);
    let _source_watch = lock_source_watch_with_id(&root, "current-lease");
    let override_path = root.child("ocm-home/source-watch/demo.json");
    let lease_token = format!("lease:stale-lease:{}", "<redacted>");
    fs::write(
        &override_path,
        json!({
            "kind": "ocm-source-watch-override",
            "envName": "demo",
            "repoRoot": path_string(&source_repo),
            "watchPid": std::process::id(),
            "token": lease_token,
            "startedAt": "2026-06-17T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );
    assert!(!resolve.status.success());
    assert!(
        stderr(&resolve).contains("metadata does not match the active lease"),
        "{}",
        stderr(&resolve)
    );
    assert!(override_path.exists());
}

#[cfg(unix)]
#[test]
fn live_legacy_source_watch_remains_active_until_its_process_exits() {
    let root = TestDir::new("source-watch-live-legacy");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    fs::create_dir_all(source_repo.join("scripts")).unwrap();
    fs::write(
        source_repo.join("scripts/watch-node.mjs"),
        "// legacy watch\n",
    )
    .unwrap();
    let env = ocm_env(&root);
    let runtime_path = create_runtime_backed_env(&root, &cwd, &env);

    let long_legacy_dir = (0..10).fold(root.child("legacy-bin"), |path, index| {
        path.join(format!(
            "segment-{index:02}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        ))
    });
    let legacy_bin = long_legacy_dir.join("node");
    let started = root.child("legacy-watch.started");
    let release = root.child("legacy-watch.release");
    write_executable_script(
        &legacy_bin,
        &format!(
            "#!/bin/sh\nprintf 'ready\\n' > \"{}\"\nwhile [ ! -f \"{}\" ]; do /bin/sleep 0.05; done\n",
            path_string(&started),
            path_string(&release)
        ),
    );
    let mut legacy_watch = Command::new("/bin/sh");
    legacy_watch
        .arg(&legacy_bin)
        .args([
            "scripts/watch-node.mjs",
            "gateway",
            "run",
            "--port",
            "21901",
        ])
        .current_dir(&source_repo)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut legacy_watch = legacy_watch.spawn().unwrap();
    assert!(
        wait_for_path(&started, Duration::from_secs(10)),
        "legacy source watch did not start"
    );

    let override_path = root.child("ocm-home/source-watch/demo.json");
    fs::create_dir_all(override_path.parent().unwrap()).unwrap();
    fs::write(root.child("ocm-home/source-watch/demo.lock"), "stale\n").unwrap();
    fs::write(
        &override_path,
        json!({
            "kind": "ocm-source-watch-override",
            "envName": "demo",
            "repoRoot": path_string(&source_repo),
            "watchPid": legacy_watch.id(),
            "token": "<redacted>",
            "startedAt": "2026-06-17T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let active = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );
    assert!(active.status.success(), "{}", stderr(&active));
    let active: Value = serde_json::from_str(&stdout(&active)).unwrap();
    assert_eq!(active["bindingKind"], "source-watch");
    assert!(override_path.exists());

    fs::write(&release, "release\n").unwrap();
    assert!(legacy_watch.wait().unwrap().success());

    let stale = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "status"],
    );
    assert!(stale.status.success(), "{}", stderr(&stale));
    let stale: Value = serde_json::from_str(&stdout(&stale)).unwrap();
    assert_eq!(stale["bindingKind"], "runtime");
    assert_eq!(stale["binaryPath"], path_string(&runtime_path));
    assert!(!override_path.exists());
}

#[test]
fn source_watch_override_flows_to_env_exec_and_status_surfaces() {
    let root = TestDir::new("source-watch-exec-status");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let source_repo = create_source_repo(&root);
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let node_log = install_fake_node(&root, &mut env);
    create_runtime_backed_env(&root, &cwd, &env);
    let _source_watch = lock_source_watch(&root);
    write_active_source_watch_override(&root, &source_repo);

    let entry = source_repo.join("openclaw.mjs");
    let exec = run_ocm(
        &cwd,
        &env,
        &["env", "exec", "demo", "--", "openclaw", "status"],
    );
    assert!(exec.status.success(), "{}", stderr(&exec));
    assert_eq!(stdout(&exec), "node-ok\n");

    let inherited = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "exec",
            "demo",
            "--",
            "sh",
            "-lc",
            "printf '%s|%s' \"$OPENCLAW_DEV_SOURCE_ROOT\" \"$OPENCLAW_BUNDLED_PLUGINS_DIR\"",
        ],
    );
    assert!(inherited.status.success(), "{}", stderr(&inherited));
    assert_eq!(
        stdout(&inherited),
        format!(
            "{}|{}",
            path_string(&source_repo),
            path_string(&source_repo.join("extensions"))
        )
    );

    let env_status = run_ocm(&cwd, &env, &["env", "status", "demo", "--json"]);
    assert!(env_status.status.success(), "{}", stderr(&env_status));
    let env_status: Value = serde_json::from_str(&stdout(&env_status)).unwrap();
    assert_eq!(env_status["resolvedKind"], "source-watch");
    assert_eq!(env_status["binaryPath"], path_string(&entry));
    assert_eq!(env_status["runDir"], path_string(&source_repo));

    let service_status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(
        service_status.status.success(),
        "{}",
        stderr(&service_status)
    );
    let service_status: Value = serde_json::from_str(&stdout(&service_status)).unwrap();
    assert_eq!(service_status["bindingKind"], "source-watch");
    assert_eq!(
        service_status["command"],
        format!("node {} gateway run --port 21901", path_string(&entry))
    );
    assert_eq!(service_status["binaryPath"], "node");
    assert_eq!(service_status["args"][0], path_string(&entry));
    assert_eq!(service_status["args"][1], "gateway");
    assert_eq!(service_status["runDir"], path_string(&source_repo));

    let node_log = fs::read_to_string(node_log).unwrap();
    assert!(node_log.contains(&format!(
        "args={} status|bundled={}|devroot={}",
        path_string(&entry),
        path_string(&source_repo.join("extensions")),
        path_string(&source_repo)
    )));
}
