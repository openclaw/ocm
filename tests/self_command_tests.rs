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

fn serve_candidate_release(
    root: &TestDir,
    release_requests: usize,
) -> (TestHttpServer, TestHttpServer) {
    let archive_path = root.child(current_release_asset_name());
    write_release_archive(
        &archive_path,
        "#!/usr/bin/env bash\nif [[ \"$1\" == \"--version\" ]]; then\n  printf '9.9.9\\n'\nelse\n  printf 'updated ocm\\n'\nfi\n",
    );
    let asset = TestHttpServer::serve_bytes(
        "/download.tar.gz",
        "application/gzip",
        &fs::read(&archive_path).unwrap(),
    );
    let metadata = format!(
        "{{\"tag_name\":\"v9.9.9\",\"assets\":[{{\"name\":\"{}\",\"browser_download_url\":\"{}\",\"digest\":\"sha256:{}\"}}]}}",
        current_release_asset_name(),
        asset.url(),
        file_sha256(&archive_path).unwrap()
    );
    let release = TestHttpServer::serve_bytes_times(
        "/release.json",
        "application/json",
        metadata.as_bytes(),
        release_requests,
    );
    (release, asset)
}

#[cfg(unix)]
#[test]
fn self_update_through_a_symlink_replaces_the_real_binary() {
    let root = TestDir::new("self-update-symlinked-binary");
    let real = fs::canonicalize(copied_ocm(&root)).unwrap();
    let linked = root.child("link/ocm");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../bin/ocm", &linked).unwrap();
    let (release, _asset) = serve_candidate_release(&root, 2);
    let env = release_env(&root, &release.url());

    let check = run_ocm_binary(
        &linked,
        root.path(),
        &env,
        &["self", "update", "--check", "--json"],
    );
    assert!(check.status.success(), "{}", stderr(&check));
    let summary: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(summary["status"], "updateAvailable");
    assert_eq!(summary["binaryPath"], path_string(&real));

    let update = run_ocm_binary(&linked, root.path(), &env, &["self", "update", "--json"]);
    assert!(update.status.success(), "{}", stderr(&update));
    let summary: serde_json::Value = serde_json::from_slice(&update.stdout).unwrap();
    assert_eq!(summary["status"], "updated");
    assert_eq!(summary["binaryPath"], path_string(&real));
    let version = Command::new(&real)
        .arg("--version")
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(version.stdout).unwrap(), "9.9.9\n");
    assert_eq!(fs::read_link(&linked).unwrap(), Path::new("../bin/ocm"));
    assert!(!root.child("link/.ocm.self-update").exists());

    // The fixture candidate only reports its version; a real OCM reads the receipt.
    fs::copy(env!("CARGO_BIN_EXE_ocm"), &real).unwrap();
    let status = run_ocm_binary(&linked, root.path(), &env, &["self", "update", "--status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let receipt: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(receipt["phase"], "updated");
    assert_eq!(receipt["binaryPath"], path_string(&real));
}

#[cfg(unix)]
#[test]
fn self_update_through_a_symlink_accepts_a_service_installed_through_it() {
    let root = TestDir::new("self-update-symlinked-service");
    let real = fs::canonicalize(copied_ocm(&root)).unwrap();
    let linked = root.child("link/ocm");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../bin/ocm", &linked).unwrap();
    let (release, _asset) = serve_candidate_release(&root, 1);
    let mut env = release_env(&root, &release.url());
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    for args in [
        vec!["launcher", "add", "stable", "--command", "openclaw"],
        vec!["env", "create", "demo", "--launcher", "stable"],
        vec!["service", "install", "demo"],
    ] {
        let output = run_ocm_binary(&linked, root.path(), &env, &args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    }
    // Leave the definition installed with no loaded daemon.
    let bootout = Command::new(&env["OCM_INTERNAL_LAUNCHCTL_BIN"])
        .arg("bootout")
        .output()
        .unwrap();
    assert!(bootout.status.success());

    let update = run_ocm_binary(&linked, root.path(), &env, &["self", "update", "--json"]);
    assert!(update.status.success(), "{}", stderr(&update));
    let summary: serde_json::Value = serde_json::from_slice(&update.stdout).unwrap();
    assert_eq!(summary["status"], "updated");
    assert_eq!(summary["binaryPath"], path_string(&real));
    let version = Command::new(&real)
        .arg("--version")
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(version.stdout).unwrap(), "9.9.9\n");
}

#[cfg(unix)]
#[test]
fn self_update_through_a_symlink_refuses_a_read_only_target_before_downloading() {
    use std::os::unix::fs::PermissionsExt;

    let root = TestDir::new("self-update-symlinked-read-only");
    let real = fs::canonicalize(copied_ocm(&root)).unwrap();
    let bin = real.parent().unwrap().to_path_buf();
    let linked = root.child("link/ocm");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let original = file_sha256(&real).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
    // Permission bits do not stop a privileged user; the failure cannot be staged then.
    if fs::write(bin.join("probe"), "").is_ok() {
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let (release, asset) = serve_candidate_release(&root, 1);
    let env = release_env(&root, &release.url());

    let output = run_ocm_binary(&linked, root.path(), &env, &["self", "update", "--json"]);
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(&format!(
            "cannot create update journal in {}: Permission denied",
            bin.display()
        )),
        "{}",
        stderr(&output)
    );
    assert!(asset.requests().is_empty());
    assert_eq!(file_sha256(&real).unwrap(), original);
    assert!(!root.child("link/.ocm.self-update").exists());
    assert!(!bin.join(".ocm.self-update").exists());
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
        assert!(!root.child("bin/.ocm.self-update").exists());
    }
}

#[cfg(unix)]
#[test]
fn self_update_unchanged_needs_no_write_access_beside_the_binary() {
    use std::os::unix::fs::PermissionsExt;

    let root = TestDir::new("self-update-unchanged-read-only");
    let copied = copied_ocm(&root);
    let bin = copied.parent().unwrap().to_path_buf();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
    // Permission bits do not stop a privileged user; the failure cannot be staged then.
    if fs::write(bin.join("probe"), "").is_ok() {
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let current = format!(
        "{{\"tag_name\":\"v{}\",\"assets\":[]}}",
        env!("CARGO_PKG_VERSION")
    );
    let newer = "{\"tag_name\":\"v9.9.9\",\"assets\":[]}";
    let release = TestHttpServer::serve_bytes_sequence(
        "/release",
        "application/json",
        vec![
            current.clone().into_bytes(),
            current.into_bytes(),
            newer.as_bytes().to_vec(),
        ],
    );
    let env = release_env(&root, &release.url());

    let mut outputs = Vec::new();
    for args in [
        vec!["self", "update", "--check", "--json"],
        vec!["self", "update", "--json"],
        vec!["self", "update", "--json"],
    ] {
        outputs.push(run_ocm_binary(&copied, root.path(), &env, &args));
    }
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    for output in &outputs[..2] {
        assert!(output.status.success(), "{}", stderr(output));
        let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["status"], "upToDate");
    }
    // A real update must still write beside the binary, so it fails clearly.
    assert!(!outputs[2].status.success());
    assert!(
        stderr(&outputs[2]).contains(&format!(
            "cannot create update journal in {}: Permission denied",
            bin.display()
        )),
        "{}",
        stderr(&outputs[2])
    );
    assert!(!bin.join(".ocm.self-update").exists());
    assert_eq!(release.requests().len(), 3);
}

#[cfg(unix)]
#[test]
fn self_update_unchanged_still_refuses_an_unfinished_receipt() {
    use std::os::unix::fs::PermissionsExt;

    let root = TestDir::new("self-update-unchanged-unfinished");
    // The receipt names the executable path the running binary reports.
    let copied = fs::canonicalize(copied_ocm(&root)).unwrap();
    let journal = copied.parent().unwrap().join(".ocm.self-update");
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    let receipt = serde_json::json!({
        "schema": 1,
        "id": "interrupted",
        "phase": "applying",
        "binaryPath": copied,
        "ocmHome": root.child("ocm-home"),
        "userHome": root.child("home"),
        "currentVersion": env!("CARGO_PKG_VERSION"),
        "targetVersion": env!("CARGO_PKG_VERSION"),
        "previousSha256": "0",
        "candidateSha256": "0",
        "daemonWasRunning": false,
        "gateways": [],
        "error": null,
        "rollbackError": null,
    });
    let receipt_bytes = serde_json::to_vec_pretty(&receipt).unwrap();
    fs::write(journal.join("receipt.json"), &receipt_bytes).unwrap();
    let metadata = format!(
        "{{\"tag_name\":\"v{}\",\"assets\":[]}}",
        env!("CARGO_PKG_VERSION")
    );
    let release =
        TestHttpServer::serve_bytes_times("/release", "application/json", metadata.as_bytes(), 1);
    let env = release_env(&root, &release.url());

    let output = run_ocm_binary(&copied, root.path(), &env, &["self", "update", "--json"]);
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("unfinished self-update"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read(journal.join("receipt.json")).unwrap(),
        receipt_bytes
    );
    assert!(release.requests().is_empty());
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
