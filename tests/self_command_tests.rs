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

#[cfg(unix)]
#[test]
fn homebrew_self_update_preserves_the_keg_and_allows_checks_through_symlinks() {
    let root = TestDir::new("self-update-homebrew");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let keg = root.child("brew/custom-cellar/ocm/0.2.39");
    let binary = keg.join("bin/ocm");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ocm"), &binary).unwrap();
    fs::write(
        keg.join("INSTALL_RECEIPT.json"),
        r#"{"homebrew_version":"4.6.0","source":{"tap":"openclaw/tap"}}"#,
    )
    .unwrap();
    let linked = root.child("brew/bin/ocm");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&binary, &linked).unwrap();
    let original = file_sha256(&binary).unwrap();
    let release = TestHttpServer::serve_bytes(
        "/release.json",
        "application/json",
        br#"{"tag_name":"v9.9.9","assets":[]}"#,
    );
    let env = release_env(&root, &release.url());

    for command in [&binary, &linked] {
        let output = run_ocm_binary(command, &cwd, &env, &["self", "update", "--raw"]);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("brew upgrade openclaw/tap/ocm"),
            "{}",
            stderr(&output)
        );
    }
    assert!(release.requests().is_empty());
    assert_eq!(file_sha256(&binary).unwrap(), original);
    assert_eq!(fs::read_dir(keg.join("bin")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(root.child("ocm-home")).unwrap().count(), 0);

    let output = run_ocm_binary(&linked, &cwd, &env, &["self", "update", "--check", "--raw"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("status: update_available"));
    assert_eq!(release.requests().len(), 1);
    assert_eq!(file_sha256(&binary).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn npm_self_update_preserves_global_local_and_npx_payloads() {
    use crate::support::npm_fixture;

    for (location, guidance) in [
        (
            "custom-prefix/lib/node_modules/@openclaw/ocm",
            "same npm prefix",
        ),
        ("project/node_modules/@openclaw/ocm", "owning project"),
        ("lib/node_modules/@openclaw/ocm", "owning project"),
        (
            "custom-cache/_npx/hash/node_modules/@openclaw/ocm",
            "temporary",
        ),
    ] {
        let root = TestDir::new("self-update-npm");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let Some((entrypoint, binary)) = npm_fixture(&root.child(location)) else {
            return;
        };
        if location.starts_with("custom-prefix/") {
            let global = root.child("custom-prefix/bin/ocm");
            fs::create_dir_all(global.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&entrypoint, &global).unwrap();
        }
        let linked = root.child("linked-ocm");
        std::os::unix::fs::symlink(&entrypoint, &linked).unwrap();
        let release = TestHttpServer::serve_bytes(
            "/release.json",
            "application/json",
            br#"{"tag_name":"v9.9.9","assets":[]}"#,
        );
        let env = release_env(&root, &release.url());
        let original = file_sha256(&binary).unwrap();
        for command in [&binary, &entrypoint, &linked] {
            let output = run_ocm_binary(
                command,
                &cwd,
                &env,
                &["--color", "never", "self", "update", "--raw"],
            );
            assert!(!output.status.success());
            assert!(stderr(&output).contains(guidance), "{}", stderr(&output));
        }
        assert!(release.requests().is_empty());
        assert_eq!(file_sha256(&binary).unwrap(), original);
        assert_eq!(fs::read_dir(binary.parent().unwrap()).unwrap().count(), 1);
        let output = run_ocm_binary(
            &linked,
            &cwd,
            &env,
            &["self", "update", "--check", "--json"],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            result["packageManagerNote"]
                .as_str()
                .unwrap()
                .contains("not npm availability")
        );
        assert_eq!(release.requests().len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn npm_owner_requires_manifest_identity_not_path_or_environment_alone() {
    let root = TestDir::new("self-update-npm-identity");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let Some((_, binary)) = crate::support::npm_fixture(&root.child("node_modules/ocm-alias"))
    else {
        return;
    };
    let package = binary
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let release = TestHttpServer::serve_bytes_times(
        "/release.json",
        "application/json",
        br#"{"tag_name":"v0.0.0","assets":[]}"#,
        2,
    );
    let mut env = release_env(&root, &release.url());
    env.insert("OCM_MANAGED_BY_NPM".to_string(), "1".to_string());
    for manifest in ["not json", r#"{"name":"unrelated","version":"9.0.0"}"#] {
        fs::write(package.join("package.json"), manifest).unwrap();
        let output = run_ocm_binary(&binary, &cwd, &env, &["self", "update", "--raw"]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("up_to_date"));
    }
    assert_eq!(release.requests().len(), 2);
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
}

#[test]
fn self_update_reports_running_daemon_skew_without_restarting_it() {
    let root = TestDir::new("self-update-running-daemon-skew");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let copied_binary = copied_ocm(&root);
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
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("daemonRefreshRequired: true"));
    assert!(text.contains("service refresh-daemon --acknowledge-gateway-restarts"));

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
