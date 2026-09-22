mod support;

use std::fs;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use serde_json::json;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_managed_node_archive, install_fake_node_and_npm, ocm_env,
    path_string, run_ocm, stderr, stdout,
};

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

fn openclaw_package_tarball(script_body: &str) -> Vec<u8> {
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
            br#"{"name":"openclaw","version":"2026.3.24","bin":{"openclaw":"openclaw.mjs"}}"#,
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn sha512_integrity(body: &[u8]) -> String {
    let digest = Sha512::digest(body);
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

fn requests_install_metadata(server: &TestHttpServer) -> bool {
    server.requests().iter().any(|request| {
        request
            .to_ascii_lowercase()
            .contains("accept: application/vnd.npm.install-v1+json")
    })
}

fn packument_body() -> Vec<u8> {
    json!({
        "dist-tags": {
            "latest": "2026.3.24",
            "beta": "2026.3.24-beta.2"
        },
        "versions": {
            "2026.3.24": {
                "version": "2026.3.24",
                "dist": {
                    "tarball": "https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz",
                    "shasum": "abc123",
                    "integrity": "sha512-stable"
                }
            },
            "2026.3.24-beta.2": {
                "version": "2026.3.24-beta.2",
                "dist": {
                    "tarball": "https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24-beta.2.tgz",
                    "shasum": "def456",
                    "integrity": "sha512-beta"
                }
            }
        },
        "time": {
            "2026.3.24": "2026-03-25T16:35:52.000Z",
            "2026.3.24-beta.2": "2026-03-25T14:11:48.000Z"
        }
    })
    .to_string()
    .into_bytes()
}

#[test]
fn release_list_uses_the_official_openclaw_source() {
    let root = TestDir::new("release-list-official");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let server = TestHttpServer::serve_bytes("/openclaw", "application/json", &packument_body());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let output = run_ocm(&cwd, &env, &["release", "list", "--raw"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    assert!(stdout.contains("2026.3.24"));
    assert!(stdout.contains("channel=stable"));
    assert!(stdout.contains("2026.3.24-beta.2"));
    assert!(stdout.contains("channel=beta"));
}

#[test]
fn release_list_can_filter_by_channel_and_release_show_prints_one_version() {
    let root = TestDir::new("release-list-show");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let list_server =
        TestHttpServer::serve_bytes("/openclaw-list", "application/json", &packument_body());
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        list_server.url(),
    );

    let stable = run_ocm(
        &cwd,
        &env,
        &["release", "list", "--channel", "stable", "--json"],
    );
    assert!(stable.status.success(), "{}", stderr(&stable));
    let stable_stdout = stdout(&stable);
    assert!(stable_stdout.contains("\"version\": \"2026.3.24\""));
    assert!(!stable_stdout.contains("2026.3.24-beta.2"));
    assert!(!requests_install_metadata(&list_server));

    let show_server =
        TestHttpServer::serve_bytes("/openclaw-show", "application/json", &packument_body());
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        show_server.url(),
    );
    let show = run_ocm(&cwd, &env, &["release", "show", "2026.3.24"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    assert!(show_stdout.contains("version: 2026.3.24"));
    assert!(show_stdout.contains("channel: stable"));
    assert!(show_stdout.contains("publishedAt: 2026-03-25T16:35:52Z"));
    assert!(!requests_install_metadata(&show_server));
    assert!(show_stdout.contains("https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz"));

    let version_show_server = TestHttpServer::serve_bytes(
        "/openclaw-show-version",
        "application/json",
        &packument_body(),
    );
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        version_show_server.url(),
    );
    let version_show = run_ocm(
        &cwd,
        &env,
        &[
            "release",
            "show",
            "--version",
            "2026.3.24",
            "--color",
            "always",
        ],
    );
    assert!(version_show.status.success(), "{}", stderr(&version_show));
    let version_show_stdout = stdout(&version_show);
    assert!(
        version_show_stdout.contains("ocm release install --version 2026.3.24"),
        "{version_show_stdout}"
    );
    assert!(!version_show_stdout.contains("ocm release install --channel stable"));

    let channel_show_server = TestHttpServer::serve_bytes(
        "/openclaw-show-channel",
        "application/json",
        &packument_body(),
    );
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        channel_show_server.url(),
    );
    let channel_show = run_ocm(&cwd, &env, &["release", "show", "--channel", "stable"]);
    assert!(channel_show.status.success(), "{}", stderr(&channel_show));
    let channel_show_stdout = stdout(&channel_show);
    assert!(channel_show_stdout.contains("version: 2026.3.24"));
    assert!(channel_show_stdout.contains("channel: stable"));

    let pretty_channel_show_server = TestHttpServer::serve_bytes(
        "/openclaw-show-pretty-channel",
        "application/json",
        &packument_body(),
    );
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        pretty_channel_show_server.url(),
    );
    let pretty_channel_show = run_ocm(
        &cwd,
        &env,
        &[
            "release",
            "show",
            "--channel",
            "stable",
            "--color",
            "always",
        ],
    );
    assert!(
        pretty_channel_show.status.success(),
        "{}",
        stderr(&pretty_channel_show)
    );
    let pretty_channel_show_stdout = stdout(&pretty_channel_show);
    assert!(pretty_channel_show_stdout.contains("ocm release install --channel stable"));

    let latest_server =
        TestHttpServer::serve_bytes("/openclaw-latest", "application/json", &packument_body());
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        latest_server.url(),
    );
    let latest = run_ocm(
        &cwd,
        &env,
        &["release", "list", "--channel", "latest", "--json"],
    );
    assert!(latest.status.success(), "{}", stderr(&latest));
    let latest_stdout = stdout(&latest);
    assert!(latest_stdout.contains("\"version\": \"2026.3.24\""));
    assert!(!latest_stdout.contains("2026.3.24-beta.2"));

    let latest_show_server = TestHttpServer::serve_bytes(
        "/openclaw-show-latest",
        "application/json",
        &packument_body(),
    );
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        latest_show_server.url(),
    );
    let latest_show = run_ocm(
        &cwd,
        &env,
        &["release", "show", "--channel", "latest", "--json"],
    );
    assert!(latest_show.status.success(), "{}", stderr(&latest_show));
    let latest_show_stdout = stdout(&latest_show);
    assert!(latest_show_stdout.contains("\"version\": \"2026.3.24\""));
    assert!(!latest_show_stdout.contains("2026.3.24-beta.2"));
}

#[test]
fn release_list_rejects_conflicting_selectors() {
    let root = TestDir::new("release-list-conflicting");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let server = TestHttpServer::serve_bytes("/openclaw", "application/json", &packument_body());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "release",
            "list",
            "--version",
            "2026.3.24",
            "--channel",
            "stable",
        ],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("release list accepts only one of --version or --channel"));
}

#[test]
fn release_install_uses_the_published_openclaw_source() {
    let root = TestDir::new("release-install");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}}}}",
        tarball_server.url(),
        integrity
    );
    let server = TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let install = run_ocm(&cwd, &env, &["release", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));
    let output = stdout(&install);
    assert!(output.contains("Installed runtime stable"));
    assert!(output.contains("install root:"));
    assert!(output.contains("use in env: ocm env create mira --runtime stable"));
    assert!(requests_install_metadata(&server));
}

#[test]
fn release_install_rejects_non_canonical_runtime_names() {
    let root = TestDir::new("release-install-canonical-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let server = TestHttpServer::serve_bytes("/openclaw", "application/json", &packument_body());
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let install = run_ocm(
        &cwd,
        &env,
        &["release", "install", "latest", "--channel", "stable"],
    );
    assert_eq!(install.status.code(), Some(1));
    assert!(
        stderr(&install).contains(
            "release install uses the canonical runtime name \"stable\" for this selector"
        )
    );
}

#[test]
fn release_install_stays_quiet_when_managed_node_fallback_is_available() {
    let root = TestDir::new("release-install-host-doctor");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
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
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    let empty_path = root.child("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    env.insert("PATH".to_string(), path_string(&empty_path));
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.21.0");

    let install = run_ocm(&cwd, &env, &["release", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));
    let output = stdout(&install);
    assert!(output.contains("Installed runtime stable"));
    assert!(!output.contains("healthy: true"));
    assert!(!output.contains("officialReleaseReady: true"));
    assert!(!output.contains("check: category=official-release"));
}

#[test]
fn release_install_reuses_a_matching_installed_runtime() {
    let root = TestDir::new("release-install-reuse");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
        2,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}}}}",
        tarball_server.url(),
        integrity
    );
    let server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let first = run_ocm(&cwd, &env, &["release", "install", "--channel", "stable"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("Installed runtime stable"));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let second = run_ocm(&cwd, &env, &["release", "install", "--channel", "stable"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).contains("Using installed runtime stable"));
    assert!(stdout(&second).contains("use in env: ocm env create mira --runtime stable"));
}

#[test]
fn release_list_and_show_surface_installed_runtime_names() {
    let root = TestDir::new("release-installed-runtime-names");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n");
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
    let server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 3);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let install = run_ocm(&cwd, &env, &["release", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let list = run_ocm(
        &cwd,
        &env,
        &["release", "list", "--version", "2026.3.24", "--json"],
    );
    assert!(list.status.success(), "{}", stderr(&list));
    let list_stdout = stdout(&list);
    assert!(list_stdout.contains("\"installedRuntimeNames\": ["));
    assert!(list_stdout.contains("\"stable\""));

    let show = run_ocm(&cwd, &env, &["release", "show", "2026.3.24"]);
    assert!(show.status.success(), "{}", stderr(&show));
    assert!(stdout(&show).contains("installedRuntimes: stable"));
}
