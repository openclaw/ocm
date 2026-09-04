mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use flate2::Compression;
use flate2::write::GzEncoder;
use ocm::infra::download::file_sha256;
use tar::{Builder, EntryType};

use crate::support::{
    TestDir, TestHttpServer, install_fake_launchctl, ocm_env, path_string, run_ocm, run_ocm_binary,
    stderr, stdout,
};

fn current_release_asset_name() -> String {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        other => panic!("unsupported test OS for self update: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => panic!("unsupported test arch for self update: {other}"),
    };
    format!("ocm-{arch}-{os}.tar.gz")
}

fn write_release_archive(path: &Path, shell_script: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }

    let file = fs::File::create(path).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);

    let script_bytes = shell_script.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_size(script_bytes.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "ocm", script_bytes)
        .unwrap();
    builder.finish().unwrap();
}

fn write_release_symlink_archive(path: &Path, shell_script: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }

    let file = fs::File::create(path).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);

    let mut link_header = tar::Header::new_gnu();
    link_header.set_entry_type(EntryType::Symlink);
    link_header.set_size(0);
    link_header.set_mode(0o755);
    link_header.set_link_name("payload").unwrap();
    link_header.set_cksum();
    builder
        .append_data(&mut link_header, "ocm", std::io::empty())
        .unwrap();

    let script_bytes = shell_script.as_bytes();
    let mut script_header = tar::Header::new_gnu();
    script_header.set_size(script_bytes.len() as u64);
    script_header.set_mode(0o755);
    script_header.set_cksum();
    builder
        .append_data(&mut script_header, "payload", script_bytes)
        .unwrap();
    builder.finish().unwrap();
}

fn release_env(root: &TestDir, release_url: &str) -> BTreeMap<String, String> {
    let mut env = ocm_env(root);
    env.insert(
        "OCM_INTERNAL_SELF_UPDATE_RELEASE_URL".to_string(),
        release_url.to_string(),
    );
    env
}

fn copied_ocm(root: &TestDir) -> std::path::PathBuf {
    let copied_binary = root.child("bin/ocm");
    fs::create_dir_all(copied_binary.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ocm"), &copied_binary).unwrap();
    copied_binary
}

fn run_self_update_from_archive(
    root: &TestDir,
    cwd: &Path,
    copied_binary: &Path,
    target_version: &str,
    archive_path: &Path,
    digest: Option<&str>,
) -> Output {
    let asset_name = current_release_asset_name();
    let asset = TestHttpServer::serve_bytes(
        "/download.tar.gz",
        "application/gzip",
        &fs::read(archive_path).unwrap(),
    );
    let digest_field = digest
        .map(|value| format!(",\"digest\":\"sha256:{value}\""))
        .unwrap_or_default();
    let metadata = format!(
        "{{\"tag_name\":\"v{target_version}\",\"assets\":[{{\"name\":\"{asset_name}\",\"browser_download_url\":\"{}\"{digest_field}}}]}}",
        asset.url()
    );
    let release =
        TestHttpServer::serve_bytes("/release.json", "application/json", metadata.as_bytes());
    let env = release_env(root, &release.url());
    run_ocm_binary(
        copied_binary,
        cwd,
        &env,
        &["self", "update", "--version", target_version, "--raw"],
    )
}

#[test]
fn self_update_check_reports_when_a_newer_release_exists() {
    let root = TestDir::new("self-update-check");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let asset_name = current_release_asset_name();
    let metadata = format!(
        "{{\"tag_name\":\"v9.9.9\",\"assets\":[{{\"name\":\"{asset_name}\",\"browser_download_url\":\"https://example.test/{asset_name}\"}}]}}"
    );
    let release =
        TestHttpServer::serve_bytes("/release.json", "application/json", metadata.as_bytes());
    let env = release_env(&root, &release.url());

    let output = run_ocm(&cwd, &env, &["self", "update", "--check", "--raw"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("mode: check"));
    assert!(text.contains("status: update_available"));
    assert!(text.contains(&format!("currentVersion: {}", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains("targetVersion: 9.9.9"));
    assert!(text.contains(&format!("assetName: {asset_name}")));
}

#[test]
fn self_update_check_ignores_older_latest_release_when_current_is_newer() {
    let root = TestDir::new("self-update-check-older-latest");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let asset_name = current_release_asset_name();
    let metadata = format!(
        "{{\"tag_name\":\"v0.2.0\",\"assets\":[{{\"name\":\"{asset_name}\",\"browser_download_url\":\"https://example.test/{asset_name}\"}}]}}"
    );
    let release =
        TestHttpServer::serve_bytes("/release.json", "application/json", metadata.as_bytes());
    let env = release_env(&root, &release.url());

    let output = run_ocm(&cwd, &env, &["self", "update", "--check", "--raw"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("mode: check"));
    assert!(text.contains("status: up_to_date"));
    assert!(text.contains(&format!("currentVersion: {}", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains("targetVersion: 0.2.0"));
}

#[test]
fn self_update_replaces_a_copied_binary_in_place() {
    let root = TestDir::new("self-update-install");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let copied_binary = copied_ocm(&root);

    let target_version = "9.9.9";
    let asset_name = current_release_asset_name();
    let archive_path = root.child(&asset_name);
    write_release_archive(
        &archive_path,
        &format!(
            "#!/usr/bin/env bash\nif [[ \"$1\" == \"--version\" ]]; then\n  printf '{target_version}\\n'\nelse\n  printf 'updated ocm\\n'\nfi\n"
        ),
    );
    let digest = file_sha256(&archive_path).unwrap();
    let output = run_self_update_from_archive(
        &root,
        &cwd,
        &copied_binary,
        target_version,
        &archive_path,
        Some(&digest),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("mode: update"));
    assert!(text.contains("status: updated"));
    assert!(text.contains(&format!("binaryPath: {}", path_string(&copied_binary))));

    let updated = Command::new(&copied_binary)
        .arg("--version")
        .env_clear()
        .output()
        .unwrap();
    assert!(updated.status.success());
    assert_eq!(String::from_utf8(updated.stdout).unwrap(), "9.9.9\n");
    let journal = copied_binary.parent().unwrap().join(".ocm.self-update");
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(journal.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["phase"], "updated");
    assert_eq!(receipt["daemonWasRunning"], false);
    assert_eq!(
        file_sha256(&journal.join("previous")).unwrap(),
        file_sha256(Path::new(env!("CARGO_BIN_EXE_ocm"))).unwrap()
    );
    assert!(
        !root
            .child("home/Library/LaunchAgents/ai.openclaw.ocm.plist")
            .exists()
    );
    assert!(
        !root
            .child("home/.config/systemd/user/ai.openclaw.ocm.service")
            .exists()
    );
}

#[test]
fn self_update_unchanged_and_check_do_not_create_a_receipt() {
    let root = TestDir::new("self-update-unchanged");
    let copied = copied_ocm(&root);
    let metadata = format!(
        "{{\"tag_name\":\"v{}\",\"assets\":[]}}",
        env!("CARGO_PKG_VERSION")
    );
    let release =
        TestHttpServer::serve_bytes_times("/release", "application/json", metadata.as_bytes(), 2);
    let env = release_env(&root, &release.url());
    for args in [
        vec!["self", "update", "--check", "--json"],
        vec!["self", "update", "--json"],
    ] {
        let output = run_ocm_binary(&copied, root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
        let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["status"], "upToDate");
        assert!(!root.child("bin/.ocm.self-update/receipt.json").exists());
    }
}

#[test]
fn self_update_refuses_an_unknown_daemon_version_before_replacement() {
    let root = TestDir::new("self-update-running-daemon-skew");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
    let original = fs::read(&copied_binary).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    for args in [
        vec!["launcher", "add", "stable", "--command", "openclaw"],
        vec!["env", "create", "demo", "--launcher", "stable"],
        vec!["service", "start", "demo"],
    ] {
        let output = run_ocm_binary(&copied_binary, &cwd, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }

    let target_version = "9.9.9";
    let asset_name = current_release_asset_name();
    let archive_path = root.child(&asset_name);
    write_release_archive(
        &archive_path,
        &format!(
            "#!/usr/bin/env bash\nif [[ \"$1\" == \"--version\" ]]; then\n  printf '{target_version}\\n'\nelse\n  printf 'updated ocm\\n'\nfi\n"
        ),
    );
    let asset = TestHttpServer::serve_bytes(
        "/download.tar.gz",
        "application/gzip",
        &fs::read(&archive_path).unwrap(),
    );
    let digest = file_sha256(&archive_path).unwrap();
    let metadata = format!(
        "{{\"tag_name\":\"v{target_version}\",\"assets\":[{{\"name\":\"{asset_name}\",\"browser_download_url\":\"{}\",\"digest\":\"sha256:{digest}\"}}]}}",
        asset.url()
    );
    let release =
        TestHttpServer::serve_bytes("/release.json", "application/json", metadata.as_bytes());
    env.insert(
        "OCM_INTERNAL_SELF_UPDATE_RELEASE_URL".to_string(),
        release.url(),
    );
    let launchctl_log = root.child("launchctl.log");
    fs::write(&launchctl_log, "").unwrap();

    let output = run_ocm_binary(
        &copied_binary,
        &cwd,
        &env,
        &["self", "update", "--version", target_version, "--raw"],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("running daemon version is unknown"));
    assert_eq!(fs::read(&copied_binary).unwrap(), original);

    let calls = fs::read_to_string(&launchctl_log).unwrap();
    assert!(calls.contains("print"));
    assert!(!calls.contains("bootout"));
    assert!(!calls.contains("bootstrap"));
}

#[test]
fn self_update_rejects_an_unsigned_release_asset() {
    let root = TestDir::new("self-update-unsigned");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
    let original = fs::read(&copied_binary).unwrap();

    let archive_path = root.child(current_release_asset_name());
    write_release_archive(
        &archive_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '9.9.9\\n'; fi\n",
    );
    let output =
        run_self_update_from_archive(&root, &cwd, &copied_binary, "9.9.9", &archive_path, None);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("does not include a digest"));
    assert_eq!(fs::read(&copied_binary).unwrap(), original);
}

#[test]
fn self_update_rejects_a_tampered_release_asset() {
    let root = TestDir::new("self-update-tampered");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
    let original = fs::read(&copied_binary).unwrap();

    let archive_path = root.child(current_release_asset_name());
    write_release_archive(
        &archive_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '9.9.9\\n'; fi\n",
    );
    let output = run_self_update_from_archive(
        &root,
        &cwd,
        &copied_binary,
        "9.9.9",
        &archive_path,
        Some(&"0".repeat(64)),
    );

    assert!(!output.status.success());
    assert!(stderr(&output).contains("sha256 mismatch"));
    assert_eq!(fs::read(&copied_binary).unwrap(), original);
}

#[test]
fn self_update_rejects_a_non_regular_archive_binary() {
    let root = TestDir::new("self-update-symlink");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
    let original = fs::read(&copied_binary).unwrap();

    let archive_path = root.child(current_release_asset_name());
    write_release_symlink_archive(
        &archive_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '9.9.9\\n'; fi\n",
    );
    let digest = file_sha256(&archive_path).unwrap();
    let output = run_self_update_from_archive(
        &root,
        &cwd,
        &copied_binary,
        "9.9.9",
        &archive_path,
        Some(&digest),
    );

    assert!(!output.status.success());
    assert!(stderr(&output).contains("ocm entry is not a regular file"));
    assert_eq!(fs::read(&copied_binary).unwrap(), original);
}

#[test]
fn self_update_rejects_an_empty_archive_binary() {
    let root = TestDir::new("self-update-empty");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
    let original = fs::read(&copied_binary).unwrap();

    let archive_path = root.child(current_release_asset_name());
    write_release_archive(&archive_path, "");
    let digest = file_sha256(&archive_path).unwrap();
    let output = run_self_update_from_archive(
        &root,
        &cwd,
        &copied_binary,
        "9.9.9",
        &archive_path,
        Some(&digest),
    );

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("staged ocm binary"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&copied_binary).unwrap(), original);
}

#[test]
fn self_update_rejects_a_binary_reporting_the_wrong_version() {
    let root = TestDir::new("self-update-wrong-version");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
    let original = fs::read(&copied_binary).unwrap();

    let archive_path = root.child(current_release_asset_name());
    write_release_archive(
        &archive_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '8.8.8\\n'; fi\n",
    );
    let digest = file_sha256(&archive_path).unwrap();
    let output = run_self_update_from_archive(
        &root,
        &cwd,
        &copied_binary,
        "9.9.9",
        &archive_path,
        Some(&digest),
    );

    assert!(!output.status.success());
    assert!(stderr(&output).contains("reported version \"8.8.8\"; expected \"9.9.9\""));
    assert_eq!(fs::read(&copied_binary).unwrap(), original);
}
