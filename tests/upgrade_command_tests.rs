mod support;

use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, sleep};
use std::time::Duration;

use base64::Engine;
use flate2::{Compression, write::GzEncoder};
use ocm::store::{env_registry_path, now_utc, supervisor_runtime_path, supervisor_state_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::Value;
use sha2::{Digest, Sha512};
use tar::{Builder, Header};

use crate::support::{
    TestDir, TestHttpServer, install_fake_launchctl, install_fake_node_and_npm, ocm_env,
    path_string, run_ocm, stderr, stdout, write_executable_script, write_text,
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

fn openclaw_package_tarball(script_body: &str, version: &str) -> Vec<u8> {
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
            format!(
                "{{\"name\":\"openclaw\",\"version\":\"{version}\",\"bin\":{{\"openclaw\":\"openclaw.mjs\"}}}}"
            )
            .as_bytes(),
            0o644,
        );
        builder.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn write_running_supervisor_runtime(
    runtime_path: &Path,
    ocm_home: &str,
    binding_name: &str,
    pid: u32,
    child_port: u32,
) {
    let log_root = runtime_path.parent().unwrap();
    let stdout_path = path_string(&log_root.join("demo.stdout.log"));
    let stderr_path = path_string(&log_root.join("demo.stderr.log"));
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: ocm_home.to_string(),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: binding_name.to_string(),
            gateway_state: "running".to_string(),
            restart_handoff: Some("none".to_string()),
            restart_count: 0,
            child_port,
            pid: Some(pid),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
        }],
        children: vec![SupervisorRuntimeChild {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: binding_name.to_string(),
            pid,
            restart_count: 0,
            child_port,
            stdout_path,
            stderr_path,
        }],
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
}

fn write_empty_supervisor_runtime(runtime_path: &Path, ocm_home: &str) {
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: ocm_home.to_string(),
        updated_at: now_utc(),
        services: Vec::new(),
        children: Vec::new(),
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
}

fn write_backoff_supervisor_runtime(
    runtime_path: &Path,
    ocm_home: &str,
    binding_name: &str,
    child_port: u32,
) {
    let log_root = runtime_path.parent().unwrap();
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: ocm_home.to_string(),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: binding_name.to_string(),
            gateway_state: "backoff".to_string(),
            restart_handoff: Some("none".to_string()),
            restart_count: 1,
            child_port,
            pid: None,
            stdout_path: path_string(&log_root.join("demo.stdout.log")),
            stderr_path: path_string(&log_root.join("demo.stderr.log")),
            last_exit_code: Some(1),
            last_error: Some("fixture target failed".to_string()),
            last_event_at: Some(now_utc()),
            next_retry_at: Some(now_utc()),
        }],
        children: Vec::new(),
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
}

fn recording_openclaw_script(version: &str) -> String {
    format!(
        r#"#!/bin/sh
home="${{OPENCLAW_HOME:-$PWD}}"
mkdir -p "$home"
command_log="${{OCM_TEST_COMMAND_LOG:-$home/sim-commands.log}}"
printf '%s\n' "$*" >> "$command_log"
has_arg() {{
  needle="$1"
  shift
  for arg in "$@"; do
    if [ "$arg" = "$needle" ]; then
      return 0
    fi
  done
  return 1
}}
case "$1" in
  --version)
    printf '{version}\n'
    exit 0
    ;;
  update)
    if [ "$2" = "finalize" ]; then
      if [ -n "${{OCM_TEST_UPDATE_FINALIZE_STARTED:-}}" ]; then
        : > "$OCM_TEST_UPDATE_FINALIZE_STARTED"
      fi
      if [ -n "${{OCM_TEST_UPDATE_FINALIZE_RELEASE:-}}" ]; then
        while [ ! -e "$OCM_TEST_UPDATE_FINALIZE_RELEASE" ]; do
          sleep 0.05
        done
      fi
      if [ -n "${{OCM_TEST_REQUIRE_STOP_MARKER_DURING_FINALIZE:-}}" ] &&
         [ ! -e "$OCM_TEST_REQUIRE_STOP_MARKER_DURING_FINALIZE" ]; then
        echo "managed service was not stopped before update finalize" >&2
        exit 24
      fi
      if [ "${{OCM_TEST_FAIL_UPDATE_FINALIZE:-}}" = "1" ]; then
        echo "forced update finalize failure" >&2
        exit 23
      fi
      has_arg "--json" "$@" && has_arg "--yes" "$@" && has_arg "--no-restart" "$@" || {{
        echo "missing update finalize flags" >&2
        exit 1
      }}
      if [ "${{OPENCLAW_UPDATE_IN_PROGRESS:-}}" != "1" ]; then
        echo "missing OPENCLAW_UPDATE_IN_PROGRESS" >&2
        exit 1
      fi
      if [ "${{OPENCLAW_UPDATE_PARENT_SUPPORTS_DOCTOR_CONFIG_WRITE:-}}" != "1" ]; then
        echo "missing OPENCLAW_UPDATE_PARENT_SUPPORTS_DOCTOR_CONFIG_WRITE" >&2
        exit 1
      fi
      printf '{{"status":"ok","mode":"finalize","postUpdate":{{"doctor":{{"status":"ok"}},"plugins":{{"status":"ok"}}}}}}\n'
      exit 0
    fi
    printf '{{"dryRun":true}}\n'
    exit 0
    ;;
  config)
    [ "$2" = "validate" ] || {{
      echo "unexpected config args: $*" >&2
      exit 1
    }}
    if [ "${{OCM_TEST_INVALID_CONFIG_UNTIL_DOCTOR:-}}" = "1" ] &&
       [ ! -f "$home/.openclaw/config-repaired" ]; then
      echo "OpenClaw config is invalid" >&2
      exit 1
    fi
    echo "Config valid"
    exit 0
    ;;
  doctor)
    has_arg "--non-interactive" "$@" && has_arg "--fix" "$@" || {{
      echo "missing doctor update flags" >&2
      exit 1
    }}
    if [ "${{OPENCLAW_UPDATE_IN_PROGRESS:-}}" != "1" ]; then
      echo "missing OPENCLAW_UPDATE_IN_PROGRESS" >&2
      exit 1
    fi
    if [ "${{OCM_TEST_REQUIRE_DOCTOR_OWNERSHIP_FLAGS:-}}" = "1" ]; then
      if [ "${{OPENCLAW_UPDATE_PARENT_SUPPORTS_GATEWAY_RESTART:-}}" != "1" ] ||
         [ "${{OPENCLAW_UPDATE_PARENT_ALLOWS_GATEWAY_SERVICE_REPAIR:-}}" != "0" ] ||
         [ "${{OPENCLAW_UPDATE_PARENT_ALLOWS_GATEWAY_ACTIVATION:-}}" != "0" ] ||
         [ "${{OPENCLAW_SERVICE_REPAIR_POLICY:-}}" != "external" ]; then
        echo "missing doctor service ownership flags" >&2
        exit 1
      fi
    fi
    if [ "${{OCM_TEST_FAIL_DOCTOR:-}}" = "1" ]; then
      echo "forced doctor failure" >&2
      exit 29
    fi
    mkdir -p "$home/.openclaw"
    if [ "${{OCM_TEST_DOCTOR_LEAVES_CONFIG_INVALID:-}}" != "1" ]; then
      touch "$home/.openclaw/config-repaired"
    fi
    echo "doctor ok"
    exit 0
    ;;
  plugins)
    [ "$2" = "update" ] && has_arg "--all" "$@" || {{
      echo "missing plugin update flags" >&2
      exit 1
    }}
    echo "No tracked plugins or hook packs to update."
    exit 0
    ;;
  gateway)
    [ "$2" = "status" ] && has_arg "--deep" "$@" && has_arg "--json" "$@" || {{
      echo "missing gateway status flags" >&2
      exit 1
    }}
    if [ -n "${{OCM_TEST_GATEWAY_STATUS_STARTED:-}}" ]; then
      : > "$OCM_TEST_GATEWAY_STATUS_STARTED"
    fi
    if [ -n "${{OCM_TEST_GATEWAY_STATUS_RELEASE:-}}" ]; then
      while [ ! -e "$OCM_TEST_GATEWAY_STATUS_RELEASE" ]; do
        sleep 0.01
      done
    fi
    if [ "${{OCM_TEST_GATEWAY_AUTH_HANDSHAKE:-}}" = "1" ]; then
      printf '{{"rpc":{{"ok":false,"error":"device identity required"}}}}\n'
      exit "${{OCM_TEST_GATEWAY_STATUS_EXIT_CODE:-0}}"
    elif [ "${{OCM_TEST_GATEWAY_UNREADY:-}}" = "1" ]; then
      printf '{{"rpc":{{"ok":false,"error":"gateway RPC is not ready"}}}}\n'
    else
      printf '{{"rpc":{{"ok":true}}}}\n'
    fi
    exit 0
    ;;
esac
echo "unexpected args: $*" >&2
exit 1
"#
    )
}

struct SeededInPlaceRollback {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    marker: PathBuf,
    transaction_id: String,
    original_recovery_root: PathBuf,
}

fn seed_in_place_rollback(root: &TestDir, recovery_binary_version: &str) -> SeededInPlaceRollback {
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(root);
    let ocm_home = PathBuf::from(env.get("OCM_HOME").unwrap());
    let runtime_name = "stable";
    let runtime_root = ocm_home.join("runtimes").join(runtime_name);
    let runtime_binary = runtime_root.join("files/bin/openclaw");
    fs::create_dir_all(runtime_binary.parent().unwrap()).unwrap();
    write_executable_script(&runtime_binary, &recording_openclaw_script("2026.6.33"));
    fs::create_dir_all(ocm_home.join("runtimes")).unwrap();
    let runtime_meta = |version: &str| {
        serde_json::json!({
            "kind": "ocm-runtime",
            "name": runtime_name,
            "binaryPath": path_string(&runtime_binary),
            "sourceKind": "installed",
            "sourceUrl": "https://example.invalid/openclaw.tgz",
            "sourceManifestUrl": "https://example.invalid/openclaw",
            "releaseVersion": version,
            "releaseChannel": "stable",
            "installRoot": path_string(&runtime_root),
            "description": null,
            "createdAt": "2026-06-11T00:00:00Z",
            "updatedAt": "2026-06-30T00:00:00Z"
        })
    };
    fs::write(
        ocm_home.join("runtimes/stable.json"),
        serde_json::to_vec(&runtime_meta("2026.6.33")).unwrap(),
    )
    .unwrap();

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", runtime_name],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_json: Value = serde_json::from_str(&stdout(&env_show)).unwrap();
    let marker =
        Path::new(env_json["root"].as_str().unwrap()).join(".openclaw/workspace/rollback-marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "before-upgrade").unwrap();

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "demo",
            "--label",
            "pre-upgrade",
            "--json",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap();
    fs::write(&marker, "after-upgrade").unwrap();

    let transaction_id = "1782864000-000000001".to_string();
    let original_recovery_root = ocm_home
        .join("upgrade-history/demo")
        .join(format!("{transaction_id}.recovery"));
    let recovery_runtime_root = original_recovery_root.join(runtime_name);
    let recovery_binary = recovery_runtime_root.join("install-root/files/bin/openclaw");
    fs::create_dir_all(recovery_binary.parent().unwrap()).unwrap();
    write_executable_script(
        &recovery_binary,
        &recording_openclaw_script(recovery_binary_version),
    );
    fs::write(
        recovery_runtime_root.join("runtime.json"),
        serde_json::to_vec(&runtime_meta("2026.6.11")).unwrap(),
    )
    .unwrap();
    fs::write(original_recovery_root.join("snapshot-id"), snapshot_id).unwrap();
    let history = serde_json::json!({
        "kind": "ocm-upgrade-transaction",
        "formatVersion": 1,
        "id": transaction_id.clone(),
        "envName": "demo",
        "source": {
            "kind": "runtime",
            "name": runtime_name,
            "openclawVersion": "2026.6.11"
        },
        "target": {
            "kind": "runtime",
            "name": runtime_name,
            "openclawVersion": "2026.6.33"
        },
        "snapshotId": snapshot_id,
        "runtimeRecovery": [{
            "runtimeName": runtime_name,
            "releaseVersion": "2026.6.11",
            "backupId": runtime_name
        }],
        "startedAt": "2026-06-30T00:00:00Z",
        "completedAt": "2026-06-30T00:01:00Z",
        "outcome": "updated",
        "migration": {"status": "validated"},
        "finalization": {"status": "completed"},
        "serviceBefore": {
            "enabled": env_json["serviceEnabled"],
            "running": env_json["serviceRunning"]
        },
        "serviceAfter": {
            "enabled": env_json["serviceEnabled"],
            "running": env_json["serviceRunning"]
        }
    });
    fs::create_dir_all(ocm_home.join("upgrade-history/demo")).unwrap();
    fs::write(
        ocm_home
            .join("upgrade-history/demo")
            .join(format!("{transaction_id}.json")),
        serde_json::to_vec(&history).unwrap(),
    )
    .unwrap();

    SeededInPlaceRollback {
        cwd,
        env,
        marker,
        transaction_id,
        original_recovery_root,
    }
}

fn destructive_finalize_openclaw_script(version: &str) -> String {
    format!(
        r#"#!/bin/sh
home="${{OPENCLAW_HOME:-$PWD}}"
case "$1" in
  --version)
    printf '{version}\n'
    exit 0
    ;;
  update)
    if [ "$2" = "finalize" ]; then
      rm -rf "$home/.openclaw/npm"
      printf '{{"status":"ok","mode":"finalize"}}\n'
      exit 0
    fi
    ;;
esac
echo "unexpected args: $*" >&2
exit 1
"#
    )
}

fn prebinding_guard_openclaw_script(version: &str, expected_runtime: &str) -> String {
    format!(
        r#"#!/bin/sh
home="${{OPENCLAW_HOME:-$PWD}}"
mkdir -p "$home"
printf '%s\n' "$*" >> "$home/sim-commands.log"
case "$1" in
  --version)
    printf '{version}\n'
    exit 0
    ;;
  update)
    if [ "$2" = "finalize" ]; then
      if ! grep -q '"defaultRuntime": "{expected_runtime}"' "$OCM_HOME/envs.json"; then
        echo "replacement runtime was published before update finalization" >&2
        exit 24
      fi
      printf '{{"status":"ok"}}\n'
      exit 0
    fi
    ;;
esac
echo "unexpected args: $*" >&2
exit 1
"#
    )
}

fn scenario_sensitive_openclaw_script(version: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  --version)
    printf '{version}\n'
    exit 0
    ;;
  update)
    printf '{{"dryRun":true}}\n'
    exit 0
    ;;
  doctor)
    if grep -q '"telegram"' "${{OPENCLAW_CONFIG_PATH:-/dev/null}}"; then
      echo "Error: Cannot find module 'grammy'" >&2
      exit 1
    fi
    echo "doctor ok"
    exit 0
    ;;
  plugins)
    echo "No tracked plugins or hook packs to update."
    exit 0
    ;;
  gateway)
    printf '{{"rpc":{{"ok":true}}}}\n'
    exit 0
    ;;
esac
echo "unexpected args: $*" >&2
exit 1
"#
    )
}

fn sha512_integrity(body: &[u8]) -> String {
    let digest = Sha512::digest(body);
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

fn init_openclaw_repo(root: &TestDir) -> PathBuf {
    let repo = root.child("repo/openclaw");
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::write(
        repo.join("package.json"),
        r#"{"name":"openclaw","version":"2026.4.20-local"}"#,
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

fn prepend_fake_bin(env: &mut std::collections::BTreeMap<String, String>, bin_dir: &Path) {
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
}

fn install_fake_simulation_pnpm(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) {
    let bin_dir = root.child("fake-sim-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let pnpm = r#"#!/bin/sh
case "$1" in
  install)
    mkdir -p node_modules/.pnpm node_modules/.bin
    touch node_modules/.bin/tsx
    exit 0
    ;;
  build|ui:build)
    echo "$1 ok"
    exit 0
    ;;
  openclaw)
    shift
    case "$1" in
      --version)
        echo "2026.4.20-local"
        exit 0
        ;;
      doctor)
        echo "Error: Cannot find module 'grammy'" >&2
        exit 1
        ;;
      plugins)
        echo "No tracked plugins or hook packs to update."
        exit 0
        ;;
      gateway)
        echo "{\"status\":\"ok\"}"
        exit 0
        ;;
      *)
        echo "fake local openclaw $*"
        exit 0
        ;;
    esac
    ;;
esac
echo "unexpected pnpm $*" >&2
exit 1
"#;
    write_executable_script(&bin_dir.join("pnpm"), pnpm);
    prepend_fake_bin(env, &bin_dir);
}

#[test]
fn upgrade_updates_a_tracked_runtime_and_refreshes_the_service() {
    let root = TestDir::new("upgrade-tracked-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );

    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    let start = run_ocm(&cwd, &env, &["start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    env.insert(
        "OCM_TEST_GATEWAY_AUTH_HANDSHAKE".to_string(),
        "1".to_string(),
    );
    env.insert(
        "OCM_TEST_GATEWAY_STATUS_EXIT_CODE".to_string(),
        "1".to_string(),
    );
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=updated"), "{output}");
    assert!(output.contains("service=started"), "{output}");
    assert!(output.contains("snapshot="), "{output}");
    assert!(output.contains("version=2026.3.25"), "{output}");

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.25");

    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_json: Value = serde_json::from_str(&stdout(&env_show)).unwrap();
    let env_root = Path::new(env_json["root"].as_str().unwrap());
    let command_log = fs::read_to_string(env_root.join("sim-commands.log")).unwrap();
    assert!(
        command_log.contains("update finalize --json --yes --no-restart"),
        "{command_log}"
    );
    assert!(command_log.contains("--version"), "{command_log}");
    assert!(
        !command_log.contains("doctor --non-interactive --fix"),
        "{command_log}"
    );
    assert!(
        !command_log.contains("plugins update --all\n"),
        "{command_log}"
    );

    let snapshots = run_ocm(&cwd, &env, &["env", "snapshot", "list", "demo", "--json"]);
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshot_json.as_array().unwrap().len(), 1);
    assert_eq!(snapshot_json[0]["label"], "pre-upgrade");

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    let record = &history_json[0];
    assert_eq!(record["kind"], "ocm-upgrade-transaction");
    assert_eq!(record["formatVersion"], 1);
    assert_eq!(record["source"]["name"], "stable");
    assert_eq!(record["source"]["openclawVersion"], "2026.3.24");
    assert_eq!(record["target"]["name"], "stable");
    assert_eq!(record["target"]["openclawVersion"], "2026.3.25");
    assert_eq!(record["snapshotId"], snapshot_json[0]["id"]);
    assert_eq!(record["outcome"], "updated");
    assert_eq!(record["migration"]["status"], "validated");
    assert_eq!(record["finalization"]["status"], "completed");
    assert_eq!(record["serviceBefore"]["running"], true);
    assert_eq!(record["serviceAfter"]["running"], true);
    assert!(record["note"].is_null());
    assert_eq!(record["runtimeRecovery"][0]["runtimeName"], "stable");
    assert_eq!(record["runtimeRecovery"][0]["releaseVersion"], "2026.3.24");
    assert_eq!(record["runtimeRecovery"][0]["backupId"], "stable");

    let ocm_home = Path::new(env.get("OCM_HOME").unwrap());
    let recovery_root = ocm_home
        .join("upgrade-history")
        .join("demo")
        .join(format!("{}.recovery", record["id"].as_str().unwrap()))
        .join("stable");
    let recovery_meta: Value =
        serde_json::from_slice(&fs::read(recovery_root.join("runtime.json")).unwrap()).unwrap();
    assert_eq!(recovery_meta["name"], "stable");
    assert_eq!(recovery_meta["releaseVersion"], "2026.3.24");
    assert!(
        recovery_root
            .join("install-root/files/node_modules/openclaw/openclaw.mjs")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(recovery_root.parent().unwrap().join("snapshot-id")).unwrap(),
        snapshot_json[0]["id"].as_str().unwrap()
    );

    let create_newer_snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "demo",
            "--label",
            "after-upgrade",
        ],
    );
    assert!(
        create_newer_snapshot.status.success(),
        "{}",
        stderr(&create_newer_snapshot)
    );

    let prune_snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "prune", "demo", "--keep", "1", "--yes"],
    );
    assert!(
        prune_snapshot.status.success(),
        "{}",
        stderr(&prune_snapshot)
    );
    assert!(stdout(&prune_snapshot).contains("Pruned 1 snapshot(s)."));
    assert!(!recovery_root.exists());

    let reuse = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    assert!(reuse.status.success(), "{}", stderr(&reuse));
    assert!(stdout(&reuse).contains("outcome=up-to-date"));
    let shown = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["serviceRunning"], true);
}

#[test]
fn upgrade_rolls_back_when_gateway_rpc_is_not_ready() {
    let root = TestDir::new("upgrade-gateway-readiness-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let health_server =
        TestHttpServer::serve_bytes_times("/health", "application/json", br#"{"ok":true}"#, 8);
    let health_url = health_server.url();
    let health_port = health_url
        .split(':')
        .nth(2)
        .and_then(|value| value.split('/').next())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    let port = health_port.to_string();
    let start = run_ocm(&cwd, &env, &["start", "demo", "--port", port.as_str()]);
    assert!(start.status.success(), "{}", stderr(&start));

    let before_runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(
        before_runtime.status.success(),
        "{}",
        stderr(&before_runtime)
    );
    let before_runtime: Value = serde_json::from_str(&stdout(&before_runtime)).unwrap();
    let runtime_entrypoint = PathBuf::from(before_runtime["installRoot"].as_str().unwrap())
        .join("files/node_modules/openclaw/openclaw.mjs");
    let runtime_hash_before = Sha512::digest(fs::read(&runtime_entrypoint).unwrap()).to_vec();

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    write_running_supervisor_runtime(&runtime_path, &ocm_home, "stable", 4242, health_port);

    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let gateway_status_started = root.child("gateway-status-started");
    let gateway_status_release = root.child("gateway-status-release");
    env.insert(
        "OCM_TEST_GATEWAY_STATUS_STARTED".to_string(),
        path_string(&gateway_status_started),
    );
    env.insert(
        "OCM_TEST_GATEWAY_STATUS_RELEASE".to_string(),
        path_string(&gateway_status_release),
    );
    let replacement_runtime_path = runtime_path.clone();
    let replacement_ocm_home = ocm_home.clone();
    let stop_marker = root.child("service-stopped-before-finalize");
    let observed_stop_marker = stop_marker.clone();
    let observer_done = Arc::new(AtomicBool::new(false));
    let missing_while_active = Arc::new(AtomicBool::new(false));
    let observer_done_thread = Arc::clone(&observer_done);
    let missing_while_active_thread = Arc::clone(&missing_while_active);
    let observed_entrypoint = runtime_entrypoint.clone();
    let restart_observer = thread::spawn(move || {
        let mut last_running = true;
        let mut runtime_active = true;
        let mut stop_count = 0;
        let mut start_count = 0;
        let mut next_pid = 4243;
        let mut target_entered_backoff = false;
        while !observer_done_thread.load(Ordering::Relaxed) {
            if runtime_active && !observed_entrypoint.exists() {
                missing_while_active_thread.store(true, Ordering::Relaxed);
            }
            let desired_running = fs::read(&registry_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|registry| registry["envs"].as_array().cloned())
                .and_then(|envs| envs.into_iter().find(|entry| entry["name"] == "demo"))
                .and_then(|entry| entry["serviceRunning"].as_bool())
                .unwrap_or(last_running);
            if desired_running != last_running {
                if desired_running {
                    write_running_supervisor_runtime(
                        &replacement_runtime_path,
                        &replacement_ocm_home,
                        "stable",
                        next_pid,
                        health_port,
                    );
                    next_pid += 1;
                    start_count += 1;
                    runtime_active = true;
                } else {
                    write_empty_supervisor_runtime(
                        &replacement_runtime_path,
                        &replacement_ocm_home,
                    );
                    fs::write(&observed_stop_marker, "stopped").unwrap();
                    stop_count += 1;
                    runtime_active = false;
                }
                last_running = desired_running;
            }
            if desired_running
                && start_count >= 1
                && !target_entered_backoff
                && gateway_status_started.exists()
            {
                write_backoff_supervisor_runtime(
                    &replacement_runtime_path,
                    &replacement_ocm_home,
                    "stable",
                    health_port,
                );
                fs::write(&gateway_status_release, []).unwrap();
                target_entered_backoff = true;
            }
            sleep(Duration::from_millis(1));
        }
        (stop_count, start_count, target_entered_backoff)
    });

    env.insert(
        "OCM_TEST_REQUIRE_STOP_MARKER_DURING_FINALIZE".to_string(),
        path_string(&stop_marker),
    );
    env.insert("OCM_TEST_GATEWAY_UNREADY".to_string(), "1".to_string());
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    observer_done.store(true, Ordering::Relaxed);
    let (stop_count, start_count, target_entered_backoff) = restart_observer.join().unwrap();

    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(
        output.contains("post-upgrade gateway readiness failed: gateway RPC is not ready"),
        "{output}"
    );
    assert!(!health_server.requests().is_empty());
    assert!(target_entered_backoff);
    assert!(stop_count >= 2, "stop_count={stop_count}");
    assert!(start_count >= 2, "start_count={start_count}");
    assert!(
        !missing_while_active.load(Ordering::Relaxed),
        "the retrying in-place runtime observed its entrypoint disappear"
    );

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");
    assert_eq!(
        Sha512::digest(fs::read(&runtime_entrypoint).unwrap()).to_vec(),
        runtime_hash_before,
        "in-place updated runtime bytes were not restored"
    );

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    let record = &history_json[0];
    assert_eq!(record["source"]["openclawVersion"], "2026.3.24");
    assert_eq!(record["target"]["openclawVersion"], "2026.3.25");
    assert_eq!(record["outcome"], "rolled-back");
    assert_eq!(record["rollback"], "restored");
    assert_eq!(record["migration"]["status"], "validated");
    assert_eq!(record["finalization"]["status"], "completed");
    assert_eq!(record["serviceAfter"]["running"], true);
    assert!(record["note"].is_null());
    let recovery_root = Path::new(env.get("OCM_HOME").unwrap())
        .join("upgrade-history")
        .join("demo")
        .join(format!("{}.recovery", record["id"].as_str().unwrap()));
    assert!(!recovery_root.exists());
}

#[cfg(unix)]
#[test]
fn failed_named_runtime_target_in_backoff_is_not_rewritten_during_rollback() {
    use std::os::unix::fs::MetadataExt;

    let root = TestDir::new("upgrade-reused-target-backoff-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let health_server =
        TestHttpServer::serve_bytes_times("/health", "application/json", br#"{"ok":true}"#, 8);
    let health_port = health_server
        .url()
        .split(':')
        .nth(2)
        .and_then(|value| value.split('/').next())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap();

    let source_version = "2026.8.1";
    let target_version = "2026.8.2";
    let source_tarball =
        openclaw_package_tarball(&recording_openclaw_script(source_version), source_version);
    let source_integrity = sha512_integrity(&source_tarball);
    let source_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.8.1.tgz",
        "application/octet-stream",
        &source_tarball,
        4,
    );
    let target_tarball =
        openclaw_package_tarball(&recording_openclaw_script(target_version), target_version);
    let target_integrity = sha512_integrity(&target_tarball);
    let target_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.8.2.tgz",
        "application/octet-stream",
        &target_tarball,
        4,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"{target_version}\"}},\"versions\":{{\"{source_version}\":{{\"version\":\"{source_version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{source_integrity}\"}}}},\"{target_version}\":{{\"version\":\"{target_version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{target_integrity}\"}}}}}},\"time\":{{\"{source_version}\":\"2026-08-01T00:00:00.000Z\",\"{target_version}\":\"2026-08-02T00:00:00.000Z\"}}}}",
        source_server.url(),
        target_server.url(),
    );
    let packument_server = TestHttpServer::serve_bytes_times(
        "/openclaw",
        "application/json",
        packument.as_bytes(),
        12,
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    install_fake_launchctl(&root, &mut env);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );

    let port = health_port.to_string();
    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--version",
            source_version,
            "--port",
            &port,
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    let install_target = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--version", target_version],
    );
    assert!(
        install_target.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&install_target),
        stderr(&install_target)
    );

    let target = run_ocm(&cwd, &env, &["runtime", "show", target_version, "--json"]);
    assert!(target.status.success(), "{}", stderr(&target));
    let target: Value = serde_json::from_str(&stdout(&target)).unwrap();
    let target_root = PathBuf::from(target["installRoot"].as_str().unwrap());
    let target_entrypoint = target_root.join("files/node_modules/openclaw/openclaw.mjs");
    let target_chunks = target_root.join("files/node_modules/openclaw/dist/chunks");
    fs::create_dir_all(&target_chunks).unwrap();
    for index in 0..2_000 {
        fs::write(
            target_chunks.join(format!("chunk-{index:04}.js")),
            format!("export const chunk{index} = {index};\n"),
        )
        .unwrap();
    }
    let watched_chunk = target_chunks.join("chunk-1000.js");
    let entrypoint_hash_before = Sha512::digest(fs::read(&target_entrypoint).unwrap()).to_vec();
    let entrypoint_inode_before = fs::metadata(&target_entrypoint).unwrap().ino();
    let chunk_inode_before = fs::metadata(&watched_chunk).unwrap().ino();

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    write_running_supervisor_runtime(&runtime_path, &ocm_home, source_version, 4242, health_port);
    let state_path = supervisor_state_path(&env, &cwd).unwrap();
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let gateway_status_started = root.child("gateway-status-started");
    let gateway_status_release = root.child("gateway-status-release");
    env.insert(
        "OCM_TEST_GATEWAY_STATUS_STARTED".to_string(),
        path_string(&gateway_status_started),
    );
    env.insert(
        "OCM_TEST_GATEWAY_STATUS_RELEASE".to_string(),
        path_string(&gateway_status_release),
    );
    let observer_done = Arc::new(AtomicBool::new(false));
    let missing_target_file = Arc::new(AtomicBool::new(false));
    let observer_done_thread = Arc::clone(&observer_done);
    let missing_target_file_thread = Arc::clone(&missing_target_file);
    let observed_entrypoint = target_entrypoint.clone();
    let observed_chunk = watched_chunk.clone();
    let observer_runtime_path = runtime_path.clone();
    let observer_ocm_home = ocm_home.clone();
    let observer = thread::spawn(move || {
        let mut last_binding = Some(source_version.to_string());
        let mut target_was_running = false;
        let mut target_restart_was_acknowledged = false;
        let mut target_entered_backoff = false;
        let mut observed_bindings = Vec::new();
        while !observer_done_thread.load(Ordering::Relaxed) {
            if !observed_entrypoint.exists() || !observed_chunk.exists() {
                missing_target_file_thread.store(true, Ordering::Relaxed);
            }
            let state = fs::read_to_string(&state_path).unwrap_or_default();
            let state_json: Value = serde_json::from_str(&state).unwrap_or(Value::Null);
            let planned_binding = state_json["children"]
                .as_array()
                .and_then(|children| children.iter().find(|child| child["envName"] == "demo"))
                .and_then(|child| child["bindingName"].as_str())
                .map(str::to_string);
            let registry = fs::read_to_string(&registry_path).unwrap_or_default();
            let registry_json: Value = serde_json::from_str(&registry).unwrap_or(Value::Null);
            let registered = registry_json["envs"]
                .as_array()
                .and_then(|envs| envs.iter().find(|entry| entry["name"] == "demo"));
            let next_binding = match registered {
                Some(entry) if entry["serviceRunning"] == true => {
                    entry["defaultRuntime"].as_str().map(str::to_string)
                }
                Some(_) => None,
                None => planned_binding,
            };
            if next_binding != last_binding {
                observed_bindings.push(next_binding.clone());
                match next_binding.as_deref() {
                    Some(value) if value == target_version => {
                        write_running_supervisor_runtime(
                            &observer_runtime_path,
                            &observer_ocm_home,
                            target_version,
                            4243,
                            health_port,
                        );
                        target_was_running = true;
                    }
                    Some(value) if value == source_version => write_running_supervisor_runtime(
                        &observer_runtime_path,
                        &observer_ocm_home,
                        source_version,
                        4244,
                        health_port,
                    ),
                    None => {
                        write_empty_supervisor_runtime(&observer_runtime_path, &observer_ocm_home)
                    }
                    _ => {}
                }
                last_binding = next_binding;
            }
            let target_restart_requested =
                state_json["restartRequests"]
                    .as_array()
                    .is_some_and(|requests| {
                        requests.iter().any(|request| request["envName"] == "demo")
                    });
            if target_was_running && !target_restart_was_acknowledged && target_restart_requested {
                write_running_supervisor_runtime(
                    &observer_runtime_path,
                    &observer_ocm_home,
                    target_version,
                    4244,
                    health_port,
                );
                target_restart_was_acknowledged = true;
            }
            if target_was_running && !target_entered_backoff && gateway_status_started.exists() {
                write_backoff_supervisor_runtime(
                    &observer_runtime_path,
                    &observer_ocm_home,
                    target_version,
                    health_port,
                );
                fs::write(&gateway_status_release, []).unwrap();
                target_entered_backoff = true;
            }
            sleep(Duration::from_millis(1));
        }
        (target_entered_backoff, observed_bindings)
    });

    env.insert("OCM_TEST_GATEWAY_UNREADY".to_string(), "1".to_string());
    let upgrade = run_ocm(
        &cwd,
        &env,
        &["upgrade", "demo", "--runtime", target_version],
    );
    observer_done.store(true, Ordering::Relaxed);
    let (target_entered_backoff, observed_bindings) = observer.join().unwrap();

    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(
        target_entered_backoff,
        "target never entered fixture backoff"
    );
    let target_observed = observed_bindings
        .iter()
        .position(|binding| binding.as_deref() == Some(target_version))
        .expect("target binding was never observed");
    let restored_source = observed_bindings
        .iter()
        .rposition(|binding| binding.as_deref() == Some(source_version))
        .expect("restored source binding was never observed");
    assert!(
        target_observed < restored_source,
        "source binding was not restored after the failed target: {observed_bindings:?}"
    );
    assert!(
        !missing_target_file.load(Ordering::Relaxed),
        "the retrying target observed its entrypoint or a chunk disappear"
    );
    assert_eq!(
        fs::metadata(&target_entrypoint).unwrap().ino(),
        entrypoint_inode_before,
        "reused target entrypoint inode changed"
    );
    assert_eq!(
        fs::metadata(&watched_chunk).unwrap().ino(),
        chunk_inode_before,
        "reused target chunk inode changed"
    );
    assert_eq!(
        Sha512::digest(fs::read(&target_entrypoint).unwrap()).to_vec(),
        entrypoint_hash_before,
        "reused target entrypoint bytes changed"
    );
    let shown = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["defaultRuntime"], source_version);
}

#[test]
fn upgrade_rejects_mutating_a_runtime_shared_with_another_env() {
    let root = TestDir::new("upgrade-shared-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );
    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    for name in ["primary", "sibling"] {
        let start = run_ocm(&cwd, &env, &["start", name, "--no-service"]);
        assert!(start.status.success(), "{}", stderr(&start));
    }

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "primary"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let error = stderr(&upgrade);
    assert!(error.contains("runtime \"stable\" is shared"), "{error}");
    assert!(error.contains("\"sibling\""), "{error}");

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");

    for name in ["primary", "sibling"] {
        let version = run_ocm(&cwd, &env, &["env", "run", name, "--", "--version"]);
        assert!(version.status.success(), "{}", stderr(&version));
        assert_eq!(stdout(&version).trim(), "2026.3.24");

        let snapshots = run_ocm(&cwd, &env, &["env", "snapshot", "list", name, "--json"]);
        assert!(snapshots.status.success(), "{}", stderr(&snapshots));
        let snapshot_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
        assert!(snapshot_json.as_array().unwrap().is_empty());
    }
}

#[test]
fn upgrade_rejects_mutating_a_target_runtime_bound_to_another_env() {
    let root = TestDir::new("upgrade-shared-target-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );
    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let source_runtime = root.child("source-openclaw");
    write_executable_script(&source_runtime, &recording_openclaw_script("source-local"));
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let add_source = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "source-local",
            "--path",
            &source_runtime.display().to_string(),
        ],
    );
    assert!(add_source.status.success(), "{}", stderr(&add_source));
    let create_source = run_ocm(
        &cwd,
        &env,
        &["env", "create", "primary", "--runtime", "source-local"],
    );
    assert!(create_source.status.success(), "{}", stderr(&create_source));
    let start_sibling = run_ocm(&cwd, &env, &["start", "sibling", "--no-service"]);
    assert!(start_sibling.status.success(), "{}", stderr(&start_sibling));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "primary", "--channel", "stable"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let error = stderr(&upgrade);
    assert!(error.contains("runtime \"stable\" is shared"), "{error}");
    assert!(error.contains("\"sibling\""), "{error}");

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");

    let source = run_ocm(&cwd, &env, &["env", "show", "primary", "--json"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let source_json: Value = serde_json::from_str(&stdout(&source)).unwrap();
    assert_eq!(source_json["defaultRuntime"], "source-local");

    let snapshots = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "list", "primary", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert!(snapshot_json.as_array().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn upgrade_switches_across_versions_from_runtime_with_broken_package_bin_symlink() {
    let root = TestDir::new("upgrade-broken-runtime-symlink-versions");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let pairs = (0..10)
        .map(|index| {
            (
                format!("2026.3.{}", 20 + index),
                format!("2026.4.{}", 20 + index),
            )
        })
        .collect::<Vec<_>>();
    let versions = pairs
        .iter()
        .flat_map(|(source, target)| [source.clone(), target.clone()])
        .collect::<Vec<_>>();
    let mut tarball_servers = Vec::new();
    let mut version_entries = Vec::new();
    let mut time_entries = Vec::new();
    for version in versions {
        let tarball = openclaw_package_tarball(&recording_openclaw_script(&version), &version);
        let integrity = sha512_integrity(&tarball);
        let path = format!("/openclaw-{version}.tgz");
        let server =
            TestHttpServer::serve_bytes_times(&path, "application/octet-stream", &tarball, 4);
        version_entries.push(format!(
            "\"{version}\":{{\"version\":\"{version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{integrity}\"}}}}",
            server.url()
        ));
        time_entries.push(format!("\"{version}\":\"2026-03-25T16:35:52.000Z\""));
        tarball_servers.push(server);
    }
    let latest = pairs.last().unwrap().1.as_str();
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"{latest}\"}},\"versions\":{{{}}},\"time\":{{{}}}}}",
        version_entries.join(","),
        time_entries.join(",")
    );
    let packument_server = TestHttpServer::serve_bytes_times(
        "/openclaw",
        "application/json",
        packument.as_bytes(),
        80,
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    for (index, (source_version, target_version)) in pairs.iter().enumerate() {
        let env_name = format!("demo-{index}");
        let start = run_ocm(
            &cwd,
            &env,
            &[
                "start",
                &env_name,
                "--version",
                source_version,
                "--no-service",
            ],
        );
        assert!(start.status.success(), "{}", stderr(&start));

        let runtime = run_ocm(&cwd, &env, &["runtime", "show", source_version, "--json"]);
        assert!(runtime.status.success(), "{}", stderr(&runtime));
        let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
        let install_root = Path::new(runtime_json["installRoot"].as_str().unwrap());
        let bin_dir = install_root.join(format!(
            "files/node_modules/openclaw/dist/extensions/demo-{index}/node_modules/.bin"
        ));
        fs::create_dir_all(&bin_dir).unwrap();
        let broken_link = bin_dir.join("missing-tool");
        std::os::unix::fs::symlink("../missing-package/bin/missing-tool", &broken_link).unwrap();
        assert!(
            fs::symlink_metadata(&broken_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!broken_link.exists());

        let verify = run_ocm(&cwd, &env, &["runtime", "verify", source_version, "--json"]);
        assert!(verify.status.success(), "{}", stderr(&verify));

        let upgrade = run_ocm(
            &cwd,
            &env,
            &["upgrade", &env_name, "--version", target_version],
        );
        assert!(
            upgrade.status.success(),
            "{source_version} -> {target_version}\nstdout:\n{}\nstderr:\n{}",
            stdout(&upgrade),
            stderr(&upgrade)
        );
        let output = stdout(&upgrade);
        assert!(
            output.contains(&format!("from=runtime:{source_version}")),
            "{output}"
        );
        assert!(
            output.contains(&format!("to=runtime:{target_version}")),
            "{output}"
        );
        assert!(output.contains("outcome=switched"), "{output}");
        assert!(output.contains("snapshot="), "{output}");

        let env_show = run_ocm(&cwd, &env, &["env", "show", &env_name, "--json"]);
        assert!(env_show.status.success(), "{}", stderr(&env_show));
        let env_json: Value = serde_json::from_str(&stdout(&env_show)).unwrap();
        assert_eq!(env_json["defaultRuntime"], *target_version);
    }

    assert_eq!(tarball_servers.len(), 20);
}

#[test]
fn upgrade_rejects_downgrade_before_snapshot_or_target_download() {
    let root = TestDir::new("upgrade-reject-downgrade");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let older_version = "2026.7.1-2";
    let newer_version = "2026.7.2-beta.3";
    let older_tarball =
        openclaw_package_tarball(&recording_openclaw_script(older_version), older_version);
    let older_integrity = sha512_integrity(&older_tarball);
    let older_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.7.1-2.tgz",
        "application/octet-stream",
        &older_tarball,
        4,
    );
    let newer_tarball =
        openclaw_package_tarball(&recording_openclaw_script(newer_version), newer_version);
    let newer_integrity = sha512_integrity(&newer_tarball);
    let newer_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.7.2-beta.3.tgz",
        "application/octet-stream",
        &newer_tarball,
        4,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"{newer_version}\",\"beta\":\"{newer_version}\"}},\"versions\":{{\"{older_version}\":{{\"version\":\"{older_version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{older_integrity}\"}}}},\"{newer_version}\":{{\"version\":\"{newer_version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{newer_integrity}\"}}}}}},\"time\":{{\"{older_version}\":\"2026-07-18T03:53:48.967Z\",\"{newer_version}\":\"2026-07-18T23:15:12.160Z\"}}}}",
        older_server.url(),
        newer_server.url()
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 8);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--version", newer_version, "--no-service"],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    assert!(older_server.requests().is_empty());

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--version", older_version]);
    assert_eq!(upgrade.status.code(), Some(1));
    let error = stderr(&upgrade);
    assert!(
        error.contains(&format!(
            "refusing to downgrade env \"demo\" from OpenClaw {newer_version} to {older_version}"
        )),
        "{error}"
    );
    assert!(error.contains("SQLite state"), "{error}");
    assert!(older_server.requests().is_empty());

    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    let environment: Value = serde_json::from_str(&stdout(&environment)).unwrap();
    assert_eq!(environment["defaultRuntime"], newer_version);

    let target = run_ocm(&cwd, &env, &["runtime", "show", older_version]);
    assert!(!target.status.success());

    let install_target = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "--version", older_version],
    );
    assert!(
        install_target.status.success(),
        "{}",
        stderr(&install_target)
    );
    let older_requests_before_named_target = older_server.requests().len();
    let named_upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", older_version]);
    assert_eq!(named_upgrade.status.code(), Some(1));
    assert!(
        stderr(&named_upgrade).contains(&format!(
            "refusing to downgrade env \"demo\" from OpenClaw {newer_version} to {older_version}"
        )),
        "{}",
        stderr(&named_upgrade)
    );
    assert_eq!(
        older_server.requests().len(),
        older_requests_before_named_target
    );

    let snapshots = run_ocm(&cwd, &env, &["env", "snapshot", "list", "demo", "--json"]);
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert!(snapshots.as_array().unwrap().is_empty());
}

#[test]
fn upgrade_accepts_correction_release_and_freezes_channel_selection() {
    let root = TestDir::new("upgrade-correction-release");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let stable_version = "2026.7.1";
    let correction_version = "2026.7.1-2";
    let stable_tarball =
        openclaw_package_tarball(&recording_openclaw_script(stable_version), stable_version);
    let stable_integrity = sha512_integrity(&stable_tarball);
    let stable_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.7.1.tgz",
        "application/octet-stream",
        &stable_tarball,
        4,
    );
    let correction_tarball = openclaw_package_tarball(
        &recording_openclaw_script(correction_version),
        correction_version,
    );
    let correction_integrity = sha512_integrity(&correction_tarball);
    let correction_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.7.1-2.tgz",
        "application/octet-stream",
        &correction_tarball,
        4,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"{correction_version}\"}},\"versions\":{{\"{stable_version}\":{{\"version\":\"{stable_version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{stable_integrity}\"}}}},\"{correction_version}\":{{\"version\":\"{correction_version}\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{correction_integrity}\"}}}}}},\"time\":{{\"{stable_version}\":\"2026-07-13T17:58:18.920Z\",\"{correction_version}\":\"2026-07-18T03:53:48.967Z\"}}}}",
        stable_server.url(),
        correction_server.url()
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 8);
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--version", stable_version, "--no-service"],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    let requests_before_upgrade = packument_server.requests().len();

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--channel", "stable"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    assert_eq!(
        packument_server.requests().len() - requests_before_upgrade,
        1
    );
    assert_eq!(correction_server.requests().len(), 1);

    let environment = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(environment.status.success(), "{}", stderr(&environment));
    let environment: Value = serde_json::from_str(&stdout(&environment)).unwrap();
    assert_eq!(environment["defaultRuntime"], "stable");
    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime["releaseVersion"], correction_version);
}

#[test]
fn upgrade_dry_run_reports_without_changing_runtime_or_creating_snapshot() {
    let root = TestDir::new("upgrade-dry-run");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball = openclaw_package_tarball(
        "#!/usr/bin/env node\nconsole.log('2026.3.24');\n",
        "2026.3.24",
    );
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 2);

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let dry_run = run_ocm(&cwd, &env, &["upgrade", "demo", "--dry-run"]);
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    let output = stdout(&dry_run);
    assert!(output.contains("outcome=would-update"), "{output}");
    assert!(output.contains("dry run"), "{output}");
    assert!(!output.contains("snapshot="), "{output}");

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");

    let snapshots = run_ocm(&cwd, &env, &["env", "snapshot", "list", "demo", "--json"]);
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshot_json.as_array().unwrap().len(), 0);

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 0);
}

#[test]
fn upgrade_simulate_tests_a_published_version_without_changing_the_source_env() {
    let root = TestDir::new("upgrade-simulate-version");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball = openclaw_package_tarball("#!/bin/sh\nprintf '2026.3.24\\n'\n", "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball = openclaw_package_tarball("#!/bin/sh\nprintf '2026.3.25\\n'\n", "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));
    let source_state = root.child("ocm-home/envs/demo/.openclaw");
    let source_config = source_state.join("openclaw.json");
    let included_config = source_state.join("config/base.json");
    fs::create_dir_all(included_config.parent().unwrap()).unwrap();
    fs::write(
        &included_config,
        fs::read_to_string(&source_config).unwrap(),
    )
    .unwrap();
    fs::write(
        &source_config,
        "{\n  \"$include\": \"./config/base.json\"\n}\n",
    )
    .unwrap();

    let simulate = run_ocm(
        &cwd,
        &env,
        &["upgrade", "simulate", "demo", "--to", "2026.3.25", "--json"],
    );
    assert!(
        simulate.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&simulate),
        stderr(&simulate)
    );
    let json: Value = serde_json::from_str(&stdout(&simulate)).unwrap();
    assert_eq!(json["sourceEnv"], "demo");
    assert_eq!(json["outcome"], "passed");
    assert_eq!(json["toBindingKind"], "runtime");
    let runtime_name = json["toBindingName"].as_str().unwrap();
    assert!(
        runtime_name.starts_with("ocm-sim-runtime-"),
        "{runtime_name}"
    );
    assert_eq!(json["cleanup"], "cleaned");
    let sim_name = json["simulationEnv"].as_str().unwrap();
    assert_ne!(sim_name, "demo");

    let source = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let source_json: Value = serde_json::from_str(&stdout(&source)).unwrap();
    assert_eq!(source_json["defaultRuntime"], "stable");
    assert_eq!(
        fs::read_to_string(&source_config).unwrap(),
        "{\n  \"$include\": \"./config/base.json\"\n}\n"
    );
    assert!(included_config.exists());

    let simulation = run_ocm(&cwd, &env, &["env", "show", sim_name, "--json"]);
    assert!(!simulation.status.success(), "{}", stdout(&simulation));
    assert!(
        stderr(&simulation).contains("does not exist"),
        "{}",
        stderr(&simulation)
    );

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", runtime_name, "--json"]);
    assert!(!runtime.status.success(), "{}", stdout(&runtime));
    assert!(
        stderr(&runtime).contains("does not exist"),
        "{}",
        stderr(&runtime)
    );
}

#[test]
fn upgrade_simulate_errors_before_cloning_when_published_target_is_missing() {
    let root = TestDir::new("upgrade-simulate-missing-target");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("#!/bin/sh\nprintf '2026.3.24\\n'\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
        10,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25-beta.1\":{{\"version\":\"2026.3.25-beta.1\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25-beta.1\":\"2026-03-26T09:00:00.000Z\"}}}}",
        tarball_server.url(),
        integrity,
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

    let start = run_ocm(
        &cwd,
        &env,
        &["start", "demo", "--version", "2026.3.24", "--no-service"],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let simulate = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "simulate",
            "demo",
            "--to",
            "2026.3.25",
            "--scenario",
            "all",
        ],
    );
    assert!(!simulate.status.success(), "{}", stdout(&simulate));
    assert!(stdout(&simulate).is_empty(), "{}", stdout(&simulate));
    let error = stderr(&simulate);
    assert!(
        error.contains("OpenClaw release version \"2026.3.25\" was not found"),
        "{error}"
    );
    assert!(
        error.contains("simulation did not create any scenario envs"),
        "{error}"
    );
    assert!(error.contains("2026.3.25-beta.1"), "{error}");

    let list = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let envs: Value = serde_json::from_str(&stdout(&list)).unwrap();
    let envs = envs.as_array().unwrap();
    assert_eq!(envs.len(), 1, "{envs:#?}");
    assert_eq!(envs[0]["name"], "demo");
}

#[test]
fn upgrade_simulate_runs_openclaw_update_contract_checks_for_published_targets() {
    let root = TestDir::new("upgrade-simulate-contract");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let simulate = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "simulate",
            "demo",
            "--to",
            "2026.3.25",
            "--keep-simulations",
            "--json",
        ],
    );
    assert!(
        simulate.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&simulate),
        stderr(&simulate)
    );
    let json: Value = serde_json::from_str(&stdout(&simulate)).unwrap();
    assert_eq!(json["outcome"], "passed");
    assert_eq!(json["cleanup"], "kept");
    let runtime_name = json["toBindingName"].as_str().unwrap();
    assert!(
        runtime_name.starts_with("ocm-sim-runtime-"),
        "{runtime_name}"
    );
    let sim_name = json["simulationEnv"].as_str().unwrap();
    let simulation = run_ocm(&cwd, &env, &["env", "show", sim_name, "--json"]);
    assert!(simulation.status.success(), "{}", stderr(&simulation));
    let simulation_json: Value = serde_json::from_str(&stdout(&simulation)).unwrap();
    let sim_root = Path::new(simulation_json["root"].as_str().unwrap());
    let command_log = fs::read_to_string(sim_root.join("sim-commands.log")).unwrap();

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", runtime_name, "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));

    assert!(
        command_log.contains("update --dry-run --json --no-restart --yes --tag 2026.3.25"),
        "{command_log}"
    );
    assert!(command_log.contains("--version"), "{command_log}");
    assert!(
        command_log.contains("doctor --non-interactive --fix"),
        "{command_log}"
    );
    assert!(
        command_log.contains("plugins update --all --dry-run"),
        "{command_log}"
    );
    assert!(
        command_log.contains("gateway status --deep --json"),
        "{command_log}"
    );
}

#[test]
fn upgrade_simulate_all_scenarios_reports_plugin_specific_failures() {
    let root = TestDir::new("upgrade-simulate-scenarios");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball = openclaw_package_tarball(
        &scenario_sensitive_openclaw_script("2026.4.15"),
        "2026.4.15",
    );
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.4.15.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball = openclaw_package_tarball(
        &scenario_sensitive_openclaw_script("2026.4.20"),
        "2026.4.20",
    );
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.4.20.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.4.15\"}},\"versions\":{{\"2026.4.15\":{{\"version\":\"2026.4.15\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.4.15\":\"2026-04-15T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.4.20\"}},\"versions\":{{\"2026.4.15\":{{\"version\":\"2026.4.15\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.4.20\":{{\"version\":\"2026.4.20\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.4.15\":\"2026-04-15T16:35:52.000Z\",\"2026.4.20\":\"2026-04-20T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "scenario-source",
            "--version",
            "2026.4.15",
            "--no-service",
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let simulate = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "simulate",
            "scenario-source",
            "--to",
            "2026.4.20",
            "--scenario",
            "all",
            "--json",
        ],
    );
    assert!(!simulate.status.success(), "{}", stdout(&simulate));
    let json: Value = serde_json::from_str(&stdout(&simulate)).unwrap();
    assert_eq!(json["count"], 3);
    assert_eq!(json["passed"], 2);
    assert_eq!(json["failed"], 1);

    let results = json["results"].as_array().unwrap();
    let current = results
        .iter()
        .find(|entry| entry["scenario"] == "current")
        .unwrap();
    let minimum = results
        .iter()
        .find(|entry| entry["scenario"] == "minimum")
        .unwrap();
    let telegram = results
        .iter()
        .find(|entry| entry["scenario"] == "telegram")
        .unwrap();
    assert_eq!(current["outcome"], "passed");
    assert_eq!(minimum["outcome"], "passed");
    assert_eq!(telegram["outcome"], "failed");
    assert_eq!(current["cleanup"], "cleaned");
    assert_eq!(minimum["cleanup"], "cleaned");
    assert_eq!(telegram["cleanup"], "cleaned");
    assert_eq!(current["toBindingName"], minimum["toBindingName"]);
    assert_eq!(minimum["toBindingName"], telegram["toBindingName"]);
    assert!(
        telegram["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "openclaw doctor"
                && check["status"] == "failed"
                && check["note"]
                    .as_str()
                    .unwrap()
                    .contains("Cannot find module 'grammy'")),
        "{telegram:#}"
    );

    let source = run_ocm(&cwd, &env, &["env", "show", "scenario-source", "--json"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let source_json: Value = serde_json::from_str(&stdout(&source)).unwrap();
    assert_eq!(source_json["defaultRuntime"], "2026.4.15");
    assert_eq!(source_json["serviceEnabled"], false);

    let env_list = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    assert!(env_list.status.success(), "{}", stderr(&env_list));
    let envs: Value = serde_json::from_str(&stdout(&env_list)).unwrap();
    let envs = envs.as_array().unwrap();
    assert_eq!(envs.len(), 1, "{envs:#?}");
    assert_eq!(envs[0]["name"], "scenario-source");
}

#[test]
fn upgrade_simulate_reports_local_repo_doctor_failures() {
    let root = TestDir::new("upgrade-simulate-local-repo");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let repo = init_openclaw_repo(&root);

    let tarball = openclaw_package_tarball("console.log('2026.3.24');\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
        10,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes_times("/openclaw", "application/json", packument.as_bytes(), 1);

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    install_fake_simulation_pnpm(&root, &mut env);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let simulate = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "simulate",
            "demo",
            "--to",
            &path_string(&repo),
            "--raw",
        ],
    );
    assert!(!simulate.status.success(), "{}", stdout(&simulate));
    let output = stdout(&simulate);
    assert!(output.contains("outcome=failed"), "{output}");
    assert!(output.contains("check=openclaw doctor"), "{output}");
    assert!(output.contains("Cannot find module 'grammy'"), "{output}");

    let source = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let source_json: Value = serde_json::from_str(&stdout(&source)).unwrap();
    assert_eq!(source_json["defaultRuntime"], "stable");
    assert!(source_json["devRepoRoot"].is_null());
}

#[test]
fn upgrade_rolls_back_runtime_when_service_restart_fails() {
    let root = TestDir::new("upgrade-service-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );

    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    let start = run_ocm(&cwd, &env, &["start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let before = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(before.status.success(), "{}", stderr(&before));
    let before_json: Value = serde_json::from_str(&stdout(&before)).unwrap();
    let env_root = PathBuf::from(before_json["root"].as_str().unwrap());
    let dotenv = env_root.join(".openclaw/.env");
    let future_state = env_root.join("durable-future/rollback-marker.txt");
    write_text(&dotenv, "OPENCLAW_ROLLBACK_SENTINEL=before\n");
    write_text(&future_state, "safe pre-upgrade state\n");

    let launchctl_bin = env.get("OCM_INTERNAL_LAUNCHCTL_BIN").unwrap();
    let stopped_marker = root.child("service-stopped");
    write_executable_script(
        std::path::Path::new(launchctl_bin),
        &format!(
            "#!/bin/sh\nmarker='{}'\ncase \"$1\" in\n  managername)\n    exit 0\n    ;;\n  print)\n    if [ -e \"$marker\" ]; then\n      printf 'Could not find service \\\"%s\\\" in domain for user gui\\n' \"$2\" >&2\n      exit 1\n    fi\n    printf 'state = waiting\\n'\n    exit 0\n    ;;\n  bootout|unload)\n    : > \"$marker\"\n    exit 0\n    ;;\n  bootstrap)\n    echo 'forced bootstrap failure' >&2\n    exit 1\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
            stopped_marker.display()
        ),
    );

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--version", "2026.3.25"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(
        output.contains("outcome=rollback-failed"),
        "stdout:\n{output}\nstderr:\n{}",
        stderr(&upgrade)
    );
    assert!(output.contains("rollback=failed"), "{output}");
    assert!(
        output.contains("failed to restart the restored service"),
        "{output}"
    );
    assert!(output.contains("snapshot="), "{output}");

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");

    let target_runtime = run_ocm(&cwd, &env, &["runtime", "show", "2026.3.25", "--json"]);
    assert!(
        !target_runtime.status.success(),
        "{}",
        stdout(&target_runtime)
    );
    assert!(
        stderr(&target_runtime).contains("runtime \"2026.3.25\" does not exist"),
        "{}",
        stderr(&target_runtime)
    );

    let restored = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(restored.status.success(), "{}", stderr(&restored));
    let restored_json: Value = serde_json::from_str(&stdout(&restored)).unwrap();
    assert_eq!(restored_json["defaultRuntime"], "stable");
    assert_eq!(restored_json["root"], before_json["root"]);
    assert_eq!(
        fs::read_to_string(&dotenv).unwrap(),
        "OPENCLAW_ROLLBACK_SENTINEL=before\n"
    );
    assert_eq!(
        fs::read_to_string(&future_state).unwrap(),
        "safe pre-upgrade state\n"
    );
}

#[test]
fn upgrade_restores_runtime_when_runtime_preparation_fails() {
    let root = TestDir::new("upgrade-runtime-prepare-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball = openclaw_package_tarball("console.log('2026.3.24');\n", "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );

    let new_tarball = openclaw_package_tarball("console.log('2026.3.25');\n", "2026.3.25");
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"sha512-not-valid\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    assert!(
        !upgrade.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&upgrade),
        stderr(&upgrade)
    );
    let output = stdout(&upgrade);
    assert!(
        output.contains("outcome=rolled-back"),
        "stdout:\n{output}\nstderr:\n{}",
        stderr(&upgrade)
    );
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(
        output.contains("runtime artifact integrity is invalid"),
        "{output}"
    );

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");
}

#[test]
fn upgrade_rolls_back_when_post_upgrade_version_verification_fails() {
    let root = TestDir::new("upgrade-version-verification-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
    );

    let wrong_version_tarball = openclaw_package_tarball(
        &destructive_finalize_openclaw_script("2026.3.24"),
        "2026.3.25",
    );
    let wrong_version_integrity = sha512_integrity(&wrong_version_tarball);
    let wrong_version_tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &wrong_version_tarball,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        wrong_version_tarball_server.url(),
        wrong_version_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--no-service"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let state_root = root.child("ocm-home/envs/demo/.openclaw");
    let plugin_payloads = [
        (
            "npm/projects/demo/package.json",
            "{\"name\":\"@example/demo\"}\n",
        ),
        (
            "npm/projects/demo/package-lock.json",
            "{\"lockfileVersion\":3}\n",
        ),
        (
            "npm/projects/demo/node_modules/demo/index.js",
            "module.exports = 'restored';\n",
        ),
    ];
    for (path, contents) in plugin_payloads {
        let path = state_root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(
        output.contains("post-upgrade version verification failed"),
        "{output}"
    );

    let runtime = run_ocm(&cwd, &env, &["runtime", "show", "stable", "--json"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let runtime_json: Value = serde_json::from_str(&stdout(&runtime)).unwrap();
    assert_eq!(runtime_json["releaseVersion"], "2026.3.24");
    for (path, contents) in plugin_payloads {
        assert_eq!(
            fs::read_to_string(state_root.join(path)).unwrap(),
            contents,
            "{path}"
        );
    }
}

#[test]
fn upgrade_reports_pinned_envs_without_moving_them() {
    let root = TestDir::new("upgrade-pinned");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball = openclaw_package_tarball("console.log('2026.3.24');\n", "2026.3.24");
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

    let start = run_ocm(
        &cwd,
        &env,
        &["start", "pinned", "--version", "2026.3.24", "--no-service"],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "pinned"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=pinned"), "{output}");
    assert!(output.contains("exact release"), "{output}");
}

#[test]
fn upgrade_can_switch_a_local_launcher_env_to_a_published_runtime() {
    let root = TestDir::new("upgrade-launcher-to-runtime");
    let cwd = root.child("workspace");
    let project_dir = cwd.join("openclaw");
    fs::create_dir_all(&project_dir).unwrap();

    let tarball = openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
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

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "hacking",
            "--command",
            "pnpm openclaw",
            "--cwd",
            &project_dir.display().to_string(),
            "--no-service",
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "hacking", "--channel", "stable"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("from=launcher:hacking.local"), "{output}");
    assert!(output.contains("to=runtime:stable"), "{output}");
    assert!(output.contains("outcome=switched"), "{output}");

    let show = run_ocm(&cwd, &env, &["env", "show", "hacking", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "stable");
    assert!(env_json["defaultLauncher"].is_null());
}

#[test]
fn upgrade_rollback_refuses_a_broken_source_launcher_before_mutation() {
    let root = TestDir::new("upgrade-rollback-broken-source-launcher");
    let cwd = root.child("workspace");
    let project_dir = cwd.join("openclaw");
    fs::create_dir_all(&project_dir).unwrap();
    let launcher_binary = root.child("launcher-openclaw");
    write_executable_script(&launcher_binary, &recording_openclaw_script("2026.3.23"));

    let tarball = openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
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

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "hacking",
            "--command",
            &path_string(&launcher_binary),
            "--cwd",
            &path_string(&project_dir),
            "--no-service",
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "hacking", "--channel", "stable"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    assert!(stdout(&upgrade).contains("outcome=switched"));

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "hacking", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 1);
    assert_eq!(history_json[0]["source"]["kind"], "launcher");
    assert_eq!(history_json[0]["source"]["openclawVersion"], "2026.3.23");

    fs::remove_dir_all(&project_dir).unwrap();
    let rollback = run_ocm(
        &cwd,
        &env,
        &["upgrade", "rollback", "hacking", "--dry-run", "--json"],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("rollback source openclaw --version failed"),
        "{}",
        stderr(&rollback)
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "hacking", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "stable");
    assert!(env_json["defaultLauncher"].is_null());

    let snapshots = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "list", "hacking", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshots_json.as_array().unwrap().len(), 1);

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "hacking", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 1);
}

#[test]
fn upgrade_can_switch_env_to_an_installed_runtime() {
    let root = TestDir::new("upgrade-installed-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(
        &new_runtime,
        &prebinding_guard_openclaw_script("new-openclaw", "old-local"),
    );

    let env = ocm_env(&root);
    let add_old = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "old-local",
            "--path",
            &old_runtime.display().to_string(),
        ],
    );
    assert!(add_old.status.success(), "{}", stderr(&add_old));

    let add_new = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "new-local",
            "--path",
            &new_runtime.display().to_string(),
        ],
    );
    assert!(add_new.status.success(), "{}", stderr(&add_new));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "new-local"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("from=runtime:old-local"), "{output}");
    assert!(output.contains("to=runtime:new-local"), "{output}");
    assert!(output.contains("outcome=switched"), "{output}");

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "new-local");
    let env_root = Path::new(env_json["root"].as_str().unwrap());
    let command_log = fs::read_to_string(env_root.join("sim-commands.log")).unwrap();
    assert!(
        command_log.contains("update finalize --json --yes --no-restart"),
        "{command_log}"
    );
    assert!(
        !command_log.contains("doctor --non-interactive --fix"),
        "{command_log}"
    );
    assert!(
        !command_log.contains("plugins update --all\n"),
        "{command_log}"
    );
}

#[test]
fn upgrade_holds_the_environment_operation_lock_until_completion() {
    let root = TestDir::new("upgrade-operation-lock");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let mut env = ocm_env(&root);
    for (name, runtime) in [("old-local", &old_runtime), ("new-local", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let finalize_started = root.child("upgrade-finalize-started");
    let finalize_release = root.child("upgrade-finalize-release");
    env.insert(
        "OCM_TEST_UPDATE_FINALIZE_STARTED".to_string(),
        path_string(&finalize_started),
    );
    env.insert(
        "OCM_TEST_UPDATE_FINALIZE_RELEASE".to_string(),
        path_string(&finalize_release),
    );

    let mut upgrade_command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    upgrade_command
        .current_dir(&cwd)
        .args(["upgrade", "demo", "--runtime", "new-local"])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let upgrade = upgrade_command.spawn().unwrap();

    for _ in 0..200 {
        if finalize_started.exists() {
            break;
        }
        sleep(Duration::from_millis(25));
    }
    if !finalize_started.exists() {
        fs::write(&finalize_release, "").unwrap();
        let output = upgrade.wait_with_output().unwrap();
        panic!(
            "upgrade did not reach finalization: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut binding_command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    binding_command
        .current_dir(&cwd)
        .args(["env", "set-runtime", "demo", "old-local"])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut binding = binding_command.spawn().unwrap();
    sleep(Duration::from_millis(250));

    if let Some(status) = binding.try_wait().unwrap() {
        fs::write(&finalize_release, "").unwrap();
        let upgrade = upgrade.wait_with_output().unwrap();
        panic!(
            "binding mutation completed with {status} while upgrade held the environment operation lock; upgrade stderr: {}",
            String::from_utf8_lossy(&upgrade.stderr)
        );
    }

    fs::write(&finalize_release, "").unwrap();
    let upgrade = upgrade.wait_with_output().unwrap();
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let binding = binding.wait_with_output().unwrap();
    assert!(binding.status.success(), "{}", stderr(&binding));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "old-local");
}

#[test]
fn upgrade_reuses_the_bound_named_runtime_without_retaining_recovery_bytes() {
    let root = TestDir::new("upgrade-reuse-bound-runtime");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let runtime_path = root.child("local-openclaw");
    write_executable_script(&runtime_path, &recording_openclaw_script("local-openclaw"));

    let env = ocm_env(&root);
    let add = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "local",
            "--path",
            &runtime_path.display().to_string(),
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--runtime", "local"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "local"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=up-to-date"), "{output}");

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    let record = &history_json[0];
    assert_eq!(record["source"]["name"], "local");
    assert_eq!(record["target"]["name"], "local");
    assert!(record["runtimeRecovery"].as_array().unwrap().is_empty());

    let recovery_root = Path::new(env.get("OCM_HOME").unwrap())
        .join("upgrade-history")
        .join("demo")
        .join(format!("{}.recovery", record["id"].as_str().unwrap()));
    assert!(!recovery_root.exists());
}

#[test]
fn upgrade_rollback_restores_and_reverses_a_runtime_switch() {
    let root = TestDir::new("upgrade-explicit-rollback-switch");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("2026.6.11"));
    write_executable_script(&new_runtime, &recording_openclaw_script("2026.6.33"));

    let env = ocm_env(&root);
    for (name, runtime) in [("old", &old_runtime), ("new", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }
    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--runtime", "old"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_json: Value = serde_json::from_str(&stdout(&env_show)).unwrap();
    let marker = Path::new(env_json["root"].as_str().unwrap())
        .join(".openclaw")
        .join("workspace")
        .join("rollback-marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "before-upgrade").unwrap();

    let upgrade = run_ocm(
        &cwd,
        &env,
        &["upgrade", "demo", "--runtime", "new", "--raw"],
    );
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    assert!(stdout(&upgrade).contains("outcome=switched"));
    fs::write(&marker, "after-upgrade").unwrap();

    let history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    let original_id = history_json[0]["id"].as_str().unwrap().to_string();

    let dry_run = run_ocm(
        &cwd,
        &env,
        &["upgrade", "rollback", "demo", "--dry-run", "--json"],
    );
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    let dry_run_json: Value = serde_json::from_str(&stdout(&dry_run)).unwrap();
    assert_eq!(dry_run_json["transactionId"], original_id);
    assert_eq!(dry_run_json["outcome"], "would-rollback");
    assert_eq!(dry_run_json["bindingName"], "old");
    assert!(dry_run_json["safetySnapshotId"].is_null());
    assert_eq!(fs::read_to_string(&marker).unwrap(), "after-upgrade");

    let rollback = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "rollback",
            "demo",
            "--transaction",
            &original_id,
            "--json",
        ],
    );
    assert!(rollback.status.success(), "{}", stderr(&rollback));
    let rollback_json: Value = serde_json::from_str(&stdout(&rollback)).unwrap();
    assert_eq!(rollback_json["transactionId"], original_id);
    assert_eq!(rollback_json["outcome"], "rolled-back");
    assert_eq!(rollback_json["previousBindingName"], "new");
    assert_eq!(rollback_json["bindingName"], "old");
    assert_eq!(rollback_json["runtimeReleaseVersion"], "2026.6.11");
    assert!(rollback_json["rollbackTransactionId"].is_string());
    assert!(rollback_json["safetySnapshotId"].is_string());
    assert_eq!(fs::read_to_string(&marker).unwrap(), "before-upgrade");

    let version = run_ocm(&cwd, &env, &["@demo", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), "2026.6.11");

    let rollback_history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(
        rollback_history.status.success(),
        "{}",
        stderr(&rollback_history)
    );
    let rollback_history_json: Value = serde_json::from_str(&stdout(&rollback_history)).unwrap();
    assert_eq!(rollback_history_json.as_array().unwrap().len(), 2);
    assert_eq!(rollback_history_json[0]["outcome"], "rolled-back");
    assert_eq!(rollback_history_json[0]["rollbackOf"], original_id);
    assert_eq!(rollback_history_json[0]["source"]["name"], "new");
    assert_eq!(rollback_history_json[0]["target"]["name"], "old");

    let reverse = run_ocm(&cwd, &env, &["upgrade", "rollback", "demo", "--raw"]);
    assert!(reverse.status.success(), "{}", stderr(&reverse));
    assert!(stdout(&reverse).contains("outcome=rolled-back"));
    assert!(stdout(&reverse).contains("to=runtime:new"));
    assert_eq!(fs::read_to_string(&marker).unwrap(), "after-upgrade");

    let version = run_ocm(&cwd, &env, &["@demo", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), "2026.6.33");

    let final_history = run_ocm(&cwd, &env, &["upgrade", "history", "demo", "--json"]);
    assert!(final_history.status.success(), "{}", stderr(&final_history));
    let final_history_json: Value = serde_json::from_str(&stdout(&final_history)).unwrap();
    assert_eq!(final_history_json.as_array().unwrap().len(), 3);
    assert_eq!(
        final_history_json[0]["rollbackOf"],
        rollback_json["rollbackTransactionId"]
    );
}

#[test]
fn upgrade_rollback_restores_retained_in_place_runtime_bytes() {
    let root = TestDir::new("upgrade-explicit-rollback-in-place");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(rollback.status.success(), "{}", stderr(&rollback));
    let rollback_json: Value = serde_json::from_str(&stdout(&rollback)).unwrap();
    assert_eq!(rollback_json["transactionId"], fixture.transaction_id);
    assert_eq!(rollback_json["outcome"], "rolled-back");
    assert_eq!(rollback_json["bindingName"], "stable");
    assert_eq!(rollback_json["runtimeReleaseVersion"], "2026.6.11");
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "before-upgrade"
    );
    assert!(!fixture.original_recovery_root.exists());

    let version = run_ocm(&fixture.cwd, &fixture.env, &["@demo", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), "2026.6.11");

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 2);
    let rollback_record = &history_json[0];
    assert_eq!(rollback_record["rollbackOf"], fixture.transaction_id);
    assert_eq!(
        rollback_record["runtimeRecovery"][0]["runtimeName"],
        "stable"
    );
    assert_eq!(
        rollback_record["runtimeRecovery"][0]["releaseVersion"],
        "2026.6.33"
    );
    assert_eq!(rollback_record["runtimeRecovery"][0]["backupId"], "stable");
    let reverse_recovery = Path::new(fixture.env.get("OCM_HOME").unwrap())
        .join("upgrade-history/demo")
        .join(format!(
            "{}.recovery/stable/install-root/files/bin/openclaw",
            rollback_record["id"].as_str().unwrap()
        ));
    assert!(reverse_recovery.is_file());

    let reverse = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(reverse.status.success(), "{}", stderr(&reverse));
    let reverse_json: Value = serde_json::from_str(&stdout(&reverse)).unwrap();
    assert_eq!(reverse_json["outcome"], "rolled-back");
    assert_eq!(reverse_json["runtimeReleaseVersion"], "2026.6.33");
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );

    let version = run_ocm(&fixture.cwd, &fixture.env, &["@demo", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), "2026.6.33");
}

#[test]
fn upgrade_rollback_failure_restores_the_pre_rollback_state() {
    let root = TestDir::new("upgrade-explicit-rollback-failure");
    let fixture = seed_in_place_rollback(&root, "broken-recovery");

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(!rollback.status.success());
    let rollback_json: Value = serde_json::from_str(&stdout(&rollback)).unwrap();
    assert_eq!(rollback_json["transactionId"], fixture.transaction_id);
    assert_eq!(rollback_json["outcome"], "failed");
    assert!(rollback_json["safetySnapshotId"].is_string());
    assert!(
        rollback_json["note"]
            .as_str()
            .unwrap()
            .contains("restored the pre-rollback state")
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );
    assert!(fixture.original_recovery_root.exists());

    let version = run_ocm(&fixture.cwd, &fixture.env, &["@demo", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), "2026.6.33");

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 2);
    assert_eq!(history_json[0]["outcome"], "failed");
    assert_eq!(history_json[0]["rollback"], "restored");
    assert_eq!(history_json[0]["rollbackOf"], fixture.transaction_id);

    let snapshots = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["env", "snapshot", "list", "demo", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshots_json.as_array().unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn upgrade_rollback_removes_an_unrecorded_safety_snapshot_after_restore() {
    let root = TestDir::new("upgrade-rollback-history-write-failure");
    let fixture = seed_in_place_rollback(&root, "broken-recovery");
    let history_dir = Path::new(fixture.env.get("OCM_HOME").unwrap()).join("upgrade-history/demo");
    let mut permissions = fs::metadata(&history_dir).unwrap().permissions();
    let original_mode = permissions.mode();
    permissions.set_mode(0o500);
    fs::set_permissions(&history_dir, permissions).unwrap();

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );

    let mut permissions = fs::metadata(&history_dir).unwrap().permissions();
    permissions.set_mode(original_mode);
    fs::set_permissions(&history_dir, permissions).unwrap();

    assert!(!rollback.status.success());
    let rollback_json: Value = serde_json::from_str(&stdout(&rollback)).unwrap();
    assert_eq!(rollback_json["outcome"], "failed");
    assert!(rollback_json["safetySnapshotId"].is_null());
    assert!(
        rollback_json["note"]
            .as_str()
            .unwrap()
            .contains("Rollback history was not recorded")
    );
    assert!(
        rollback_json["note"]
            .as_str()
            .unwrap()
            .contains("Unrecorded safety snapshot was removed")
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );
    assert!(fixture.original_recovery_root.exists());

    let snapshots = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["env", "snapshot", "list", "demo", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshots_json.as_array().unwrap().len(), 1);

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 1);
}

#[test]
fn upgrade_rollback_cleans_snapshot_when_transaction_setup_fails() {
    let root = TestDir::new("upgrade-explicit-rollback-setup-failure");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    let ocm_home = Path::new(fixture.env.get("OCM_HOME").unwrap());
    fs::write(ocm_home.join("tmp"), "block transaction backup directory").unwrap();

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("failed to create transaction runtime backup directory")
            || stderr(&rollback).contains("Not a directory")
            || stderr(&rollback).contains("File exists"),
        "{}",
        stderr(&rollback)
    );

    let snapshots = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["env", "snapshot", "list", "demo", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshots_json.as_array().unwrap().len(), 1);

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );
    assert!(fixture.original_recovery_root.exists());
}

#[test]
fn upgrade_rollback_refuses_missing_in_place_recovery_before_mutation() {
    let root = TestDir::new("upgrade-explicit-rollback-missing-recovery");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    fs::remove_dir_all(&fixture.original_recovery_root).unwrap();

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("runtime recovery"),
        "{}",
        stderr(&rollback)
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );

    let snapshots = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["env", "snapshot", "list", "demo", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshots_json.as_array().unwrap().len(), 1);
}

#[test]
fn upgrade_rollback_refuses_recovery_from_a_different_snapshot_before_mutation() {
    let root = TestDir::new("upgrade-explicit-rollback-mismatched-recovery-snapshot");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    fs::write(
        fixture.original_recovery_root.join("snapshot-id"),
        "different-snapshot",
    )
    .unwrap();

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("belongs to snapshot \"different-snapshot\", expected"),
        "{}",
        stderr(&rollback)
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );
    assert!(fixture.original_recovery_root.exists());

    let snapshots = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["env", "snapshot", "list", "demo", "--json"],
    );
    assert!(snapshots.status.success(), "{}", stderr(&snapshots));
    let snapshots_json: Value = serde_json::from_str(&stdout(&snapshots)).unwrap();
    assert_eq!(snapshots_json.as_array().unwrap().len(), 1);

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 1);
}

#[test]
fn upgrade_rollback_refuses_drifted_target_version_before_mutation() {
    let root = TestDir::new("upgrade-explicit-rollback-target-drift");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    let runtime_binary =
        Path::new(fixture.env.get("OCM_HOME").unwrap()).join("runtimes/stable/files/bin/openclaw");
    write_executable_script(&runtime_binary, &recording_openclaw_script("2026.7.1"));

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("reports OpenClaw 2026.7.1, expected 2026.6.33"),
        "{}",
        stderr(&rollback)
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );
    assert!(fixture.original_recovery_root.exists());

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 1);
}

#[test]
fn upgrade_rollback_refuses_to_replace_a_runtime_shared_after_upgrade() {
    let root = TestDir::new("upgrade-explicit-rollback-shared-runtime");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    let sibling = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["env", "create", "sibling", "--runtime", "stable"],
    );
    assert!(sibling.status.success(), "{}", stderr(&sibling));

    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(!rollback.status.success());
    assert!(
        stderr(&rollback).contains("runtime \"stable\" is shared with \"sibling\""),
        "{}",
        stderr(&rollback)
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "after-upgrade"
    );

    let version = run_ocm(&fixture.cwd, &fixture.env, &["@demo", "--", "--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version).trim(), "2026.6.33");
}

#[test]
fn upgrade_rollback_refuses_a_transaction_with_a_successful_child() {
    let root = TestDir::new("upgrade-explicit-rollback-already-used");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    let rollback = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--json"],
    );
    assert!(rollback.status.success(), "{}", stderr(&rollback));

    let repeated = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &[
            "upgrade",
            "rollback",
            "demo",
            "--transaction",
            &fixture.transaction_id,
            "--json",
        ],
    );
    assert!(!repeated.status.success());
    assert!(
        stderr(&repeated).contains("has already been rolled back"),
        "{}",
        stderr(&repeated)
    );
    assert_eq!(
        fs::read_to_string(&fixture.marker).unwrap(),
        "before-upgrade"
    );

    let history = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "history", "demo", "--json"],
    );
    assert!(history.status.success(), "{}", stderr(&history));
    let history_json: Value = serde_json::from_str(&stdout(&history)).unwrap();
    assert_eq!(history_json.as_array().unwrap().len(), 2);
}

#[test]
fn upgrade_rollback_skips_newer_automatic_failure_rollback_history() {
    let root = TestDir::new("upgrade-explicit-rollback-skips-automatic");
    let fixture = seed_in_place_rollback(&root, "2026.6.11");
    let automatic_id = "1783728000-000000001";
    let automatic = serde_json::json!({
        "kind": "ocm-upgrade-transaction",
        "formatVersion": 1,
        "id": automatic_id,
        "envName": "demo",
        "source": {
            "kind": "runtime",
            "name": "stable",
            "openclawVersion": "2026.6.33"
        },
        "target": {
            "kind": "runtime",
            "name": "stable",
            "openclawVersion": "2026.7.1"
        },
        "snapshotId": "failed-upgrade-snapshot",
        "runtimeRecovery": [],
        "startedAt": "2026-07-11T00:00:00Z",
        "completedAt": "2026-07-11T00:01:00Z",
        "outcome": "rolled-back",
        "migration": {"status": "failed"},
        "finalization": {"status": "not-run"},
        "serviceBefore": {"enabled": false, "running": false},
        "serviceAfter": {"enabled": false, "running": false},
        "rollback": "restored"
    });
    let history_dir = Path::new(fixture.env.get("OCM_HOME").unwrap()).join("upgrade-history/demo");
    fs::write(
        history_dir.join(format!("{automatic_id}.json")),
        serde_json::to_vec(&automatic).unwrap(),
    )
    .unwrap();

    let dry_run = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["upgrade", "rollback", "demo", "--dry-run", "--json"],
    );
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    let dry_run_json: Value = serde_json::from_str(&stdout(&dry_run)).unwrap();
    assert_eq!(dry_run_json["transactionId"], fixture.transaction_id);

    let explicit = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &[
            "upgrade",
            "rollback",
            "demo",
            "--transaction",
            automatic_id,
            "--json",
        ],
    );
    assert!(!explicit.status.success());
    assert!(
        stderr(&explicit).contains("cannot be rolled back because its outcome is \"rolled-back\""),
        "{}",
        stderr(&explicit)
    );
}

#[test]
fn upgrade_rollback_restarts_and_verifies_a_managed_service() {
    let root = TestDir::new("upgrade-explicit-rollback-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let health_server =
        TestHttpServer::serve_bytes_times("/health", "application/json", br#"{"ok":true}"#, 20);
    let health_port = health_server
        .url()
        .split(':')
        .nth(2)
        .and_then(|value| value.split('/').next())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("2026.6.11"));
    write_executable_script(&new_runtime, &recording_openclaw_script("2026.6.33"));

    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    for (name, runtime) in [("old", &old_runtime), ("new", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "demo",
            "--runtime",
            "old",
            "--port",
            &health_port.to_string(),
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));
    let env_show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let env_json: Value = serde_json::from_str(&stdout(&env_show)).unwrap();
    let marker =
        Path::new(env_json["root"].as_str().unwrap()).join(".openclaw/workspace/rollback-marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "before-upgrade").unwrap();

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "demo", "--json"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap();
    let bind = run_ocm(&cwd, &env, &["env", "set-runtime", "demo", "new"]);
    assert!(bind.status.success(), "{}", stderr(&bind));
    fs::write(&marker, "after-upgrade").unwrap();

    let transaction_id = "1782864000-000000001";
    let history = serde_json::json!({
        "kind": "ocm-upgrade-transaction",
        "formatVersion": 1,
        "id": transaction_id,
        "envName": "demo",
        "source": {
            "kind": "runtime",
            "name": "old",
            "openclawVersion": "2026.6.11"
        },
        "target": {
            "kind": "runtime",
            "name": "new",
            "openclawVersion": "2026.6.33"
        },
        "snapshotId": snapshot_id,
        "runtimeRecovery": [],
        "startedAt": "2026-07-01T00:00:00Z",
        "completedAt": "2026-07-01T00:01:00Z",
        "outcome": "switched",
        "migration": {"status": "validated"},
        "finalization": {"status": "completed"},
        "serviceBefore": {"enabled": true, "running": true},
        "serviceAfter": {"enabled": true, "running": true}
    });
    let history_dir = Path::new(env.get("OCM_HOME").unwrap()).join("upgrade-history/demo");
    fs::create_dir_all(&history_dir).unwrap();
    fs::write(
        history_dir.join(format!("{transaction_id}.json")),
        serde_json::to_vec(&history).unwrap(),
    )
    .unwrap();

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    write_running_supervisor_runtime(&runtime_path, &ocm_home, "new", 4242, health_port);
    let state_path = supervisor_state_path(&env, &cwd).unwrap();
    let observed_runtime_path = runtime_path.clone();
    let observed_ocm_home = ocm_home.clone();
    let service_observer = thread::spawn(move || {
        let mut saw_stop = false;
        for _ in 0..400 {
            let state = fs::read_to_string(&state_path).unwrap_or_default();
            let parsed: Value = serde_json::from_str(&state).unwrap_or(Value::Null);
            let children = parsed["children"].as_array().cloned().unwrap_or_default();
            if !saw_stop && children.is_empty() {
                write_empty_supervisor_runtime(&observed_runtime_path, &observed_ocm_home);
                saw_stop = true;
            } else if saw_stop
                && children
                    .iter()
                    .any(|child| child["envName"] == "demo" && child["bindingName"] == "old")
            {
                write_running_supervisor_runtime(
                    &observed_runtime_path,
                    &observed_ocm_home,
                    "old",
                    4243,
                    health_port,
                );
                return;
            }
            sleep(Duration::from_millis(25));
        }
        panic!("rollback did not stop and restart the managed service");
    });

    let rollback = run_ocm(&cwd, &env, &["upgrade", "rollback", "demo", "--raw"]);
    service_observer.join().unwrap();
    assert!(rollback.status.success(), "{}", stderr(&rollback));
    let output = stdout(&rollback);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("service=started"), "{output}");
    assert!(output.contains("to=runtime:old"), "{output}");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "before-upgrade");
    assert!(!health_server.requests().is_empty());

    let service = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(service.status.success(), "{}", stderr(&service));
    let service_json: Value = serde_json::from_str(&stdout(&service)).unwrap();
    assert_eq!(service_json["running"], true);
    assert_eq!(service_json["bindingName"], "old");
}

#[test]
fn upgrade_repairs_target_config_before_finalization() {
    let root = TestDir::new("upgrade-target-config-repair");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let mut env = ocm_env(&root);
    for (name, runtime) in [("old-local", &old_runtime), ("new-local", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let env_root = root.child("ocm-home/envs/demo");
    fs::write(
        env_root.join(".openclaw/openclaw.json"),
        "{\"meta\":{\"lastTouchedAt\":\"legacy\"}}\n",
    )
    .unwrap();

    env.insert(
        "OCM_TEST_INVALID_CONFIG_UNTIL_DOCTOR".to_string(),
        "1".to_string(),
    );
    env.insert(
        "OCM_TEST_REQUIRE_DOCTOR_OWNERSHIP_FLAGS".to_string(),
        "1".to_string(),
    );
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "new-local"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=switched"), "{output}");
    assert!(output.contains("config repair"), "{output}");

    let command_log = fs::read_to_string(env_root.join("sim-commands.log")).unwrap();
    let validate_before = command_log.find("config validate").unwrap();
    let doctor = command_log.find("doctor --non-interactive --fix").unwrap();
    let validate_after = command_log.rfind("config validate").unwrap();
    let finalize = command_log
        .find("update finalize --json --yes --no-restart")
        .unwrap();
    assert!(validate_before < doctor, "{command_log}");
    assert!(doctor < validate_after, "{command_log}");
    assert!(validate_after < finalize, "{command_log}");
    assert!(env_root.join(".openclaw/config-repaired").exists());

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "new-local");
}

#[test]
fn upgrade_rolls_back_when_target_config_doctor_fails() {
    let root = TestDir::new("upgrade-target-config-doctor-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let mut env = ocm_env(&root);
    for (name, runtime) in [("old-local", &old_runtime), ("new-local", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let env_root = root.child("ocm-home/envs/demo");
    let config_path = env_root.join(".openclaw/openclaw.json");
    let original_config = "{\"meta\":{\"lastTouchedAt\":\"legacy\"}}\n";
    fs::write(&config_path, original_config).unwrap();

    env.insert(
        "OCM_TEST_INVALID_CONFIG_UNTIL_DOCTOR".to_string(),
        "1".to_string(),
    );
    env.insert("OCM_TEST_FAIL_DOCTOR".to_string(), "1".to_string());
    let command_log_path = root.child("doctor-failure-commands.log");
    env.insert(
        "OCM_TEST_COMMAND_LOG".to_string(),
        path_string(&command_log_path),
    );
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "new-local"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(output.contains("openclaw doctor failed"), "{output}");

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "old-local");
    assert_eq!(fs::read_to_string(config_path).unwrap(), original_config);
    assert!(!env_root.join(".openclaw/config-repaired").exists());

    let command_log = fs::read_to_string(command_log_path).unwrap();
    assert!(command_log.contains("config validate"), "{command_log}");
    assert!(
        command_log.contains("doctor --non-interactive --fix"),
        "{command_log}"
    );
    assert!(
        !command_log.contains("update finalize --json --yes --no-restart"),
        "{command_log}"
    );
}

#[test]
fn upgrade_rolls_back_when_doctor_does_not_repair_target_config() {
    let root = TestDir::new("upgrade-target-config-revalidation-failure");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let mut env = ocm_env(&root);
    for (name, runtime) in [("old-local", &old_runtime), ("new-local", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let env_root = root.child("ocm-home/envs/demo");
    let config_path = env_root.join(".openclaw/openclaw.json");
    let original_config = "{\"meta\":{\"lastTouchedAt\":\"legacy\"}}\n";
    fs::write(&config_path, original_config).unwrap();

    env.insert(
        "OCM_TEST_INVALID_CONFIG_UNTIL_DOCTOR".to_string(),
        "1".to_string(),
    );
    env.insert(
        "OCM_TEST_DOCTOR_LEAVES_CONFIG_INVALID".to_string(),
        "1".to_string(),
    );
    let command_log_path = root.child("revalidation-failure-commands.log");
    env.insert(
        "OCM_TEST_COMMAND_LOG".to_string(),
        path_string(&command_log_path),
    );
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "new-local"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(
        output.contains("openclaw config validate after doctor failed"),
        "{output}"
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "old-local");
    assert_eq!(fs::read_to_string(config_path).unwrap(), original_config);

    let command_log = fs::read_to_string(command_log_path).unwrap();
    assert_eq!(command_log.matches("config validate").count(), 2);
    assert!(
        command_log.contains("doctor --non-interactive --fix"),
        "{command_log}"
    );
    assert!(
        !command_log.contains("update finalize --json --yes --no-restart"),
        "{command_log}"
    );
}

#[test]
fn upgrade_skips_config_repair_when_openclaw_config_is_missing() {
    let root = TestDir::new("upgrade-missing-config");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let env = ocm_env(&root);
    for (name, runtime) in [("old-local", &old_runtime), ("new-local", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let env_root = root.child("ocm-home/envs/demo");
    let config_path = env_root.join(".openclaw/openclaw.json");
    assert!(!config_path.exists());

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "new-local"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=switched"), "{output}");
    assert!(!output.contains("config repair"), "{output}");

    let command_log = fs::read_to_string(env_root.join("sim-commands.log")).unwrap();
    assert!(!command_log.contains("config validate"), "{command_log}");
    assert!(
        !command_log.contains("doctor --non-interactive --fix"),
        "{command_log}"
    );
    assert!(
        command_log.contains("update finalize --json --yes --no-restart"),
        "{command_log}"
    );
    assert!(!config_path.exists());

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "new-local");
}

#[test]
fn env_set_runtime_uses_the_transactional_upgrade_path_for_existing_bindings() {
    let root = TestDir::new("set-runtime-transactional-upgrade");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let mut env = ocm_env(&root);
    for (name, runtime) in [("old-local", &old_runtime), ("new-local", &new_runtime)] {
        let add = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                &runtime.display().to_string(),
            ],
        );
        assert!(add.status.success(), "{}", stderr(&add));
    }

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    env.insert("OCM_TEST_FAIL_UPDATE_FINALIZE".to_string(), "1".to_string());
    let bind = run_ocm(&cwd, &env, &["env", "set-runtime", "demo", "new-local"]);
    assert!(!bind.status.success(), "{}", stdout(&bind));
    assert!(
        stderr(&bind).contains("restored the pre-upgrade snapshot"),
        "{}",
        stderr(&bind)
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "old-local");
}

#[test]
fn upgrade_rolls_back_runtime_binding_when_update_finalization_fails() {
    let root = TestDir::new("upgrade-finalize-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let old_runtime = root.child("old-openclaw");
    let new_runtime = root.child("new-openclaw");
    write_executable_script(&old_runtime, &recording_openclaw_script("old-openclaw"));
    write_executable_script(&new_runtime, &recording_openclaw_script("new-openclaw"));

    let mut env = ocm_env(&root);
    let add_old = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "old-local",
            "--path",
            &old_runtime.display().to_string(),
        ],
    );
    assert!(add_old.status.success(), "{}", stderr(&add_old));

    let add_new = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "new-local",
            "--path",
            &new_runtime.display().to_string(),
        ],
    );
    assert!(add_new.status.success(), "{}", stderr(&add_new));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "old-local"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    fs::write(
        root.child("ocm-home/envs/demo/.openclaw/openclaw.json"),
        r#"{"agents":{"list":[{"id":"main","default":true},{"id":"clawforce"}]}}"#,
    )
    .unwrap();
    let secondary_skill =
        root.child("ocm-home/envs/demo/.openclaw/workspace-clawforce/skills/social/SKILL.md");
    fs::create_dir_all(secondary_skill.parent().unwrap()).unwrap();
    fs::write(&secondary_skill, "skill before upgrade\n").unwrap();

    env.insert("OCM_TEST_FAIL_UPDATE_FINALIZE".to_string(), "1".to_string());
    let upgrade = run_ocm(&cwd, &env, &["upgrade", "demo", "--runtime", "new-local"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("outcome=rolled-back"), "{output}");
    assert!(output.contains("rollback=restored"), "{output}");
    assert!(
        output.contains("openclaw update finalize failed"),
        "{output}"
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let env_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(env_json["defaultRuntime"], "old-local");
    assert_eq!(
        fs::read_to_string(secondary_skill).unwrap(),
        "skill before upgrade\n"
    );
}

#[test]
fn upgrade_keeps_a_stopped_installed_service_stopped() {
    let root = TestDir::new("upgrade-stopped-service-start");
    let cwd = root.child("workspace");
    let project_dir = cwd.join("openclaw");
    fs::create_dir_all(&project_dir).unwrap();

    let tarball = openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
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
    install_fake_launchctl(&root, &mut env);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );

    let start = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "hacking",
            "--command",
            "pnpm openclaw",
            "--cwd",
            &project_dir.display().to_string(),
        ],
    );
    assert!(start.status.success(), "{}", stderr(&start));

    let stop = run_ocm(&cwd, &env, &["service", "stop", "hacking"]);
    assert!(stop.status.success(), "{}", stderr(&stop));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "hacking", "--channel", "stable"]);
    assert!(upgrade.status.success(), "{}", stderr(&upgrade));
    let output = stdout(&upgrade);
    assert!(output.contains("from=launcher:hacking.local"), "{output}");
    assert!(output.contains("to=runtime:stable"), "{output}");
    assert!(!output.contains("service="), "{output}");
}

#[test]
fn upgrade_all_updates_safe_envs_and_skips_local_or_pinned_ones() {
    let root = TestDir::new("upgrade-all");
    let cwd = root.child("workspace");
    let project_dir = cwd.join("openclaw");
    fs::create_dir_all(&project_dir).unwrap();

    let old_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.24"), "2026.3.24");
    let old_integrity = sha512_integrity(&old_tarball);
    let old_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &old_tarball,
        10,
    );
    let new_tarball =
        openclaw_package_tarball(&recording_openclaw_script("2026.3.25"), "2026.3.25");
    let new_integrity = sha512_integrity(&new_tarball);
    let new_tarball_server = TestHttpServer::serve_bytes_times(
        "/openclaw-2026.3.25.tgz",
        "application/octet-stream",
        &new_tarball,
        10,
    );

    let initial_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity
    );
    let updated_packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.25\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}},\"2026.3.25\":{{\"version\":\"2026.3.25\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\",\"2026.3.25\":\"2026-03-26T09:00:00.000Z\"}}}}",
        old_tarball_server.url(),
        old_integrity,
        new_tarball_server.url(),
        new_integrity
    );
    let packument_server = TestHttpServer::serve_bytes_sequence(
        "/openclaw",
        "application/json",
        vec![
            initial_packument.as_bytes().to_vec(),
            initial_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
            updated_packument.as_bytes().to_vec(),
        ],
    );

    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let stable = run_ocm(&cwd, &env, &["start", "stable-env", "--no-service"]);
    assert!(stable.status.success(), "{}", stderr(&stable));
    let pinned = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "pinned-env",
            "--version",
            "2026.3.24",
            "--no-service",
        ],
    );
    assert!(pinned.status.success(), "{}", stderr(&pinned));
    let local = run_ocm(
        &cwd,
        &env,
        &[
            "start",
            "local-env",
            "--command",
            "pnpm openclaw",
            "--cwd",
            &project_dir.display().to_string(),
            "--no-service",
        ],
    );
    assert!(local.status.success(), "{}", stderr(&local));

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "--all", "--json"]);
    assert!(
        upgrade.status.success(),
        "stderr:\n{}\nstdout:\n{}",
        stderr(&upgrade),
        stdout(&upgrade)
    );
    let json: Value = serde_json::from_str(&stdout(&upgrade)).unwrap();
    assert_eq!(json["count"], 3);
    assert_eq!(json["changed"], 1);
    assert_eq!(json["current"], 0);
    assert_eq!(json["skipped"], 2);
    assert_eq!(json["failed"], 0);
}

#[test]
fn upgrade_all_json_exits_failed_when_any_env_fails() {
    let root = TestDir::new("upgrade-all-json-failed");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let runtime_path = root.child("openclaw");
    write_executable_script(&runtime_path, "#!/bin/sh\necho fake-openclaw\n");

    let env = ocm_env(&root);
    let add_runtime = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "add",
            "missing-soon",
            "--path",
            &runtime_path.display().to_string(),
        ],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "broken", "--runtime", "missing-soon"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    fs::remove_file(root.child("ocm-home/runtimes/missing-soon.json")).unwrap();

    let upgrade = run_ocm(&cwd, &env, &["upgrade", "--all", "--json"]);
    assert!(!upgrade.status.success(), "{}", stdout(&upgrade));
    let json: Value = serde_json::from_str(&stdout(&upgrade)).unwrap();
    assert_eq!(json["count"], 1);
    assert_eq!(json["failed"], 1);
    assert_eq!(json["results"][0]["outcome"], "failed");
    assert!(
        json["results"][0]["note"]
            .as_str()
            .unwrap()
            .contains("runtime \"missing-soon\" does not exist")
    );
}
