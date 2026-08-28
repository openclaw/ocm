mod support;

use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use ocm::env::EnvironmentService;
use ocm::store::{now_utc, supervisor_runtime_path, supervisor_state_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::{Value, json};

use crate::support::{
    TestDir, install_fake_launchctl, install_fake_systemd_tools, managed_service_definition_path,
    ocm_env, path_string, run_ocm, stderr, write_executable_script,
};

const COPIED_BINARY_BUSY_ATTEMPTS: usize = 5;
const COPIED_BINARY_BUSY_RETRY_MS: u64 = 20;

fn launchd_env(root: &TestDir) -> BTreeMap<String, String> {
    let mut env = ocm_env(root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(root, &mut env);
    env
}

fn systemd_env(root: &TestDir) -> BTreeMap<String, String> {
    let mut env = ocm_env(root);
    install_fake_systemd_tools(root, &mut env);
    env
}

fn unsupported_env(root: &TestDir) -> BTreeMap<String, String> {
    let mut env = ocm_env(root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "unsupported".to_string(),
    );
    env
}

fn setup_launcher_env(cwd: &Path, env: &BTreeMap<String, String>) {
    let launcher = run_ocm(
        cwd,
        env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let created = run_ocm(cwd, env, &["env", "create", "demo", "--launcher", "stable"]);
    assert!(created.status.success(), "{}", stderr(&created));
}

fn setup_gateway_aware_restart_fixture(
    root: &TestDir,
    restart_handoff: &str,
) -> (
    std::path::PathBuf,
    BTreeMap<String, String>,
    std::path::PathBuf,
) {
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = launchd_env(root);
    let invocation_log = root.child("gateway-restart-invocations.log");
    let launcher = root.child("bin/openclaw");
    write_executable_script(
        &launcher,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '{{\"ok\":true}}\\n'\n",
            path_string(&invocation_log)
        ),
    );

    let added = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "stable",
            "--command",
            &path_string(&launcher),
        ],
    );
    assert!(added.status.success(), "{}", stderr(&added));
    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: env.get("OCM_HOME").unwrap().clone(),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            gateway_state: "running".to_string(),
            restart_handoff: Some(restart_handoff.to_string()),
            restart_count: 0,
            child_port: 18789,
            pid: Some(4242),
            stdout_path: path_string(&root.child("demo.stdout.log")),
            stderr_path: path_string(&root.child("demo.stderr.log")),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
        }],
        children: vec![SupervisorRuntimeChild {
            env_name: "demo".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            pid: 4242,
            restart_count: 0,
            child_port: 18789,
            stdout_path: path_string(&root.child("demo.stdout.log")),
            stderr_path: path_string(&root.child("demo.stderr.log")),
        }],
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
    env.insert("OCM_ACTIVE_ENV".to_string(), "demo".to_string());
    env.insert("OPENCLAW_SERVICE_KIND".to_string(), "gateway".to_string());

    (cwd, env, invocation_log)
}

fn json_output(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn copy_executable_fixture(source: &Path, destination: &Path) {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::copy(source, destination).unwrap();
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(destination).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(destination, permissions).unwrap();
    }
}

fn copy_test_ocm_binary(destination: &Path) {
    copy_executable_fixture(Path::new(env!("CARGO_BIN_EXE_ocm")), destination);
    let identity = retry_executable_busy(|| {
        Command::new(destination)
            .args(["__daemon", "identity"])
            .env_clear()
            .output()
    })
    .unwrap();
    assert!(identity.status.success());
    assert_eq!(
        String::from_utf8_lossy(&identity.stdout).trim(),
        "ocm-service-supervisor"
    );
}

fn retry_executable_busy<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    for attempt in 0..COPIED_BINARY_BUSY_ATTEMPTS {
        match operation() {
            Ok(output) => return Ok(output),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < COPIED_BINARY_BUSY_ATTEMPTS =>
            {
                sleep(Duration::from_millis(COPIED_BINARY_BUSY_RETRY_MS));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("busy retry loop always returns on its final attempt")
}

fn prepend_path(env: &mut BTreeMap<String, String>, dir: &Path) {
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let path = if existing_path.is_empty() {
        path_string(dir)
    } else {
        format!("{}:{existing_path}", path_string(dir))
    };
    env.insert("PATH".to_string(), path);
}

#[test]
fn daemon_identity_does_not_initialize_a_store() {
    let root = TestDir::new("daemon-identity-no-store");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(&cwd)
        .env_clear()
        .args(["__daemon", "identity"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "ocm-service-supervisor"
    );
    assert!(!cwd.join(".ocm").exists());
}

#[test]
fn copied_binary_spawn_retries_executable_file_busy() {
    let attempts = std::cell::Cell::new(0);

    let result = retry_executable_busy(|| {
        let attempt = attempts.get();
        attempts.set(attempt + 1);
        if attempt == 0 {
            Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
        } else {
            Ok(())
        }
    });

    assert!(result.is_ok());
    assert_eq!(attempts.get(), 2);
}

#[test]
fn copied_binary_spawn_does_not_retry_other_errors() {
    let attempts = std::cell::Cell::new(0);

    let result: std::io::Result<()> = retry_executable_busy(|| {
        attempts.set(attempts.get() + 1);
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    });

    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(attempts.get(), 1);
}

#[test]
fn service_install_requires_a_target_env() {
    let root = TestDir::new("service-install-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    let output = run_ocm(&cwd, &env, &["service", "install"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("service install requires <env>"));
}

#[test]
fn service_lifecycle_commands_require_a_target_env() {
    let root = TestDir::new("service-lifecycle-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    for action in ["start", "stop", "restart", "uninstall"] {
        let output = run_ocm(&cwd, &env, &["service", action]);
        assert!(!output.status.success(), "{action} unexpectedly succeeded");
        assert!(stderr(&output).contains(&format!("service {action} requires <env>")));
    }
}

#[test]
fn service_status_defaults_to_the_all_env_view() {
    let root = TestDir::new("service-status-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "status", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert!(body["services"].is_array());
    assert_eq!(body["services"].as_array().unwrap().len(), 1);
    assert_eq!(body["services"][0]["envName"], "demo");
}

#[test]
fn unknown_service_commands_use_service_specific_errors() {
    let root = TestDir::new("service-unknown-command");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    let output = run_ocm(&cwd, &env, &["service", "wat"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unknown service command: wat"));
}

#[test]
fn service_install_enables_the_env_and_installs_the_ocm_service() {
    let root = TestDir::new("service-install");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "install", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], true);
    assert_eq!(body["desiredRunning"], false);

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_body = json_output(&env_show);
    assert_eq!(env_body["serviceEnabled"], true);
    assert_eq!(env_body["serviceRunning"], false);

    let service_path = managed_service_definition_path(&env, &cwd, "ocm");
    assert!(
        service_path.exists(),
        "missing {}",
        path_string(&service_path)
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(service_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn service_install_uses_valid_path_ocm_for_dev_artifact() {
    let root = TestDir::new("service-install-valid-path-ocm");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = launchd_env(&root);
    let installed_dir = root.child("installed-bin");
    let installed_ocm = installed_dir.join("ocm");
    copy_test_ocm_binary(&installed_ocm);
    prepend_path(&mut env, &installed_dir);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "install", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let plist = fs::read_to_string(managed_service_definition_path(&env, &cwd, "ocm")).unwrap();
    assert!(plist.contains(&path_string(&installed_ocm)), "{plist}");
}

#[test]
fn service_install_skips_conflicting_path_ocm_before_valid_candidate() {
    let root = TestDir::new("service-install-conflicting-path-ocm");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = launchd_env(&root);
    let bad_dir = root.child("bad-bin");
    let bad_ocm = bad_dir.join("ocm");
    copy_executable_fixture(Path::new("/bin/mkdir"), &bad_ocm);
    let installed_dir = root.child("installed-bin");
    let installed_ocm = installed_dir.join("ocm");
    copy_test_ocm_binary(&installed_ocm);
    prepend_path(&mut env, &installed_dir);
    prepend_path(&mut env, &bad_dir);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "install", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let plist = fs::read_to_string(managed_service_definition_path(&env, &cwd, "ocm")).unwrap();
    assert!(plist.contains(&path_string(&installed_ocm)), "{plist}");
    assert!(!plist.contains(&path_string(&bad_ocm)), "{plist}");
    assert!(!cwd.join("__daemon").exists());
    assert!(!cwd.join("identity").exists());
}

#[test]
fn service_install_resolves_relative_path_ocm_for_service_definition() {
    let root = TestDir::new("service-install-relative-path-ocm");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = launchd_env(&root);
    let relative_dir = Path::new("target/release");
    let installed_ocm = cwd.join(relative_dir).join("ocm");
    copy_test_ocm_binary(&installed_ocm);
    prepend_path(&mut env, relative_dir);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "install", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let plist = fs::read_to_string(managed_service_definition_path(&env, &cwd, "ocm")).unwrap();
    assert!(plist.contains(&path_string(&installed_ocm)), "{plist}");
    assert!(!plist.contains(">target/release/ocm<"), "{plist}");
}

#[test]
fn service_stop_and_uninstall_do_not_require_a_managed_service_backend() {
    let root = TestDir::new("service-stop-uninstall-unsupported");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = unsupported_env(&root);
    setup_launcher_env(&cwd, &env);

    let stop = run_ocm(&cwd, &env, &["service", "stop", "demo", "--json"]);
    assert!(stop.status.success(), "{}", stderr(&stop));
    let stop_body = json_output(&stop);
    assert_eq!(stop_body["installed"], true);
    assert_eq!(stop_body["desiredRunning"], false);

    let uninstall = run_ocm(&cwd, &env, &["service", "uninstall", "demo", "--json"]);
    assert!(uninstall.status.success(), "{}", stderr(&uninstall));
    let uninstall_body = json_output(&uninstall);
    assert_eq!(uninstall_body["installed"], false);
    assert_eq!(uninstall_body["desiredRunning"], false);
}

#[test]
fn service_start_still_requires_a_managed_service_backend() {
    let root = TestDir::new("service-start-unsupported");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = unsupported_env(&root);
    setup_launcher_env(&cwd, &env);
    let before = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(before.status.success(), "{}", stderr(&before));
    let before = json_output(&before);

    let output = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("managed services are not supported on this platform yet"));

    let after = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(after.status.success(), "{}", stderr(&after));
    let after = json_output(&after);
    assert_eq!(after["serviceEnabled"], before["serviceEnabled"]);
    assert_eq!(after["serviceRunning"], before["serviceRunning"]);
}

#[test]
fn service_start_marks_the_env_running() {
    let root = TestDir::new("service-start");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "start", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], true);
    assert_eq!(body["desiredRunning"], true);

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let env_body = json_output(&env_show);
    assert_eq!(env_body["serviceEnabled"], true);
    assert_eq!(env_body["serviceRunning"], true);
}

#[test]
fn service_start_replaces_stale_launchd_job_with_same_label() {
    let root = TestDir::new("service-start-stale-launchd");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let stale_plist = root.child("stale-home/Library/LaunchAgents/ai.openclaw.ocm.plist");
    fs::write(
        root.child("launchctl-print.txt"),
        format!(
            "path = {}\nstate = running\npid = 78428\n",
            path_string(&stale_plist)
        ),
    )
    .unwrap();

    let output = run_ocm(&cwd, &env, &["service", "start", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], true);
    assert_eq!(body["desiredRunning"], true);

    let log = fs::read_to_string(root.child("launchctl.log")).unwrap();
    assert!(
        log.lines()
            .any(|line| line.starts_with("bootout gui/") && line.ends_with("/ai.openclaw.ocm")),
        "{log}"
    );
    assert!(
        log.lines().any(|line| line.starts_with("bootstrap gui/")
            && line.ends_with(&path_string(&managed_service_definition_path(
                &env, &cwd, "ocm"
            )))),
        "{log}"
    );
}

#[test]
fn service_stop_keeps_the_env_installed_but_stopped() {
    let root = TestDir::new("service-stop");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let output = run_ocm(&cwd, &env, &["service", "stop", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], true);
    assert_eq!(body["desiredRunning"], false);

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let env_body = json_output(&env_show);
    assert_eq!(env_body["serviceEnabled"], true);
    assert_eq!(env_body["serviceRunning"], false);
    assert!(!managed_service_definition_path(&env, &cwd, "ocm").exists());
}

#[test]
fn service_restart_restores_running_policy() {
    let root = TestDir::new("service-restart");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let installed = run_ocm(&cwd, &env, &["service", "install", "demo"]);
    assert!(installed.status.success(), "{}", stderr(&installed));

    let output = run_ocm(&cwd, &env, &["service", "restart", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], true);
    assert_eq!(body["desiredRunning"], true);
}

#[test]
fn service_restart_requests_immediate_recovery_handoff_without_self_deadlock() {
    let root = TestDir::new("service-restart-gateway-aware");
    let (cwd, env, invocation_log) = setup_gateway_aware_restart_fixture(&root, "protocol-v1");

    let output = run_ocm(&cwd, &env, &["service", "restart", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["action"], "restart");
    assert_eq!(body["running"], true);
    assert!(body["warnings"].as_array().unwrap().iter().any(|warning| {
        warning
            .as_str()
            .unwrap()
            .contains("scheduled without waiting")
    }));
    // The handoff returns before the detached command necessarily writes its log.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let invocation = fs::read_to_string(&invocation_log).unwrap_or_default();
        if invocation == "gateway restart --force --json\n" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restart handoff invocation was not observed: {invocation:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn service_uninstall_disables_the_env_service() {
    let root = TestDir::new("service-uninstall");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let output = run_ocm(&cwd, &env, &["service", "uninstall", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], false);
    assert_eq!(body["desiredRunning"], false);

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let env_body = json_output(&env_show);
    assert_eq!(env_body["serviceEnabled"], false);
    assert_eq!(env_body["serviceRunning"], false);
    assert!(!managed_service_definition_path(&env, &cwd, "ocm").exists());
}

#[test]
fn systemd_service_uninstall_is_idempotent_after_stop_removed_the_unit() {
    let root = TestDir::new("service-systemd-stop-uninstall");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = systemd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let stopped = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert!(!managed_service_definition_path(&env, &cwd, "ocm").exists());

    write_executable_script(
        &root.child("fake-bin/systemctl"),
        "#!/bin/sh\nprintf 'unexpected systemctl call: %s\\n' \"$*\" >&2\nexit 1\n",
    );
    let uninstalled = run_ocm(&cwd, &env, &["service", "uninstall", "demo"]);
    assert!(uninstalled.status.success(), "{}", stderr(&uninstalled));
}

#[test]
fn service_stop_keeps_the_daemon_while_a_sibling_env_is_running() {
    let root = TestDir::new("service-stop-running-sibling");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);
    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "prod", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    for name in ["demo", "prod"] {
        let started = run_ocm(&cwd, &env, &["service", "start", name]);
        assert!(started.status.success(), "{}", stderr(&started));
    }

    let stopped = run_ocm(&cwd, &env, &["service", "stop", "demo", "--json"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert!(managed_service_definition_path(&env, &cwd, "ocm").exists());
}

#[test]
fn service_start_rejects_a_daemon_owned_by_another_store() {
    let root = TestDir::new("service-start-other-store-owner");
    let first_cwd = root.child("first-workspace");
    fs::create_dir_all(&first_cwd).unwrap();
    let first_env = launchd_env(&root);
    setup_launcher_env(&first_cwd, &first_env);
    let first_started = run_ocm(&first_cwd, &first_env, &["service", "start", "demo"]);
    assert!(first_started.status.success(), "{}", stderr(&first_started));

    let second_cwd = root.child("second-workspace");
    fs::create_dir_all(&second_cwd).unwrap();
    let mut second_env = first_env.clone();
    second_env.insert(
        "OCM_HOME".to_string(),
        path_string(&root.child("second-ocm-home")),
    );
    setup_launcher_env(&second_cwd, &second_env);

    let second_started = run_ocm(&second_cwd, &second_env, &["service", "start", "demo"]);
    assert!(!second_started.status.success());
    assert!(
        stderr(&second_started).contains("already bound to a different OCM_HOME"),
        "{}",
        stderr(&second_started)
    );
}

#[cfg(unix)]
#[test]
fn failed_initial_service_start_releases_the_daemon_owner() {
    let root = TestDir::new("service-start-initial-policy-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let ocm_home = Path::new(env.get("OCM_HOME").unwrap());
    let registry_lock = ocm_home.join("envs.lock");
    let mut permissions = fs::metadata(&registry_lock).unwrap().permissions();
    permissions.set_mode(0o400);
    fs::set_permissions(&registry_lock, permissions).unwrap();

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);

    let mut permissions = fs::metadata(&registry_lock).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&registry_lock, permissions).unwrap();
    assert!(!started.status.success());
    assert!(
        stderr(&started).contains("failed to open environment registry lock"),
        "{}",
        stderr(&started)
    );
    assert!(!managed_service_definition_path(&env, &cwd, "ocm").exists());
    assert!(!root.child("launchctl-print.txt").exists());

    let second_cwd = root.child("second-workspace");
    fs::create_dir_all(&second_cwd).unwrap();
    let mut second_env = env.clone();
    second_env.insert(
        "OCM_HOME".to_string(),
        path_string(&root.child("second-ocm-home")),
    );
    setup_launcher_env(&second_cwd, &second_env);
    let second_started = run_ocm(&second_cwd, &second_env, &["service", "start", "demo"]);
    assert!(
        second_started.status.success(),
        "{}",
        stderr(&second_started)
    );
}

#[test]
fn concurrent_service_starts_allow_only_one_store_owner() {
    let root = TestDir::new("service-start-concurrent-store-owners");
    let first_cwd = root.child("first-workspace");
    let second_cwd = root.child("second-workspace");
    fs::create_dir_all(&first_cwd).unwrap();
    fs::create_dir_all(&second_cwd).unwrap();

    let first_env = launchd_env(&root);
    setup_launcher_env(&first_cwd, &first_env);
    let mut second_env = first_env.clone();
    second_env.insert(
        "OCM_HOME".to_string(),
        path_string(&root.child("second-ocm-home")),
    );
    setup_launcher_env(&second_cwd, &second_env);

    let barrier = Arc::new(Barrier::new(3));
    let first_barrier = Arc::clone(&barrier);
    let first = thread::spawn(move || {
        first_barrier.wait();
        run_ocm(&first_cwd, &first_env, &["service", "start", "demo"])
    });
    let second_barrier = Arc::clone(&barrier);
    let second = thread::spawn(move || {
        second_barrier.wait();
        run_ocm(&second_cwd, &second_env, &["service", "start", "demo"])
    });
    barrier.wait();

    let outputs = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1
    );
    let rejected = outputs
        .iter()
        .find(|output| !output.status.success())
        .unwrap();
    assert!(
        stderr(rejected).contains("already bound to a different OCM_HOME"),
        "{}",
        stderr(rejected)
    );
}

#[test]
fn service_stop_does_not_remove_a_daemon_owned_by_another_store() {
    let root = TestDir::new("service-stop-other-store-owner");
    let first_cwd = root.child("first-workspace");
    fs::create_dir_all(&first_cwd).unwrap();
    let first_env = launchd_env(&root);
    setup_launcher_env(&first_cwd, &first_env);
    let first_started = run_ocm(&first_cwd, &first_env, &["service", "start", "demo"]);
    assert!(first_started.status.success(), "{}", stderr(&first_started));

    let second_cwd = root.child("second-workspace");
    fs::create_dir_all(&second_cwd).unwrap();
    let mut second_env = first_env.clone();
    second_env.insert(
        "OCM_HOME".to_string(),
        path_string(&root.child("second-ocm-home")),
    );
    setup_launcher_env(&second_cwd, &second_env);

    let stopped = run_ocm(&second_cwd, &second_env, &["service", "stop", "demo"]);
    assert!(!stopped.status.success());
    assert!(
        stderr(&stopped).contains("already bound to a different OCM_HOME"),
        "{}",
        stderr(&stopped)
    );
    assert!(managed_service_definition_path(&first_env, &first_cwd, "ocm").exists());
}

#[test]
fn service_stop_preserves_the_daemon_when_launchctl_cannot_unload_it() {
    let root = TestDir::new("service-stop-launchctl-unload-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let log_path = root.child("launchctl.log");
    let print_path = root.child("launchctl-print.txt");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\ncase \"$1\" in\n  print)\n    /bin/cat \"{}\"\n    ;;\n  bootout|unload)\n    printf 'forced unload failure\\n' >&2\n    exit 1\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
        path_string(&log_path),
        path_string(&print_path),
    );
    write_executable_script(&root.child("fake-bin/launchctl"), &script);

    let stopped = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(!stopped.status.success());
    assert!(
        stderr(&stopped).contains("launchctl failed to unload"),
        "{}",
        stderr(&stopped)
    );
    assert!(managed_service_definition_path(&env, &cwd, "ocm").exists());
    let shown = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert_eq!(json_output(&shown)["serviceRunning"], true);
}

#[test]
fn service_stop_preserves_the_daemon_when_launchctl_cannot_confirm_state() {
    let root = TestDir::new("service-stop-launchctl-print-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let log_path = root.child("launchctl.log");
    fs::write(&log_path, "").unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"print\" ]; then\n  printf 'Operation not permitted\\n' >&2\n  exit 1\nfi\nprintf 'unexpected mutation\\n' >&2\nexit 1\n",
        path_string(&log_path),
    );
    write_executable_script(&root.child("fake-bin/launchctl"), &script);

    let stopped = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(!stopped.status.success());
    assert!(
        stderr(&stopped).contains("launchctl print failed"),
        "{}",
        stderr(&stopped)
    );
    assert!(managed_service_definition_path(&env, &cwd, "ocm").exists());
    let calls = fs::read_to_string(log_path).unwrap();
    assert!(
        calls.lines().next().is_some_and(
            |line| line.starts_with("print gui/") && line.ends_with("/ai.openclaw.ocm")
        )
    );
}

#[test]
fn service_status_all_reports_env_and_ocm_service_state_in_json() {
    let root = TestDir::new("service-status-all");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let output = run_ocm(&cwd, &env, &["service", "status", "--all", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["ocmServiceInstalled"], true);
    assert_eq!(body["services"][0]["envName"], "demo");
    assert_eq!(body["services"][0]["installed"], true);
    assert_eq!(body["services"][0]["desiredRunning"], true);
}

#[test]
fn service_status_version_contract_is_read_only_for_mismatch_unknown_and_current() {
    let root = TestDir::new("service-status-version-mismatch");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = json!({
        "kind": "ocm-supervisor-runtime",
        "ocmHome": path_string(&root.child("ocm-home")),
        "daemonVersion": "0.1.0",
        "updatedAt": "2026-08-17T00:00:00Z",
        "services": [{
            "envName": "demo",
            "bindingKind": "launcher",
            "bindingName": "stable",
            "gatewayState": "running",
            "restartHandoff": "protocol-v1",
            "restartCount": 0,
            "childPort": 18789,
            "pid": 4242,
            "stdoutPath": path_string(&root.child("demo.stdout.log")),
            "stderrPath": path_string(&root.child("demo.stderr.log")),
            "lastExitCode": null,
            "lastError": null,
            "lastEventAt": null,
            "nextRetryAt": null
        }],
        "children": [{
            "envName": "demo",
            "bindingKind": "launcher",
            "bindingName": "stable",
            "pid": 4242,
            "restartCount": 0,
            "childPort": 18789,
            "stdoutPath": path_string(&root.child("demo.stdout.log")),
            "stderrPath": path_string(&root.child("demo.stderr.log"))
        }]
    });
    let runtime_before = serde_json::to_vec(&runtime).unwrap();
    fs::write(&runtime_path, &runtime_before).unwrap();

    let output = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);

    assert_eq!(body["ocmCliVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["ocmServiceVersion"], "0.1.0");
    assert_eq!(body["ocmServiceVersionStatus"], "mismatch");
    assert!(
        body["ocmServiceVersionNote"]
            .as_str()
            .unwrap()
            .contains("service refresh-daemon --acknowledge-gateway-restarts")
    );
    assert_eq!(body["childPid"], 4242);
    assert_eq!(fs::read(&runtime_path).unwrap(), runtime_before);

    let mut unknown_runtime = runtime.clone();
    unknown_runtime
        .as_object_mut()
        .unwrap()
        .remove("daemonVersion");
    let unknown_before = serde_json::to_vec(&unknown_runtime).unwrap();
    fs::write(&runtime_path, &unknown_before).unwrap();
    let unknown = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(unknown.status.success(), "{}", stderr(&unknown));
    let unknown_body = json_output(&unknown);
    assert_eq!(unknown_body["ocmServiceVersion"], Value::Null);
    assert_eq!(unknown_body["ocmServiceVersionStatus"], "unknown");
    assert!(
        unknown_body["ocmServiceVersionNote"]
            .as_str()
            .unwrap()
            .contains("does not report its version")
    );
    assert_eq!(unknown_body["childPid"], 4242);
    assert_eq!(fs::read(&runtime_path).unwrap(), unknown_before);

    let mut current_runtime = runtime;
    current_runtime["daemonVersion"] = Value::String(env!("CARGO_PKG_VERSION").to_string());
    let current_before = serde_json::to_vec(&current_runtime).unwrap();
    fs::write(&runtime_path, &current_before).unwrap();
    let current = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(current.status.success(), "{}", stderr(&current));
    let current_body = json_output(&current);
    assert_eq!(current_body["ocmServiceVersionStatus"], "current");
    assert_eq!(current_body["ocmServiceVersionNote"], Value::Null);
    assert_eq!(current_body["childPid"], 4242);
    assert_eq!(fs::read(&runtime_path).unwrap(), current_before);
}

#[test]
fn service_refresh_daemon_requires_interruption_ack_and_preserves_desired_state() {
    let root = TestDir::new("service-refresh-daemon-ack");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let state_path = supervisor_state_path(&env, &cwd).unwrap();
    let state_before = fs::read(&state_path).unwrap();
    let launchctl_log = root.child("launchctl.log");
    fs::write(&launchctl_log, "").unwrap();

    let refused = run_ocm(&cwd, &env, &["service", "refresh-daemon", "--json"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("--acknowledge-gateway-restarts"),
        "{}",
        stderr(&refused)
    );
    let refused_calls = fs::read_to_string(&launchctl_log).unwrap();
    assert!(!refused_calls.contains("bootout"));
    assert!(!refused_calls.contains("bootstrap"));
    assert_eq!(fs::read(&state_path).unwrap(), state_before);

    fs::write(&launchctl_log, "").unwrap();
    let refreshed = run_ocm(
        &cwd,
        &env,
        &[
            "service",
            "refresh-daemon",
            "--acknowledge-gateway-restarts",
            "--json",
        ],
    );
    assert!(refreshed.status.success(), "{}", stderr(&refreshed));
    let body = json_output(&refreshed);
    assert_eq!(body["action"], "refresh");
    assert_eq!(body["gatewayInterruptionExpected"], true);
    assert_eq!(body["gatewayInterruptionAcknowledged"], true);
    assert!(body["note"].as_str().unwrap().contains("interrupted"));
    let refreshed_calls = fs::read_to_string(&launchctl_log).unwrap();
    assert!(refreshed_calls.contains("bootout"));
    assert!(refreshed_calls.contains("bootstrap"));
    assert_eq!(fs::read(&state_path).unwrap(), state_before);
}

#[test]
fn systemd_service_install_writes_the_ocm_unit() {
    let root = TestDir::new("service-systemd-install");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = systemd_env(&root);
    setup_launcher_env(&cwd, &env);

    let output = run_ocm(&cwd, &env, &["service", "install", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["installed"], true);

    let service_path = managed_service_definition_path(&env, &cwd, "ocm");
    assert!(
        service_path.exists(),
        "missing {}",
        path_string(&service_path)
    );
    let unit = fs::read_to_string(service_path).unwrap();
    assert!(unit.contains("__daemon run"));
}

#[test]
fn service_start_requires_a_valid_binding() {
    let root = TestDir::new("service-start-binding");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let output = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("has no default runtime, launcher, or dev binding"));
}

#[test]
fn service_status_reports_missing_binding_issue() {
    let root = TestDir::new("service-status-missing-binding");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(!started.status.success());

    let install = run_ocm(&cwd, &env, &["service", "install", "demo"]);
    assert!(!install.status.success());

    let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let body = json_output(&status);
    assert!(
        body["issue"]
            .as_str()
            .unwrap()
            .contains("has no default runtime, launcher, or dev binding")
    );
}

#[test]
fn service_status_uses_runtime_bindings_too() {
    let root = TestDir::new("service-runtime-binding");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    let runtime_path = root.child("bin/openclaw");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let runtime = run_ocm(
        &cwd,
        &env,
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
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    let output = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["bindingKind"], "runtime");
    assert_eq!(body["bindingName"], "stable");
    assert_eq!(body["binaryPath"], path_string(&runtime_path));
}

#[test]
fn service_status_keeps_simple_package_manager_launchers_as_direct_exec() {
    let root = TestDir::new("service-status-package-manager-launcher");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", "pnpm openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let body = json_output(&status);
    assert_eq!(body["bindingKind"], "launcher");
    assert_eq!(body["binaryPath"], "pnpm");
    assert_eq!(
        body["args"],
        Value::Array(vec![
            Value::String("openclaw".to_string()),
            Value::String("gateway".to_string()),
            Value::String("run".to_string()),
            Value::String("--port".to_string()),
            body["args"][4].clone(),
        ])
    );
    assert!(body["args"][4].as_str().unwrap().parse::<u16>().is_ok());
}

#[test]
fn service_status_ignores_stale_runtime_children_when_the_daemon_is_down() {
    let root = TestDir::new("service-status-stale-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    EnvironmentService::new(&env, &cwd)
        .set_service_policy("demo", Some(true), Some(true))
        .unwrap();

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let stale_runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: path_string(&root.child("ocm-home")),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        updated_at: now_utc(),
        services: Vec::new(),
        children: vec![SupervisorRuntimeChild {
            env_name: "demo".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            pid: 4242,
            restart_count: 3,
            child_port: 18789,
            stdout_path: path_string(&root.child("stale.stdout.log")),
            stderr_path: path_string(&root.child("stale.stderr.log")),
        }],
    };
    fs::write(&runtime_path, serde_json::to_vec(&stale_runtime).unwrap()).unwrap();

    let output = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["running"], false);
    assert_eq!(body["childPid"], Value::Null);
    assert_eq!(body["childRestartCount"], Value::Null);
    assert!(
        body["issue"]
            .as_str()
            .unwrap()
            .contains("OCM background service is not installed")
    );
}

#[test]
fn service_status_reports_clean_backoff_as_restarting_without_issue() {
    let root = TestDir::new("service-status-clean-backoff");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let retry_at = now_utc();
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: path_string(&root.child("ocm-home")),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            gateway_state: "backoff".to_string(),
            restart_handoff: Some("protocol-v1".to_string()),
            restart_count: 1,
            child_port: 18789,
            pid: None,
            stdout_path: path_string(&root.child("demo.stdout.log")),
            stderr_path: path_string(&root.child("demo.stderr.log")),
            last_exit_code: Some(0),
            last_error: Some("process exited with 0; retrying after backoff".to_string()),
            last_event_at: Some(now_utc()),
            next_retry_at: Some(retry_at),
        }],
        children: Vec::new(),
    };
    fs::write(&runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();

    let output = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["running"], false);
    assert_eq!(body["gatewayState"], "restarting");
    assert_eq!(body["restartHandoff"], "protocol-v1");
    assert_eq!(body["lastExitCode"], 0);
    assert_eq!(body["issue"], Value::Null);
}

#[test]
fn service_status_keeps_failed_backoff_as_issue() {
    let root = TestDir::new("service-status-failed-backoff");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = launchd_env(&root);
    setup_launcher_env(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: path_string(&root.child("ocm-home")),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            gateway_state: "backoff".to_string(),
            restart_handoff: Some("legacy".to_string()),
            restart_count: 1,
            child_port: 18789,
            pid: None,
            stdout_path: path_string(&root.child("demo.stdout.log")),
            stderr_path: path_string(&root.child("demo.stderr.log")),
            last_exit_code: Some(1),
            last_error: Some("process exited with 1; retrying after backoff".to_string()),
            last_event_at: Some(now_utc()),
            next_retry_at: Some(now_utc()),
        }],
        children: Vec::new(),
    };
    fs::write(&runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();

    let output = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = json_output(&output);
    assert_eq!(body["running"], false);
    assert_eq!(body["gatewayState"], "backoff");
    assert_eq!(body["restartHandoff"], "legacy");
    assert_eq!(
        body["issue"],
        "process exited with 1; retrying after backoff"
    );
}
