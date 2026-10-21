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
        let native_script = |behavior: &str| {
            format!(
                "#!/usr/bin/env node\nconst peer = {};\nconst mode = {};\nconst behavior = {};\nconst requests = {};\nconst queries = {};\n{}",
                json!(peer),
                json!(mode),
                json!(behavior),
                json!(root.child("requests.jsonl")),
                json!(root.child("queries.jsonl")),
                include_str!("tls_native_config.mjs"),
            )
        };
        let binding = if let Some(behavior) = native {
            let binary = root.child("package/node_modules/openclaw/openclaw.mjs");
            write_executable_script(&binary, &native_script(behavior));
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
            let initial_certificate = if native == Some("retry-generation") {
                "missing-first.pem"
            } else {
                "fixture-cert.pem"
            };
            fs::write(&dotenv, format!("# Native config owns dotenv parsing and interpolation.\nREADINESS_DOTENV_CERT=\"{initial_certificate}\" # public fixture certificate\n")).unwrap();
            if native != Some("retry-generation") {
                paths.push(dotenv);
            }
        } else if certificate == "include" {
            config["env"] = json!({"vars": {"READINESS_CERT": "fixture-cert.pem"}});
            config["gateway"]["tls"] = json!({"$include": "tls.json5"});
            let included = environment.join(".openclaw/tls.json5");
            fs::write(&included, "// Relative to the Gateway launch directory, not this file.\n{ enabled: true, certPath: '${READINESS_CERT}', }\n").unwrap();
            paths.push(included);
        } else if certificate != "default" {
            config["gateway"]["tls"]["certPath"] = json!(cert);
        }
        if certificate == "shared-unrelated" {
            let shared = root.child("shared");
            fs::create_dir_all(&shared).unwrap();
            let agents = shared.join("agents.json5");
            fs::write(&agents, "{ defaults: {} }\n").unwrap();
            config["agents"] = json!({"$include": agents});
            // Native launchers can explicitly allow shared configuration roots.
            // Readiness needs only Gateway settings, not the unrelated graph.
            let original = fs::read_to_string(&launcher).unwrap();
            write_executable_script(
                &launcher,
                &original.replacen(
                    "#!/bin/sh\n",
                    &format!(
                        "#!/bin/sh\nexport OPENCLAW_INCLUDE_ROOTS={}\n",
                        quote_posix(&path_string(&shared))
                    ),
                    1,
                ),
            );
            paths.push(agents);
        }
        if certificate == "blocked-tls-include" {
            let shared = root.child("shared");
            fs::create_dir_all(&shared).unwrap();
            let included = shared.join("tls.json");
            fs::write(
                &included,
                serde_json::to_vec_pretty(&json!({"certPath": cert})).unwrap(),
            )
            .unwrap();
            // enabled=true is visible even though readiness cannot traverse the
            // selected external include. It must never classify this as HTTP.
            config["gateway"]["tls"] = json!({"enabled": true, "$include": included});
            paths.push(included);
        }
        if certificate == "blocked-root-after-tls" {
            let known = environment.join(".openclaw/known-config.json");
            fs::write(&known, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
            let shared = root.child("shared");
            fs::create_dir_all(&shared).unwrap();
            let outside = shared.join("shared.json");
            fs::write(&outside, b"{}\n").unwrap();
            // The later include prevents a complete answer, but the earlier
            // in-bound file already establishes observed TLS evidence. There is
            // no determining enabled sibling that could mask this uncertainty.
            config = json!({"$include": ["known-config.json", outside]});
            paths.extend([known, outside]);
        }
        if certificate == "native-http-override" {
            let known = environment.join(".openclaw/known-true.json");
            config["gateway"]["tls"]["enabled"] = json!(true);
            fs::write(&known, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
            let shared = root.child("shared");
            fs::create_dir_all(&shared).unwrap();
            let outside = shared.join("http.json");
            fs::write(&outside, br#"{"gateway":{"tls":{"enabled":false}}}"#).unwrap();
            config = json!({"$include": ["known-true.json", outside]});
            paths.extend([known, outside]);
            // Only the known-native reader receives this include authority from
            // dotenv. Its later HTTP override completes the initial decision.
            let dotenv = environment.join(".openclaw/.env");
            let contents = fs::read_to_string(&dotenv).unwrap();
            fs::write(
                &dotenv,
                format!("{contents}OPENCLAW_INCLUDE_ROOTS={}\n", json!(shared)),
            )
            .unwrap();
        }
        if matches!(certificate, "native-root" | "opaque-root") {
            let shared = root.child("shared");
            fs::create_dir_all(&shared).unwrap();
            let included = shared.join("gateway.json");
            fs::write(&included, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
            config = json!({"$include": included});
            paths.push(included);
            if certificate == "native-root" {
                let dotenv = environment.join(".openclaw/.env");
                let contents = fs::read_to_string(&dotenv).unwrap();
                fs::write(
                    &dotenv,
                    format!("{contents}OPENCLAW_INCLUDE_ROOTS={}\n", json!(shared)),
                )
                .unwrap();
            } else {
                let entrypoint = root.child("opaque-gateway.mjs");
                fs::write(&entrypoint, native_script("opaque")).unwrap();
                write_executable_script(
                    &launcher,
                    &format!(
                        "#!/bin/sh\nexport OPENCLAW_INCLUDE_ROOTS={}\nexec {} {} \"$@\"\n",
                        quote_posix(&path_string(&shared)),
                        quote_posix(&node),
                        quote_posix(&path_string(&entrypoint)),
                    ),
                );
            }
        }
        let source = environment.join(".openclaw/openclaw.json");
        fs::write(&source, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        if !matches!(mode, "tls-then-missing" | "tls-then-http-config") {
            paths.push(source);
        }
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
        self.assert_sources_unchanged();
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

    fn assert_sources_unchanged(&self) {
        for (path, bytes) in &self.sources {
            assert_eq!(
                fs::read(path).unwrap(),
                *bytes,
                "readiness changed {}",
                path.display()
            );
        }
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
    let mut fixture = ReadinessFixture::new("tls", "include");
    // A caller shell must not override the supervised Gateway's config.env.vars.
    fixture
        .env
        .insert("READINESS_CERT".into(), "caller-missing-cert.pem".into());
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
        ("tls", "invalid-settings", false),
        ("tls", "registered", false),
    ] {
        let mut fixture = ReadinessFixture::with_native(mode, "explicit", Some(behavior));
        if behavior == "success" {
            // The daemon was started without this caller-only value; its .env
            // remains the source used by the supervised Gateway and config getter.
            fixture.env.insert(
                "READINESS_DOTENV_CERT".into(),
                "caller-missing-cert.pem".into(),
            );
        }
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
                json!(["config", "get", "gateway", "--json"])
            );
            let pid = queries[0]["pid"].as_i64().unwrap() as i32;
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "native config child was not settled"
            );
            if matches!(behavior, "failure" | "timeout" | "invalid-settings") {
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

struct InterruptedLookup {
    cli: Child,
    fixture_root: PathBuf,
    queries: PathBuf,
    owned: Vec<(i32, String)>,
}

impl InterruptedLookup {
    fn start(fixture: &ReadinessFixture, args: &[&str], ignore_hup: bool) -> Self {
        use std::os::unix::process::CommandExt;

        let output = fs::File::create(fixture.root.child("interrupted.stdout")).unwrap();
        let error = fs::File::create(fixture.root.child("interrupted.stderr")).unwrap();
        let mut command = Command::new(support::ocm_test_binary_path());
        command
            .args(args)
            .env_clear()
            .envs(&fixture.env)
            // Signal cleanup must finish well before the ordinary timeout.
            .env("OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS", "15000")
            .current_dir(&fixture.cwd)
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(error);
        if ignore_hup {
            // Detached workers inherit this policy across exec. Set it only in
            // the fixture child, never in the test runner or supervisor.
            unsafe {
                command.pre_exec(|| {
                    if libc::signal(libc::SIGHUP, libc::SIG_IGN) == libc::SIG_ERR {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let cli = command.spawn().unwrap();
        Self {
            cli,
            fixture_root: fixture.root.path().to_path_buf(),
            queries: fixture.root.child("queries.jsonl"),
            owned: Vec::new(),
        }
    }

    fn observe_helper(&mut self) -> Option<(i32, i32)> {
        let queries = fs::read_to_string(&self.queries).ok()?;
        let query = queries
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|query| query["descendantPid"].as_i64().is_some())?;
        let helper = query["pid"].as_i64()? as i32;
        let descendant = query["descendantPid"].as_i64()? as i32;
        for pid in [helper, descendant] {
            if !self.owned.iter().any(|(owned, _)| *owned == pid)
                && let Some(identity) = lookup_process_identity(pid)
                && identity.contains(&path_string(&self.fixture_root))
            {
                self.owned.push((pid, identity));
            }
        }
        Some((helper, descendant))
    }
}

impl Drop for InterruptedLookup {
    fn drop(&mut self) {
        // Preserve ownership before stopping the caller, also on a setup panic.
        self.observe_helper();
        if self.cli.try_wait().ok().flatten().is_none() {
            let _ = self.cli.kill();
        }
        let _ = self.cli.wait();
        for (pid, identity) in &self.owned {
            // The unique fixture path and recorded birth/command protect against
            // PID reuse. Never signal an unrecognized process from a stale log.
            if lookup_process_identity(*pid).as_ref() == Some(identity) {
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .owned
            .iter()
            .any(|(pid, identity)| lookup_process_identity(*pid).as_ref() == Some(identity))
        {
            if Instant::now() >= deadline {
                if std::thread::panicking() {
                    eprintln!("fixture native lookup cleanup could not verify settlement");
                } else {
                    panic!("fixture native lookup cleanup could not verify settlement");
                }
                break;
            }
            sleep(Duration::from_millis(20));
        }
    }
}

fn lookup_process_identity(pid: i32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args([
            "-ww",
            "-o",
            "lstart=",
            "-o",
            "args=",
            "-p",
            &pid.to_string(),
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn lookup_process_live(pid: i32) -> bool {
    // Same zombie treatment as the supervisor shutdown fixtures: an unreaped
    // exited descendant cannot execute or retain capture pipes.
    let output = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    if output.status.success() {
        return !String::from_utf8_lossy(&output.stdout)
            .trim()
            .starts_with('Z');
    }
    assert!(
        output.status.code() == Some(1) && output.stdout.is_empty(),
        "cannot inspect fixture child"
    );
    false
}

#[test]
fn service_readiness_interruption_settles_native_helper_and_pipe_holding_descendant() {
    for (signal, restart, ignore_hup) in [
        (libc::SIGINT, false, false),
        (libc::SIGINT, true, false),
        (libc::SIGTERM, false, false),
        (libc::SIGTERM, true, false),
        (libc::SIGTERM, false, true),
    ] {
        let fixture = ReadinessFixture::with_native("tls", "explicit", Some("interrupt"));
        if restart {
            fixture.action(&["service", "start", "demo", "--json"], true);
        }
        fs::write(
            fixture.root.child("queries.jsonl.block"),
            b"block native lookup",
        )
        .unwrap();
        let args: &[&str] = if restart {
            &["service", "restart", "demo", "--force", "--json"]
        } else {
            &["service", "start", "demo", "--json"]
        };
        let mut lookup = InterruptedLookup::start(&fixture, args, ignore_hup);
        let deadline = Instant::now() + Duration::from_secs(8);
        let (helper, descendant) = loop {
            if let Some(pids) = lookup.observe_helper()
                && fixture
                    .root
                    .child("queries.jsonl.descendant-ready")
                    .exists()
            {
                break pids;
            }
            assert!(
                lookup.cli.try_wait().unwrap().is_none(),
                "CLI exited before blocked lookup"
            );
            assert!(Instant::now() < deadline, "native helper did not block");
            sleep(Duration::from_millis(20));
        };
        assert!(lookup_process_live(helper) && lookup_process_live(descendant));
        assert_eq!(
            lookup.owned.len(),
            2,
            "fixture did not retain both process identities"
        );
        if ignore_hup {
            assert_eq!(
                unsafe { libc::kill(lookup.cli.id() as i32, libc::SIGHUP) },
                0
            );
            let observe_until = Instant::now() + Duration::from_millis(300);
            while Instant::now() < observe_until {
                assert!(
                    lookup.cli.try_wait().unwrap().is_none(),
                    "capture replaced inherited ignored SIGHUP"
                );
                sleep(Duration::from_millis(20));
            }
            assert!(
                lookup_process_live(helper) && lookup_process_live(descendant),
                "ignored SIGHUP cancelled the native lookup"
            );
        }
        assert_eq!(unsafe { libc::kill(lookup.cli.id() as i32, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = lookup.cli.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "interrupted CLI did not exit promptly"
            );
            sleep(Duration::from_millis(20));
        };
        assert!(!status.success(), "interrupted readiness reported success");
        while lookup_process_live(helper) || lookup_process_live(descendant) {
            assert!(
                Instant::now() < deadline,
                "interrupted native lookup left a live process"
            );
            sleep(Duration::from_millis(20));
        }
        for stream in ["interrupted.stdout", "interrupted.stderr"] {
            assert!(
                !fs::read_to_string(fixture.root.child(stream))
                    .unwrap()
                    .contains("NATIVE_CONFIG_PRIVATE_SENTINEL"),
                "private lookup output leaked"
            );
        }
        fixture.assert_sources_unchanged();
        fixture.stop();
    }
}

#[test]
fn service_readiness_ignores_unrelated_shared_config_includes() {
    for mode in ["http", "tls"] {
        let fixture = ReadinessFixture::new(mode, "shared-unrelated");
        fixture.action(&["service", "start", "demo", "--json"], true);
        fixture.action(&["service", "restart", "demo", "--force", "--json"], true);
        fixture.stop();
    }
}

#[test]
fn service_readiness_native_shared_root_config_resolves_http_and_tls() {
    for mode in ["http", "tls"] {
        let fixture = ReadinessFixture::with_native(mode, "native-root", Some("success"));
        fixture.action(&["service", "start", "demo", "--json"], true);
        let queries = fs::read_to_string(fixture.root.child("queries.jsonl")).unwrap();
        let queries: Vec<Value> = queries
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(queries.len(), 1);
        assert_eq!(
            queries[0]["args"],
            json!(["config", "get", "gateway", "--json"])
        );
        let requests = fs::read_to_string(fixture.root.child("requests.jsonl")).unwrap();
        assert!(requests.lines().any(|line| {
            let request: Value = serde_json::from_str(line).unwrap();
            request["path"] == "/health" && request["tls"] == (mode == "tls")
        }));
        fixture.stop();
    }
}

#[test]
fn service_readiness_opaque_shared_root_contract() {
    for (mode, ready) in [("http", true), ("tls", false)] {
        let fixture = ReadinessFixture::new(mode, "opaque-root");
        fixture.action(&["service", "start", "demo", "--json"], ready);
        let listening: Value = serde_json::from_slice(
            &fs::read(fixture.root.child("requests.jsonl.listening")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            listening["tls"],
            mode == "tls",
            "peer did not establish its configured listener"
        );
        assert!(
            !fixture.root.child("queries.jsonl").exists(),
            "opaque launcher received a config command"
        );
        if mode == "tls" {
            assert!(
                !fixture.root.child("requests.jsonl").exists(),
                "hidden TLS was claimed ready through an HTTP request"
            );
        }
        fixture.stop();
    }
}

#[test]
fn service_readiness_visible_tls_does_not_downgrade_when_selected_include_is_inaccessible() {
    for certificate in ["blocked-tls-include", "blocked-root-after-tls"] {
        let fixture = ReadinessFixture::new("plaintext-only", certificate);
        fixture.action(&["service", "start", "demo", "--json"], false);
        assert!(
            fixture.root.child("requests.jsonl.listening").exists(),
            "plaintext peer never listened"
        );
        assert!(
            !fixture.root.child("requests.jsonl").exists(),
            "visible TLS was downgraded after include failure"
        );
        fixture.stop();
    }
}

#[test]
fn service_readiness_keeps_tls_after_config_disappears_or_changes_during_wait() {
    for mode in ["tls-then-missing", "tls-then-http-config"] {
        let fixture = ReadinessFixture::new(mode, "explicit");
        let config = fixture.root.child("environment/.openclaw/openclaw.json");
        let mut expected: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        expected["gateway"]["tls"]["enabled"] = json!(false);
        fixture.action(&["service", "start", "demo", "--json"], false);
        assert_eq!(
            fs::read_to_string(fixture.root.child("requests.jsonl.switched")).unwrap(),
            mode
        );
        // The peer switches the same listening port to HTTP200 and mutates its
        // fixture config before delivering the first authenticated TLS503. This
        // makes the next poll a deterministic opportunity for a bad downgrade.
        if mode == "tls-then-missing" {
            assert!(
                !config.exists(),
                "fixture config disappearance was not retained"
            );
        } else {
            let actual: Value = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
            assert_eq!(
                actual, expected,
                "only the fixture's intentional transport setting should change"
            );
        }
        let requests = fs::read_to_string(fixture.root.child("requests.jsonl")).unwrap();
        let requests: Vec<Value> = requests
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests.len(),
            1,
            "readiness sent an HTTP request after choosing TLS"
        );
        assert_eq!(requests[0]["path"], "/health");
        assert_eq!(requests[0]["tls"], true);
        fixture.stop();
    }
}

#[test]
fn service_readiness_requeries_native_config_for_scheduled_replacement_child() {
    // The first child exits only after its native result; the supervisor's
    // one-second backoff places the replacement beyond the short wait deadline.
    // Retain the ordinary in-budget replacement as a control.
    for timeout_ms in [6000, 1000] {
        assert_native_config_for_scheduled_replacement_child(timeout_ms);
    }
}

fn assert_native_config_for_scheduled_replacement_child(timeout_ms: u64) {
    let mut fixture = ReadinessFixture::with_native("tls", "explicit", Some("retry-generation"));
    fixture.env.insert(
        "OCM_INTERNAL_GATEWAY_READINESS_TIMEOUT_MS".into(),
        timeout_ms.to_string(),
    );
    let dotenv = fixture.root.child("environment/.openclaw/.env");
    let before = fs::read_to_string(&dotenv).unwrap();
    assert!(before.contains("missing-first.pem"));
    let began = Instant::now();
    let status = fixture.action(&["service", "start", "demo", "--json"], true);
    if timeout_ms == 1000 {
        assert!(
            began.elapsed() >= Duration::from_millis(timeout_ms),
            "replacement did not exercise the late-observation boundary"
        );
    }
    let read_records = |name| -> Vec<Value> {
        fs::read_to_string(fixture.root.child(name))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    let generations = read_records("queries.jsonl.generations");
    assert_eq!(
        generations.len(),
        2,
        "expected exactly one scheduled Gateway replacement"
    );
    let first = generations[0]["pid"].as_i64().unwrap() as i32;
    let second = generations[1]["pid"].as_i64().unwrap() as i32;
    assert_ne!(first, second);
    assert_eq!(status["childPid"], second);
    assert_eq!(status["childRestartCount"], 1);
    assert!(
        !lookup_process_live(first),
        "first Gateway generation remained live"
    );
    assert!(lookup_process_live(second));

    let queries = read_records("queries.jsonl");
    let results = read_records("queries.jsonl.results");
    assert_eq!(
        queries.len(),
        2,
        "native query cache outlived its supervised child"
    );
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["certificate"], "missing-first.pem");
    assert_eq!(results[1]["certificate"], "fixture-cert.pem");
    for (query, result) in queries.iter().zip(&results) {
        assert_eq!(query["args"], json!(["config", "get", "gateway", "--json"]));
        assert_eq!(query["pid"], result["pid"]);
        assert!(
            !lookup_process_live(query["pid"].as_i64().unwrap() as i32),
            "native helper remained live"
        );
    }
    let requests = read_records("requests.jsonl");
    assert!(!requests.is_empty());
    assert!(
        requests.iter().all(|request| request["tls"] == true
            && request["path"] == "/health"
            && request["pid"] == second),
        "readiness accepted a stale child or sent plaintext HTTP"
    );
    assert_eq!(
        fs::read_to_string(dotenv).unwrap(),
        before.replace("missing-first.pem", "fixture-cert.pem")
    );
    fixture.assert_sources_unchanged();
    fixture.stop();
    assert!(
        !lookup_process_live(second),
        "replacement Gateway remained live after stop"
    );
}

#[test]
fn service_readiness_native_http_override_completes_initial_partial_tls_decision() {
    let fixture = ReadinessFixture::with_native("http", "native-http-override", Some("success"));
    fixture.action(&["service", "start", "demo", "--json"], true);
    let queries = fs::read_to_string(fixture.root.child("queries.jsonl")).unwrap();
    let queries: Vec<Value> = queries
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        queries.len(),
        1,
        "initial partial TLS evidence must use one authoritative lookup"
    );
    assert_eq!(
        queries[0]["args"],
        json!(["config", "get", "gateway", "--json"])
    );
    let requests = fs::read_to_string(fixture.root.child("requests.jsonl")).unwrap();
    let requests: Vec<Value> = requests
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request["path"] == "/health" && request["tls"] == false),
        "initial native HTTP decision did not reach the configured listener"
    );
    fixture.stop();
}
