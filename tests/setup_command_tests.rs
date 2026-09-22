mod support;

use std::fs;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_git_package_manager, install_fake_launchctl,
    install_fake_managed_node_archive, install_fake_node_and_npm, ocm_env, run_ocm,
    run_ocm_with_stdin, stderr, stdout,
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

#[test]
fn setup_can_prepare_latest_stable_without_onboarding() {
    let root = TestDir::new("setup-stable");
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
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], "1\n\nn\nn\n");
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("OpenClaw setup"));
    let env_name = output
        .lines()
        .find_map(|line| line.strip_prefix("Started env "))
        .expect("setup output should name the created environment");

    let show = run_ocm(&cwd, &env, &["env", "show", env_name, "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultRuntime"], "stable");
}

#[test]
fn setup_can_prepare_a_local_command_launcher() {
    let root = TestDir::new("setup-local-command");
    let cwd = root.child("workspace");
    let project_dir = cwd.join("openclaw");
    fs::create_dir_all(&project_dir).unwrap();
    let env = ocm_env(&root);

    let input = format!(
        "4\nhacking\npnpm openclaw\n{}\nn\nn\n",
        project_dir.display()
    );
    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], &input);
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("Started env hacking"));
    assert!(output.contains("launcher: hacking.local"));

    let show = run_ocm(&cwd, &env, &["env", "show", "hacking", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultLauncher"], "hacking.local");
}

#[test]
fn setup_detects_a_local_openclaw_checkout_and_uses_default_local_values() {
    let root = TestDir::new("setup-detect-local-checkout");
    let repo = root.child("workspace/openclaw");
    let scripts_dir = repo.join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::write(
        repo.join("package.json"),
        r#"{"name":"openclaw","version":"2026.3.28"}"#,
    )
    .unwrap();
    fs::write(scripts_dir.join("run-node.mjs"), "console.log('run');\n").unwrap();
    let env = ocm_env(&root);

    let setup = run_ocm_with_stdin(&repo, &env, &["setup"], "4\n\n\n\nn\nn\n");
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("Detected local OpenClaw checkout:"));
    assert!(output.contains("Started env dev"));
    assert!(output.contains("launcher: dev.local"));

    let show = run_ocm(&repo, &env, &["launcher", "show", "dev.local", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["command"], "pnpm openclaw");
    assert_eq!(
        show_json["cwd"],
        fs::canonicalize(&repo).unwrap().display().to_string()
    );
}

#[test]
fn setup_points_existing_plain_openclaw_users_at_migrate() {
    let root = TestDir::new("setup-detect-plain-home");
    let cwd = root.child("workspace");
    let plain_home = root.child("home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(plain_home.join("workspace")).unwrap();
    fs::write(plain_home.join("openclaw.json"), "{}\n").unwrap();
    let env = ocm_env(&root);

    let input = format!("4\nquick\n/bin/echo\n{}\nn\nn\n", cwd.display());
    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], &input);
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("Detected existing plain OpenClaw home:"));
    assert!(output.contains(&plain_home.display().to_string()));
    assert!(output.contains("Use \"ocm migrate <env>\" if you want to bring that state under OCM instead of starting fresh."));
}

#[test]
fn setup_defaults_service_install_to_yes_in_raw_mode() {
    let root = TestDir::new("setup-default-service-yes");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    let input = format!("4\nquick\n/bin/echo\n{}\n\nn\n", cwd.display());
    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], &input);
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("Started env quick"));
    assert!(output.contains("service: running"));

    let launchctl_log = fs::read_to_string(root.child("launchctl.log")).unwrap();
    assert!(launchctl_log.contains("bootstrap gui/"));
    assert!(launchctl_log.contains("bootout gui/"));
}

#[test]
fn setup_skips_background_service_prompt_on_unsupported_backends() {
    let root = TestDir::new("setup-unsupported-service-backend");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "unsupported".to_string(),
    );

    let input = format!("4\nquick\n/bin/echo\n{}\nn\n", cwd.display());
    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], &input);
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("managed services are not supported on this platform yet"));
    assert!(!output.contains("Keep OpenClaw running in the background?"));
    assert!(output.contains("Started env quick"));

    let show = run_ocm(&cwd, &env, &["env", "show", "quick", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultLauncher"], "quick.local");
}

#[test]
fn setup_skips_background_service_prompt_when_launchctl_is_unavailable() {
    let root = TestDir::new("setup-missing-launchctl");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    env.insert(
        "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
        root.child("missing-bin/launchctl").display().to_string(),
    );

    let input = format!("4\nquick\n/bin/echo\n{}\nn\n", cwd.display());
    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], &input);
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("managed services require launchctl on this machine"));
    assert!(!output.contains("Keep OpenClaw running in the background?"));
    assert!(output.contains("Started env quick"));
}

#[test]
fn setup_skips_background_service_prompt_when_launchctl_is_unusable() {
    let root = TestDir::new("setup-unusable-launchctl");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    env.insert(
        "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
        "/bin/sh".to_string(),
    );

    let input = format!("4\nquick\n/bin/echo\n{}\nn\n", cwd.display());
    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], &input);
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("managed services require a usable launchctl session"));
    assert!(!output.contains("Keep OpenClaw running in the background?"));
    assert!(output.contains("Started env quick"));
}

#[test]
fn setup_can_offer_git_install_before_using_managed_node_fallback() {
    let root = TestDir::new("setup-host-doctor-missing");
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
    env.insert("PATH".to_string(), empty_path.to_string_lossy().to_string());
    env.insert(
        "OCM_INTERNAL_HOST_PLATFORM".to_string(),
        "linux".to_string(),
    );
    env.insert(
        "OCM_INTERNAL_HOST_PACKAGE_MANAGER".to_string(),
        "apt-get".to_string(),
    );
    env.insert("OCM_INTERNAL_HOST_IS_ROOT".to_string(), "false".to_string());
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.21.0");
    let log_path = install_fake_git_package_manager(&root, &mut env, "apt-get");

    let setup = run_ocm_with_stdin(&cwd, &env, &["setup"], "1\ny\n\nn\nn\n");
    assert!(setup.status.success(), "{}", stderr(&setup));
    let output = stdout(&setup);
    assert!(output.contains("OpenClaw setup"));
    assert!(output.contains("tool: git"));
    assert!(output.contains("changed: true"));
    assert!(output.contains("manager: apt-get"));
    assert!(output.contains("Started env "));
    assert!(!output.contains("healthy: true"));
    assert!(!output.contains("officialReleaseReady: true"));
    assert!(!output.contains("check: category=official-release"));

    let log = fs::read_to_string(log_path).unwrap();
    assert!(log.contains("update"));
    assert!(log.contains("install -y git"));

    let list = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let envs: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(envs.as_array().unwrap().len(), 1);
}
