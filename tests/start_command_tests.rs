mod support;

use std::fs;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_launchctl, install_fake_managed_node_archive,
    install_fake_node_and_npm, ocm_env, path_string, run_ocm, stderr, stdout,
    write_executable_script,
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
fn start_generates_an_env_name_and_uses_latest_stable_runtime() {
    let root = TestDir::new("start-default-stable");
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
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start"]);
    assert!(start.status.success(), "{}", stderr(&start));
    let output = stdout(&start);
    let env_name = output
        .lines()
        .find_map(|line| line.strip_prefix("Started env "))
        .expect("start output should name the created environment");
    assert_ne!(env_name, "default");
    assert!(output.contains("runtime: stable"));
    assert!(output.contains("config: minimum local"));
    assert!(output.contains(&format!("onboard: ocm @{env_name} -- onboard")));
    assert!(output.contains("service: running"));

    let show = run_ocm(&cwd, &env, &["env", "show", env_name, "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["name"], env_name);
    assert_eq!(show_json["defaultRuntime"], "stable");

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["name"], "stable");
    assert_eq!(runtime_json["releaseSelectorKind"], "channel");
    assert_eq!(runtime_json["releaseSelectorValue"], "stable");

    let config_path = root.child(format!("ocm-home/envs/{env_name}/.openclaw/openclaw.json"));
    let config_json: Value =
        serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(config_json["gateway"]["mode"], "local");
    assert_eq!(config_json["gateway"]["bind"], "loopback");
    assert!(
        config_json["agents"]["defaults"]
            .get("skipBootstrap")
            .is_none()
    );
    assert!(config_json["agents"].get("entries").is_none());
    assert!(config_json["agents"].get("list").is_none());
}

#[test]
fn start_without_a_name_generates_a_new_env_each_time() {
    let root = TestDir::new("start-generated-name-repeat");
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
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 4);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let first = run_ocm(&cwd, &env, &["start", "--no-service"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let first_name = stdout(&first)
        .lines()
        .find_map(|line| line.strip_prefix("Started env "))
        .expect("first start output should name the created environment")
        .to_string();

    let second = run_ocm(&cwd, &env, &["start", "--no-service"]);
    assert!(second.status.success(), "{}", stderr(&second));
    let second_name = stdout(&second)
        .lines()
        .find_map(|line| line.strip_prefix("Started env "))
        .expect("second start output should name the created environment")
        .to_string();

    assert_ne!(first_name, second_name);
}

#[test]
fn start_rejects_services_on_unsupported_backends() {
    let root = TestDir::new("start-unsupported-service-backend");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "unsupported".to_string(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(start.status.code(), Some(1));
    assert!(stderr(&start).contains("managed services are not supported on this platform yet"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("environment \"demo\" does not exist"));
}

#[test]
fn start_rejects_services_when_launchctl_is_unavailable() {
    let root = TestDir::new("start-missing-launchctl");
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

    let start = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(start.status.code(), Some(1));
    assert!(stderr(&start).contains("managed services require launchctl on this machine"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("environment \"demo\" does not exist"));
}

#[test]
fn start_rejects_services_when_launchctl_is_unusable() {
    let root = TestDir::new("start-unusable-launchctl");
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

    let start = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(start.status.code(), Some(1));
    assert!(stderr(&start).contains("managed services require a usable launchctl session"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("environment \"demo\" does not exist"));
}

#[test]
fn start_can_create_a_local_command_launcher() {
    let root = TestDir::new("start-command-launcher");
    let cwd = root.child("workspace");
    let project_dir = cwd.join("openclaw");
    fs::create_dir_all(&project_dir).unwrap();
    let env = ocm_env(&root);

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "hacking",
            "--command",
            "pnpm openclaw",
            "--cwd",
            &project_dir.to_string_lossy(),
            "--no-service",
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    let output = stdout(&start);
    assert!(output.contains("Started env hacking"));
    assert!(output.contains("launcher: hacking.local"));

    let launcher = run_ocm(&cwd, &env, &["launcher", "show", "hacking.local", "--json"]);
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let launcher_json: Value = serde_json::from_str(&stdout(&launcher)).unwrap();
    assert_eq!(launcher_json["command"], "pnpm openclaw");
    assert_eq!(
        launcher_json["cwd"],
        project_dir.to_string_lossy().to_string()
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "hacking", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultLauncher"], "hacking.local");
}

#[test]
fn start_reuses_launcher_with_equivalent_cwd_and_preserves_conflicts() {
    let root = TestDir::new("start-command-cwd-reuse");
    let cwd = root.child("workspace");
    fs::create_dir_all(cwd.join("other")).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let env = ocm_env(&root);
    let start = |command: &str, directory: Option<&str>| {
        let mut args = vec![
            "start",
            "demo",
            "--command",
            command,
            "--no-service",
            "--json",
        ];
        if let Some(directory) = directory {
            args.extend(["--cwd", directory]);
        }
        run_ocm(&cwd, &env, &args)
    };

    let created = start("openclaw", Some("."));
    assert!(created.status.success(), "{}", stderr(&created));
    let launcher_path = root.child("ocm-home/launchers/demo.local.json");
    let original = fs::read(&launcher_path).unwrap();
    let launcher: Value = serde_json::from_slice(&original).unwrap();
    let absolute_cwd = path_string(&cwd);
    assert_eq!(launcher["cwd"], absolute_cwd);

    for equivalent in [".", "./other/..", absolute_cwd.as_str()] {
        let repeated = start("openclaw", Some(equivalent));
        assert!(repeated.status.success(), "{}", stderr(&repeated));
        let summary: Value = serde_json::from_str(&stdout(&repeated)).unwrap();
        assert_eq!(summary["created"], false);
        assert_eq!(summary["defaultLauncher"], "demo.local");
        assert_eq!(fs::read(&launcher_path).unwrap(), original);
    }

    let other_cwd = path_string(&cwd.join("other"));
    for (command, directory) in [
        ("openclaw", Some("./other")),
        ("openclaw", Some(other_cwd.as_str())),
        ("openclaw", None),
        ("different-command", Some(".")),
    ] {
        let refused = start(command, directory);
        assert_eq!(refused.status.code(), Some(1));
        assert!(stderr(&refused).contains("already exists with different settings"));
        assert_eq!(fs::read(&launcher_path).unwrap(), original);
    }
}

#[test]
#[cfg(unix)]
fn start_preserves_relative_cwd_when_directory_name_ends_in_space() {
    let root = TestDir::new("start-command-cwd-space");
    let cwd = root.child("workspace ");
    fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let env = ocm_env(&root);
    let args = [
        "start",
        "demo",
        "--command",
        "openclaw",
        "--cwd",
        ".",
        "--no-service",
    ];
    for _ in 0..2 {
        let started = run_ocm(&cwd, &env, &args);
        assert!(started.status.success(), "{}", stderr(&started));
        let launcher: Value = serde_json::from_slice(
            &fs::read(root.child("ocm-home/launchers/demo.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(launcher["cwd"], path_string(&cwd));
    }
}

#[test]
fn start_points_existing_plain_openclaw_users_at_migrate_when_creating_fresh_envs() {
    let root = TestDir::new("start-detect-plain-home");
    let cwd = root.child("workspace");
    let plain_home = root.child("home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(plain_home.join("workspace")).unwrap();
    fs::write(plain_home.join("openclaw.json"), "{}\n").unwrap();
    let env = ocm_env(&root);

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "fresh",
            "--command",
            "/bin/echo",
            "--cwd",
            &cwd.to_string_lossy(),
            "--no-service",
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    let output = stdout(&start);
    assert!(output.contains("Started env fresh"));
    assert!(output.contains("existing plain home:"));
    assert!(output.contains(&plain_home.display().to_string()));
    assert!(output.contains("migrate instead: ocm migrate <env>"));
}

#[test]
fn start_reuses_existing_env_without_forcing_onboarding() {
    let root = TestDir::new("start-existing");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));
    let output = stdout(&start);
    assert!(output.contains("Using env demo"));
    assert!(output.contains("launcher: stable"));
    assert!(output.contains("config: minimum local"));
    assert!(output.contains("onboard: ocm @demo -- onboard"));
    assert!(!output.contains("onboarding: running now"));
}

#[test]
fn start_rejects_json_when_onboarding_would_run() {
    let root = TestDir::new("start-json-onboarding");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &["start", "--json", "--no-service", "--onboard"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("start cannot combine --json with --onboard"));
}

#[test]
fn start_uses_managed_node_fallback_without_host_doctor_noise() {
    let root = TestDir::new("start-host-doctor-missing");
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
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 4);
    let mut env = ocm_env(&root);
    let empty_path = root.child("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    env.insert("PATH".to_string(), path_string(&empty_path));
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.21.0");

    let start = run_ocm(&cwd, &env, &["start", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));
    let output = stdout(&start);
    assert!(output.contains("Started env "));
    assert!(!output.contains("healthy: true"));
    assert!(!output.contains("officialReleaseReady: true"));
    assert!(!output.contains("check: category=official-release"));
    assert!(!output.contains("tool: git"));

    let list = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let envs: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(envs.as_array().unwrap().len(), 1);
}

#[test]
fn start_reports_recovery_steps_when_onboarding_fails() {
    let root = TestDir::new("start-onboarding-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let failing_openclaw = root.child("bin/failing-openclaw");
    write_executable_script(&failing_openclaw, "#!/bin/sh\nexit 1\n");

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--command",
            &path_string(&failing_openclaw),
            "--no-service",
            "--onboard",
        ],
    );
    assert_eq!(start.status.code(), Some(1));
    assert!(stdout(&start).contains("Started env demo"));
    let error = stderr(&start);
    assert!(error.contains("env demo is ready, but onboarding exited with code 1"));
    assert!(error.contains("retry: ocm @demo -- onboard"));
    assert!(error.contains("run: ocm @demo -- status"));
    assert!(error.contains("keep running: ocm service install demo"));
    assert!(!error.contains("Run \"ocm help\" for usage."));
}

#[test]
fn start_onboards_before_activating_a_new_service() {
    for service_requested in [true, false] {
        let root = TestDir::new("start-onboarding-order");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
        install_fake_launchctl(&root, &mut env);
        let peer = root.child("bin/openclaw");
        write_executable_script(
            &peer,
            "#!/bin/sh\nset -eu\n[ \"$1\" = onboard ]\n[ \"$2\" = --no-install-daemon ]\ncp \"$OCM_HOME/envs.json\" \"$OCM_HOME/during-onboarding.json\"\nprintf '{\"gateway\":{\"mode\":\"local\"}}\\n' > \"$OPENCLAW_CONFIG_PATH\"\n",
        );
        let peer_path = path_string(&peer);
        let mut args = vec!["start", "demo", "--command", &peer_path, "--onboard"];
        if !service_requested {
            args.push("--no-service");
        }
        let start = run_ocm(&cwd, &env, &args);
        assert!(start.status.success(), "{}", stderr(&start));
        let during: Value = serde_json::from_str(
            &fs::read_to_string(root.child("ocm-home/during-onboarding.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(during["envs"][0]["serviceRunning"], false);
        let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        let after: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_eq!(after["serviceRunning"], service_requested);
        assert_eq!(
            stdout(&start).contains("Service running for env demo"),
            service_requested
        );

        // Reusing a configured env still reaches onboarding and keeps the
        // existing service policy while the command runs.
        let repeated = run_ocm(&cwd, &env, &args);
        assert!(repeated.status.success(), "{}", stderr(&repeated));
        let during: Value = serde_json::from_str(
            &fs::read_to_string(root.child("ocm-home/during-onboarding.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(during["envs"][0]["serviceRunning"], service_requested);
    }
}

#[test]
fn failed_or_cancelled_onboarding_does_not_activate_a_new_service() {
    for exit_code in [1, 130] {
        let root = TestDir::new("start-onboarding-stays-stopped");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
        install_fake_launchctl(&root, &mut env);
        let peer = root.child("bin/openclaw");
        write_executable_script(&peer, &format!("#!/bin/sh\nexit {exit_code}\n"));

        let start = run_ocm(
            &cwd,
            &env,
            &[
                "start",
                "demo",
                "--command",
                &path_string(&peer),
                "--onboard",
            ],
        );
        assert_eq!(start.status.code(), Some(exit_code), "{}", stderr(&start));
        assert!(stderr(&start).contains("retry: ocm start demo --onboard"));
        assert!(!stdout(&start).contains("Service running"));
        let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        let after: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_eq!(after["serviceRunning"], false);
        assert!(
            !root
                .child("ocm-home/envs/demo/.openclaw/openclaw.json")
                .exists()
        );
        let log = fs::read_to_string(root.child("launchctl.log")).unwrap_or_default();
        assert!(!log.contains("bootstrap gui/"), "{log}");
    }
}

#[test]
fn failed_onboarding_preserves_an_existing_service() {
    let root = TestDir::new("start-onboarding-existing-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
    install_fake_launchctl(&root, &mut env);
    let peer = root.child("bin/openclaw");
    write_executable_script(&peer, "#!/bin/sh\nexit 1\n");
    let start = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--command", &path_string(&peer)],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    fs::write(root.child("launchctl.log"), "").unwrap();

    let onboard = run_ocm(&cwd, &env, &["start", "demo", "--onboard"]);
    assert_eq!(onboard.status.code(), Some(1));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let after: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(after["serviceRunning"], true);
    let log = fs::read_to_string(root.child("launchctl.log")).unwrap();
    assert!(!log.contains("bootout"), "{log}");
    assert!(!log.contains("bootstrap"), "{log}");
}
