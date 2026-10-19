mod support;

use std::fs;
use std::net::TcpListener;

use serde_json::Value;

use crate::support::{
    TestDir, TestHttpServer, install_fake_managed_node_archive, install_fake_node_and_npm,
    install_fake_service_manager, ocm_env, openclaw_package_tarball, run_ocm, sha512_integrity,
    stderr, stdout, write_executable_script,
};

fn allocate_free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn env_status_reports_the_resolved_launcher() {
    let root = TestDir::new("env-status-launcher");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let port = allocate_free_port().to_string();

    let add = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "fallback",
            "--command",
            "printf launcher",
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "demo",
            "--launcher",
            "fallback",
            "--port",
            &port,
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let status = run_ocm(&cwd, &env, &["env", "status", "demo"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let output = stdout(&status);
    assert!(output.contains(&format!("gatewayPort: {port}")));
    assert!(output.contains("gatewayPortSource: metadata"));
    assert!(output.contains("resolvedKind: launcher"));
    assert!(output.contains("resolvedName: fallback"));
    assert!(output.contains("command: printf launcher"));
    assert!(output.contains("managedServiceState: absent"));
    assert!(output.contains("openclawState: stopped"));
}

#[cfg(unix)]
#[test]
fn env_status_preserves_retained_ownership_and_prioritizes_inspection() {
    use ocm::store::{get_environment, save_environment, supervisor_runtime_path};
    use serde_json::json;

    let root = TestDir::new("env-status-retained-ownership");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let port = allocate_free_port().to_string();
    for args in [
        vec!["launcher", "add", "stable", "--command", "printf launcher"],
        vec![
            "env",
            "create",
            "demo",
            "--launcher",
            "stable",
            "--port",
            &port,
        ],
    ] {
        let output = run_ocm(&cwd, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let path = supervisor_runtime_path(&env, &cwd).unwrap();
    let mut publisher = serde_json::to_value(support::enable_fake_daemon_gateway_admission(
        &root, &mut env,
    ))
    .unwrap();
    publisher["process"]["startedAt"] = json!("older-publisher-generation");
    env.remove("OCM_TEST_NATIVE_DAEMON_PID");
    let row = json!({
        "envName": "demo", "bindingKind": "launcher", "bindingName": "stable",
        "gatewayState": "pending", "restartCount": 0, "childPort": port.parse::<u32>().unwrap(),
        "pid": null, "stdoutPath": "fixture.stdout", "stderrPath": "fixture.stderr",
        "lastExitCode": null, "lastError": null, "lastEventAt": null, "nextRetryAt": null
    });
    let mut runtime = json!({
        "kind": "ocm-supervisor-runtime", "ocmHome": env["OCM_HOME"],
        "daemonVersion": env!("CARGO_PKG_VERSION"), "gatewayAdmission": publisher,
        "updatedAt": "2026-01-01T00:00:00Z", "services": [row], "children": [],
        "retainedOwnership": [{"envName": "demo", "claimId": "retained-status-fixture", "publisher": publisher}]
    });
    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    for bound in [true, false] {
        for desired in [false, true] {
            meta.default_launcher = bound.then(|| "stable".to_string());
            meta.service_enabled = desired;
            meta.service_running = desired;
            save_environment(meta.clone(), &env, &cwd).unwrap();
            support::write_json_replacing_path(&path, &runtime);
            let before = fs::read(&path).unwrap();
            let service = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
            assert!(service.status.success(), "{}", stderr(&service));
            let service: Value = serde_json::from_slice(&service.stdout).unwrap();
            assert_eq!(service["gatewayState"], "unknown");
            let status = run_ocm(&cwd, &env, &["env", "status", "demo", "--json"]);
            assert!(status.status.success(), "{}", stderr(&status));
            let status: Value = serde_json::from_slice(&status.stdout).unwrap();
            assert_eq!(status["openclawState"], service["gatewayState"]);
            assert_eq!(status["serviceIssue"], service["issue"]);
            for flag in ["--raw", "--color=always"] {
                let output = run_ocm(&cwd, &env, &["env", "status", "demo", flag]);
                assert!(output.status.success(), "{}", stderr(&output));
                let text = stdout(&output);
                assert!(text.contains("unknown"), "{text}");
                assert!(text.contains("ocm service recover demo"), "{text}");
                for conflicting in [
                    "ocm start demo",
                    "ocm service start demo",
                    "ocm service install demo",
                    "ocm service restart demo",
                    "ocm @demo",
                ] {
                    assert!(!text.contains(conflicting), "{text}");
                }
                if flag == "--color=always" {
                    assert!(text.contains("Inspect ownership"), "{text}");
                }
            }
            assert_eq!(fs::read(&path).unwrap(), before);
            let after = get_environment("demo", &env, &cwd).unwrap();
            assert_eq!(after.service_running, desired);
            assert_eq!(after.service_enabled, desired);
        }
    }
    // A sibling claim must not make this target unknown; ordinary intent still projects as before.
    runtime["services"][0]["envName"] = json!("sibling");
    runtime["retainedOwnership"][0]["envName"] = json!("sibling");
    support::write_json_replacing_path(&path, &runtime);
    for (desired, expected) in [(false, "stopped"), (true, "pending")] {
        meta.default_launcher = Some("stable".to_string());
        meta.service_enabled = desired;
        meta.service_running = desired;
        save_environment(meta.clone(), &env, &cwd).unwrap();
        let status = run_ocm(&cwd, &env, &["env", "status", "demo", "--json"]);
        assert!(status.status.success(), "{}", stderr(&status));
        let status: Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["openclawState"], expected);
    }
}

#[test]
fn env_status_reports_a_broken_runtime_without_failing() {
    let root = TestDir::new("env-status-broken-runtime");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    fs::remove_file(&runtime_path).unwrap();

    let status = run_ocm(&cwd, &env, &["env", "status", "demo"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let output = stdout(&status);
    assert!(output.contains("resolvedKind: runtime"));
    assert!(output.contains("resolvedName: stable"));
    assert!(output.contains("runtimeHealth: broken"));
    assert!(output.contains("issue: runtime \"stable\" binary path does not exist:"));
}

#[test]
fn env_status_reports_when_an_environment_has_no_binding() {
    let root = TestDir::new("env-status-unbound");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let port = allocate_free_port().to_string();

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--port", &port]);
    assert!(create.status.success(), "{}", stderr(&create));

    let status = run_ocm(&cwd, &env, &["env", "status", "demo"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let output = stdout(&status);
    assert!(output.contains("envName: demo"));
    assert!(output.contains(&format!("gatewayPort: {port}")));
    assert!(output.contains("gatewayPortSource: metadata"));
    assert!(output.contains("managedServiceState: absent"));
    assert!(output.contains("openclawState: stopped"));
    assert!(
        output.contains(
            "issue: environment \"demo\" has no default runtime, launcher, or dev binding"
        )
    );
}

#[test]
fn env_status_json_reports_runtime_health_and_binding_shape() {
    let root = TestDir::new("env-status-json-runtime");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let port = allocate_free_port().to_string();

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "demo",
            "--runtime",
            "stable",
            "--port",
            &port,
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let status = run_ocm(&cwd, &env, &["env", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(value["envName"], "demo");
    assert_eq!(value["gatewayPort"], port.parse::<u64>().unwrap());
    assert_eq!(value["gatewayPortSource"], "metadata");
    assert_eq!(value["resolvedKind"], "runtime");
    assert_eq!(value["resolvedName"], "stable");
    assert_eq!(value["runtimeHealth"], "ok");
    assert_eq!(value["runtimeSourceKind"], "registered");
    assert_eq!(value["managedServiceState"], "absent");
    assert_eq!(value["openclawState"], "stopped");
    assert!(value["issue"].is_null());
}

#[test]
fn env_status_reports_release_backed_runtime_details() {
    let root = TestDir::new("env-status-release-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball =
        openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let manifest_body = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let manifest_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", manifest_body.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        manifest_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let status = run_ocm(&cwd, &env, &["env", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(value["resolvedKind"], "runtime");
    assert_eq!(value["runtimeSourceKind"], "installed");
    assert_eq!(value["runtimeReleaseVersion"], "2026.3.24");
    assert_eq!(value["runtimeReleaseChannel"], "stable");
    assert_eq!(value["runtimeHealth"], "ok");
    assert!(value["gatewayPort"].as_u64().unwrap() >= 18_789);
    assert_eq!(value["gatewayPortSource"], "computed");
}

#[test]
fn env_status_keeps_official_runtime_healthy_when_managed_fallback_is_available() {
    let root = TestDir::new("env-status-official-runtime-host-issue");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball =
        openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let empty_path = root.child("empty-bin");
    fs::create_dir_all(&empty_path).unwrap();
    let mut status_env = env.clone();
    status_env.insert("PATH".to_string(), empty_path.to_string_lossy().to_string());
    status_env.insert(
        "OCM_INTERNAL_NPM_BIN".to_string(),
        root.child("fake-node-bin/npm")
            .to_string_lossy()
            .to_string(),
    );
    let _managed_node_server = install_fake_managed_node_archive(&root, &mut status_env, "24.21.0");

    let status = run_ocm(&cwd, &status_env, &["env", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(value["runtimeHealth"], "ok");
    assert_eq!(value["issue"], Value::Null);
}

#[test]
fn env_status_and_execution_resolve_the_effective_config_gateway_port() {
    let root = TestDir::new("env-status-config-port");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "inspect",
            "--command",
            "printf '%s' \"$OPENCLAW_GATEWAY_PORT\"",
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "inspect", "--json"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let created: Value = serde_json::from_str(&stdout(&create)).unwrap();
    let automatic_port = created["gatewayPort"].as_u64().unwrap();
    let configured_port = if automatic_port <= 64_535 {
        automatic_port + 1000
    } else {
        automatic_port - 1000
    };

    let state_dir = root.child("ocm-home/envs/demo/.openclaw");
    let config_path = state_dir.join("openclaw.json");
    let strict = format!(r#"{{"gateway":{{"port":{configured_port}}}}}"#);
    fs::write(state_dir.join("gateway.json5"), &strict).unwrap();
    fs::write(
        state_dir.join("port.json5"),
        format!("{{port: {configured_port}}}"),
    )
    .unwrap();
    fs::write(state_dir.join("first.json5"), "{gateway: {port: 1}}").unwrap();
    fs::write(state_dir.parent().unwrap().join("outside.json5"), &strict).unwrap();

    let assert_port = |name: &str, port: u64, source: &str| {
        let status = run_ocm(&cwd, &env, &["env", "status", name, "--json"]);
        assert!(status.status.success(), "{}", stderr(&status));
        let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
        assert_eq!(value["gatewayPort"], port);
        assert_eq!(value["gatewayPortSource"], source);

        let show = run_ocm(&cwd, &env, &["env", "show", name, "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        let value: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_eq!(value["gatewayPort"], port);

        #[cfg(unix)]
        for args in [
            vec!["env", "run", name, "--"],
            vec![
                "env",
                "exec",
                name,
                "--",
                "sh",
                "-c",
                "printf '%s' \"$OPENCLAW_GATEWAY_PORT\"",
            ],
        ] {
            let output = run_ocm(&cwd, &env, &args);
            assert!(output.status.success(), "{}", stderr(&output));
            assert_eq!(stdout(&output), port.to_string());
        }
    };

    for content in [
        strict.clone(),
        format!("{{ // JSON5 comment\n gateway: {{port: {configured_port},}},}}"),
        r#"{"$include":"./gateway.json5"}"#.to_string(),
        r#"{gateway: {$include: './port.json5'}}"#.to_string(),
        r#"{$include: ['./first.json5', './gateway.json5']}"#.to_string(),
        format!("{{$include: './first.json5', gateway: {{port: {configured_port}}}}}"),
    ] {
        fs::write(&config_path, &content).unwrap();
        let doctor = run_ocm(&cwd, &env, &["env", "doctor", "demo", "--json"]);
        assert!(doctor.status.success(), "{}", stderr(&doctor));
        let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
        assert_eq!(value["configStatus"], "ok", "{content}");
        assert_eq!(value["healthy"], true, "{content}");
        assert_port("demo", configured_port, "config");
        assert_eq!(fs::read_to_string(&config_path).unwrap(), content);
    }

    for content in [
        "{gateway: [",
        "{gateway: {port: 65536}}",
        "{gateway: {port: '18888'}}",
        "{$include: 42}",
        "{$include: './missing.json5'}",
        "{$include: '../outside.json5'}",
    ] {
        fs::write(&config_path, content).unwrap();
        assert_port("demo", automatic_port, "computed");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside.json5", state_dir.join("linked.json5")).unwrap();
        fs::write(&config_path, "{$include: './linked.json5'}").unwrap();
        assert_port("demo", automatic_port, "computed");
    }
    fs::remove_file(&config_path).unwrap();
    assert_port("demo", automatic_port, "computed");

    let explicit_port = if configured_port == 30_000 {
        31_000
    } else {
        30_000
    };
    let explicit_port_arg = explicit_port.to_string();
    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "explicit",
            "--launcher",
            "inspect",
            "--port",
            &explicit_port_arg,
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let explicit_state = root.child("ocm-home/envs/explicit/.openclaw");
    fs::write(explicit_state.join("gateway.json5"), strict).unwrap();
    fs::write(
        explicit_state.join("openclaw.json"),
        "{$include: './gateway.json5'}",
    )
    .unwrap();
    assert_port("explicit", explicit_port, "metadata");
}
