mod support;

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, sleep};
use std::time::{Duration, Instant};

use fs2::FileExt;
use ocm::store::{env_registry_path, supervisor_runtime_path, supervisor_state_path};
use ocm::supervisor::SupervisorRuntimeChild;
use serde_json::Value;

use crate::support::{
    TestDir, TestHttpServer, install_fake_launchctl, install_fake_node_and_npm, ocm_env,
    openclaw_package_tarball, openclaw_package_tarball_with_manifest, path_string, run_ocm,
    sha512_integrity, stderr, write_empty_supervisor_runtime,
};

const PREPARE_DELAY: Duration = Duration::from_millis(1_500);
const TARGET_CONFIG_DELAY_SECONDS: &str = "0.11";
const CONFIG_SNAPSHOT_DELAY_SECONDS: &str = "0.13";
const DOCTOR_DELAY_SECONDS: &str = "0.17";
const PLUGIN_DELAY_SECONDS: &str = "0.19";
const TARGET_CONVERGENCE_DELAY_SECONDS: &str = "0.23";
const COMPLETION_DELAY_SECONDS: &str = "0.29";
const FINALIZER_AND_PUBLICATION_DELAY: Duration = Duration::from_millis(1_830);
const GATEWAY_STARTUP_DELAY: Duration = Duration::from_millis(370);

#[derive(Clone, Default)]
struct Timeline(Arc<Mutex<HashMap<&'static str, Instant>>>);

impl Timeline {
    fn mark(&self, phase: &'static str) {
        self.0
            .lock()
            .unwrap()
            .entry(phase)
            .or_insert_with(Instant::now);
    }

    fn at(&self, phase: &'static str) -> Instant {
        *self
            .0
            .lock()
            .unwrap()
            .get(phase)
            .unwrap_or_else(|| panic!("phase {phase} was not observed"))
    }

    fn report(&self, origin: Instant) -> String {
        let mut entries = self
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(phase, at)| (*phase, at.duration_since(origin)))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, at)| *at);
        entries
            .into_iter()
            .map(|(phase, at)| format!("{phase}={:.3}s", at.as_secs_f64()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn delayed_bytes_server(
    path: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    timeline: Timeline,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap_or(0);
        assert!(
            request[..read].starts_with(format!("GET {path} ").as_bytes()),
            "unexpected delayed fixture request: {}",
            String::from_utf8_lossy(&request[..read])
        );
        timeline.mark("target_prepare_started");
        sleep(PREPARE_DELAY);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
        timeline.mark("target_prepare_released");
    });
    (format!("http://{addr}{path}"), handle)
}

struct ControlledHealthServer {
    port: u32,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl ControlledHealthServer {
    fn start(available: Arc<AtomicBool>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = u32::from(listener.local_addr().unwrap().port());
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("health listener failed: {error}"),
                };
                let connection_available = Arc::clone(&available);
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                    let mut request = [0_u8; 1024];
                    let read = stream.read(&mut request).unwrap_or(0);
                    if !request[..read].starts_with(b"GET /health ") {
                        return;
                    }
                    let ok = connection_available.load(Ordering::SeqCst);
                    let status = if ok {
                        "200 OK"
                    } else {
                        "503 Service Unavailable"
                    };
                    let body = if ok {
                        br#"{"ok":true}"#.as_slice()
                    } else {
                        br#"{"ok":false}"#.as_slice()
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                });
            }
        });
        Self { port, stop, handle }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port as u16));
        self.handle.join().unwrap();
    }
}

fn health_observation(port: u32) -> Option<bool> {
    let Ok(mut stream) = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(500),
    ) else {
        return None;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return None;
    }
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    if response.starts_with(b"HTTP/1.1 200 OK") {
        Some(true)
    } else if response.starts_with(b"HTTP/1.1 503 Service Unavailable") {
        Some(false)
    } else {
        None
    }
}

fn write_running_supervisor_runtime(
    runtime_path: &Path,
    ocm_home: &str,
    binding_kind: &str,
    binding_name: &str,
    pid: u32,
    child_port: u32,
) {
    let log_root = runtime_path.parent().unwrap();
    let stdout_path = path_string(&log_root.join("demo.stdout.log"));
    let stderr_path = path_string(&log_root.join("demo.stderr.log"));
    support::write_running_supervisor_runtime(
        runtime_path,
        ocm_home,
        SupervisorRuntimeChild {
            launch_spec_sha256: None,
            env_name: "demo".to_string(),
            binding_kind: binding_kind.to_string(),
            binding_name: binding_name.to_string(),
            pid,
            restart_count: 0,
            child_port,
            stdout_path,
            stderr_path,
        },
    );
}

fn fixture_openclaw_script(version: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  --version)
    printf '{version}\n'
    exit 0
    ;;
  config)
    sleep "${{OCM_TEST_TARGET_CONFIG_VALIDATE_DELAY:-0}}"
    printf 'Config valid\n'
    exit 0
    ;;
  doctor)
    printf '%s\n' '{{"ok":false,"checksRun":0,"checksSkipped":1,"findings":[{{"checkId":"core/doctor/lint-selection","severity":"error","message":"Unknown health check id selected by --only: codex/managed-app-server.","path":"codex/managed-app-server"}}]}}'
    exit 1
    ;;
  update)
    if [ "$2" = "preflight" ]; then
      if [ "$#" != "7" ] || [ "$3" != "--json" ] || [ "$4" != "--source-root" ] || [ "$6" != "--timeout" ] || [ "$7" != "300" ]; then
        printf 'invalid preflight arguments\n' >&2
        exit 41
      fi
      if env | grep -Ei '^(OPENCLAW_UPDATE_|OPENCLAW_(SUPERVISOR_MODE|LAUNCHD_LABEL|SYSTEMD_UNIT|WINDOWS_TASK_NAME|SERVICE_MARKER|SERVICE_KIND|CONTROL_PLANE_UPDATE_SENTINEL_META|GATEWAY_SERVICE_PID|COMPATIBILITY_HOST_VERSION|NO_RESPAWN)=|LAUNCH_JOB_LABEL=|LAUNCH_JOB_NAME=|XPC_SERVICE_NAME=|INVOCATION_ID=|SYSTEMD_EXEC_PID=|JOURNAL_STREAM=)' >/dev/null; then
        printf 'preflight inherited update authority\n' >&2
        exit 42
      fi
      printf '%s\n' "$5" "$OPENCLAW_STATE_DIR" "$OPENCLAW_CONFIG_PATH" > "$OCM_TEST_PREFLIGHT_OBSERVATION"
      cat "$OCM_TEST_PREFLIGHT_RECEIPT"
      exit "${{OCM_TEST_PREFLIGHT_EXIT:-0}}"
    fi
    if [ "$2" = "finalize" ]; then
      if [ "$OPENCLAW_UPDATE_POST_CORE" != "1" ]; then
        printf 'missing post-core capability handshake\n' >&2
        exit 32
      fi
      : > "$OCM_TEST_UPDATE_FINALIZE_STARTED"
      while [ ! -e "$OCM_TEST_UPDATE_FINALIZE_RELEASE" ]; do
        sleep 0.01
      done
      sleep "${{OCM_TEST_TARGET_CONFIG_VALIDATE_DELAY:-0}}"
      sleep "${{OCM_TEST_CONFIG_SNAPSHOT_DELAY:-0}}"
      sleep "${{OCM_TEST_DOCTOR_DELAY:-0}}"
      sleep "${{OCM_TEST_PLUGIN_DELAY:-0}}"
      sleep "${{OCM_TEST_TARGET_CONFIG_CONVERGENCE_DELAY:-0}}"
      printf 'prepared\n' > "$OCM_TEST_COMPLETION_INPUT"
      printf '{{"status":"ok","mode":"finalize","phaseTimings":[{{"phase":"targetConfigValidation","startedOffsetMs":0,"durationMs":110,"outcome":"completed"}},{{"phase":"configSnapshot","startedOffsetMs":110,"durationMs":130,"outcome":"completed"}},{{"phase":"doctor","startedOffsetMs":240,"durationMs":170,"outcome":"completed"}},{{"phase":"plugins","startedOffsetMs":410,"durationMs":190,"outcome":"completed"}},{{"phase":"targetConfigConvergence","startedOffsetMs":600,"durationMs":230,"outcome":"completed"}},{{"phase":"completionCache","startedOffsetMs":830,"durationMs":0,"outcome":"deferred"}}]}}\n'
      exit 0
    fi
    ;;
  completion)
    : > "$OCM_TEST_COMPLETION_STARTED"
    if [ "$(cat "$OCM_TEST_COMPLETION_INPUT")" != "committed" ]; then
      printf 'completion used stale prepared input\n' >&2
      exit 31
    fi
    sleep "${{OCM_TEST_COMPLETION_DELAY:-0}}"
    printf 'Completion cache written\n'
    exit 0
    ;;
  gateway)
    printf '{{"rpc":{{"ok":true}}}}\n'
    exit 0
    ;;
esac
printf 'unexpected command: %s\n' "$*" >&2
exit 1
"#
    )
}

fn env_desired_running(registry_path: &Path, fallback: bool) -> bool {
    fs::read(registry_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|registry| registry["envs"].as_array().cloned())
        .and_then(|envs| envs.into_iter().find(|entry| entry["name"] == "demo"))
        .and_then(|entry| entry["serviceRunning"].as_bool())
        .unwrap_or(fallback)
}

fn preflight_package(version: &str, protocol: Value) -> Vec<u8> {
    openclaw_package_tarball_with_manifest(
        &fixture_openclaw_script(version),
        &serde_json::json!({
            "name": "openclaw",
            "version": version,
            "bin": {"openclaw": "openclaw.mjs"},
            "openclaw": {"updatePreflightProtocol": protocol}
        })
        .to_string(),
    )
}

fn preflight_receipt_json(status: &str, version: &str) -> Value {
    serde_json::json!({
        "protocol": 1,
        "mode": "preflight",
        "status": status,
        "candidateVersion": version,
        "coverage": "isolated-core-startup",
        "phase": "readiness",
        "reason": "synthetic readiness result"
    })
}

fn assert_preflight_observation(observation: &str, source_root: &Path, environment: &Value) {
    let lines = observation.lines().collect::<Vec<_>>();
    let paths = ocm::store::derive_env_paths(environment["root"].as_str().unwrap());
    assert_eq!(
        lines,
        [
            path_string(source_root),
            path_string(&paths.state_dir),
            path_string(&paths.config_path),
        ],
        "preflight did not receive the actual source package and profile paths"
    );
}

#[cfg(unix)]
#[test]
fn upgrade_preflights_the_package_behind_a_migrated_openclaw_executable() {
    let root = TestDir::new("upgrade-preflight-migrated-executable");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    // A global npm install links bin/openclaw to the installed package entrypoint.
    // `ocm migrate` stores this absolute path unquoted although `#` is shell syntax
    // elsewhere in a launcher command.
    let package = root.child("npm#1/lib/node_modules/openclaw");
    fs::create_dir_all(&package).unwrap();
    support::write_executable_script(
        &package.join("openclaw.mjs"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 2026.8.13; fi\nexit 0\n",
    );
    fs::write(
        package.join("package.json"),
        r#"{"name":"openclaw","version":"2026.8.13","bin":{"openclaw":"openclaw.mjs"}}"#,
    )
    .unwrap();
    let bin = root.child("npm#1/bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(
        "../lib/node_modules/openclaw/openclaw.mjs",
        bin.join("openclaw"),
    )
    .unwrap();
    let home = root.child("home/.openclaw");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("openclaw.json"), "{}").unwrap();

    let version = "2026.8.14";
    let candidate = r#"#!/bin/sh
case "$1" in
  --version) echo 2026.8.14; exit 0 ;;
  update)
    if [ "$2" = preflight ]; then
      printf '%s\n' "$5" "$OPENCLAW_STATE_DIR" "$OPENCLAW_CONFIG_PATH" > "$OCM_TEST_PREFLIGHT_OBSERVATION"
      cat "$OCM_TEST_PREFLIGHT_RECEIPT"
      exit 0
    fi
    printf '{"status":"ok","mode":"finalize","postUpdate":{}}\n'
    exit 0 ;;
  gateway) printf '{"rpc":{"ok":true}}\n'; exit 0 ;;
esac
exit 0
"#;
    let tarball = openclaw_package_tarball_with_manifest(
        candidate,
        &serde_json::json!({
            "name": "openclaw",
            "version": version,
            "bin": {"openclaw": "openclaw.mjs"},
            "openclaw": {"updatePreflightProtocol": 1}
        })
        .to_string(),
    );
    let tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.8.14.tgz",
        "application/octet-stream",
        &tarball,
        4,
    );
    let packument = serde_json::json!({
        "dist-tags": {"latest": version},
        "versions": {version: {
            "version": version,
            "dist": {"tarball": tarball_server.url(), "integrity": sha512_integrity(&tarball)}
        }}
    });
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        (0..8).map(|_| packument.to_string().into_bytes()).collect(),
    );
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".into(),
        packument_server.url(),
    );
    let path = format!("{}:{}", path_string(&bin), env["PATH"]);
    env.insert("PATH".into(), path);
    let observation = root.child("preflight-observation");
    let receipt = root.child("preflight-receipt.json");
    fs::write(
        &receipt,
        preflight_receipt_json("ready", version).to_string(),
    )
    .unwrap();
    env.insert(
        "OCM_TEST_PREFLIGHT_OBSERVATION".into(),
        path_string(&observation),
    );
    env.insert("OCM_TEST_PREFLIGHT_RECEIPT".into(), path_string(&receipt));

    let migrated = run_ocm(&cwd, &env, &["migrate", "mira"]);
    assert!(migrated.status.success(), "{}", stderr(&migrated));
    let launcher = run_ocm(&cwd, &env, &["launcher", "show", "mira.migrated", "--json"]);
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let launcher: Value = serde_json::from_slice(&launcher.stdout).unwrap();
    // Migration quotes the path only when the shell would not read it as one word.
    let command = launcher["command"].as_str().unwrap();
    let executable = path_string(&bin.join("openclaw"));
    assert!(
        command == executable || command == ocm::infra::shell::quote_posix(&executable),
        "{command}"
    );

    let upgraded = run_ocm(
        &cwd,
        &env,
        &["upgrade", "mira", "--version", version, "--json"],
    );
    let summary: Value = serde_json::from_slice(&upgraded.stdout)
        .unwrap_or_else(|_| panic!("{}", stderr(&upgraded)));
    assert!(upgraded.status.success(), "{summary}");
    assert_eq!(summary["outcome"], "switched", "{summary}");
    let shown = run_ocm(&cwd, &env, &["env", "show", "mira", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let environment: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert!(environment["defaultLauncher"].is_null(), "{environment}");
    assert!(environment["defaultRuntime"].is_string(), "{environment}");
    assert_preflight_observation(
        &fs::read_to_string(&observation).expect("preflight did not run"),
        &fs::canonicalize(&package).unwrap(),
        &environment,
    );
}

#[test]
fn upgrade_preflight_refusal_keeps_tracked_source_running() {
    assert_refused_preflights(false, false);
}

#[test]
fn upgrade_preflight_refuses_invalid_receipts_before_explicit_cutover() {
    assert_refused_preflights(false, true);
}

#[test]
fn upgrade_preflight_refusal_keeps_launcher_source_running() {
    assert_refused_preflights(true, true);
}

fn assert_refused_preflights(launcher: bool, explicit: bool) {
    let root = TestDir::new("upgrade-preflight-refusal");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let available = Arc::new(AtomicBool::new(true));
    let health_server = ControlledHealthServer::start(Arc::clone(&available));
    let source_version = "2026.8.13";
    let source_tarball =
        openclaw_package_tarball(&fixture_openclaw_script(source_version), source_version);
    let source_server = TestHttpServer::serve_bytes_times(
        "/source.tgz",
        "application/octet-stream",
        &source_tarball,
        8,
    );
    let source_release = serde_json::json!({
        "version": source_version,
        "dist": {"tarball": source_server.url(), "integrity": sha512_integrity(&source_tarball)}
    });
    let cases = if explicit && !launcher {
        vec![
            ("blocked", 3),
            ("unverified", 2),
            ("unverified-execution", 2),
            ("ready-nonzero", 3),
            ("version-mismatch", 0),
            ("coverage-mismatch", 0),
            ("protocol-mismatch", 0),
            ("invalid", 0),
            ("unsupported", 0),
            ("package-version-mismatch", 0),
        ]
    } else {
        vec![("blocked", 3)]
    };
    let mut versions = serde_json::Map::new();
    versions.insert(source_version.into(), source_release.clone());
    let mut candidates = Vec::new();
    let mut target_servers = Vec::new();
    for (index, (case, code)) in cases.iter().enumerate() {
        let version = format!("2026.8.{}", 14 + index);
        let protocol = if *case == "unsupported" { 2 } else { 1 };
        let package_version = if *case == "package-version-mismatch" {
            "2026.8.12"
        } else {
            &version
        };
        let bytes = preflight_package(package_version, serde_json::json!(protocol));
        let server = TestHttpServer::serve_bytes_times(
            &format!("/target-{index}.tgz"),
            "application/octet-stream",
            &bytes,
            8,
        );
        versions.insert(
            version.clone(),
            serde_json::json!({
                "version": version,
                "dist": {"tarball": server.url(), "integrity": sha512_integrity(&bytes)}
            }),
        );
        candidates.push((*case, *code, version));
        target_servers.push(server);
    }
    let initial = serde_json::json!({
        "dist-tags": {"latest": source_version},
        "versions": {source_version: source_release}
    });
    let updated = serde_json::json!({
        "dist-tags": {"latest": "2026.8.14"},
        "versions": versions
    });
    let mut responses = vec![initial.to_string().into_bytes()];
    responses.extend((0..64).map(|_| updated.to_string().into_bytes()));
    let packument_server =
        TestHttpServer::serve_bytes_sequence("/openclaw", "application/json", responses);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".into(),
        packument_server.url(),
    );
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
    install_fake_launchctl(&root, &mut env);
    env.insert(
        "OCM_TEST_NATIVE_DAEMON_PID".into(),
        std::process::id().to_string(),
    );
    let installed = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "stable", "--channel", "stable"],
    );
    assert!(installed.status.success(), "{}", stderr(&installed));
    let source = ocm::store::get_runtime("stable", &env, &cwd).unwrap();
    let source_binary = fs::canonicalize(&source.binary_path).unwrap();
    let source_root = source_binary.parent().unwrap();
    let (binding_kind, binding_name, selector) = if launcher {
        // A literal installed-package launcher has no development runner.
        support::write_executable_script(
            &root.child("fake-node-bin/node"),
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo v22.22.3; exit 0; fi\nscript=\"$1\"\nshift\nexec /bin/sh \"$script\" \"$@\"\n",
        );
        let added = run_ocm(
            &cwd,
            &env,
            &[
                "launcher",
                "add",
                "source",
                "--command",
                &format!("node '{}'", path_string(&source_binary)),
                "--cwd",
                &path_string(&cwd),
            ],
        );
        assert!(added.status.success(), "{}", stderr(&added));
        ("launcher", "source", "--launcher")
    } else {
        ("runtime", "stable", "--runtime")
    };
    let created = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "demo",
            selector,
            binding_name,
            "--port",
            &health_server.port.to_string(),
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let created_env = ocm::store::get_environment("demo", &env, &cwd).unwrap();
    let created_paths = ocm::store::derive_env_paths(&created_env.root);
    fs::write(
        &created_paths.config_path,
        br#"{"gateway":{"mode":"local","auth":{"mode":"token","token":"synthetic-token"}}}"#,
    )
    .unwrap();
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let environment: Value = serde_json::from_slice(&env_show.stdout).unwrap();
    let paths = ocm::store::derive_env_paths(environment["root"].as_str().unwrap());
    let config_before = fs::read(&paths.config_path).unwrap();
    fs::write(paths.workspace_dir.join("sentinel"), "source data").unwrap();
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let registry_before = fs::read(&registry_path).unwrap();
    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env["OCM_HOME"].clone();
    write_running_supervisor_runtime(
        &runtime_path,
        &ocm_home,
        binding_kind,
        binding_name,
        4242,
        health_server.port,
    );
    let observation = root.child("preflight-observation");
    let receipt_path = root.child("preflight-receipt.json");
    let finalizer_started = root.child("finalizer-started");
    let finalizer_release = root.child("finalizer-release");
    let completion_started = root.child("completion-started");
    let completion_input = root.child("completion-input");
    fs::write(&finalizer_release, "release").unwrap();
    for (key, path) in [
        ("OCM_TEST_PREFLIGHT_OBSERVATION", &observation),
        ("OCM_TEST_PREFLIGHT_RECEIPT", &receipt_path),
        ("OCM_TEST_UPDATE_FINALIZE_STARTED", &finalizer_started),
        ("OCM_TEST_UPDATE_FINALIZE_RELEASE", &finalizer_release),
        ("OCM_TEST_COMPLETION_STARTED", &completion_started),
        ("OCM_TEST_COMPLETION_INPUT", &completion_input),
    ] {
        env.insert(key.into(), path_string(path));
    }
    env.insert("OPENCLAW_UPDATE_POST_CORE".into(), "1".into());
    for key in [
        "OPENCLAW_UPDATE_UNKNOWN_AUTHORITY",
        "openclaw_update_lowercase_authority",
        "OPENCLAW_COMPATIBILITY_HOST_VERSION",
        "openclaw_gateway_service_pid",
        "LAUNCH_JOB_LABEL",
        "LAUNCH_JOB_NAME",
        "XPC_SERVICE_NAME",
        "INVOCATION_ID",
        "SYSTEMD_EXEC_PID",
        "JOURNAL_STREAM",
        "openclaw_supervisor_mode",
        "openclaw_service_marker",
        "openclaw_service_kind",
        "openclaw_launchd_label",
        "openclaw_systemd_unit",
        "openclaw_windows_task_name",
        "OPENCLAW_CONTROL_PLANE_UPDATE_SENTINEL_META",
    ] {
        env.insert(key.into(), "synthetic-authority".into());
    }
    let done = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let observer_done = Arc::clone(&done);
    let observer_stopped = Arc::clone(&stopped);
    let observer_available = Arc::clone(&available);
    let observer_runtime = runtime_path.clone();
    let observer_registry = registry_path.clone();
    let health_port = health_server.port;
    let observer = thread::spawn(move || {
        let mut was_running = true;
        while !observer_done.load(Ordering::Relaxed) {
            let running = env_desired_running(&observer_registry, was_running);
            if running != was_running {
                if running {
                    write_running_supervisor_runtime(
                        &observer_runtime,
                        &ocm_home,
                        binding_kind,
                        binding_name,
                        4242,
                        health_port,
                    );
                } else {
                    observer_stopped.store(true, Ordering::SeqCst);
                    write_empty_supervisor_runtime(&observer_runtime, &ocm_home);
                }
                observer_available.store(running, Ordering::SeqCst);
                was_running = running;
            }
            sleep(Duration::from_millis(1));
        }
    });
    let results = candidates
        .into_iter()
        .map(|(case, code, version)| {
            let mut receipt = preflight_receipt_json(
                if case.starts_with("unverified") {
                    "unverified"
                } else if case == "blocked" {
                    "blocked"
                } else {
                    "ready"
                },
                &version,
            );
            match case {
                "version-mismatch" => receipt["candidateVersion"] = serde_json::json!("2026.1.1"),
                "coverage-mismatch" => receipt["coverage"] = serde_json::json!("complete-startup"),
                "protocol-mismatch" => receipt["protocol"] = serde_json::json!(2),
                "unverified-execution" => {
                    receipt["candidateVersion"] = Value::Null;
                    receipt.as_object_mut().unwrap().remove("phase");
                }
                _ => {}
            }
            fs::write(
                &receipt_path,
                if case == "invalid" {
                    "not-json".into()
                } else {
                    receipt.to_string()
                },
            )
            .unwrap();
            env.insert("OCM_TEST_PREFLIGHT_EXIT".into(), code.to_string());
            let output = if explicit {
                run_ocm(
                    &cwd,
                    &env,
                    &["upgrade", "demo", "--version", &version, "--json"],
                )
            } else {
                run_ocm(&cwd, &env, &["upgrade", "demo", "--json"])
            };
            let invoked = observation.exists();
            let observed = fs::read_to_string(&observation).ok();
            if invoked {
                fs::remove_file(&observation).unwrap();
            }
            (case, output, invoked, observed)
        })
        .collect::<Vec<_>>();
    done.store(true, Ordering::Relaxed);
    observer.join().unwrap();
    let health_after = health_observation(health_server.port);
    health_server.stop();
    assert!(
        !stopped.load(Ordering::SeqCst),
        "source stopped before readiness was accepted"
    );
    assert_eq!(health_after, Some(true));
    assert_eq!(fs::read(&registry_path).unwrap(), registry_before);
    assert_eq!(fs::read(&paths.config_path).unwrap(), config_before);
    assert_eq!(
        fs::read_to_string(paths.workspace_dir.join("sentinel")).unwrap(),
        "source data"
    );
    assert!(
        !finalizer_started.exists(),
        "cold finalization ran after preflight refusal"
    );
    for (case, output, invoked, observed) in results {
        assert!(
            !output.status.success(),
            "{case}: unexpectedly accepted candidate"
        );
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                stderr(&output)
            )
        });
        assert_eq!(summary["outcome"], "failed", "{case}: {summary}");
        assert!(summary["snapshotId"].is_null(), "{case}: {summary}");
        assert_eq!(
            invoked,
            !matches!(case, "unsupported" | "package-version-mismatch"),
            "{case}: {summary}",
        );
        if case == "unverified-execution" {
            assert!(
                summary["note"]
                    .as_str()
                    .unwrap()
                    .contains("during execution")
            );
        }
        if let Some(observed) = observed {
            assert_preflight_observation(&observed, source_root, &environment);
        }
    }
}

#[test]
fn upgrade_batch_continue_keeps_gateway_refused_before_cutover() {
    let root = TestDir::new("upgrade-batch-continue-precutover");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let available = Arc::new(AtomicBool::new(true));
    let health_server = ControlledHealthServer::start(Arc::clone(&available));
    let mut env = ocm_env(&root);
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
    install_fake_launchctl(&root, &mut env);
    env.insert(
        "OCM_TEST_NATIVE_DAEMON_PID".into(),
        std::process::id().to_string(),
    );
    let receipt_path = root.child("preflight-receipt.json");
    fs::write(
        &receipt_path,
        preflight_receipt_json("blocked", "2026.8.14").to_string(),
    )
    .unwrap();
    for (key, value) in [
        ("OCM_TEST_PREFLIGHT_RECEIPT", path_string(&receipt_path)),
        (
            "OCM_TEST_PREFLIGHT_OBSERVATION",
            path_string(&root.child("preflight-observation")),
        ),
        ("OCM_TEST_PREFLIGHT_EXIT", "3".to_string()),
    ] {
        env.insert(key.into(), value);
    }

    // `refused` advertises the native preflight, which blocks startup before
    // cutover. `broken` is a legacy package whose cold finalization fails after
    // the source gateway was stopped, with a diagnostic long enough that the
    // batch note must be bounded after the stop line is appended.
    let finalize = "    if [ \"$2\" = \"finalize\" ]; then\n";
    let broken_script = fixture_openclaw_script("2026.8.15").replacen(
        finalize,
        &format!(
            "{finalize}      printf 'synthetic finalize failure\\n' >&2\n      i=0\n      while [ \"$i\" -lt 20 ]; do printf 'finalize detail %s\\n' \"$i\" >&2; i=$((i + 1)); done\n      printf '%s\\n' \"{}\" >&2\n      exit 23\n",
            "x".repeat(6_000)
        ),
        1,
    );
    assert_ne!(broken_script, fixture_openclaw_script("2026.8.15"));
    for (name, version, script, preflight) in [
        (
            "old",
            "2026.8.13",
            fixture_openclaw_script("2026.8.13"),
            false,
        ),
        (
            "refused",
            "2026.8.14",
            fixture_openclaw_script("2026.8.14"),
            true,
        ),
        ("broken", "2026.8.15", broken_script, false),
    ] {
        let package = root.child(format!("packages/{name}"));
        fs::create_dir_all(&package).unwrap();
        support::write_executable_script(&package.join("openclaw.mjs"), &script);
        let mut manifest = serde_json::json!({
            "name": "openclaw",
            "version": version,
            "bin": {"openclaw": "openclaw.mjs"}
        });
        if preflight {
            manifest["openclaw"] = serde_json::json!({"updatePreflightProtocol": 1});
        }
        fs::write(package.join("package.json"), manifest.to_string()).unwrap();
        let added = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &path_string(&package.join("openclaw.mjs")),
            ],
        );
        assert!(added.status.success(), "{}", stderr(&added));
    }

    // demo runs a supervised Gateway; peer has no installed service.
    let port = health_server.port.to_string();
    for args in [
        vec!["env", "create", "demo", "--runtime", "old", "--port", &port],
        vec!["env", "create", "peer", "--runtime", "old"],
    ] {
        let created = run_ocm(&cwd, &env, &args);
        assert!(created.status.success(), "{}", stderr(&created));
    }
    for name in ["demo", "peer"] {
        let meta = ocm::store::get_environment(name, &env, &cwd).unwrap();
        fs::write(
            ocm::store::derive_env_paths(&meta.root).config_path,
            br#"{"gateway":{"mode":"local","auth":{"mode":"token","token":"synthetic-token"}}}"#,
        )
        .unwrap();
    }
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env["OCM_HOME"].clone();
    write_running_supervisor_runtime(
        &runtime_path,
        &ocm_home,
        "runtime",
        "old",
        4242,
        health_server.port,
    );
    // Mirror the supervisor: the Gateway runs exactly while its service is desired.
    let done = Arc::new(AtomicBool::new(false));
    let observer_done = Arc::clone(&done);
    let observer_available = Arc::clone(&available);
    let observer_runtime = runtime_path.clone();
    let observer_registry = registry_path.clone();
    let health_port = health_server.port;
    let observer = thread::spawn(move || {
        let mut was_running = true;
        while !observer_done.load(Ordering::Relaxed) {
            let running = env_desired_running(&observer_registry, was_running);
            if running != was_running {
                if running {
                    write_running_supervisor_runtime(
                        &observer_runtime,
                        &ocm_home,
                        "runtime",
                        "old",
                        4242,
                        health_port,
                    );
                } else {
                    write_empty_supervisor_runtime(&observer_runtime, &ocm_home);
                }
                observer_available.store(running, Ordering::SeqCst);
                was_running = running;
            }
            sleep(Duration::from_millis(1));
        }
    });
    let batch = |runtime: &str, policy: &[&str]| {
        let mut args = vec![
            "upgrade",
            "batch",
            "--runtime",
            runtime,
            "--envs",
            "demo,peer",
        ];
        args.extend(policy);
        args.extend(["--accept-fleet-outage", "--json"]);
        let output = run_ocm(&cwd, &env, &args);
        let summary: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{runtime}: {}", stderr(&output)));
        assert!(!output.status.success(), "{summary}");
        summary
    };
    let result = |summary: &Value, name: &str| {
        summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["envName"] == name)
            .unwrap_or_else(|| panic!("{name} missing: {summary}"))
            .clone()
    };
    let desired_running = || {
        let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
        assert!(status.status.success(), "{}", stderr(&status));
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["desiredRunning"].clone()
    };

    // The default policy is continue; a refusal before cutover changes nothing.
    let refused = batch("refused", &[]);
    assert_eq!(refused["failurePolicy"], "continue", "{refused}");
    assert_eq!(desired_running(), true, "{refused}");
    assert_eq!(health_observation(health_server.port), Some(true));
    for name in ["demo", "peer"] {
        let item = result(&refused, name);
        assert_eq!(item["outcome"], "failed", "{item}");
        assert!(item["snapshotId"].is_null(), "{item}");
        assert!(item["rollback"].is_null(), "{item}");
        let note = item["note"].as_str().unwrap();
        assert!(note.contains("failed before cutover"), "{note}");
        assert!(
            note.contains("candidate update preflight blocked"),
            "{note}"
        );
        assert!(!note.contains("left the gateway stopped"), "{note}");
    }

    // A failure after cutover still leaves the Gateway stopped for recovery.
    let failed = batch("broken", &["--failure-policy", "continue"]);
    let demo = result(&failed, "demo");
    assert_eq!(demo["outcome"], "failed", "{demo}");
    assert_eq!(demo["rollback"], "disabled", "{demo}");
    assert!(demo["snapshotId"].is_string(), "{demo}");
    let note = demo["note"].as_str().unwrap();
    assert!(
        note.contains("upgrade failed and rollback was disabled"),
        "{note}"
    );
    assert!(
        note.contains("failure policy left the gateway stopped for external recovery"),
        "{note}"
    );
    assert!(note.lines().count() <= 12, "{note}");
    assert!(note.chars().count() <= 4_096, "{note}");
    let peer = result(&failed, "peer");
    assert_eq!(peer["rollback"], "disabled", "{peer}");
    let note = peer["note"].as_str().unwrap();
    assert!(!note.contains("left the gateway stopped"), "{note}");
    let registry: Value = serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    let peer_entry = registry["envs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "peer")
        .unwrap();
    assert_eq!(peer_entry["serviceEnabled"], false, "{peer_entry}");
    assert_eq!(desired_running(), false);
    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history.as_array().unwrap().len(), 1, "{history}");

    done.store(true, Ordering::Relaxed);
    observer.join().unwrap();
    health_server.stop();
}

#[test]
fn upgrade_prepares_target_before_cutover_and_bounds_stop_to_ready() {
    let root = TestDir::new("upgrade-availability-ordering");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let timeline = Timeline::default();
    let origin = Instant::now();

    let available = Arc::new(AtomicBool::new(true));
    let health_server = ControlledHealthServer::start(Arc::clone(&available));

    let old_tarball = openclaw_package_tarball(&fixture_openclaw_script("2026.8.13"), "2026.8.13");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.8.13.tgz",
        "application/octet-stream",
        &old_tarball,
        8,
    );
    let new_tarball = preflight_package("2026.8.14", serde_json::json!(1));
    let new_integrity = sha512_integrity(&new_tarball);
    let (new_url, delayed_server) = delayed_bytes_server(
        "/openclaw-2026.8.14.tgz",
        "application/octet-stream",
        new_tarball,
        timeline.clone(),
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.8.13\"}},\"versions\":{{\"2026.8.13\":{{\"version\":\"2026.8.13\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.8.13\":\"2026-08-13T00:00:00.000Z\"}}}}",
        old_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.8.14\"}},\"versions\":{{\"2026.8.13\":{{\"version\":\"2026.8.13\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.8.14\":{{\"version\":\"2026.8.14\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.8.13\":\"2026-08-13T00:00:00.000Z\",\"2026.8.14\":\"2026-08-14T00:00:00.000Z\"}}}}",
        old_server.url(),
        old_integrity,
        new_url,
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    env.insert(
        "OCM_TEST_NATIVE_DAEMON_PID".into(),
        std::process::id().to_string(),
    );

    let start = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--port", &health_server.port.to_string()],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_json: Value = serde_json::from_slice(&env_show.stdout).unwrap();
    let source_runtime = ocm::store::get_runtime("stable", &env, &cwd).unwrap();
    let source_runtime = fs::canonicalize(&source_runtime.binary_path).unwrap();
    let source_runtime = source_runtime.parent().unwrap().to_path_buf();
    let snapshot_fixture = PathBuf::from(env_json["root"].as_str().unwrap()).join("fanout");
    fs::create_dir_all(&snapshot_fixture).unwrap();
    for index in 0..2_000 {
        fs::write(
            snapshot_fixture.join(format!("entry-{index:05}")),
            b"snapshot fixture\n",
        )
        .unwrap();
    }

    let finalize_started = root.child("finalize-started");
    let finalize_release = root.child("finalize-release");
    env.insert(
        "OCM_TEST_UPDATE_FINALIZE_STARTED".to_string(),
        path_string(&finalize_started),
    );
    env.insert(
        "OCM_TEST_UPDATE_FINALIZE_RELEASE".to_string(),
        path_string(&finalize_release),
    );
    let completion_started = root.child("completion-started");
    let completion_input = root.child("completion-input");
    env.insert(
        "OCM_TEST_COMPLETION_STARTED".to_string(),
        path_string(&completion_started),
    );
    env.insert(
        "OCM_TEST_COMPLETION_INPUT".to_string(),
        path_string(&completion_input),
    );
    for (key, value) in [
        (
            "OCM_TEST_TARGET_CONFIG_VALIDATE_DELAY",
            TARGET_CONFIG_DELAY_SECONDS,
        ),
        (
            "OCM_TEST_CONFIG_SNAPSHOT_DELAY",
            CONFIG_SNAPSHOT_DELAY_SECONDS,
        ),
        ("OCM_TEST_DOCTOR_DELAY", DOCTOR_DELAY_SECONDS),
        ("OCM_TEST_PLUGIN_DELAY", PLUGIN_DELAY_SECONDS),
        (
            "OCM_TEST_TARGET_CONFIG_CONVERGENCE_DELAY",
            TARGET_CONVERGENCE_DELAY_SECONDS,
        ),
        ("OCM_TEST_COMPLETION_DELAY", COMPLETION_DELAY_SECONDS),
    ] {
        env.insert(key.to_string(), value.to_string());
    }

    let preflight_observation = root.child("preflight-observation");
    let preflight_receipt = root.child("preflight-receipt.json");
    fs::write(
        &preflight_receipt,
        preflight_receipt_json("ready", "2026.8.14").to_string(),
    )
    .unwrap();
    env.insert(
        "OCM_TEST_PREFLIGHT_OBSERVATION".into(),
        path_string(&preflight_observation),
    );
    env.insert(
        "OCM_TEST_PREFLIGHT_RECEIPT".into(),
        path_string(&preflight_receipt),
    );
    // Even inherited cold-finalizer authority must be removed from the probe.
    env.insert("OPENCLAW_UPDATE_POST_CORE".into(), "1".into());
    env.insert("OPENCLAW_UPDATE_SKIP_RESTART".into(), "1".into());
    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    let supervisor_lock_path = supervisor_state_path(&env, &cwd)
        .unwrap()
        .with_extension("lock");
    fs::create_dir_all(supervisor_lock_path.parent().unwrap()).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    write_running_supervisor_runtime(
        &runtime_path,
        &ocm_home,
        "runtime",
        "stable",
        4242,
        health_server.port,
    );

    let observer_done = Arc::new(AtomicBool::new(false));
    let observer_done_thread = Arc::clone(&observer_done);
    let observer_available = Arc::clone(&available);
    let observer_timeline = timeline.clone();
    let observer_runtime_path = runtime_path.clone();
    let observer_ocm_home = ocm_home.clone();
    let observer_completion_started = completion_started.clone();
    let observer_completion_input = completion_input.clone();
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let health_port = health_server.port;
    let observer = thread::spawn(move || {
        let mut last_running = true;
        while !observer_done_thread.load(Ordering::Relaxed) {
            let running = env_desired_running(&registry_path, last_running);
            if running != last_running {
                if running {
                    sleep(GATEWAY_STARTUP_DELAY);
                    fs::write(&observer_completion_input, b"committed\n").unwrap();
                    write_running_supervisor_runtime(
                        &observer_runtime_path,
                        &observer_ocm_home,
                        "runtime",
                        "stable",
                        4243,
                        health_port,
                    );
                    observer_available.store(true, Ordering::SeqCst);
                    observer_timeline.mark("target_ready");
                } else {
                    // Record the cutoff before health probes can observe the outage.
                    observer_timeline.mark("service_stopped");
                    observer_available.store(false, Ordering::SeqCst);
                    write_empty_supervisor_runtime(&observer_runtime_path, &observer_ocm_home);
                }
                last_running = running;
            }
            if observer_completion_started.exists() {
                observer_timeline.mark("completion_started");
            }
            sleep(Duration::from_millis(1));
        }
    });

    let samples_done = Arc::new(AtomicBool::new(false));
    let samples_done_thread = Arc::clone(&samples_done);
    let samples = Arc::new(Mutex::new(Vec::<(Instant, bool)>::new()));
    let samples_thread = Arc::clone(&samples);
    let health_poller = thread::spawn(move || {
        while !samples_done_thread.load(Ordering::Relaxed) {
            if let Some(ok) = health_observation(health_port) {
                samples_thread.lock().unwrap().push((Instant::now(), ok));
            }
            sleep(Duration::from_millis(20));
        }
    });

    let finalize_timeline = timeline.clone();
    let finalize_releaser = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(180);
        while !finalize_started.exists() {
            assert!(Instant::now() < deadline, "finalization never started");
            sleep(Duration::from_millis(5));
        }
        finalize_timeline.mark("finalization_started");
        let supervisor_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(supervisor_lock_path)
            .unwrap();
        supervisor_lock.lock_exclusive().unwrap();
        fs::write(finalize_release, b"release\n").unwrap();
        finalize_timeline.mark("finalization_released");
        sleep(FINALIZER_AND_PUBLICATION_DELAY);
        FileExt::unlock(&supervisor_lock).unwrap();
        finalize_timeline.mark("publication_released");
    });

    #[cfg(target_os = "macos")]
    let log_relative = ".openclaw-rosita-node/logs/node.error.log";
    #[cfg(target_os = "macos")]
    let log_writer = support::active_service_log::ActiveServiceLog::start(
        &Path::new(env_json["root"].as_str().unwrap()).join(log_relative),
    );
    timeline.mark("command_started");
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    timeline.mark("command_finished");
    #[cfg(target_os = "macos")]
    let final_log = log_writer.finish();

    finalize_releaser.join().unwrap();
    delayed_server.join().unwrap();
    observer_done.store(true, Ordering::Relaxed);
    observer.join().unwrap();
    samples_done.store(true, Ordering::Relaxed);
    health_poller.join().unwrap();
    health_server.stop();

    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    // The source path is passed before publication, even though the named runtime
    // now contains the candidate after the successful cutover.
    assert_preflight_observation(
        &fs::read_to_string(&preflight_observation).unwrap(),
        &source_runtime,
        &env_json,
    );
    let prepare_released = timeline.at("target_prepare_released");
    let service_stopped = timeline.at("service_stopped");
    let finalization_started = timeline.at("finalization_started");
    let finalization_released = timeline.at("finalization_released");
    let publication_released = timeline.at("publication_released");
    let target_ready = timeline.at("target_ready");
    let completion_started = timeline.at("completion_started");
    let report = timeline.report(origin);

    let command_started = timeline.at("command_started");
    let preparation_health = samples
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, _)| *at >= command_started && *at < service_stopped)
        .copied()
        .collect::<Vec<_>>();
    assert!(
        !preparation_health.is_empty(),
        "no preparation health samples; {report}"
    );
    let failed_preparation_health = preparation_health
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(at, _)| at.duration_since(origin).as_secs_f64())
        .collect::<Vec<_>>();
    assert!(
        failed_preparation_health.is_empty(),
        "source Gateway was unavailable during target preparation at {failed_preparation_health:?}; {report}"
    );
    assert!(
        prepare_released <= service_stopped,
        "target preparation crossed the cutover boundary; {report}"
    );
    assert!(
        service_stopped <= finalization_started,
        "finalization started before service quiescence; {report}"
    );
    assert!(
        finalization_released <= target_ready,
        "target became ready before finalization completed; {report}"
    );
    assert!(
        publication_released <= target_ready,
        "target became ready before supervisor publication was released; {report}"
    );
    assert!(
        target_ready <= completion_started,
        "completion cache refresh started before the target was ready; {report}"
    );
    let downtime = target_ready.duration_since(service_stopped);
    eprintln!(
        "availability proof: stop-to-ready={:.3}s {report}",
        downtime.as_secs_f64()
    );

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_slice(&history.stdout).unwrap();
    #[cfg(target_os = "macos")]
    {
        let snapshot_id = history_json[0]["snapshotId"].as_str().unwrap();
        let snapshot = run_ocm(
            &cwd,
            &env,
            &["env", "snapshot", "show", "demo", snapshot_id, "--json"],
        );
        assert!(snapshot.status.success(), "{}", stderr(&snapshot));
        let snapshot_json: Value = serde_json::from_slice(&snapshot.stdout).unwrap();
        let captured =
            fs::read(Path::new(snapshot_json["archivePath"].as_str().unwrap()).join(log_relative))
                .unwrap();
        assert!(!captured.is_empty());
        assert!(final_log.starts_with(&captured));
    }
    let phases = history_json[0]["phases"]
        .as_array()
        .expect("upgrade receipt must include structured phase timings");
    eprintln!("phase receipt: {}", serde_json::to_string(phases).unwrap());
    let phase = |owner: &str, name: &str, window: &str| {
        phases
            .iter()
            .find(|phase| {
                phase["owner"] == owner && phase["phase"] == name && phase["window"] == window
            })
            .unwrap_or_else(|| panic!("missing {owner}/{name}/{window} timing: {history_json}"))
    };
    for (owner, name, window) in [
        ("ocm", "preparation", "preparation"),
        ("ocm", "snapshotPreparation", "preparation"),
        ("ocm", "quiescenceSnapshot", "stopped"),
        ("ocm", "targetConfigValidation", "stopped"),
        ("openclaw", "targetConfigValidation", "stopped"),
        ("openclaw", "configSnapshot", "stopped"),
        ("openclaw", "doctor", "stopped"),
        ("openclaw", "plugins", "stopped"),
        ("openclaw", "targetConfigConvergence", "stopped"),
        ("openclaw", "completionCache", "stopped"),
        ("ocm", "bindingPublish", "stopped"),
        ("ocm", "supervisorConvergence", "stopped"),
        ("ocm", "processSpawn", "stopped"),
        ("ocm", "httpReady", "stopped"),
        ("ocm", "completionCache", "post-ready"),
        ("ocm", "deepVerification", "post-ready"),
    ] {
        let _ = phase(owner, name, window);
    }
    assert_eq!(
        phase("openclaw", "completionCache", "stopped")["outcome"],
        "deferred"
    );
    assert_eq!(
        phase("ocm", "completionCache", "post-ready")["outcome"],
        "completed"
    );
    for (name, expected_duration_ms) in [
        ("targetConfigValidation", 110),
        ("configSnapshot", 130),
        ("doctor", 170),
        ("plugins", 190),
        ("targetConfigConvergence", 230),
    ] {
        assert_eq!(
            phase("openclaw", name, "stopped")["durationMs"],
            expected_duration_ms,
            "controlled delay was attributed to the wrong phase"
        );
    }
    let phase_start = |owner: &str, name: &str, window: &str| {
        phase(owner, name, window)["startedOffsetMs"]
            .as_u64()
            .unwrap()
    };
    let phase_end = |owner: &str, name: &str, window: &str| {
        let timing = phase(owner, name, window);
        timing["startedOffsetMs"].as_u64().unwrap() + timing["durationMs"].as_u64().unwrap()
    };
    assert!(
        phase_end("ocm", "preparation", "preparation")
            <= phase_start("ocm", "snapshotPreparation", "preparation")
    );
    assert!(
        phase_end("ocm", "snapshotPreparation", "preparation")
            <= phase_start("ocm", "quiescenceSnapshot", "stopped")
    );
    assert!(
        phase_end("openclaw", "targetConfigConvergence", "stopped")
            <= phase_start("ocm", "bindingPublish", "stopped")
    );
    assert!(
        phase_end("ocm", "httpReady", "stopped")
            <= phase_start("ocm", "completionCache", "post-ready")
    );
    assert!(
        phase_end("ocm", "completionCache", "post-ready")
            <= phase_start("ocm", "deepVerification", "post-ready")
    );
}
