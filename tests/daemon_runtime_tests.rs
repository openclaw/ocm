mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

use ocm::env::EnvironmentService;
use ocm::supervisor::{SupervisorService, sync_supervisor_binding_if_present};
use serde_json::{Value, to_value};

use crate::support::{
    TestDir, install_fake_launchctl, install_fake_systemd_tools, ocm_env, path_string, run_ocm,
    stderr, stdout, write_executable_script,
};

static DAEMON_RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());

fn daemon_runtime_test_lock() -> MutexGuard<'static, ()> {
    DAEMON_RUNTIME_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn_daemon_process(cwd: &Path, env: &BTreeMap<String, String>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    command.current_dir(cwd);
    command.args(["__daemon", "run"]);
    command.env_clear();
    command.envs(env);
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.spawn().unwrap()
}

fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(50));
    }
    false
}

fn wait_for_runtime_children(
    path: &Path,
    expected_children: usize,
    env_name: Option<&str>,
    timeout: Duration,
) -> Option<Value> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(raw) = fs::read(path)
            && let Ok(body) = serde_json::from_slice::<Value>(&raw)
            && body["children"].as_array().map(|children| children.len()) == Some(expected_children)
        {
            let matches_env = env_name.is_none_or(|name| {
                body["children"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .any(|child| child["envName"] == name)
            });
            if matches_env {
                return Some(body);
            }
        }
        sleep(Duration::from_millis(50));
    }
    None
}

fn wait_for_file_value(path: &Path, expected: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(raw) = fs::read_to_string(path)
            && raw.trim() == expected
        {
            return true;
        }
        sleep(Duration::from_millis(50));
    }
    false
}

fn wait_for_runtime_service_state(
    path: &Path,
    env_name: &str,
    gateway_state: &str,
    timeout: Duration,
) -> Option<Value> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(raw) = fs::read(path)
            && let Ok(body) = serde_json::from_slice::<Value>(&raw)
            && let Some(service) = body["services"].as_array().and_then(|services| {
                services
                    .iter()
                    .find(|service| service["envName"] == env_name)
            })
            && service["gatewayState"] == gateway_state
        {
            return Some(service.clone());
        }
        sleep(Duration::from_millis(50));
    }
    None
}

fn runtime_child_pid(body: &Value, env_name: &str) -> Option<u64> {
    body["children"].as_array().and_then(|children| {
        children
            .iter()
            .find(|child| child["envName"] == env_name)
            .and_then(|child| child["pid"].as_u64())
    })
}

fn wait_for_runtime_child_pid_change(
    path: &Path,
    target_env: &str,
    previous_target_pid: u64,
    sibling_env: &str,
    previous_sibling_pid: u64,
    timeout: Duration,
) -> Option<Value> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(raw) = fs::read(path)
            && let Ok(body) = serde_json::from_slice::<Value>(&raw)
            && body["children"].as_array().map(|children| children.len()) == Some(2)
            && let Some(target_pid) = runtime_child_pid(&body, target_env)
            && let Some(sibling_pid) = runtime_child_pid(&body, sibling_env)
            && target_pid != previous_target_pid
            && sibling_pid == previous_sibling_pid
        {
            return Some(body);
        }
        sleep(Duration::from_millis(50));
    }
    None
}

fn restart_handoff_protocol_shell_snippet(intent_path: &Path) -> String {
    format!(
        r#"if [ "${{1:-}}" = "gateway" ] && [ "${{2:-}}" = "restart-handoff" ]; then
  case "${{3:-}}" in
    capabilities)
      printf '%s\n' '{{"ok":true,"protocol":"openclaw.gateway.restart-handoff","protocolVersion":1,"operations":["consume"]}}'
      exit 0
      ;;
    consume)
      if [ ! -f '{intent_path}' ]; then
        printf '%s\n' '{{"ok":true,"protocol":"openclaw.gateway.restart-handoff","protocolVersion":1,"status":"none","reason":"missing"}}'
        exit 0
      fi
      intent_pid=$(cat '{intent_path}')
      if [ "$intent_pid" != "${{5:-}}" ]; then
        printf '{{"ok":true,"protocol":"openclaw.gateway.restart-handoff","protocolVersion":1,"status":"rejected","reason":"pid-mismatch","handoffPid":%s}}\n' "$intent_pid"
        exit 0
      fi
      rm -f '{intent_path}'
      printf '{{"ok":true,"protocol":"openclaw.gateway.restart-handoff","protocolVersion":1,"status":"accepted","handoff":{{"pid":%s,"supervisorMode":"external"}}}}\n' "$intent_pid"
      exit 0
      ;;
  esac
fi
"#,
        intent_path = path_string(intent_path),
    )
}

fn write_legacy_openclaw_script(path: &Path, contents: &str) {
    let body = contents
        .strip_prefix("#!/bin/sh\n")
        .expect("fake OpenClaw script must use the expected shell shebang");
    write_executable_script(
        path,
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = 'gateway' ] && [ \"${{2:-}}\" = 'restart-handoff' ]; then exit 64; fi\n{body}"
        ),
    );
}

fn write_health_gateway_script(path: &Path, mode: &str, delay_ms: u64) {
    let behavior = match mode {
        "healthy" => format!("setTimeout(() => server.listen(port, '127.0.0.1'), {delay_ms});"),
        "timeout" => "setInterval(() => {}, 1000);".to_string(),
        "exit-23" => format!("setTimeout(() => process.exit(23), {delay_ms});"),
        _ => panic!("unknown gateway test mode: {mode}"),
    };
    write_executable_script(
        path,
        &format!(
            r#"#!/usr/bin/env node
import http from 'node:http';

const args = process.argv.slice(2);
if (args[0] === 'gateway' && args[1] === 'restart-handoff') {{
  process.exit(64);
}}
const portIndex = args.indexOf('--port');
const port = Number(args[portIndex + 1]);
const server = http.createServer((request, response) => {{
  response.writeHead(request.url === '/health' ? 200 : 404);
  response.end(request.url === '/health' ? 'ok' : 'not found');
}});
{behavior}
"#
        ),
    );
}

fn setup_gateway_readiness_fixture(
    root: &TestDir,
    mode: &str,
    delay_ms: u64,
    timeout_ms: u64,
) -> (std::path::PathBuf, BTreeMap<String, String>) {
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(root);
    env.remove("OCM_INTERNAL_SKIP_SERVICE_READINESS");
    env.insert(
        "OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS".to_string(),
        timeout_ms.to_string(),
    );
    install_fake_systemd_tools(root, &mut env);

    let script = root.child("bin/readiness-openclaw.mjs");
    write_health_gateway_script(&script, mode, delay_ms);
    let launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "readiness",
            "--command",
            &path_string(&script),
        ],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "readiness"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let installed = run_ocm(&cwd, &env, &["service", "install", "demo"]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    (cwd, env)
}

fn read_persisted_service_state(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_persisted_service_state(path: &Path, value: &Value) {
    let raw = serde_json::to_string_pretty(value).unwrap();
    fs::write(path, format!("{raw}\n")).unwrap();
}

fn stop_process(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn set_service_enabled(cwd: &Path, env: &BTreeMap<String, String>, name: &str, enabled: bool) {
    EnvironmentService::new(env, cwd)
        .set_service_policy(name, Some(enabled), Some(enabled))
        .unwrap();
}

fn setup_service_fixture(
    root: &TestDir,
) -> (
    std::path::PathBuf,
    std::collections::BTreeMap<String, String>,
) {
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(root);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let runtime_path = root.child("bin/openclaw");
    write_legacy_openclaw_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let runtime = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "managed",
            "--path",
            &path_string(&runtime_path),
        ],
    );
    assert!(runtime.status.success(), "{}", stderr(&runtime));

    let demo = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(demo.status.success(), "{}", stderr(&demo));
    set_service_enabled(&cwd, &env, "demo", true);

    let prod = run_ocm(
        &cwd,
        &env,
        &["env", "create", "prod", "--runtime", "managed"],
    );
    assert!(prod.status.success(), "{}", stderr(&prod));
    set_service_enabled(&cwd, &env, "prod", true);

    let bare = run_ocm(&cwd, &env, &["env", "create", "bare"]);
    assert!(bare.status.success(), "{}", stderr(&bare));

    (cwd, env)
}

fn setup_daemon_run_fixture(
    root: &TestDir,
) -> (
    std::path::PathBuf,
    std::collections::BTreeMap<String, String>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    setup_daemon_run_fixture_with_child_sleep(root, 1)
}

fn setup_daemon_run_fixture_with_child_sleep(
    root: &TestDir,
    child_sleep_seconds: u64,
) -> (
    std::path::PathBuf,
    std::collections::BTreeMap<String, String>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(root);

    let launcher_marker = root.child("launcher-ran.txt");
    let runtime_marker = root.child("runtime-ran.txt");

    let launcher_script = root.child("bin/launcher-openclaw");
    write_legacy_openclaw_script(
        &launcher_script,
        &format!(
            "#!/bin/sh\nprintf 'launcher\\n' > '{}'\nprintf 'launcher stdout\\n'\nprintf 'launcher stderr\\n' >&2\nsleep {}\n",
            path_string(&launcher_marker),
            child_sleep_seconds
        ),
    );
    let runtime_script = root.child("bin/runtime-openclaw");
    write_legacy_openclaw_script(
        &runtime_script,
        &format!(
            "#!/bin/sh\nprintf 'runtime\\n' > '{}'\nprintf 'runtime stdout\\n'\nprintf 'runtime stderr\\n' >&2\nsleep {}\n",
            path_string(&runtime_marker),
            child_sleep_seconds
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "dev",
            "--command",
            &path_string(&launcher_script),
        ],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let runtime = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "managed",
            "--path",
            &path_string(&runtime_script),
        ],
    );
    assert!(runtime.status.success(), "{}", stderr(&runtime));

    let demo = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(demo.status.success(), "{}", stderr(&demo));
    set_service_enabled(&cwd, &env, "demo", true);

    let prod = run_ocm(
        &cwd,
        &env,
        &["env", "create", "prod", "--runtime", "managed"],
    );
    assert!(prod.status.success(), "{}", stderr(&prod));
    set_service_enabled(&cwd, &env, "prod", true);

    (cwd, env, launcher_marker, runtime_marker)
}

#[test]
fn service_state_plans_runnable_children_and_skips_disabled_envs() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("service-state-plan");
    let (cwd, env) = setup_service_fixture(&root);
    let service = SupervisorService::new(&env, &cwd);

    let body = to_value(service.plan().unwrap()).unwrap();
    assert_eq!(body["persisted"], false);
    assert!(
        body["statePath"]
            .as_str()
            .unwrap()
            .ends_with("/supervisor/state.json")
    );

    let children = body["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);

    let demo = children
        .iter()
        .find(|child| child["envName"] == "demo")
        .unwrap();
    assert_eq!(demo["bindingKind"], "launcher");
    let demo_port = demo["childPort"].as_u64().unwrap();
    assert!(demo_port >= 18_789);
    assert!(
        demo["processEnv"]["OPENCLAW_HOME"]
            .as_str()
            .unwrap()
            .contains("/envs/demo")
    );
    assert_eq!(demo["processEnv"]["OPENCLAW_SERVICE_MARKER"], "openclaw");
    assert!(demo["processEnv"].get("OPENCLAW_SERVICE_KIND").is_none());
    assert_eq!(demo["processEnv"]["OPENCLAW_SUPERVISOR_MODE"], "external");
    assert_eq!(demo["processEnv"]["OPENCLAW_NO_RESPAWN"], "1");
    assert!(demo["processEnv"].get("OPENCLAW_LAUNCHD_LABEL").is_none());
    assert!(demo["processEnv"].get("OPENCLAW_SYSTEMD_UNIT").is_none());

    let prod = children
        .iter()
        .find(|child| child["envName"] == "prod")
        .unwrap();
    assert_eq!(prod["bindingKind"], "runtime");
    assert_eq!(prod["bindingName"], "managed");
    let prod_port = prod["childPort"].as_u64().unwrap();
    assert!(prod_port.abs_diff(demo_port) > 110);
    assert!(
        prod["binaryPath"]
            .as_str()
            .unwrap()
            .ends_with("/bin/openclaw")
    );

    let skipped = body["skippedEnvs"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["envName"], "bare");
    assert_eq!(skipped[0]["reason"], "service is disabled");

    EnvironmentService::new(&env, &cwd)
        .set_service_running("demo", false)
        .unwrap();
    let body = to_value(service.plan().unwrap()).unwrap();
    assert!(
        !body["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["envName"] == "demo")
    );
    assert!(
        body["skippedEnvs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["envName"] == "demo" && entry["reason"] == "service is stopped")
    );
}

#[test]
fn daemon_run_persists_live_runtime_children() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-runtime-state");
    let (cwd, env, _, _) = setup_daemon_run_fixture_with_child_sleep(&root, 10);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let service = SupervisorService::new(&env, &cwd);

    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let runtime = wait_for_runtime_children(&runtime_path, 2, Some("demo"), Duration::from_secs(5))
        .expect("daemon runtime state did not report running children");
    assert_eq!(runtime["kind"], "ocm-supervisor-runtime");
    assert_eq!(runtime["daemonVersion"], env!("CARGO_PKG_VERSION"));

    let runtime_body = to_value(service.runtime().unwrap()).unwrap();
    assert_eq!(runtime_body["present"], true);
    assert_eq!(runtime_body["daemonVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(runtime_body["runtimePath"], path_string(&runtime_path));
    assert_eq!(runtime_body["children"].as_array().unwrap().len(), 2);

    stop_process(&mut daemon);
    let cleared = wait_for_runtime_children(&runtime_path, 0, None, Duration::from_secs(5))
        .expect("daemon runtime state did not clear after shutdown");
    assert!(cleared["updatedAt"].as_str().is_some());
}

#[test]
fn daemon_run_once_executes_planned_children() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-once");
    let (cwd, env, launcher_marker, runtime_marker) = setup_daemon_run_fixture(&root);
    let service = SupervisorService::new(&env, &cwd);

    service.sync().unwrap();

    let run = run_ocm(&cwd, &env, &["__daemon", "run", "--once", "--json"]);
    assert!(run.status.success(), "{}", stderr(&run));
    let body: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(body["once"], true);
    assert_eq!(body["childCount"], 2);
    assert_eq!(body["childResults"].as_array().unwrap().len(), 2);
    assert!(
        body["childResults"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["success"] == true)
    );

    assert_eq!(fs::read_to_string(launcher_marker).unwrap(), "launcher\n");
    assert_eq!(fs::read_to_string(runtime_marker).unwrap(), "runtime\n");
}

#[test]
fn env_changes_refresh_persisted_service_state_without_extra_commands() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("service-state-refresh");
    let (cwd, env) = setup_service_fixture(&root);
    let service = SupervisorService::new(&env, &cwd);
    let state_path = root.child("ocm-home/supervisor/state.json");

    service.sync().unwrap();

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev-b", "--command", "openclaw --beta"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));
    let rebind = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "dev-b"]);
    assert!(rebind.status.success(), "{}", stderr(&rebind));

    let show_body = read_persisted_service_state(&state_path);
    let demo = show_body["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "demo")
        .unwrap();
    assert_eq!(demo["bindingName"], "dev-b");

    let created = run_ocm(&cwd, &env, &["env", "create", "extra", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "extra", true);

    let show_body = read_persisted_service_state(&state_path);
    assert!(
        show_body["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["envName"] == "extra")
    );

    let removed = run_ocm(&cwd, &env, &["env", "remove", "extra", "--force"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    let show_body = read_persisted_service_state(&state_path);
    assert!(
        !show_body["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["envName"] == "extra")
    );
}

#[test]
fn env_clone_preserves_unrelated_supervisor_child_specs() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("env-clone-preserves-supervisor-siblings");
    let (cwd, mut env) = setup_service_fixture(&root);
    let service = SupervisorService::new(&env, &cwd);
    let state_path = root.child("ocm-home/supervisor/state.json");

    service.sync().unwrap();
    env.insert(
        "NODE_OPTIONS".to_string(),
        "--max-old-space-size=2048".to_string(),
    );

    let cloned = run_ocm(&cwd, &env, &["env", "clone", "demo", "demo-clone"]);
    assert!(cloned.status.success(), "{}", stderr(&cloned));

    let state = read_persisted_service_state(&state_path);
    let demo = state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "demo")
        .expect("demo child spec should remain present");
    assert!(
        demo["processEnv"]["NODE_OPTIONS"].is_null(),
        "clone must not rebuild unrelated child specs from caller environment"
    );
    assert!(
        state["children"]
            .as_array()
            .unwrap()
            .iter()
            .all(|child| child["envName"] != "demo-clone")
    );
    assert!(
        state["skippedEnvs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["envName"] == "demo-clone")
    );
}

#[test]
fn child_restart_request_rebuilds_missing_or_stale_state() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("restart-request-rebuilds-state");
    let (cwd, mut env) = setup_service_fixture(&root);
    let service = SupervisorService::new(&env, &cwd);
    let state_path = root.child("ocm-home/supervisor/state.json");

    service.sync().unwrap();
    fs::remove_file(&state_path).unwrap();

    let missing_state_request = service.request_child_restart("demo").unwrap();
    let state = read_persisted_service_state(&state_path);
    assert!(
        state["restartRequests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["envName"] == "demo"
                && request["requestId"] == missing_state_request)
    );

    let mut stale_state = state.clone();
    stale_state["children"] = Value::Array(Vec::new());
    write_persisted_service_state(&state_path, &stale_state);

    let stale_state_request = service.request_child_restart("demo").unwrap();
    let state = read_persisted_service_state(&state_path);
    assert!(
        state["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["envName"] == "demo")
    );
    assert!(
        state["restartRequests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["envName"] == "demo"
                && request["requestId"] == stale_state_request)
    );

    env.insert(
        "NODE_OPTIONS".to_string(),
        "--max-old-space-size=2048".to_string(),
    );
    let service = SupervisorService::new(&env, &cwd);
    let refreshed_env_request = service.request_child_restart("demo").unwrap();
    let state = read_persisted_service_state(&state_path);
    let demo = state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "demo")
        .expect("demo child spec should be rebuilt");
    assert_eq!(
        demo["processEnv"]["NODE_OPTIONS"],
        "--max-old-space-size=2048"
    );
    assert!(
        state["restartRequests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["envName"] == "demo"
                && request["requestId"] == refreshed_env_request)
    );
}

#[test]
fn child_restart_recovery_preserves_unrelated_child_specs() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("restart-recovery-preserves-siblings");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_systemd_tools(&root, &mut env);
    let state_path = root.child("ocm-home/supervisor/state.json");

    for name in ["rescue", "main"] {
        let launcher = run_ocm(
            &cwd,
            &env,
            &["launcher", "add", name, "--command", "openclaw"],
        );
        assert!(launcher.status.success(), "{}", stderr(&launcher));
        let created = run_ocm(&cwd, &env, &["env", "create", name, "--launcher", name]);
        assert!(created.status.success(), "{}", stderr(&created));
        set_service_enabled(&cwd, &env, name, true);
    }

    SupervisorService::new(&env, &cwd).sync().unwrap();

    let mut restart_env = env.clone();
    restart_env.insert(
        "NODE_OPTIONS".to_string(),
        "--max-old-space-size=2048".to_string(),
    );
    SupervisorService::new(&restart_env, &cwd)
        .recover_child_restart("rescue")
        .unwrap();

    let state = read_persisted_service_state(&state_path);
    let children = state["children"].as_array().unwrap();
    let rescue = children
        .iter()
        .find(|child| child["envName"] == "rescue")
        .unwrap();
    let main = children
        .iter()
        .find(|child| child["envName"] == "main")
        .unwrap();

    assert_eq!(
        rescue["processEnv"]["NODE_OPTIONS"],
        "--max-old-space-size=2048"
    );
    assert!(main["processEnv"]["NODE_OPTIONS"].is_null());
    assert!(
        state["restartRequests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["envName"] == "rescue")
    );
}

#[test]
fn daemon_run_reloads_children_after_binding_changes() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-reconcile");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);

    let old_started = root.child("old-started.txt");
    let old_stopped = root.child("old-stopped.txt");
    let new_started = root.child("new-started.txt");

    let old_script = root.child("bin/launcher-old");
    write_legacy_openclaw_script(
        &old_script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'printf \"stopped\\n\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&old_started),
            path_string(&old_stopped),
        ),
    );
    let new_script = root.child("bin/launcher-new");
    write_legacy_openclaw_script(
        &new_script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&new_started),
        ),
    );

    let add_old = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "old",
            "--command",
            &path_string(&old_script),
        ],
    );
    assert!(add_old.status.success(), "{}", stderr(&add_old));
    let add_new = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "new",
            "--command",
            &path_string(&new_script),
        ],
    );
    assert!(add_new.status.success(), "{}", stderr(&add_new));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "old"]);
    assert!(create.status.success(), "{}", stderr(&create));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&old_started, Duration::from_secs(5)));

    let switch = run_ocm(&cwd, &env, &["env", "set-launcher", "demo", "new"]);
    assert!(switch.status.success(), "{}", stderr(&switch));

    assert!(wait_for_file(&old_stopped, Duration::from_secs(5)));
    assert!(wait_for_file(&new_started, Duration::from_secs(5)));

    stop_process(&mut daemon);
}

#[test]
fn publishing_an_unbound_runtime_preserves_unrelated_active_child_spec_and_pid() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-unbound-runtime-publish");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let state_path = root.child("ocm-home/supervisor/state.json");
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let runtime_meta_path = root.child("ocm-home/runtimes/runtime-a.json");
    let started = root.child("runtime-a-started.txt");
    let stopped = root.child("runtime-a-stopped.txt");

    let runtime_a = root.child("bin/runtime-a");
    write_legacy_openclaw_script(
        &runtime_a,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 3600; done\n",
            path_string(&started),
            path_string(&stopped),
        ),
    );
    let add_a = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "runtime-a",
            "--path",
            &path_string(&runtime_a),
        ],
    );
    assert!(add_a.status.success(), "{}", stderr(&add_a));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "env-a", "--runtime", "runtime-a"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    set_service_enabled(&cwd, &env, "env-a", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let initial_runtime =
        wait_for_runtime_children(&runtime_path, 1, Some("env-a"), Duration::from_secs(5))
            .expect("daemon runtime state did not report env-a");
    let initial_pid = runtime_child_pid(&initial_runtime, "env-a").unwrap();

    let mut latent_runtime_meta = read_persisted_service_state(&runtime_meta_path);
    latent_runtime_meta["releaseVersion"] = Value::String("latent-v2".to_string());
    write_persisted_service_state(&runtime_meta_path, &latent_runtime_meta);

    let runtime_b = root.child("bin/runtime-b");
    write_legacy_openclaw_script(&runtime_b, "#!/bin/sh\nexit 0\n");
    let add_b = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "runtime-b",
            "--path",
            &path_string(&runtime_b),
        ],
    );
    let add_b_error = stderr(&add_b);

    sleep(Duration::from_millis(800));
    let final_runtime =
        wait_for_runtime_children(&runtime_path, 1, Some("env-a"), Duration::from_secs(2))
            .expect("daemon runtime state stopped reporting env-a");
    let final_pid = runtime_child_pid(&final_runtime, "env-a").unwrap();
    let state = read_persisted_service_state(&state_path);
    let persisted_env_a = state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "env-a")
        .unwrap()
        .clone();
    let started_count = fs::read_to_string(&started).unwrap().lines().count();
    let stopped_exists = stopped.exists();

    stop_process(&mut daemon);

    assert!(add_b.status.success(), "{add_b_error}");
    assert_eq!(final_pid, initial_pid, "unrelated child PID changed");
    assert_eq!(started_count, 1, "unrelated child started more than once");
    assert!(!stopped_exists, "unrelated child received a stop signal");
    assert!(
        persisted_env_a["runtimeReleaseVersion"].is_null(),
        "unrelated latent runtime metadata was applied"
    );
}

#[test]
fn targeted_runtime_refresh_ignores_unrelated_drift_and_restarts_effective_change_once() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-targeted-runtime-refresh");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let state_path = root.child("ocm-home/supervisor/state.json");
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let runtime_a_meta_path = root.child("ocm-home/runtimes/runtime-a.json");
    let runtime_b_meta_path = root.child("ocm-home/runtimes/runtime-b.json");
    let runtime_a_started = root.child("runtime-a-started.txt");
    let runtime_a_stopped = root.child("runtime-a-stopped.txt");
    let runtime_b_started = root.child("runtime-b-started.txt");
    let runtime_b_stopped = root.child("runtime-b-stopped.txt");

    let runtime_a = root.child("bin/runtime-a");
    write_legacy_openclaw_script(
        &runtime_a,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 3600; done\n",
            path_string(&runtime_a_started),
            path_string(&runtime_a_stopped),
        ),
    );
    let runtime_b = root.child("bin/runtime-b");
    write_legacy_openclaw_script(
        &runtime_b,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&runtime_b_started),
            path_string(&runtime_b_stopped),
        ),
    );
    let runtime_b_next = root.child("bin/runtime-b-next");
    write_legacy_openclaw_script(
        &runtime_b_next,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&runtime_b_started),
            path_string(&runtime_b_stopped),
        ),
    );

    for (name, path) in [("runtime-a", &runtime_a), ("runtime-b", &runtime_b)] {
        let add = run_ocm(
            &cwd,
            &env,
            &["runtime", "add", name, "--path", &path_string(path)],
        );
        assert!(add.status.success(), "{}", stderr(&add));
        let env_name = name.replace("runtime", "env");
        let create = run_ocm(&cwd, &env, &["env", "create", &env_name, "--runtime", name]);
        assert!(create.status.success(), "{}", stderr(&create));
        set_service_enabled(&cwd, &env, &env_name, true);
    }
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let initial_runtime = wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(5))
        .expect("daemon runtime state did not report both children");
    let env_a_pid = runtime_child_pid(&initial_runtime, "env-a").unwrap();
    let env_b_pid = runtime_child_pid(&initial_runtime, "env-b").unwrap();

    let mut runtime_a_meta = read_persisted_service_state(&runtime_a_meta_path);
    runtime_a_meta["releaseVersion"] = Value::String("latent-a-v2".to_string());
    write_persisted_service_state(&runtime_a_meta_path, &runtime_a_meta);

    let mut runtime_b_meta = read_persisted_service_state(&runtime_b_meta_path);
    runtime_b_meta["description"] = Value::String("metadata only".to_string());
    write_persisted_service_state(&runtime_b_meta_path, &runtime_b_meta);
    sync_supervisor_binding_if_present(&env, &cwd, "runtime", "runtime-b").unwrap();

    sleep(Duration::from_millis(800));
    let metadata_runtime =
        wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(2))
            .expect("daemon runtime state stopped reporting both children");
    let metadata_a_pid = runtime_child_pid(&metadata_runtime, "env-a").unwrap();
    let metadata_b_pid = runtime_child_pid(&metadata_runtime, "env-b").unwrap();
    let metadata_state = read_persisted_service_state(&state_path);
    let metadata_env_a = metadata_state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "env-a")
        .unwrap();
    let metadata_preserved = metadata_a_pid == env_a_pid
        && metadata_b_pid == env_b_pid
        && metadata_env_a["runtimeReleaseVersion"].is_null()
        && !runtime_a_stopped.exists()
        && !runtime_b_stopped.exists();

    runtime_b_meta["binaryPath"] = Value::String(path_string(&runtime_b_next));
    runtime_b_meta["releaseVersion"] = Value::String("effective-b-v2".to_string());
    write_persisted_service_state(&runtime_b_meta_path, &runtime_b_meta);
    sync_supervisor_binding_if_present(&env, &cwd, "runtime", "runtime-b").unwrap();

    let changed_runtime = wait_for_runtime_child_pid_change(
        &runtime_path,
        "env-b",
        env_b_pid,
        "env-a",
        env_a_pid,
        Duration::from_secs(10),
    )
    .expect("effective runtime change did not replace only env-b");
    sleep(Duration::from_millis(500));
    let final_a_pid = runtime_child_pid(&changed_runtime, "env-a").unwrap();
    let final_b_pid = runtime_child_pid(&changed_runtime, "env-b").unwrap();
    let runtime_a_start_count = fs::read_to_string(&runtime_a_started)
        .unwrap()
        .lines()
        .count();
    let runtime_b_start_count = fs::read_to_string(&runtime_b_started)
        .unwrap()
        .lines()
        .count();
    let runtime_b_stop_count = fs::read_to_string(&runtime_b_stopped)
        .unwrap()
        .lines()
        .count();

    stop_process(&mut daemon);

    assert!(
        metadata_preserved,
        "metadata-only refresh changed active state"
    );
    assert_eq!(final_a_pid, env_a_pid, "unrelated child PID changed");
    assert_ne!(final_b_pid, env_b_pid, "effective child PID did not change");
    assert_eq!(runtime_a_start_count, 1, "unrelated child restarted");
    assert_eq!(
        runtime_b_start_count, 2,
        "effective child did not restart once"
    );
    assert_eq!(
        runtime_b_stop_count, 1,
        "effective child stop count was not one"
    );
}

#[test]
fn service_uninstall_removes_only_target_child_despite_unrelated_drift() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-targeted-service-uninstall");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_systemd_tools(&root, &mut env);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let state_path = root.child("ocm-home/supervisor/state.json");
    let target_started = root.child("target-started.txt");
    let target_stopped = root.child("target-stopped.txt");
    let sibling_started = root.child("sibling-started.txt");
    let sibling_stopped = root.child("sibling-stopped.txt");

    let target_runtime = root.child("bin/target-runtime");
    write_legacy_openclaw_script(
        &target_runtime,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&target_started),
            path_string(&target_stopped),
        ),
    );
    let sibling_runtime = root.child("bin/sibling-runtime");
    write_legacy_openclaw_script(
        &sibling_runtime,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&sibling_started),
            path_string(&sibling_stopped),
        ),
    );

    for (runtime_name, runtime_path, env_name) in [
        ("target-runtime", &target_runtime, "target"),
        ("sibling-runtime", &sibling_runtime, "sibling"),
    ] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                runtime_name,
                "--path",
                &path_string(runtime_path),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
        let create = run_ocm(
            &cwd,
            &env,
            &["env", "create", env_name, "--runtime", runtime_name],
        );
        assert!(create.status.success(), "{}", stderr(&create));
        set_service_enabled(&cwd, &env, env_name, true);
    }
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let initial_runtime = wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(5))
        .expect("daemon runtime state did not report both children");
    let target_pid = runtime_child_pid(&initial_runtime, "target").unwrap();
    let sibling_pid = runtime_child_pid(&initial_runtime, "sibling").unwrap();

    let sibling_meta_path = root.child("ocm-home/runtimes/sibling-runtime.json");
    let mut sibling_meta = read_persisted_service_state(&sibling_meta_path);
    sibling_meta["releaseVersion"] = Value::String("latent-sibling-v2".to_string());
    write_persisted_service_state(&sibling_meta_path, &sibling_meta);

    let uninstall = run_ocm(&cwd, &env, &["service", "uninstall", "target", "--json"]);
    assert!(uninstall.status.success(), "{}", stderr(&uninstall));

    let final_runtime =
        wait_for_runtime_children(&runtime_path, 1, Some("sibling"), Duration::from_secs(5))
            .expect("daemon runtime state did not retain only the sibling");
    let final_state = read_persisted_service_state(&state_path);
    let persisted_sibling = final_state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "sibling")
        .unwrap();

    assert_eq!(
        runtime_child_pid(&final_runtime, "sibling"),
        Some(sibling_pid),
        "sibling child PID changed"
    );
    assert_ne!(
        runtime_child_pid(&final_runtime, "target"),
        Some(target_pid),
        "target child remained active"
    );
    assert!(wait_for_file(&target_stopped, Duration::from_secs(5)));
    assert!(
        !sibling_stopped.exists(),
        "sibling child was stopped by target uninstall"
    );
    assert_eq!(
        fs::read_to_string(&sibling_started)
            .unwrap()
            .lines()
            .count(),
        1,
        "sibling child restarted"
    );
    assert!(
        persisted_sibling["runtimeReleaseVersion"].is_null(),
        "target uninstall applied unrelated sibling drift"
    );

    stop_process(&mut daemon);
}

#[test]
fn service_start_preserves_running_siblings_despite_unrelated_drift() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-targeted-service-start");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let state_path = root.child("ocm-home/supervisor/state.json");

    for (runtime_name, env_name) in [("target-runtime", "target"), ("sibling-runtime", "sibling")] {
        let runtime = root.child(format!("bin/{runtime_name}"));
        write_legacy_openclaw_script(
            &runtime,
            "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
        );
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                runtime_name,
                "--path",
                &path_string(&runtime),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
        let create = run_ocm(
            &cwd,
            &env,
            &["env", "create", env_name, "--runtime", runtime_name],
        );
        assert!(create.status.success(), "{}", stderr(&create));
        set_service_enabled(&cwd, &env, env_name, true);
    }
    service.sync().unwrap();
    service.install_daemon().unwrap();
    let daemon_status = service.daemon_status().unwrap();
    assert!(
        daemon_status.running,
        "fixture must report the managed daemon as running: {daemon_status:?}"
    );

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let initial_runtime = wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(5))
        .expect("daemon runtime state did not report both children");
    let target_pid = runtime_child_pid(&initial_runtime, "target").unwrap();
    let sibling_pid = runtime_child_pid(&initial_runtime, "sibling").unwrap();

    let sibling_meta_path = root.child("ocm-home/runtimes/sibling-runtime.json");
    let mut sibling_meta = read_persisted_service_state(&sibling_meta_path);
    sibling_meta["releaseVersion"] = Value::String("latent-sibling-v2".to_string());
    write_persisted_service_state(&sibling_meta_path, &sibling_meta);

    let start = run_ocm(&cwd, &env, &["service", "start", "target", "--json"]);
    assert!(start.status.success(), "{}", stderr(&start));
    sleep(Duration::from_millis(800));

    let final_runtime = wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(2))
        .expect("daemon runtime state stopped reporting both children");
    let final_state = read_persisted_service_state(&state_path);
    let persisted_sibling = final_state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "sibling")
        .unwrap();

    assert_eq!(
        runtime_child_pid(&final_runtime, "target"),
        Some(target_pid),
        "already-running target child PID changed"
    );
    assert_eq!(
        runtime_child_pid(&final_runtime, "sibling"),
        Some(sibling_pid),
        "sibling child PID changed"
    );
    assert!(
        persisted_sibling["runtimeReleaseVersion"].is_null(),
        "target start applied unrelated sibling drift"
    );

    stop_process(&mut daemon);
}

#[test]
fn service_start_reconciles_siblings_before_activating_stopped_daemon() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-stopped-targeted-service-start");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let service = SupervisorService::new(&env, &cwd);
    let state_path = root.child("ocm-home/supervisor/state.json");

    for (runtime_name, env_name) in [("target-runtime", "target"), ("sibling-runtime", "sibling")] {
        let runtime = root.child(format!("bin/{runtime_name}"));
        write_legacy_openclaw_script(
            &runtime,
            "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
        );
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                runtime_name,
                "--path",
                &path_string(&runtime),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
        let create = run_ocm(
            &cwd,
            &env,
            &["env", "create", env_name, "--runtime", runtime_name],
        );
        assert!(create.status.success(), "{}", stderr(&create));
        set_service_enabled(&cwd, &env, env_name, true);
    }
    service.sync().unwrap();

    let sibling_meta_path = root.child("ocm-home/runtimes/sibling-runtime.json");
    let mut sibling_meta = read_persisted_service_state(&sibling_meta_path);
    sibling_meta["releaseVersion"] = Value::String("latent-sibling-v2".to_string());
    write_persisted_service_state(&sibling_meta_path, &sibling_meta);

    let start = run_ocm(&cwd, &env, &["service", "start", "target", "--json"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let final_state = read_persisted_service_state(&state_path);
    let persisted_sibling = final_state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "sibling")
        .expect("sibling child spec should remain present");

    assert_eq!(
        persisted_sibling["runtimeReleaseVersion"], "latent-sibling-v2",
        "stopped-daemon activation must reconcile stale sibling state"
    );
}

#[test]
fn env_destroy_removes_only_target_child_despite_unrelated_drift() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-targeted-env-destroy");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_systemd_tools(&root, &mut env);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let state_path = root.child("ocm-home/supervisor/state.json");
    let target_started = root.child("target-started.txt");
    let target_stopped = root.child("target-stopped.txt");
    let sibling_started = root.child("sibling-started.txt");
    let sibling_stopped = root.child("sibling-stopped.txt");

    let target_runtime = root.child("bin/target-runtime");
    write_legacy_openclaw_script(
        &target_runtime,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&target_started),
            path_string(&target_stopped),
        ),
    );
    let sibling_runtime = root.child("bin/sibling-runtime");
    write_legacy_openclaw_script(
        &sibling_runtime,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&sibling_started),
            path_string(&sibling_stopped),
        ),
    );

    for (runtime_name, runtime_path, env_name) in [
        ("target-runtime", &target_runtime, "target"),
        ("sibling-runtime", &sibling_runtime, "sibling"),
    ] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                runtime_name,
                "--path",
                &path_string(runtime_path),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
        let create = run_ocm(
            &cwd,
            &env,
            &["env", "create", env_name, "--runtime", runtime_name],
        );
        assert!(create.status.success(), "{}", stderr(&create));
        set_service_enabled(&cwd, &env, env_name, true);
    }
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let initial_runtime = wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(5))
        .expect("daemon runtime state did not report both children");
    let target_pid = runtime_child_pid(&initial_runtime, "target").unwrap();
    let sibling_pid = runtime_child_pid(&initial_runtime, "sibling").unwrap();

    let sibling_meta_path = root.child("ocm-home/runtimes/sibling-runtime.json");
    let mut sibling_meta = read_persisted_service_state(&sibling_meta_path);
    sibling_meta["releaseVersion"] = Value::String("latent-sibling-v2".to_string());
    write_persisted_service_state(&sibling_meta_path, &sibling_meta);

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "target", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: Value = serde_json::from_str(&stdout(&preview)).unwrap();
    let state_token = preview_json["stateToken"].as_str().unwrap();
    let destroy = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "destroy",
            "target",
            "--yes",
            "--if-state-token",
            state_token,
            "--json",
        ],
    );
    assert!(destroy.status.success(), "{}", stderr(&destroy));

    let final_runtime =
        wait_for_runtime_children(&runtime_path, 1, Some("sibling"), Duration::from_secs(5))
            .expect("daemon runtime state did not retain only the sibling");
    let final_state = read_persisted_service_state(&state_path);
    let persisted_sibling = final_state["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == "sibling")
        .unwrap();

    assert_eq!(
        runtime_child_pid(&final_runtime, "sibling"),
        Some(sibling_pid),
        "sibling child PID changed"
    );
    assert_ne!(
        runtime_child_pid(&final_runtime, "target"),
        Some(target_pid),
        "target child remained active"
    );
    assert!(wait_for_file(&target_stopped, Duration::from_secs(5)));
    assert!(
        !sibling_stopped.exists(),
        "sibling child was stopped by target environment destroy"
    );
    assert_eq!(
        fs::read_to_string(&sibling_started)
            .unwrap()
            .lines()
            .count(),
        1,
        "sibling child restarted"
    );
    assert!(
        persisted_sibling["runtimeReleaseVersion"].is_null(),
        "target environment destroy applied unrelated sibling drift"
    );
    assert!(
        EnvironmentService::new(&env, &cwd)
            .find("target")
            .unwrap()
            .is_none(),
        "target environment metadata remained after destroy"
    );

    stop_process(&mut daemon);
}

#[test]
fn service_restart_preserves_legacy_fallback_and_restarts_only_the_target_child() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-targeted-service-restart");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_systemd_tools(&root, &mut env);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let rescue_started = root.child("rescue-started.txt");
    let rescue_stopped = root.child("rescue-stopped.txt");
    let main_started = root.child("main-started.txt");
    let main_stopped = root.child("main-stopped.txt");
    let rescue_script = root.child("bin/rescue-openclaw");
    let main_script = root.child("bin/main-openclaw");
    write_legacy_openclaw_script(
        &rescue_script,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&rescue_started),
            path_string(&rescue_stopped),
        ),
    );
    write_legacy_openclaw_script(
        &main_script,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'printf \"%s\\n\" \"$$\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&main_started),
            path_string(&main_stopped),
        ),
    );

    let rescue_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "rescue",
            "--command",
            &path_string(&rescue_script),
        ],
    );
    assert!(
        rescue_launcher.status.success(),
        "{}",
        stderr(&rescue_launcher)
    );
    let main_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "main",
            "--command",
            &path_string(&main_script),
        ],
    );
    assert!(main_launcher.status.success(), "{}", stderr(&main_launcher));

    let rescue_env = run_ocm(
        &cwd,
        &env,
        &["env", "create", "rescue", "--launcher", "rescue"],
    );
    assert!(rescue_env.status.success(), "{}", stderr(&rescue_env));
    set_service_enabled(&cwd, &env, "rescue", true);
    let main_env = run_ocm(&cwd, &env, &["env", "create", "main", "--launcher", "main"]);
    assert!(main_env.status.success(), "{}", stderr(&main_env));
    set_service_enabled(&cwd, &env, "main", true);
    service.sync().unwrap();
    service.install_daemon().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&rescue_started, Duration::from_secs(5)));
    assert!(wait_for_file(&main_started, Duration::from_secs(5)));
    let initial_runtime = wait_for_runtime_children(&runtime_path, 2, None, Duration::from_secs(5))
        .expect("daemon runtime state did not report both running children");
    let rescue_pid = runtime_child_pid(&initial_runtime, "rescue").unwrap();
    let main_pid = runtime_child_pid(&initial_runtime, "main").unwrap();

    let mut restart_env = env.clone();
    restart_env.insert(
        "NODE_OPTIONS".to_string(),
        "--max-old-space-size=2048".to_string(),
    );
    let restart = run_ocm(
        &cwd,
        &restart_env,
        &["service", "restart", "rescue", "--json"],
    );
    assert!(restart.status.success(), "{}", stderr(&restart));
    let restart_body: serde_json::Value = serde_json::from_slice(&restart.stdout).unwrap();
    assert!(
        restart_body["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("used the legacy direct supervisor restart path"))
    );
    let restarted = wait_for_runtime_child_pid_change(
        &runtime_path,
        "rescue",
        rescue_pid,
        "main",
        main_pid,
        Duration::from_secs(10),
    )
    .expect("targeted restart did not replace only the requested child");

    assert_eq!(runtime_child_pid(&restarted, "main"), Some(main_pid));
    assert_ne!(runtime_child_pid(&restarted, "rescue"), Some(rescue_pid));
    assert!(wait_for_file(&rescue_stopped, Duration::from_secs(5)));
    assert!(
        !main_stopped.exists(),
        "sibling child was stopped by targeted restart"
    );

    stop_process(&mut daemon);
}

#[test]
fn service_restart_requeues_a_stopped_desired_child() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-restart-stopped-child");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_systemd_tools(&root, &mut env);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let exit_once = root.child("exit-once");
    let started = root.child("started.txt");
    let script = root.child("bin/openclaw");
    fs::write(&exit_once, "1\n").unwrap();
    write_legacy_openclaw_script(
        &script,
        &format!(
            "#!/bin/sh\nif [ -f '{}' ]; then rm -f '{}'; exit 0; fi\nprintf '%s\\n' \"$$\" >> '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&exit_once),
            path_string(&exit_once),
            path_string(&started),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();
    service.install_daemon().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    wait_for_runtime_service_state(&runtime_path, "demo", "stopped", Duration::from_secs(5))
        .expect("daemon runtime state did not report the quick clean exit as stopped");
    assert!(!started.exists());

    let restart = run_ocm(
        &cwd,
        &env,
        &["service", "restart", "demo", "--force", "--json"],
    );
    assert!(restart.status.success(), "{}", stderr(&restart));
    let runtime =
        wait_for_runtime_children(&runtime_path, 1, Some("demo"), Duration::from_secs(10))
            .expect("service restart did not requeue the stopped desired child");

    assert!(runtime_child_pid(&runtime, "demo").is_some());
    assert!(wait_for_file(&started, Duration::from_secs(5)));

    stop_process(&mut daemon);
}

#[test]
fn service_start_and_restart_wait_for_gateway_health() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("service-readiness-healthy");
    let (cwd, mut env) = setup_gateway_readiness_fixture(&root, "healthy", 0, 5_000);
    let mut daemon = spawn_daemon_process(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo", "--json"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let started_body: Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(started_body["gatewayReady"], true);
    assert_eq!(started_body["gatewayState"], "running");
    assert_eq!(started_body["issue"], Value::Null);

    env.insert("OCM_ACTIVE_ENV".to_string(), "demo".to_string());
    let restarted = run_ocm(&cwd, &env, &["service", "restart", "demo", "--json"]);
    assert!(restarted.status.success(), "{}", stderr(&restarted));
    let restarted_body: Value = serde_json::from_slice(&restarted.stdout).unwrap();
    assert_eq!(restarted_body["gatewayReady"], true);
    assert_eq!(restarted_body["gatewayState"], "running");

    stop_process(&mut daemon);
}

#[test]
fn service_start_waits_for_slow_gateway_health() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("service-readiness-slow");
    let (cwd, env) = setup_gateway_readiness_fixture(&root, "healthy", 700, 5_000);
    let mut daemon = spawn_daemon_process(&cwd, &env);

    let started_at = Instant::now();
    let started = run_ocm(&cwd, &env, &["service", "start", "demo", "--json"]);
    assert!(started.status.success(), "{}", stderr(&started));
    assert!(started_at.elapsed() >= Duration::from_millis(500));
    let body: Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(body["gatewayReady"], true);

    stop_process(&mut daemon);
}

#[test]
fn service_start_reports_failed_backoff_with_the_child_error() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("service-readiness-backoff");
    let (cwd, env) = setup_gateway_readiness_fixture(&root, "exit-23", 50, 5_000);
    let mut daemon = spawn_daemon_process(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo", "--json"]);
    assert!(!started.status.success());
    let body: Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(body["gatewayReady"], false);
    assert!(matches!(
        body["gatewayState"].as_str(),
        Some("backoff" | "stopped")
    ));
    assert!(body["issue"].as_str().unwrap().contains("23"));

    stop_process(&mut daemon);
}

#[test]
fn service_start_reports_an_explicit_readiness_timeout() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("service-readiness-timeout");
    let (cwd, env) = setup_gateway_readiness_fixture(&root, "timeout", 0, 700);
    let mut daemon = spawn_daemon_process(&cwd, &env);

    let started = run_ocm(&cwd, &env, &["service", "start", "demo", "--json"]);
    assert!(!started.status.success());
    let body: Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(body["gatewayReady"], false);
    assert!(
        body["issue"]
            .as_str()
            .unwrap()
            .contains("did not become ready within")
    );

    stop_process(&mut daemon);
}

#[test]
fn daemon_keeps_running_when_one_env_fails_to_spawn() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-spawn-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let good_started = root.child("good-started.txt");
    let bad_started = root.child("bad-started.txt");
    let missing_cwd = root.child("missing-cwd");

    let good_script = root.child("bin/good-openclaw");
    write_legacy_openclaw_script(
        &good_script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&good_started),
        ),
    );
    let bad_script = root.child("bin/bad-openclaw");
    write_legacy_openclaw_script(
        &bad_script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&bad_started),
        ),
    );

    let good_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "good",
            "--command",
            &path_string(&good_script),
        ],
    );
    assert!(good_launcher.status.success(), "{}", stderr(&good_launcher));
    let bad_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "bad",
            "--command",
            &path_string(&bad_script),
            "--cwd",
            &path_string(&missing_cwd),
        ],
    );
    assert!(bad_launcher.status.success(), "{}", stderr(&bad_launcher));

    let good_env = run_ocm(&cwd, &env, &["env", "create", "good", "--launcher", "good"]);
    assert!(good_env.status.success(), "{}", stderr(&good_env));
    set_service_enabled(&cwd, &env, "good", true);

    let bad_env = run_ocm(&cwd, &env, &["env", "create", "bad", "--launcher", "bad"]);
    assert!(bad_env.status.success(), "{}", stderr(&bad_env));
    set_service_enabled(&cwd, &env, "bad", true);

    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&good_started, Duration::from_secs(5)));
    let runtime = wait_for_runtime_children(&runtime_path, 1, Some("good"), Duration::from_secs(5))
        .expect("daemon runtime state did not keep the healthy child running");
    assert_eq!(runtime["children"][0]["envName"], "good");
    assert!(!bad_started.exists());
    assert!(daemon.try_wait().unwrap().is_none());

    fs::create_dir_all(&missing_cwd).unwrap();

    let runtime = wait_for_runtime_children(&runtime_path, 2, Some("bad"), Duration::from_secs(5))
        .expect("daemon runtime state did not recover the previously failing child");
    assert_eq!(runtime["children"].as_array().unwrap().len(), 2);
    assert!(wait_for_file(&bad_started, Duration::from_secs(5)));

    stop_process(&mut daemon);
}

#[test]
fn daemon_stops_a_running_child_after_service_stop() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-service-stop");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let started = root.child("started.txt");
    let stopped = root.child("stopped.txt");
    let script = root.child("bin/openclaw");
    write_legacy_openclaw_script(
        &script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'printf \"stopped\\n\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&started),
            path_string(&stopped),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&started, Duration::from_secs(5)));
    wait_for_runtime_children(&runtime_path, 1, Some("demo"), Duration::from_secs(5))
        .expect("daemon runtime state did not report the running child");

    let stop = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(stop.status.success(), "{}", stderr(&stop));

    wait_for_runtime_children(&runtime_path, 0, None, Duration::from_secs(5))
        .expect("daemon runtime state did not clear after service stop");
    assert!(wait_for_file(&stopped, Duration::from_secs(5)));

    stop_process(&mut daemon);
}

#[cfg(unix)]
#[test]
fn daemon_stops_the_full_dev_process_tree_after_service_stop() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-service-stop-tree");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let started = root.child("started.txt");
    let stopped = root.child("stopped.txt");
    let child_pid_file = root.child("child.pid");
    let script = root.child("bin/openclaw");
    write_legacy_openclaw_script(
        &script,
        &format!(
            "#!/bin/sh\nsh -c 'echo $$ > \"{}\"; trap \"exit 0\" TERM INT; while :; do sleep 1; done' &\nprintf 'started\\n' >> '{}'\ntrap 'printf \"stopped\\n\" >> \"{}\"; exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&child_pid_file),
            path_string(&started),
            path_string(&stopped),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&started, Duration::from_secs(5)));
    assert!(wait_for_file(&child_pid_file, Duration::from_secs(5)));
    wait_for_runtime_children(&runtime_path, 1, Some("demo"), Duration::from_secs(5))
        .expect("daemon runtime state did not report the running child");

    let child_pid = fs::read_to_string(&child_pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(process_exists(child_pid));

    let stop = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(stop.status.success(), "{}", stderr(&stop));

    wait_for_runtime_children(&runtime_path, 0, None, Duration::from_secs(5))
        .expect("daemon runtime state did not clear after service stop");
    assert!(wait_for_file(&stopped, Duration::from_secs(5)));

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && process_exists(child_pid) {
        sleep(Duration::from_millis(50));
    }
    assert!(
        !process_exists(child_pid),
        "background descendant still alive after service stop"
    );

    stop_process(&mut daemon);
}

#[cfg(unix)]
#[test]
fn daemon_cleans_descendants_after_a_child_exits_before_restart() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-exit-process-group-cleanup");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);

    let starts = root.child("starts.txt");
    let descendant_pid_file = root.child("descendant.pid");
    let script = root.child("bin/openclaw.mjs");
    write_legacy_openclaw_script(
        &script,
        &format!(
            "#!/bin/sh\ncount=0\nif [ -f '{starts}' ]; then count=$(cat '{starts}'); fi\ncount=$((count + 1))\nprintf '%s\n' \"$count\" > '{starts}'\nif [ \"$count\" -eq 1 ]; then\n  trap '' HUP\n  sleep 60 &\n  printf '%s\n' \"$!\" > '{descendant_pid_file}'\n  sleep 1\n  exit 1\nfi\nsleep 10\n",
            starts = path_string(&starts),
            descendant_pid_file = path_string(&descendant_pid_file),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&descendant_pid_file, Duration::from_secs(5)));
    let descendant_pid = fs::read_to_string(&descendant_pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(process_exists(descendant_pid));

    assert!(wait_for_file_value(&starts, "2", Duration::from_secs(5)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && process_exists(descendant_pid) {
        sleep(Duration::from_millis(50));
    }
    assert!(
        !process_exists(descendant_pid),
        "background descendant still alive after supervised child exit"
    );

    stop_process(&mut daemon);
}

#[test]
fn daemon_restarts_quick_clean_exit_with_openclaw_handoff() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-clean-handoff");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let starts = root.child("starts.txt");
    let child_env = root.child("child-env.txt");
    let intent = root.child("restart-intent.txt");
    let script = root.child("bin/openclaw.mjs");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\n{protocol}printf '%s|%s|%s|%s|%s|%s\\n' \"${{OPENCLAW_SUPERVISOR_MODE:-unset}}\" \"${{OPENCLAW_SERVICE_MARKER:-unset}}\" \"${{OPENCLAW_SERVICE_KIND:-unset}}\" \"${{OPENCLAW_NO_RESPAWN:-unset}}\" \"${{OPENCLAW_LAUNCHD_LABEL:-unset}}\" \"${{OPENCLAW_SYSTEMD_UNIT:-unset}}\" > '{child_env}'\ncount=0\nif [ -f '{starts}' ]; then count=$(cat '{starts}'); fi\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{starts}'\nif [ \"$count\" -eq 1 ]; then\nprintf '%s\\n' \"$$\" > '{intent}'\nexit 0\nfi\nsleep 10\n",
            protocol = restart_handoff_protocol_shell_snippet(&intent),
            child_env = path_string(&child_env),
            starts = path_string(&starts),
            intent = path_string(&intent),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file_value(&starts, "2", Duration::from_secs(6)));
    let runtime = wait_for_runtime_children(&runtime_path, 1, Some("demo"), Duration::from_secs(5))
        .expect("daemon runtime state did not report the restarted child");
    assert!(runtime["children"][0]["restartCount"].as_u64().unwrap() >= 1);
    assert_eq!(
        fs::read_to_string(&child_env).unwrap().trim(),
        "external|openclaw|gateway|unset|unset|unset"
    );

    stop_process(&mut daemon);
}

#[test]
fn daemon_keeps_protocol_capable_wrapper_in_safe_no_respawn_mode() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-legacy-no-respawn");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let child_env = root.child("child-env.txt");
    let intent = root.child("restart-intent.txt");
    let script = root.child("bin/openclaw");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\n{protocol}printf '%s|%s|%s|%s|%s|%s\\n' \"${{OPENCLAW_SUPERVISOR_MODE:-unset}}\" \"${{OPENCLAW_SERVICE_MARKER:-unset}}\" \"${{OPENCLAW_SERVICE_KIND:-unset}}\" \"${{OPENCLAW_NO_RESPAWN:-unset}}\" \"${{OPENCLAW_WINDOWS_TASK_NAME:-unset}}\" \"${{OPENCLAW_SYSTEMD_UNIT:-unset}}\" > '{child_env}'\nexit 0\n",
            protocol = restart_handoff_protocol_shell_snippet(&intent),
            child_env = path_string(&child_env),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "legacy",
            "--command",
            &path_string(&script),
        ],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "legacy"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    let service_state =
        wait_for_runtime_service_state(&runtime_path, "demo", "stopped", Duration::from_secs(6))
            .expect("daemon runtime state did not stop after the wrapper quick clean exit");
    let last_error = service_state["lastError"].as_str().unwrap();
    assert!(last_error.contains("does not support a PID-safe external restart handoff"));
    assert!(last_error.contains("does not preserve the supervised gateway PID"));
    assert_eq!(
        fs::read_to_string(&child_env).unwrap().trim(),
        "unset|openclaw|unset|1|unset|unset"
    );
    assert!(daemon.try_wait().unwrap().is_none());

    stop_process(&mut daemon);
}

#[test]
fn daemon_stops_quick_clean_exit_without_restart_handoff() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-clean-no-handoff");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let started = root.child("started.txt");
    let intent = root.child("restart-intent.txt");
    let script = root.child("bin/openclaw.mjs");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\n{protocol}count=0\nif [ -f '{started}' ]; then count=$(cat '{started}'); fi\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{started}'\nexit 0\n",
            protocol = restart_handoff_protocol_shell_snippet(&intent),
            started = path_string(&started),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file_value(&started, "1", Duration::from_secs(6)));
    let service_state =
        wait_for_runtime_service_state(&runtime_path, "demo", "stopped", Duration::from_secs(5))
            .expect("daemon runtime state did not stop after the quick clean exit");
    assert_eq!(service_state["restartCount"], 0);
    assert!(
        service_state["lastError"]
            .as_str()
            .unwrap()
            .contains("without an accepted OpenClaw restart handoff")
    );

    sleep(Duration::from_secs(2));
    let starts = fs::read_to_string(&started).unwrap();
    assert_eq!(starts.trim(), "1");
    assert!(daemon.try_wait().unwrap().is_none());

    stop_process(&mut daemon);
}

#[test]
fn daemon_stops_repeated_quick_clean_exit_after_restart_handoff() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-run-clean-exit");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");

    let started = root.child("started.txt");
    let intent = root.child("restart-intent.txt");
    let script = root.child("bin/openclaw.mjs");
    write_executable_script(
        &script,
        &format!(
            "#!/bin/sh\n{protocol}count=0\nif [ -f '{started}' ]; then count=$(cat '{started}'); fi\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{started}'\nprintf '%s\\n' \"$$\" > '{intent}'\nexit 0\n",
            protocol = restart_handoff_protocol_shell_snippet(&intent),
            started = path_string(&started),
            intent = path_string(&intent),
        ),
    );

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "dev", "--command", &path_string(&script)],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "dev"]);
    assert!(created.status.success(), "{}", stderr(&created));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file_value(&started, "2", Duration::from_secs(6)));
    let service_state =
        wait_for_runtime_service_state(&runtime_path, "demo", "stopped", Duration::from_secs(5))
            .expect("daemon runtime state did not stop after the repeated quick clean exit");
    assert_eq!(service_state["restartCount"], 1);
    assert!(
        service_state["lastError"]
            .as_str()
            .unwrap()
            .contains("avoid a restart loop")
    );

    sleep(Duration::from_secs(2));
    let starts = fs::read_to_string(&started).unwrap();
    assert_eq!(starts.trim(), "2");
    assert!(daemon.try_wait().unwrap().is_none());

    stop_process(&mut daemon);
}

#[test]
fn live_runtime_changes_recreate_missing_supervisor_state() {
    let _guard = daemon_runtime_test_lock();
    let root = TestDir::new("daemon-runtime-recovers-missing-state");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let service = SupervisorService::new(&env, &cwd);
    let runtime_path = root.child("ocm-home/supervisor/runtime.json");
    let state_path = root.child("ocm-home/supervisor/state.json");

    let first_started = root.child("first-started.txt");
    let second_started = root.child("second-started.txt");
    let first_script = root.child("bin/first-openclaw");
    write_legacy_openclaw_script(
        &first_script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&first_started),
        ),
    );
    let second_script = root.child("bin/second-openclaw");
    write_legacy_openclaw_script(
        &second_script,
        &format!(
            "#!/bin/sh\nprintf 'started\\n' >> '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            path_string(&second_started),
        ),
    );

    let first_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "first",
            "--command",
            &path_string(&first_script),
        ],
    );
    assert!(
        first_launcher.status.success(),
        "{}",
        stderr(&first_launcher)
    );
    let second_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "second",
            "--command",
            &path_string(&second_script),
        ],
    );
    assert!(
        second_launcher.status.success(),
        "{}",
        stderr(&second_launcher)
    );

    let demo = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "first"],
    );
    assert!(demo.status.success(), "{}", stderr(&demo));
    set_service_enabled(&cwd, &env, "demo", true);
    service.sync().unwrap();

    let mut daemon = spawn_daemon_process(&cwd, &env);
    assert!(wait_for_file(&first_started, Duration::from_secs(5)));
    wait_for_runtime_children(&runtime_path, 1, Some("demo"), Duration::from_secs(5))
        .expect("daemon runtime state did not report the first child");

    fs::remove_file(&state_path).unwrap();
    assert!(!state_path.exists());

    let extra = run_ocm(
        &cwd,
        &env,
        &["env", "create", "extra", "--launcher", "second"],
    );
    assert!(extra.status.success(), "{}", stderr(&extra));
    set_service_enabled(&cwd, &env, "extra", true);

    assert!(wait_for_file(&state_path, Duration::from_secs(5)));
    wait_for_runtime_children(&runtime_path, 2, Some("extra"), Duration::from_secs(5))
        .expect("daemon did not recover after the supervisor state file was recreated");
    assert!(wait_for_file(&second_started, Duration::from_secs(5)));

    stop_process(&mut daemon);
}
