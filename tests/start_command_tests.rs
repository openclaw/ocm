mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, hold_environment_operation, install_fake_launchctl,
    install_fake_managed_node_archive, install_fake_node_and_npm, install_fake_systemd_tools,
    ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr, stdout, write_executable_script,
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
    assert_launcher_absent(&cwd, &env, "demo.local");

    // A corrected retry from another directory behaves like a first start.
    let checkout = root.child("checkout");
    fs::create_dir_all(&checkout).unwrap();
    let retry = run_ocm(
        &checkout,
        &env,
        &[
            "start",
            "demo",
            "--command",
            "pnpm openclaw",
            "--cwd",
            ".",
            "--no-service",
        ],
    );
    assert!(retry.status.success(), "{}", stderr(&retry));
    let launcher = run_ocm(&cwd, &env, &["launcher", "show", "demo.local", "--json"]);
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let launcher: Value = serde_json::from_str(&stdout(&launcher)).unwrap();
    assert_eq!(launcher["command"], "pnpm openclaw");
    assert_eq!(
        launcher["cwd"],
        path_string(&checkout.canonicalize().unwrap())
    );
}

fn assert_launcher_absent(cwd: &Path, env: &BTreeMap<String, String>, name: &str) {
    let show = run_ocm(cwd, env, &["launcher", "show", name]);
    assert_eq!(show.status.code(), Some(1));
    assert!(
        stderr(&show).contains(&format!("launcher \"{name}\" does not exist")),
        "{}",
        stderr(&show)
    );
}

#[test]
fn start_refused_by_a_non_empty_root_removes_the_launcher_it_created() {
    let root = TestDir::new("start-refused-root-launcher");
    let cwd = root.child("workspace");
    let occupied = root.child("occupied");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&occupied).unwrap();
    fs::write(occupied.join("keep.txt"), "user data").unwrap();
    let env = ocm_env(&root);
    let occupied_arg = path_string(&occupied);

    let refused = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--command",
            "openclaw",
            "--root",
            &occupied_arg,
            "--no-service",
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("root already exists and is not empty"),
        "{}",
        stderr(&refused)
    );
    assert_launcher_absent(&cwd, &env, "demo.local");
    assert_eq!(
        fs::read_to_string(occupied.join("keep.txt")).unwrap(),
        "user data"
    );

    let retry = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--command",
            "pnpm openclaw",
            "--no-service",
        ],
    );
    assert!(retry.status.success(), "{}", stderr(&retry));
    assert!(stdout(&retry).contains("launcher: demo.local"));
}

#[test]
fn start_refused_for_an_existing_env_removes_only_a_launcher_it_created() {
    let root = TestDir::new("start-refused-existing-launcher");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "unsupported".to_string(),
    );

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let refused = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("managed services are not supported on this platform yet"));
    assert_launcher_absent(&cwd, &env, "demo.local");
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert!(show["defaultLauncher"].is_null());

    // A launcher that already existed is reused by the refused start and kept.
    let added = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "demo.local", "--command", "openclaw"],
    );
    assert!(added.status.success(), "{}", stderr(&added));
    let launcher_path = root.child("ocm-home/launchers/demo.local.json");
    let original = fs::read(&launcher_path).unwrap();
    let refused = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("managed services are not supported on this platform yet"));
    assert_eq!(fs::read(&launcher_path).unwrap(), original);
}

#[test]
fn start_refused_by_unreadable_config_removes_the_launcher_it_created() {
    let root = TestDir::new("start-refused-config-launcher");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let config_path = root.child("ocm-home/envs/demo/.openclaw/openclaw.json");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(&config_path, "{ not json").unwrap();

    let refused = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--command", "openclaw", "--no-service"],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert_launcher_absent(&cwd, &env, "demo.local");
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert!(show["defaultLauncher"].is_null());
    assert_eq!(fs::read_to_string(&config_path).unwrap(), "{ not json");

    fs::remove_file(&config_path).unwrap();
    let retry = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--command",
            "pnpm openclaw",
            "--no-service",
        ],
    );
    assert!(retry.status.success(), "{}", stderr(&retry));
    assert!(stdout(&retry).contains("launcher: demo.local"));
}

// A failure after the env was created and bound (here the service install)
// happens outside the refusal rollback, so the env keeps its launcher. This
// does not reach the bound-launcher check in discard_unbound_start_launcher,
// which only covers a create or rebind that fails after saving the env.
#[test]
fn start_failing_after_creating_the_env_keeps_its_launcher() {
    let root = TestDir::new("start-bound-launcher-kept");
    let cwd = root.child("workspace");
    let bin_dir = root.child("fake-bin");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();
    let launchctl = bin_dir.join("launchctl");
    write_executable_script(
        &launchctl,
        "#!/bin/sh\ncase \"$1\" in\n  bootstrap) echo 'Bootstrap failed: 5: Input/output error' >&2; exit 5;;\n  print) echo 'Could not find service' >&2; exit 113;;\n  *) exit 0;;\nesac\n",
    );
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    env.insert(
        "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
        path_string(&launchctl),
    );

    let failed = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(failed.status.code(), Some(1), "{}", stdout(&failed));
    assert!(
        !stderr(&failed).contains("rollback error"),
        "{}",
        stderr(&failed)
    );
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show["defaultLauncher"], "demo.local");
    let launcher = run_ocm(&cwd, &env, &["launcher", "show", "demo.local"]);
    assert!(launcher.status.success(), "{}", stderr(&launcher));
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
fn start_does_not_recreate_an_env_removed_while_it_waits_for_the_operation_lock() {
    let root = TestDir::new("start-removed-env");
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
    let env_root = PathBuf::from(
        ocm::store::get_environment("demo", &env, &cwd)
            .unwrap()
            .root,
    );
    let registry = ocm::store::env_registry_path(&env, &cwd).unwrap();

    let operation = hold_environment_operation(&root, "demo");
    let mut start = Command::new(ocm_test_binary_path())
        .current_dir(&cwd)
        .args(["start", "demo", "--no-service"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        assert!(
            start.try_wait().unwrap().is_none(),
            "start finished while the environment operation lock was held"
        );
        thread::sleep(Duration::from_millis(50));
    }

    // Apply what `env remove` does under the operation lock: drop the
    // registry entry and delete the root.
    let mut registry_value: Value =
        serde_json::from_str(&fs::read_to_string(&registry).unwrap()).unwrap();
    registry_value["envs"]
        .as_array_mut()
        .unwrap()
        .retain(|meta| meta["name"] != "demo");
    fs::write(
        &registry,
        serde_json::to_vec_pretty(&registry_value).unwrap(),
    )
    .unwrap();
    fs::remove_dir_all(&env_root).unwrap();
    drop(operation);

    let output = start.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "start reported success for a removed env: {}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("environment \"demo\" does not exist"),
        "{}",
        stderr(&output)
    );
    assert!(!stdout(&output).contains("Using env demo"));
    assert!(
        !env_root.exists(),
        "start recreated the removed env root at {}",
        env_root.display()
    );
}

#[test]
fn start_preserves_included_settings_and_exported_workspace() {
    for shape in ["root-include", "inline", "root-siblings"] {
        let root = TestDir::new(shape);
        let cwd = root.child("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        for args in [
            vec!["launcher", "add", "stable", "--command", "openclaw"],
            vec!["env", "create", "demo", "--launcher", "stable"],
        ] {
            let output = run_ocm(&cwd, &env, &args);
            assert!(output.status.success(), "{}", stderr(&output));
        }
        let state = root.child("ocm-home/envs/demo/.openclaw");
        let workspace = state.join("team/authored");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("notes.txt"), "authored workspace").unwrap();
        let authored = serde_json::json!({
            "gateway": {"mode": "local", "bind": "lan", "port": 28431},
            "agents": {"defaults": {"workspace": path_string(&workspace)}}
        });
        let included = serde_json::to_vec(&authored).unwrap();
        fs::write(state.join("base.json"), &included).unwrap();
        let config = match shape {
            "root-include" => serde_json::json!({"$include": "./base.json"}),
            "inline" => authored.clone(),
            _ => serde_json::json!({
                "$include": "./base.json",
                "gateway": {"bind": "loopback"},
                "agents": {"defaults": {"workspace": path_string(&workspace)}}
            }),
        };
        let config_path = state.join("openclaw.json");
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        for phase in ["before", "after"] {
            if phase == "after" {
                let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service", "--json"]);
                assert!(start.status.success(), "{}", stderr(&start));
                let summary: Value = serde_json::from_str(&stdout(&start)).unwrap();
                assert_eq!(summary["gatewayPort"], 28431);
                assert_eq!(summary["serviceStarted"], false);
            }
            let archive_path = root.child(format!("{phase}.tar"));
            let export = run_ocm(
                &cwd,
                &env,
                &[
                    "env",
                    "export",
                    "demo",
                    "--output",
                    &path_string(&archive_path),
                ],
            );
            assert!(export.status.success(), "{}", stderr(&export));
            let mut archive = tar::Archive::new(fs::File::open(archive_path).unwrap());
            assert!(
                archive.entries().unwrap().any(|entry| {
                    entry.unwrap().path().unwrap()
                        == std::path::Path::new("root/.openclaw/team/authored/notes.txt")
                }),
                "{shape} {phase} lost authored workspace from export"
            );
        }
        let raw: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        if shape == "root-include" {
            assert_eq!(raw["$include"], "./base.json");
            assert!(raw["gateway"].get("bind").is_none());
            assert!(raw.get("agents").is_none());
        } else {
            assert_eq!(
                raw["gateway"]["bind"],
                if shape == "inline" { "lan" } else { "loopback" }
            );
            assert_eq!(
                raw["agents"]["defaults"]["workspace"],
                path_string(&workspace)
            );
        }
        assert_eq!(fs::read(state.join("base.json")).unwrap(), included);
        assert_eq!(
            fs::read_to_string(workspace.join("notes.txt")).unwrap(),
            "authored workspace"
        );
        let once = fs::read(&config_path).unwrap();
        let repeat = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
        assert!(repeat.status.success(), "{}", stderr(&repeat));
        assert_eq!(fs::read(&config_path).unwrap(), once);
    }
}

#[test]
fn start_minimum_setup_does_not_traverse_unrelated_external_includes() {
    for shape in ["complete-agents", "missing-bind", "internal-root"] {
        let root = TestDir::new(shape);
        let cwd = root.child("cwd");
        let shared = root.child("shared");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&shared).unwrap();
        let mut env = ocm_env(&root);
        env.insert("OPENCLAW_INCLUDE_ROOTS".to_string(), path_string(&shared));
        for args in [
            vec!["launcher", "add", "stable", "--command", "openclaw"],
            vec![
                "env",
                "create",
                "demo",
                "--launcher",
                "stable",
                "--port",
                "28431",
            ],
        ] {
            let result = run_ocm(&cwd, &env, &args);
            assert!(result.status.success(), "{}", stderr(&result));
        }
        let state = root.child("ocm-home/envs/demo/.openclaw");
        let workspace = state.join("team");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("marker"), "authored data").unwrap();
        let external = shared.join("settings.json5");
        let payload = if shape == "complete-agents" {
            "{list:[{id:'main'}]}\n"
        } else {
            "{providers:{}}\n"
        };
        fs::write(&external, payload).unwrap();
        let mut authored = serde_json::json!({
            "gateway": {"mode": "local", "bind": "lan", "port": 28431},
            "agents": {"defaults": {"workspace": path_string(&workspace)}}
        });
        if shape == "complete-agents" {
            authored["agents"]["$include"] = serde_json::json!(path_string(&external));
        } else {
            authored["models"] = serde_json::json!({"$include": path_string(&external)});
        }
        if shape == "missing-bind" {
            authored["gateway"].as_object_mut().unwrap().remove("bind");
        }
        let included = serde_json::to_vec(&authored).unwrap();
        let config = if shape == "internal-root" {
            fs::write(state.join("base.json"), &included).unwrap();
            serde_json::json!({"$include": "./base.json"})
        } else {
            authored
        };
        let config_path = state.join("openclaw.json");
        let original = serde_json::to_vec(&config).unwrap();
        fs::write(&config_path, &original).unwrap();
        let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
        assert!(start.status.success(), "{shape}: {}", stderr(&start));
        let raw: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        match shape {
            "complete-agents" => assert_eq!(fs::read(&config_path).unwrap(), original),
            "missing-bind" => assert_eq!(raw["gateway"]["bind"], "loopback"),
            _ => {
                assert_eq!(raw["$include"], "./base.json");
                assert!(raw["gateway"].get("bind").is_none());
                assert!(raw.get("agents").is_none());
                assert_eq!(fs::read(state.join("base.json")).unwrap(), included);
            }
        }
        assert_eq!(fs::read_to_string(external).unwrap(), payload);
        assert_eq!(
            fs::read_to_string(workspace.join("marker")).unwrap(),
            "authored data"
        );
    }
}

#[test]
fn start_uses_admitted_launcher_include_roots_and_refuses_unknown_required_settings() {
    let root = TestDir::new("start-admitted-includes");
    let cwd = root.child("cwd");
    let launcher_cwd = root.child("launcher");
    let shared = launcher_cwd.join("shared");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&shared).unwrap();
    let mut env = ocm_env(&root);
    let add = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "stable",
            "--command",
            "openclaw",
            "--cwd",
            &path_string(&launcher_cwd),
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
            "stable",
            "--port",
            "28431",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let state = root.child("ocm-home/envs/demo/.openclaw");
    let include = shared.join("config.json5");
    let contents = serde_json::json!({"gateway":{"mode":"local","bind":"lan","port":28432},"agents":{"defaults":{"workspace":path_string(&state.join("custom"))}}}).to_string();
    fs::write(&include, &contents).unwrap();
    let config_path = state.join("openclaw.json");
    let original = serde_json::json!({"$include":path_string(&include),"env":{"OPENCLAW_INCLUDE_ROOTS":path_string(&shared)}}).to_string();
    fs::write(&config_path, &original).unwrap();
    let refused = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("outside the admitted configuration roots"));
    assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
    let changed = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--command",
            "different-openclaw",
            "--no-service",
        ],
    );
    assert!(!changed.status.success());
    let shown = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&shown)).unwrap()["defaultLauncher"],
        "stable"
    );
    env.insert("OPENCLAW_INCLUDE_ROOTS".into(), "shared".into());
    let started = run_ocm(&cwd, &env, &["start", "demo", "--no-service", "--json"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let summary: Value = serde_json::from_str(&stdout(&started)).unwrap();
    assert_eq!(summary["gatewayPort"], 28431);
    let raw: Value = serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(raw["gateway"], serde_json::json!({"port":28431}));
    assert!(raw.get("agents").is_none());
    assert_eq!(fs::read_to_string(&include).unwrap(), contents);
    let once = fs::read(&config_path).unwrap();
    let repeated = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(repeated.status.success(), "{}", stderr(&repeated));
    assert_eq!(fs::read(&config_path).unwrap(), once);
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
    let _managed_node = install_fake_managed_node_archive(&root, &mut env, "24.22.1");

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

#[test]
fn start_rerun_keeps_a_running_service_in_the_supervisor_plan() {
    for systemd in [false, true] {
        let root = TestDir::new("start-rerun-keeps-running-service");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        let binary = if systemd {
            install_fake_systemd_tools(&root, &mut env);
            "systemctl"
        } else {
            env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
            install_fake_launchctl(&root, &mut env);
            "launchctl"
        };
        let peer = root.child("bin/openclaw");
        write_executable_script(&peer, "#!/bin/sh\nexit 0\n");
        let first = run_ocm(
            &cwd,
            &env,
            &["start", "demo", "--command", &path_string(&peer)],
        );
        assert!(first.status.success(), "{}", stderr(&first));
        let state_path = root.child("ocm-home/supervisor/state.json");
        let initial: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(
            initial["children"].as_array().unwrap().len(),
            1,
            "{initial}"
        );

        // The daemon acts on whatever plan it reads, so record the persisted
        // plan at every service-manager call the rerun makes.
        let snapshots = root.child("plan-snapshots");
        fs::create_dir_all(&snapshots).unwrap();
        let manager_path = root.child(format!("fake-bin/{binary}"));
        let original = fs::read_to_string(&manager_path).unwrap();
        write_executable_script(
            &manager_path,
            &format!(
                "#!/bin/sh\nn=$(ls '{snapshots}' | wc -l | tr -d ' ')\ncp '{state}' '{snapshots}/'\"$n\".json\n{original}",
                snapshots = path_string(&snapshots),
                state = path_string(&state_path),
            ),
        );

        let rerun = run_ocm(&cwd, &env, &["start", "demo", "--json"]);
        assert!(rerun.status.success(), "{}", stderr(&rerun));
        let summary: Value = serde_json::from_str(&stdout(&rerun)).unwrap();
        assert_eq!(summary["created"], false);
        assert_eq!(summary["serviceStarted"], true);
        let mut observed = 0;
        for entry in fs::read_dir(&snapshots).unwrap() {
            let path = entry.unwrap().path();
            let plan: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                plan["children"],
                initial["children"],
                "rerun published a plan without the running child ({})",
                path.display()
            );
            observed += 1;
        }
        assert!(observed > 0, "the rerun made no service-manager call");
        let after: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(after["children"], initial["children"]);
        assert_eq!(after["restartRequests"], initial["restartRequests"]);

        // Start still activates a stopped service, reinstalling the daemon
        // that stopping the last running service removed.
        fs::write(&manager_path, original).unwrap();
        let stopped = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
        assert!(stopped.status.success(), "{}", stderr(&stopped));
        let restarted = run_ocm(&cwd, &env, &["start", "demo", "--json"]);
        assert!(restarted.status.success(), "{}", stderr(&restarted));
        let resumed: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(
            resumed["children"].as_array().unwrap().len(),
            1,
            "{resumed}"
        );
        let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        let shown: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_eq!(shown["serviceEnabled"], true);
        assert_eq!(shown["serviceRunning"], true);
    }
}
