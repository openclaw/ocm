use super::*;
use ocm::infra::shell::quote_posix;
use std::path::PathBuf;
use std::process::{Child, Stdio};

struct ReadinessFixture {
    root: TestDir,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    daemon: Option<Child>,
    sources: Vec<(PathBuf, Vec<u8>)>,
    _port: ReadinessPort,
}

impl ReadinessFixture {
    fn new(mode: &str, certificate: &str) -> Self {
        Self::with_native(mode, certificate, None)
    }

    fn with_native(mode: &str, certificate: &str, native: Option<&str>) -> Self {
        let root = TestDir::new("service-tls-readiness");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = systemd_env(&root);
        env.remove("OCM_INTERNAL_SKIP_SERVICE_READINESS");
        env.insert(
            "OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS".into(),
            "3000".into(),
        );
        let peer = root.child("peer.cjs");
        fs::write(&peer, include_str!("tls_readiness.cjs")).unwrap();
        let node = Command::new("node")
            .args(["-p", "process.execPath"])
            .output()
            .expect("service readiness fixtures require the declared Node toolchain");
        assert!(node.status.success(), "{}", stderr(&node));
        let node = String::from_utf8(node.stdout).unwrap().trim().to_string();
        let launcher = root.child("gateway-peer");
        write_executable_script(
            &launcher,
            &format!(
                "#!/bin/sh\nexec {} {} {} {}\n",
                quote_posix(&node),
                quote_posix(&path_string(&peer)),
                quote_posix(mode),
                quote_posix(&path_string(&root.child("requests.jsonl")))
            ),
        );
        let binding = if let Some(behavior) = native {
            let binary = root.child("package/node_modules/openclaw/openclaw.mjs");
            let script = format!(
                "#!/usr/bin/env node\nconst peer = {};\nconst mode = {};\nconst behavior = {};\nconst requests = {};\nconst queries = {};\n{}",
                json!(peer),
                json!(mode),
                json!(behavior),
                json!(root.child("requests.jsonl")),
                json!(root.child("queries.jsonl")),
                include_str!("tls_native_config.mjs"),
            );
            write_executable_script(&binary, &script);
            fs::write(
                binary.with_file_name("package.json"),
                r#"{"name":"openclaw","version":"0.0.0","engines":{"node":">=22"}}"#,
            )
            .unwrap();
            let added = run_ocm(
                &cwd,
                &env,
                &["runtime", "add", "peer", "--path", &path_string(&binary)],
            );
            assert!(added.status.success(), "{}", stderr(&added));
            if behavior != "registered" {
                // Synthetic installed-package metadata: no installation, download,
                // real OpenClaw execution or user runtime is involved.
                let path = ocm::store::runtime_meta_path("peer", &env, &cwd).unwrap();
                let mut runtime: ocm::runtime::RuntimeMeta =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                runtime.source_kind = ocm::runtime::RuntimeSourceKind::Installed;
                fs::write(path, serde_json::to_vec_pretty(&runtime).unwrap()).unwrap();
            }
            "--runtime"
        } else {
            let added = run_ocm(
                &cwd,
                &env,
                &[
                    "launcher",
                    "add",
                    "peer",
                    "--command",
                    &path_string(&launcher),
                ],
            );
            assert!(added.status.success(), "{}", stderr(&added));
            "--launcher"
        };
        let reserved_port = ReadinessPort::reserve();
        let port = reserved_port.0.to_string();
        let environment = root.child("environment");
        let created = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "create",
                "demo",
                binding,
                "peer",
                "--root",
                &path_string(&environment),
                "--port",
                &port,
            ],
        );
        assert!(created.status.success(), "{}", stderr(&created));

        let cert_output = Command::new(&node)
            .arg(&peer)
            .arg(if certificate == "wrong" {
                "--other-certificate"
            } else {
                "--certificate"
            })
            .output()
            .unwrap();
        assert!(cert_output.status.success(), "{}", stderr(&cert_output));
        let cert = if certificate == "default" {
            environment.join(".openclaw/gateway/tls/gateway-cert.pem")
        } else {
            environment.join("fixture-cert.pem")
        };
        fs::create_dir_all(cert.parent().unwrap()).unwrap();
        fs::write(&cert, cert_output.stdout).unwrap();
        let mut config = json!({
            "gateway": {"mode": "local", "bind": "loopback", "port": port.parse::<u16>().unwrap(),
                "auth": {"mode": "token", "token": "synthetic-readiness-token"},
                "tls": {"enabled": mode != "http"}}
        });
        // No configured private key exists: readiness requires the public leaf,
        // while the synthetic server keeps its own public test-only key.
        let mut paths = vec![cert.clone()];
        if native.is_some() {
            config["gateway"]["tls"]["certPath"] = json!("${READINESS_DOTENV_CERT}");
            let dotenv = environment.join(".openclaw/.env");
            fs::write(&dotenv, "# Native config owns dotenv parsing and interpolation.\nREADINESS_DOTENV_CERT=\"fixture-cert.pem\" # public fixture certificate\n").unwrap();
            paths.push(dotenv);
        } else if certificate == "include" {
            config["env"] = json!({"vars": {"READINESS_CERT": "fixture-cert.pem"}});
            config["gateway"]["tls"] = json!({"$include": "tls.json5"});
            let included = environment.join(".openclaw/tls.json5");
            fs::write(&included, "// Relative to the Gateway launch directory, not this file.\n{ enabled: true, certPath: '${READINESS_CERT}', }\n").unwrap();
            paths.push(included);
        } else if certificate != "default" {
            config["gateway"]["tls"]["certPath"] = json!(cert);
        }
        let source = environment.join(".openclaw/openclaw.json");
        fs::write(&source, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        paths.push(source);
        let sources = paths
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        let installed = run_ocm(&cwd, &env, &["service", "install", "demo", "--json"]);
        assert!(installed.status.success(), "{}", stderr(&installed));
        let mut fixture = Self {
            root,
            cwd,
            env,
            daemon: None,
            sources,
            _port: reserved_port,
        };
        let log = fs::File::create(fixture.root.child("daemon.log")).unwrap();
        fixture.daemon = Some(
            Command::new(support::ocm_test_binary_path())
                .args(["__daemon", "run"])
                .env_clear()
                .envs(&fixture.env)
                .current_dir(&fixture.cwd)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let pid = fixture.daemon.as_ref().unwrap().id();
        fixture
            .env
            .insert("OCM_TEST_NATIVE_DAEMON_PID".into(), pid.to_string());
        // Both CLI and daemon's manager observations must see the same publisher.
        fs::write(
            &fixture.env["OCM_TEST_NATIVE_DAEMON_PID_FILE"],
            pid.to_string(),
        )
        .unwrap();
        let runtime_path = supervisor_runtime_path(&fixture.env, &fixture.cwd).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(bytes) = fs::read(&runtime_path)
                && let Ok(runtime) = serde_json::from_slice::<Value>(&bytes)
                && runtime["gatewayAdmission"]["process"]["pid"] == pid
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "daemon admission timed out: {}",
                fs::read_to_string(fixture.root.child("daemon.log")).unwrap()
            );
            sleep(Duration::from_millis(20));
        }
        fixture
    }

    fn action(&self, args: &[&str], ready: bool) -> Value {
        let began = Instant::now();
        let output = run_ocm(&self.cwd, &self.env, args);
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("NATIVE_CONFIG_PRIVATE_SENTINEL")
        );
        assert!(!stderr(&output).contains("NATIVE_CONFIG_PRIVATE_SENTINEL"));
        assert_eq!(
            output.status.success(),
            ready,
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            stderr(&output)
        );
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["gatewayReady"], ready, "{summary}");
        assert_eq!(
            summary["running"], true,
            "readiness must observe a live supervised process: {summary}"
        );
        assert_eq!(summary["gatewayState"], "running", "{summary}");
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "readiness exceeded its bounded budget"
        );
        if !ready {
            assert!(
                summary["issue"]
                    .as_str()
                    .is_some_and(|issue| issue.contains("did not become ready")),
                "{summary}"
            );
        }
        for (path, bytes) in &self.sources {
            assert_eq!(
                fs::read(path).unwrap(),
                *bytes,
                "readiness changed {}",
                path.display()
            );
        }
        let status = run_ocm(
            &self.cwd,
            &self.env,
            &["service", "status", "demo", "--json"],
        );
        assert!(status.status.success(), "{}", stderr(&status));
        let status: Value = serde_json::from_slice(&status.stdout).unwrap();
        assert!(status["childPid"].as_u64().is_some(), "{status}");
        status
    }

    fn stop(&self) {
        let stopped = run_ocm(&self.cwd, &self.env, &["service", "stop", "demo", "--json"]);
        assert!(stopped.status.success(), "{}", stderr(&stopped));
        let summary: Value = serde_json::from_slice(&stopped.stdout).unwrap();
        assert_eq!(summary["running"], false, "{summary}");
    }
}

impl Drop for ReadinessFixture {
    fn drop(&mut self) {
        if let Some(daemon) = &mut self.daemon {
            // The supervisor owns and joins its process groups on SIGTERM, also
            // when an assertion above panics. Peers have a finite lifetime and
            // watch their parent as an additional fallback.
            if daemon.try_wait().ok().flatten().is_none() {
                unsafe {
                    libc::kill(daemon.id() as i32, libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(6);
                while Instant::now() < deadline {
                    if daemon.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    sleep(Duration::from_millis(20));
                }
                let _ = daemon.kill();
            }
            let _ = daemon.wait();
        }
    }
}

#[test]
fn service_readiness_probes_http_and_tls_across_start_and_forced_restart() {
    for (mode, certificate) in [
        ("http", "explicit"),
        ("tls", "explicit"),
        ("tls12", "default"),
    ] {
        let fixture = ReadinessFixture::new(mode, certificate);
        let started = fixture.action(&["service", "start", "demo", "--json"], true);
        let restarted = fixture.action(&["service", "restart", "demo", "--force", "--json"], true);
        assert_ne!(
            started["childPid"], restarted["childPid"],
            "restart must prove the replacement child"
        );
        let requests = fs::read_to_string(fixture.root.child("requests.jsonl")).unwrap();
        for summary in [&started, &restarted] {
            assert!(
                requests.lines().any(|line| {
                    let request: Value = serde_json::from_str(line).unwrap();
                    request["pid"] == summary["childPid"]
                        && request["path"] == "/health"
                        && request["tls"] == (mode != "http")
                }),
                "no matching health request: {summary}; {requests}"
            );
        }
        fixture.stop();
    }
}

#[test]
fn service_readiness_resolves_included_tls_and_config_env_relative_certificate() {
    let fixture = ReadinessFixture::new("tls", "include");
    fixture.action(&["service", "start", "demo", "--json"], true);
    fixture.stop();
}

#[test]
fn service_readiness_rejects_wrong_leaf_unhealthy_plaintext_silent_and_unreachable_peers() {
    for (mode, certificate) in [
        ("tls", "wrong"),
        ("unhealthy-tls", "explicit"),
        ("plaintext-only", "explicit"),
        ("silent-tls", "explicit"),
        ("unreachable", "explicit"),
    ] {
        let fixture = ReadinessFixture::new(mode, certificate);
        fixture.action(&["service", "start", "demo", "--json"], false);
        if matches!(mode, "tls" | "plaintext-only" | "silent-tls") {
            assert!(
                !fixture.root.child("requests.jsonl").exists(),
                "untrusted or plaintext peer received an HTTP request"
            );
        }
        fixture.stop();
    }
}

// Match the complete Gateway base..=base+110 family, outside ephemeral client
// ports. Keep process-local claims through restarts; bind probes alone cannot
// exclude another case that has temporarily stopped its listener.
static READINESS_PORTS: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

struct ReadinessPort(u16);

impl ReadinessPort {
    fn reserve() -> Self {
        #[cfg(target_os = "linux")]
        let raw = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
        #[cfg(target_os = "macos")]
        let raw = {
            let output = Command::new("/usr/sbin/sysctl")
                .args([
                    "-n",
                    "net.inet.ip.portrange.first",
                    "net.inet.ip.portrange.last",
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", stderr(&output));
            String::from_utf8(output.stdout).unwrap()
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let raw = "49152 65535".to_string();
        let endpoints: Vec<u16> = raw
            .split_whitespace()
            .map(|port| port.parse().unwrap())
            .collect();
        assert_eq!(endpoints.len(), 2);
        let first = endpoints[0].min(endpoints[1]);
        let last = endpoints[0].max(endpoints[1]);
        let mut claimed = READINESS_PORTS.lock().unwrap();
        for port in 24000_u16..=(u16::MAX - 110) {
            if port <= last && port + 110 >= first {
                continue;
            }
            if claimed.iter().any(|other| port.abs_diff(*other) <= 110) {
                continue;
            }
            let family: Result<Vec<_>, _> = (port..=port + 110)
                .map(|port| std::net::TcpListener::bind(("127.0.0.1", port)))
                .collect();
            if family.is_ok() {
                claimed.push(port);
                return Self(port);
            }
        }
        panic!("no available Gateway port family outside {first}..={last}");
    }
}

impl Drop for ReadinessPort {
    fn drop(&mut self) {
        READINESS_PORTS
            .lock()
            .unwrap()
            .retain(|port| *port != self.0);
    }
}

#[test]
fn service_readiness_native_dotenv_lookup_is_once_per_wait_and_keeps_output_private() {
    for (mode, behavior, ready) in [
        ("tls", "success", true),
        ("unhealthy-tls", "success", false),
        ("tls", "failure", false),
        ("tls", "timeout", false),
        ("tls", "registered", false),
    ] {
        let fixture = ReadinessFixture::with_native(mode, "explicit", Some(behavior));
        fixture.action(&["service", "start", "demo", "--json"], ready);
        let queries_path = fixture.root.child("queries.jsonl");
        if behavior == "registered" {
            assert!(
                !queries_path.exists(),
                "arbitrary registered runtime received a config command"
            );
        } else {
            let queries = fs::read_to_string(queries_path).unwrap();
            let queries: Vec<Value> = queries
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                queries.len(),
                1,
                "native config lookup repeated within readiness retries"
            );
            assert_eq!(
                queries[0]["args"],
                json!(["config", "get", "gateway.tls.certPath", "--json"])
            );
            let pid = queries[0]["pid"].as_i64().unwrap() as i32;
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "native config child was not settled"
            );
            if behavior == "failure" || behavior == "timeout" {
                assert!(
                    !fixture.root.child("requests.jsonl").exists(),
                    "failed native lookup was bypassed"
                );
            }
        }
        if mode == "unhealthy-tls" {
            let requests = fs::read_to_string(fixture.root.child("requests.jsonl")).unwrap();
            assert!(
                requests.lines().count() >= 2,
                "negative peer did not exercise readiness retries"
            );
        }
        fixture.stop();
    }
}
