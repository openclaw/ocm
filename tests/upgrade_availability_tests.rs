mod support;

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, sleep};
use std::time::{Duration, Instant};

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use ocm::store::{env_registry_path, now_utc, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::Value;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_launchctl, install_fake_node_and_npm, ocm_env,
    path_string, run_ocm, stderr,
};

const PREPARE_DELAY: Duration = Duration::from_millis(1_500);
const FINALIZE_DELAY: Duration = Duration::from_millis(1_500);

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

fn append_tar_file(
    builder: &mut Builder<&mut GzEncoder<Vec<u8>>>,
    path: &str,
    body: &[u8],
    mode: u32,
) {
    let mut header = Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    builder.append_data(&mut header, path, body).unwrap();
}

fn openclaw_package_tarball(script_body: &str, version: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        append_tar_file(
            &mut builder,
            "package/openclaw.mjs",
            script_body.as_bytes(),
            0o755,
        );
        append_tar_file(
            &mut builder,
            "package/package.json",
            format!(
                "{{\"name\":\"openclaw\",\"version\":\"{version}\",\"bin\":{{\"openclaw\":\"openclaw.mjs\"}}}}"
            )
            .as_bytes(),
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn sha512_integrity(body: &[u8]) -> String {
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(body))
    )
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

fn health_ok(port: u32) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(500),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = Vec::new();
    stream.read_to_end(&mut response).is_ok() && response.starts_with(b"HTTP/1.1 200 OK")
}

fn write_running_supervisor_runtime(
    runtime_path: &Path,
    ocm_home: &str,
    binding_name: &str,
    pid: u32,
    child_port: u32,
) {
    let log_root = runtime_path.parent().unwrap();
    let stdout_path = path_string(&log_root.join("demo.stdout.log"));
    let stderr_path = path_string(&log_root.join("demo.stderr.log"));
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: ocm_home.to_string(),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: binding_name.to_string(),
            gateway_state: "running".to_string(),
            restart_handoff: Some("none".to_string()),
            restart_count: 0,
            child_port,
            pid: Some(pid),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
        }],
        children: vec![SupervisorRuntimeChild {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: binding_name.to_string(),
            pid,
            restart_count: 0,
            child_port,
            stdout_path,
            stderr_path,
        }],
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
}

fn write_empty_supervisor_runtime(runtime_path: &Path, ocm_home: &str) {
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: ocm_home.to_string(),
        updated_at: now_utc(),
        services: Vec::new(),
        children: Vec::new(),
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
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
    printf 'Config valid\n'
    exit 0
    ;;
  update)
    if [ "$2" = "finalize" ]; then
      : > "$OCM_TEST_UPDATE_FINALIZE_STARTED"
      while [ ! -e "$OCM_TEST_UPDATE_FINALIZE_RELEASE" ]; do
        sleep 0.01
      done
      printf '{{"status":"ok","mode":"finalize"}}\n'
      exit 0
    fi
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
    let new_tarball = openclaw_package_tarball(&fixture_openclaw_script("2026.8.14"), "2026.8.14");
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

    let start = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--port", &health_server.port.to_string()],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_json: Value = serde_json::from_slice(&env_show.stdout).unwrap();
    let snapshot_fixture = PathBuf::from(env_json["root"].as_str().unwrap()).join("fanout");
    fs::create_dir_all(&snapshot_fixture).unwrap();
    for index in 0..20_000 {
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

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    write_running_supervisor_runtime(&runtime_path, &ocm_home, "stable", 4242, health_server.port);

    let observer_done = Arc::new(AtomicBool::new(false));
    let observer_done_thread = Arc::clone(&observer_done);
    let observer_available = Arc::clone(&available);
    let observer_timeline = timeline.clone();
    let observer_runtime_path = runtime_path.clone();
    let observer_ocm_home = ocm_home.clone();
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let health_port = health_server.port;
    let observer = thread::spawn(move || {
        let mut last_running = true;
        while !observer_done_thread.load(Ordering::Relaxed) {
            let running = env_desired_running(&registry_path, last_running);
            if running != last_running {
                if running {
                    write_running_supervisor_runtime(
                        &observer_runtime_path,
                        &observer_ocm_home,
                        "stable",
                        4243,
                        health_port,
                    );
                    observer_available.store(true, Ordering::SeqCst);
                    observer_timeline.mark("target_ready");
                } else {
                    observer_available.store(false, Ordering::SeqCst);
                    write_empty_supervisor_runtime(&observer_runtime_path, &observer_ocm_home);
                    observer_timeline.mark("service_stopped");
                }
                last_running = running;
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
            let ok = health_ok(health_port);
            samples_thread.lock().unwrap().push((Instant::now(), ok));
            sleep(Duration::from_millis(20));
        }
    });

    let finalize_timeline = timeline.clone();
    let finalize_releaser = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !finalize_started.exists() {
            assert!(Instant::now() < deadline, "finalization never started");
            sleep(Duration::from_millis(5));
        }
        finalize_timeline.mark("finalization_started");
        sleep(FINALIZE_DELAY);
        fs::write(finalize_release, b"release\n").unwrap();
        finalize_timeline.mark("finalization_released");
    });

    timeline.mark("command_started");
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    timeline.mark("command_finished");

    finalize_releaser.join().unwrap();
    delayed_server.join().unwrap();
    observer_done.store(true, Ordering::Relaxed);
    observer.join().unwrap();
    samples_done.store(true, Ordering::Relaxed);
    health_poller.join().unwrap();
    health_server.stop();

    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let prepare_released = timeline.at("target_prepare_released");
    let service_stopped = timeline.at("service_stopped");
    let finalization_started = timeline.at("finalization_started");
    let finalization_released = timeline.at("finalization_released");
    let target_ready = timeline.at("target_ready");
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
    let downtime = target_ready.duration_since(service_stopped);
    eprintln!(
        "availability proof: stop-to-ready={:.3}s {report}",
        downtime.as_secs_f64()
    );
}
