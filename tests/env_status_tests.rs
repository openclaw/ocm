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
    let _managed_node_server = install_fake_managed_node_archive(&root, &mut status_env, "24.15.0");

    let status = run_ocm(&cwd, &status_env, &["env", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(value["runtimeHealth"], "ok");
    assert_eq!(value["issue"], Value::Null);
}

#[test]
fn env_status_reports_the_config_derived_gateway_port_after_onboarding_writes_it() {
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
            "fallback",
            "--command",
            "printf launcher",
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "fallback"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let env_root = root.child("ocm-home/envs/demo");
    let config_path = env_root.join(".openclaw/openclaw.json");
    fs::write(&config_path, "{\"gateway\":{\"port\":18888}}").unwrap();

    let status = run_ocm(&cwd, &env, &["env", "status", "demo"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let output = stdout(&status);
    assert!(output.contains("gatewayPort: 18888"));
    assert!(output.contains("gatewayPortSource: config"));
}
