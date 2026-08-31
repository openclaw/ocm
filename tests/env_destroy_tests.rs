mod support;

use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::thread::sleep;
#[cfg(unix)]
use std::time::{Duration, Instant};

use ocm::env::EnvDevMeta;
use ocm::store::{get_environment, save_environment};

use crate::support::{
    TestDir, managed_service_definition_path, ocm_env, path_string, run_ocm, stderr, stdout,
    write_executable_script,
};

fn install_fake_launchctl(root: &TestDir, env: &mut BTreeMap<String, String>) {
    let bin_dir = root.child("fake-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let log_path = root.child("launchctl.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\ncase \"$1\" in\n  print)\n    printf 'Could not find service\\n' >&2\n    exit 1\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
        path_string(&log_path)
    );
    write_executable_script(&bin_dir.join("launchctl"), &script);

    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(&bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(&bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
}

fn ocm_launchd_env(root: &TestDir) -> BTreeMap<String, String> {
    let mut env = ocm_env(root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    env
}

fn init_openclaw_repo(root: &TestDir) -> PathBuf {
    let repo = root.child("repo/openclaw");
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::write(
        repo.join("package.json"),
        r#"{"name":"openclaw","version":"2026.4.19"}"#,
    )
    .unwrap();
    fs::write(repo.join("scripts/run-node.mjs"), "console.log('run');\n").unwrap();
    fs::write(
        repo.join("scripts/watch-node.mjs"),
        "console.log('watch');\n",
    )
    .unwrap();

    let init = Command::new("git").arg("init").arg(&repo).output().unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let email = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "config",
            "user.email",
            "tests@example.com",
        ])
        .output()
        .unwrap();
    assert!(
        email.status.success(),
        "{}",
        String::from_utf8_lossy(&email.stderr)
    );
    let name = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "config",
            "user.name",
            "OCM Tests",
        ])
        .output()
        .unwrap();
    assert!(
        name.status.success(),
        "{}",
        String::from_utf8_lossy(&name.stderr)
    );
    let add = Command::new("git")
        .args(["-C", &path_string(&repo), "add", "."])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("git")
        .args(["-C", &path_string(&repo), "commit", "-m", "init"])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );

    repo
}

fn prepend_fake_bin(env: &mut BTreeMap<String, String>, bin_dir: &Path) {
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
}

#[cfg(unix)]
fn run_ocm_through_path_wrapper(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    target_root: &Path,
    args: &[&str],
) -> std::process::Output {
    let mut command = Command::new("/bin/sh");
    command
        .current_dir(cwd)
        .arg("-c")
        .arg(
            "target_root=$1; shift; binary=$1; shift; \"$binary\" \"$@\"; status=$?; : \"$target_root\"; exit \"$status\"",
        )
        .arg("ocm-destroy-wrapper")
        .arg(target_root)
        .arg(env!("CARGO_BIN_EXE_ocm"))
        .args(args)
        .env_clear()
        .envs(env);
    command.output().unwrap()
}

fn install_fake_dev_runners(root: &TestDir, env: &mut BTreeMap<String, String>) {
    let bin_dir = root.child("fake-dev-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let pnpm_log = root.child("pnpm.log");
    let node_log = root.child("node.log");
    let pnpm = format!(
        "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' \"$PWD\" \"$OPENCLAW_CONFIG_PATH\" \"$OPENCLAW_GATEWAY_PORT\" \"$*\" >> \"{}\"\n",
        path_string(&pnpm_log)
    );
    let node = format!(
        "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' \"$PWD\" \"$OPENCLAW_CONFIG_PATH\" \"$OPENCLAW_GATEWAY_PORT\" \"$*\" >> \"{}\"\n",
        path_string(&node_log)
    );
    write_executable_script(&bin_dir.join("pnpm"), &pnpm);
    write_executable_script(&bin_dir.join("node"), &node);
    prepend_fake_bin(env, &bin_dir);
}

#[cfg(unix)]
fn wait_for_listener(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok() {
            return;
        }
        sleep(Duration::from_millis(50));
    }
    panic!("listener on port {port} did not start in time");
}

#[cfg(unix)]
fn wait_for_listener_port(
    ready_path: &Path,
    child: &mut std::process::Child,
    log_path: &Path,
) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(port_text) = fs::read_to_string(ready_path) {
            let port = port_text
                .trim()
                .parse::<u16>()
                .expect("listener helper wrote an invalid port");
            wait_for_listener(port);
            return port;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let log = fs::read_to_string(log_path).unwrap_or_default();
            panic!("listener helper exited before becoming ready: {status}; {log}");
        }
        sleep(Duration::from_millis(50));
    }
    let log = fs::read_to_string(log_path).unwrap_or_default();
    panic!("listener did not report a bound port in time; {log}");
}

#[test]
fn env_destroy_preview_reports_service_snapshot_and_env_steps() {
    let root = TestDir::new("env-destroy-preview");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);
    install_fake_launchctl(&root, &mut env);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "demo",
            "--label",
            "before-destroy",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let install = run_ocm(&cwd, &env, &["service", "install", "demo"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let output = stdout(&preview);
    assert!(output.contains("Destroy preview for env demo"));
    assert!(output.contains("snapshots: 1"));
    assert!(output.contains("service: ocm"));
    assert!(output.contains("snapshots: remove 1 env snapshot(s)"));
    assert!(output.contains("service: disable env gateway in the OCM background service"));
    assert!(output.contains("env: remove env root and metadata"));
    assert!(output.contains("re-run with --yes to destroy it"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));

    let json_preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(json_preview.status.success(), "{}", stderr(&json_preview));
    let json: serde_json::Value = serde_json::from_str(&stdout(&json_preview)).unwrap();
    assert_eq!(json["apply"], false);
    assert!(
        json["stateToken"]
            .as_str()
            .is_some_and(|token| token.starts_with("v1:"))
    );
}

#[test]
fn env_destroy_state_token_refuses_stale_service_state_without_teardown() {
    let root = TestDir::new("env-destroy-state-token");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);
    install_fake_launchctl(&root, &mut env);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    let stale_guard = preview_json["stateToken"].as_str().unwrap();

    let install = run_ocm(&cwd, &env, &["service", "install", "demo"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let guarded = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "destroy",
            "demo",
            "--yes",
            "--if-state-token",
            stale_guard,
            "--json",
        ],
    );
    assert!(!guarded.status.success());
    assert!(stderr(&guarded).is_empty(), "{}", stderr(&guarded));
    let guarded_json: serde_json::Value = serde_json::from_str(&stdout(&guarded)).unwrap();
    assert_eq!(guarded_json["code"], "state_changed");
    assert_eq!(guarded_json["removed"], false);
    assert_ne!(guarded_json["stateToken"], preview_json["stateToken"]);

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&status)).unwrap()["installed"],
        true
    );

    let mut destroy = None;
    for _ in 0..2 {
        let fresh_preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
        assert!(fresh_preview.status.success(), "{}", stderr(&fresh_preview));
        let fresh_json: serde_json::Value = serde_json::from_str(&stdout(&fresh_preview)).unwrap();
        let fresh_guard = fresh_json["stateToken"].as_str().unwrap();
        let attempt = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "destroy",
                "demo",
                "--yes",
                "--if-state-token",
                fresh_guard,
                "--json",
            ],
        );
        if attempt.status.success() {
            destroy = Some(attempt);
            break;
        }
        let output: serde_json::Value = serde_json::from_str(&stdout(&attempt)).unwrap();
        assert_eq!(output["code"], "state_changed");
    }
    let destroy = destroy.expect("fresh guarded destroy should converge after one state refresh");
    assert!(destroy.status.success(), "{}", stderr(&destroy));
    let destroy_json: serde_json::Value = serde_json::from_str(&stdout(&destroy)).unwrap();
    assert_eq!(destroy_json["removed"], true);
}

#[test]
fn env_destroy_state_token_requires_guarded_apply_mode() {
    let root = TestDir::new("env-destroy-state-token-options");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let without_yes = run_ocm(
        &cwd,
        &env,
        &["env", "destroy", "demo", "--if-state-token", "v1:test"],
    );
    assert!(!without_yes.status.success());
    assert!(stderr(&without_yes).contains("--if-state-token requires --yes"));

    let empty = run_ocm(
        &cwd,
        &env,
        &["env", "destroy", "demo", "--yes", "--if-state-token="],
    );
    assert!(!empty.status.success());
    assert!(stderr(&empty).contains("--if-state-token requires a non-empty value"));

    let forced = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "destroy",
            "demo",
            "--yes",
            "--force",
            "--if-state-token",
            "v1:test",
        ],
    );
    assert!(!forced.status.success());
    assert!(stderr(&forced).contains("accepts only one of --force or --if-state-token"));
}

#[test]
fn env_destroy_state_token_refuses_unpreviewed_snapshots() {
    let root = TestDir::new("env-destroy-state-token-snapshot");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    let stale_guard = preview_json["stateToken"].as_str().unwrap();

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "demo"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let guarded = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "destroy",
            "demo",
            "--yes",
            "--if-state-token",
            stale_guard,
            "--json",
        ],
    );
    assert!(!guarded.status.success());
    let guarded_json: serde_json::Value = serde_json::from_str(&stdout(&guarded)).unwrap();
    assert_eq!(guarded_json["code"], "state_changed");
    assert_eq!(guarded_json["removed"], false);

    let snapshots = run_ocm(&cwd, &env, &["env", "snapshot", "list", "demo", "--json"]);
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&snapshots))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn env_destroy_yes_uninstalls_service_removes_snapshots_and_deletes_env() {
    let root = TestDir::new("env-destroy-apply");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);
    install_fake_launchctl(&root, &mut env);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "demo",
            "--label",
            "before-destroy",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let history_root = root.child("ocm-home/upgrade-history");
    fs::create_dir_all(&history_root).unwrap();
    fs::write(history_root.join("demo"), "block linked recovery traversal").unwrap();

    let install = run_ocm(&cwd, &env, &["service", "install", "demo"]);
    assert!(install.status.success(), "{}", stderr(&install));
    let service_path = managed_service_definition_path(&env, &cwd, "ocm");

    let destroy = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
    assert!(destroy.status.success(), "{}", stderr(&destroy));
    let output = stdout(&destroy);
    assert!(output.contains("Destroyed env demo"));
    assert!(output.contains("snapshots removed: 1"));
    assert!(output.contains("service removed: ocm"));
    assert!(
        output.contains("warning: snapshot")
            && output.contains("linked upgrade recovery cleanup failed"),
        "{output}"
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(!show.status.success());
    assert!(stderr(&show).contains("environment \"demo\" does not exist"));

    assert!(!service_path.exists());
    assert!(!root.child("ocm-home/snapshots/demo").exists());
}

#[test]
fn env_destroy_requires_force_for_protected_envs() {
    let root = TestDir::new("env-destroy-protected");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo", "--protect"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let blocked = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
    assert!(!blocked.status.success());
    let output = stdout(&blocked);
    assert!(output.contains("Destroy preview for env demo"));
    assert!(output.contains("env is protected; re-run with --force to destroy it"));

    let forced = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes", "--force"]);
    assert!(forced.status.success(), "{}", stderr(&forced));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(!show.status.success());
}

#[test]
fn env_destroy_yes_removes_dev_worktree() {
    let root = TestDir::new("env-destroy-dev-worktree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);
    install_fake_launchctl(&root, &mut env);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));

    let worktree = repo.join(".worktrees/demo");
    assert!(worktree.exists());

    let destroy = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
    assert!(destroy.status.success(), "{}", stderr(&destroy));
    assert!(!worktree.exists(), "dev worktree should be removed");
}

#[test]
fn env_destroy_preserves_recovery_data_when_worktree_removal_fails() {
    let root = TestDir::new("env-destroy-worktree-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "demo"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let invalid_worktree = root.child("invalid-worktree");
    fs::write(&invalid_worktree, "not a directory").unwrap();
    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    meta.dev = Some(EnvDevMeta {
        repo_root: path_string(&root.child("missing-repo")),
        worktree_root: path_string(&invalid_worktree),
    });
    let env_root = PathBuf::from(&meta.root);
    save_environment(meta, &env, &cwd).unwrap();

    let destroy = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
    assert!(!destroy.status.success());
    assert!(env_root.exists(), "env root should remain recoverable");
    assert!(get_environment("demo", &env, &cwd).is_ok());

    let snapshots = run_ocm(&cwd, &env, &["env", "snapshot", "list", "demo", "--json"]);
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&snapshots))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn env_destroy_yes_terminates_live_listener_processes() {
    let root = TestDir::new("env-destroy-live-processes");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);
    install_fake_launchctl(&root, &mut env);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));

    let created = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(created.status.success(), "{}", stderr(&created));

    let server = root.child("listener.py");
    let ready = root.child("listener-ready.txt");
    let second_ready = root.child("listener-ready-2.txt");
    let listener_log = root.child("listener.log");
    let second_listener_log = root.child("listener-2.log");
    fs::write(
        &server,
        r#"import argparse, os, socket, time
parser = argparse.ArgumentParser()
parser.add_argument("--ready", required=True)
args = parser.parse_args()
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0))
s.listen(1)
with open(args.ready + ".tmp", "w", encoding="utf-8") as ready:
    ready.write(str(s.getsockname()[1]))
os.replace(args.ready + ".tmp", args.ready)
time.sleep(60)
"#,
    )
    .unwrap();

    let listener_output = fs::File::create(&listener_log).unwrap();
    let listener_error = listener_output.try_clone().unwrap();
    let mut child = Command::new("python3")
        .current_dir(root.child("ocm-home/envs/demo"))
        .arg(&server)
        .arg("--ready")
        .arg(&ready)
        .stdin(Stdio::null())
        .stdout(Stdio::from(listener_output))
        .stderr(Stdio::from(listener_error))
        .spawn()
        .unwrap();
    let second_listener_output = fs::File::create(&second_listener_log).unwrap();
    let second_listener_error = second_listener_output.try_clone().unwrap();
    let mut second_child = Command::new("python3")
        .current_dir(root.child("ocm-home/envs/demo"))
        .arg(&server)
        .arg("--ready")
        .arg(&second_ready)
        .stdin(Stdio::null())
        .stdout(Stdio::from(second_listener_output))
        .stderr(Stdio::from(second_listener_error))
        .spawn()
        .unwrap();

    let port = wait_for_listener_port(&ready, &mut child, &listener_log);
    let second_port =
        wait_for_listener_port(&second_ready, &mut second_child, &second_listener_log);

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    assert!(
        preview_json["processCount"]
            .as_u64()
            .is_some_and(|count| count >= 2)
    );
    assert!(preview_json.get("processCandidates").is_none());
    let guard = preview_json["stateToken"].as_str().unwrap();
    let second_preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(
        second_preview.status.success(),
        "{}",
        stderr(&second_preview)
    );
    let second_preview_json: serde_json::Value =
        serde_json::from_str(&stdout(&second_preview)).unwrap();
    assert_eq!(
        second_preview_json["stateToken"],
        preview_json["stateToken"]
    );
    let destroy = run_ocm(
        &cwd,
        &env,
        &["env", "destroy", "demo", "--yes", "--if-state-token", guard],
    );
    assert!(destroy.status.success(), "{}", stderr(&destroy));

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() && second_child.try_wait().unwrap().is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    assert!(
        child.try_wait().unwrap().is_some(),
        "listener should exit after destroy"
    );
    assert!(
        second_child.try_wait().unwrap().is_some(),
        "second listener should exit after destroy"
    );
    assert!(
        TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_err(),
        "listener port should be closed after destroy"
    );
    assert!(
        TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, second_port)).is_err(),
        "second listener port should be closed after destroy"
    );
}

#[cfg(unix)]
#[test]
fn env_destroy_ignores_processes_in_sibling_prefix_paths() {
    let root = TestDir::new("env-destroy-sibling-prefix");
    let cwd = root.child("workspace");
    let sibling_root = root.child("ocm-home/envs/demo-sibling");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sibling_root).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let mut sibling_process = Command::new("python3")
        .current_dir(&sibling_root)
        .args(["-c", "import time; time.sleep(60)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(100));

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    assert_eq!(preview_json["processCount"], 0);
    assert!(
        sibling_process.try_wait().unwrap().is_none(),
        "destroy preview must not terminate a process from a sibling prefix path"
    );

    sibling_process.kill().unwrap();
    sibling_process.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn env_destroy_ignores_its_invocation_wrapper_when_the_command_mentions_the_target_root() {
    let root = TestDir::new("env-destroy-invocation-wrapper");
    let cwd = root.child("workspace");
    let sibling_root = root.child("ocm-home/envs/demo-sibling");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&sibling_root).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let stopped = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let target_root = PathBuf::from(get_environment("demo", &env, &cwd).unwrap().root);

    let mut sibling_process = Command::new("python3")
        .current_dir(&sibling_root)
        .args(["-c", "import time; time.sleep(60)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(100));

    let preview = run_ocm_through_path_wrapper(
        &cwd,
        &env,
        &target_root,
        &["env", "destroy", "demo", "--json"],
    );
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    assert_eq!(preview_json["processCount"], 0);
    let guard = preview_json["stateToken"].as_str().unwrap();

    let destroy = run_ocm_through_path_wrapper(
        &cwd,
        &env,
        &target_root,
        &[
            "env",
            "destroy",
            "demo",
            "--yes",
            "--if-state-token",
            guard,
            "--json",
        ],
    );
    assert!(destroy.status.success(), "{}", stderr(&destroy));
    assert!(
        sibling_process.try_wait().unwrap().is_none(),
        "destroy must not terminate an independent sibling process"
    );

    sibling_process.kill().unwrap();
    sibling_process.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn env_destroy_partial_apply_preserves_completed_process_count() {
    let root = TestDir::new("env-destroy-partial-process-count");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let mut first = Command::new("python3")
        .current_dir(root.child("ocm-home/envs/demo"))
        .args(["-c", "import time; time.sleep(60)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut second = Command::new("python3")
        .current_dir(root.child("ocm-home/envs/demo"))
        .args(["-c", "import time; time.sleep(60)"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(100));

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    assert_eq!(preview_json["processCount"], 2);
    let guard = preview_json["stateToken"].as_str().unwrap();

    let bin_dir = root.child("selective-kill-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let kill_script = format!(
        "#!/bin/sh\nif [ \"$2\" = \"{}\" ]; then exit 0; fi\nexec /bin/kill \"$@\"\n",
        second.id()
    );
    write_executable_script(&bin_dir.join("kill"), &kill_script);
    prepend_fake_bin(&mut env, &bin_dir);

    let destroy = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "destroy",
            "demo",
            "--yes",
            "--if-state-token",
            guard,
            "--json",
        ],
    );
    assert!(!destroy.status.success());
    let destroy_json: serde_json::Value = serde_json::from_str(&stdout(&destroy)).unwrap();
    assert_eq!(destroy_json["code"], "partial_apply");
    assert_eq!(destroy_json["processesTerminated"], 1);
    let _ = first.wait();
    assert!(first.try_wait().unwrap().is_some());
    assert!(second.try_wait().unwrap().is_none());

    let _ = Command::new("/bin/kill")
        .args(["-KILL", &second.id().to_string()])
        .status();
    let _ = second.wait();
    let cleanup = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
}

#[cfg(unix)]
#[test]
fn env_destroy_reports_partial_apply_when_process_state_changes_during_teardown() {
    let root = TestDir::new("env-destroy-partial-process-change");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let helper = root.child("replace-on-term.py");
    let ready = root.child("replace-ready.txt");
    let replacement_pid = root.child("replacement-pid.txt");
    let helper_log = root.child("replace-on-term.log");
    fs::write(
        &helper,
        r#"import os, signal, subprocess, sys, time
ready_path, replacement_pid_path = sys.argv[1:3]
def replace(_signum, _frame):
    replacement = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        cwd=os.getcwd(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    with open(replacement_pid_path + ".tmp", "w", encoding="utf-8") as output:
        output.write(str(replacement.pid))
    os.replace(replacement_pid_path + ".tmp", replacement_pid_path)
    raise SystemExit(0)
signal.signal(signal.SIGTERM, replace)
with open(ready_path, "w", encoding="utf-8") as output:
    output.write("ready")
    output.flush()
while True:
    time.sleep(1)
"#,
    )
    .unwrap();
    let output = fs::File::create(&helper_log).unwrap();
    let error = output.try_clone().unwrap();
    let mut child = Command::new("python3")
        .current_dir(root.child("ocm-home/envs/demo"))
        .arg(&helper)
        .arg(&ready)
        .arg(&replacement_pid)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error))
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            let _ = child.wait();
            let log = fs::read_to_string(&helper_log).unwrap_or_default();
            panic!("replacement helper exited before becoming ready: {status}; {log}");
        }
        sleep(Duration::from_millis(50));
    }
    if !ready.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("replacement helper did not become ready");
    }

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    let preview_json: serde_json::Value = serde_json::from_str(&stdout(&preview)).unwrap();
    let guard = preview_json["stateToken"].as_str().unwrap();
    let destroy = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "destroy",
            "demo",
            "--yes",
            "--if-state-token",
            guard,
            "--json",
        ],
    );
    assert!(!destroy.status.success());
    assert!(stderr(&destroy).is_empty(), "{}", stderr(&destroy));
    let destroy_json: serde_json::Value = serde_json::from_str(&stdout(&destroy)).unwrap();
    assert_eq!(destroy_json["code"], "partial_apply");
    assert_eq!(destroy_json["removed"], false);
    assert_eq!(destroy_json["processesTerminated"], 1);
    assert!(
        destroy_json["blockers"]
            .as_array()
            .is_some_and(|blockers| blockers.iter().any(|blocker| blocker
                .as_str()
                .is_some_and(|message| message.contains("teardown began"))))
    );
    let helper_status = child.wait().unwrap();
    assert!(helper_status.success());

    let replacement_pid = fs::read_to_string(&replacement_pid)
        .unwrap()
        .trim()
        .to_string();
    let _ = Command::new("kill")
        .args(["-KILL", &replacement_pid])
        .status();
    let cleanup = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
}

#[test]
fn env_destroy_refuses_to_issue_a_guard_when_process_inspection_fails() {
    let root = TestDir::new("env-destroy-process-inspection-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_launchd_env(&root);

    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    let bin_dir = root.child("failing-ps-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("ps"), "#!/bin/sh\nexit 1\n");
    prepend_fake_bin(&mut env, &bin_dir);

    let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
    assert!(!preview.status.success());
    assert!(stdout(&preview).is_empty());
    assert!(stderr(&preview).contains("failed to inspect running processes"));
}
