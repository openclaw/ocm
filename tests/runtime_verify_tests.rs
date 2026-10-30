mod support;

use std::fs;

use ocm::infra::download::file_sha256;
use serde_json::Value;

use crate::support::{
    TestDir, TestHttpServer, install_fake_node_and_npm, ocm_env, openclaw_package_tarball,
    path_string, run_ocm, sha512_integrity, stderr, stdout, write_executable_script,
};

#[test]
fn runtime_verify_reports_a_healthy_runtime() {
    let root = TestDir::new("runtime-verify-healthy");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "stable"]);
    assert!(verify.status.success(), "{}", stderr(&verify));
    let output = stdout(&verify);
    assert!(output.contains("name: stable"));
    assert!(output.contains("healthy: true"));
    assert!(output.contains("sourceKind: registered"));
}

#[test]
fn runtime_verify_uses_exit_code_one_for_broken_runtimes() {
    let root = TestDir::new("runtime-verify-broken");
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
    fs::remove_file(&runtime_path).unwrap();

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "stable", "--json"]);
    assert_eq!(verify.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&verify)).unwrap();
    assert_eq!(value["name"], "stable");
    assert_eq!(value["healthy"], false);
    assert!(
        value["issue"]
            .as_str()
            .unwrap()
            .contains("binary path does not exist:")
    );
}

#[cfg(unix)]
#[test]
fn runtime_verify_reports_a_binary_the_current_user_cannot_execute() {
    use std::os::unix::fs::PermissionsExt;

    let root = TestDir::new("runtime-verify-not-executable");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let source_path = bin_dir.join("plain");
    write_executable_script(&source_path, "#!/bin/sh\nexit 0\n");
    fs::set_permissions(&source_path, fs::Permissions::from_mode(0o644)).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "copied", "--path", "./bin/plain"],
    );
    assert!(install.status.success(), "{}", stderr(&install));
    fs::set_permissions(&runtime_path, fs::Permissions::from_mode(0o644)).unwrap();

    for name in ["stable", "copied"] {
        let verify = run_ocm(&cwd, &env, &["runtime", "verify", name, "--json"]);
        assert_eq!(verify.status.code(), Some(1), "{}", stdout(&verify));
        let value: Value = serde_json::from_str(&stdout(&verify)).unwrap();
        assert_eq!(value["healthy"], false);
        assert!(
            value["issue"]
                .as_str()
                .unwrap()
                .contains("binary path is not executable by the current user:"),
            "{value}"
        );

        // Inspection is unchanged; only health reports the permission problem.
        let show = run_ocm(&cwd, &env, &["runtime", "show", name]);
        assert!(show.status.success(), "{}", stderr(&show));
    }

    fs::set_permissions(&runtime_path, fs::Permissions::from_mode(0o755)).unwrap();
    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "stable"]);
    assert!(verify.status.success(), "{}", stderr(&verify));
    assert!(stdout(&verify).contains("healthy: true"));
}

#[test]
fn runtime_verify_all_reports_mixed_runtime_health() {
    let root = TestDir::new("runtime-verify-all");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let stable_path = bin_dir.join("stable");
    let broken_path = bin_dir.join("broken");
    write_executable_script(&stable_path, "#!/bin/sh\nexit 0\n");
    write_executable_script(&broken_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add_stable = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_stable.status.success(), "{}", stderr(&add_stable));
    let add_broken = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "broken", "--path", "./bin/broken"],
    );
    assert!(add_broken.status.success(), "{}", stderr(&add_broken));
    fs::remove_file(&broken_path).unwrap();

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "--all"]);
    assert_eq!(verify.status.code(), Some(1));
    let output = stdout(&verify);
    assert!(output.contains("stable"));
    assert!(output.contains("healthy=true"));
    assert!(output.contains("broken"));
    assert!(output.contains("healthy=false"));
    assert!(output.contains("issue=binary path does not exist:"));

    let verify_json = run_ocm(&cwd, &env, &["runtime", "verify", "--all", "--json"]);
    assert_eq!(verify_json.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&verify_json)).unwrap();
    let array = value.as_array().unwrap();
    assert_eq!(array.len(), 2);
    assert!(array.iter().any(|item| {
        item["name"] == "stable" && item["healthy"] == true && item["sourceKind"] == "registered"
    }));
    assert!(array.iter().any(|item| {
        item["name"] == "broken"
            && item["healthy"] == false
            && item["issue"]
                .as_str()
                .unwrap()
                .contains("binary path does not exist:")
    }));
}

#[test]
fn runtime_verify_reports_checksum_drift_for_release_backed_runtimes() {
    let root = TestDir::new("runtime-verify-checksum-drift");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let artifact_body = b"expected-runtime";
    let digest_path = root.child("sha256/openclaw-stable");
    fs::create_dir_all(digest_path.parent().unwrap()).unwrap();
    fs::write(&digest_path, artifact_body).unwrap();
    let sha256 = file_sha256(&digest_path).unwrap();

    let artifact_server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw-stable",
        "application/octet-stream",
        artifact_body,
    );
    let manifest_body = format!(
        "{{\"releases\":[{{\"version\":\"0.2.0\",\"channel\":\"stable\",\"url\":\"{}\",\"sha256\":\"{}\"}}]}}",
        artifact_server.url(),
        sha256
    );
    let manifest_server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        manifest_body.as_bytes(),
    );
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            &manifest_server.url(),
            "--version",
            "0.2.0",
        ],
    );
    assert!(install.status.success(), "{}", stderr(&install));

    let runtime_path = root
        .child("ocm-home")
        .join("runtimes/stable/files/openclaw-stable");
    fs::write(runtime_path, b"tampered-runtime").unwrap();

    let verify = run_ocm(&cwd, &env, &["runtime", "verify", "stable", "--json"]);
    assert_eq!(verify.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&verify)).unwrap();
    assert_eq!(value["healthy"], false);
    assert!(
        value["issue"]
            .as_str()
            .unwrap()
            .contains("sha256 mismatch:")
    );
}

#[test]
fn runtime_verify_keeps_official_runtimes_healthy_when_managed_fallback_is_available() {
    let root = TestDir::new("runtime-verify-official-missing-node");
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

    let empty_path = root.child("empty-bin");
    fs::create_dir_all(&empty_path).unwrap();
    let mut verify_env = env.clone();
    verify_env.insert("PATH".to_string(), path_string(&empty_path));
    verify_env.insert(
        "OCM_INTERNAL_NPM_BIN".to_string(),
        path_string(&root.child("fake-node-bin/npm")),
    );

    let verify = run_ocm(
        &cwd,
        &verify_env,
        &["runtime", "verify", "stable", "--json"],
    );
    assert!(verify.status.success(), "{}", stderr(&verify));
    let value: Value = serde_json::from_str(&stdout(&verify)).unwrap();
    assert_eq!(value["healthy"], true);
    assert_eq!(value["issue"], Value::Null);
}
