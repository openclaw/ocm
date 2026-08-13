mod support;

use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;
use std::{fs, path::Path};

use ocm::store::{env_registry_path, now_utc, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::support::{
    TestDir, TestHttpServer, install_fake_launchctl, ocm_env, path_string, run_ocm, stderr, stdout,
    write_executable_script, write_text,
};

fn write_running_snapshot_service(
    root: &TestDir,
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    port: u32,
) {
    let runtime_path = supervisor_runtime_path(env, cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let stdout_path = path_string(&root.child("source.stdout.log"));
    let stderr_path = path_string(&root.child("source.stderr.log"));
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: env.get("OCM_HOME").unwrap().clone(),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "source".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            gateway_state: "running".to_string(),
            restart_handoff: Some("none".to_string()),
            restart_count: 0,
            child_port: port,
            pid: Some(4242),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
        }],
        children: vec![SupervisorRuntimeChild {
            env_name: "source".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            pid: 4242,
            restart_count: 0,
            child_port: port,
            stdout_path,
            stderr_path,
        }],
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
}

fn write_empty_snapshot_service(runtime_path: &Path, ocm_home: &str) {
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: ocm_home.to_string(),
        updated_at: now_utc(),
        services: Vec::new(),
        children: Vec::new(),
    };
    fs::write(runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
}

#[test]
fn env_snapshot_create_captures_the_current_environment_state() {
    let root = TestDir::new("env-snapshot-create");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "source", "--port", "19789", "--protect"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt"),
        "hello snapshot",
    );

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "source",
            "--label",
            "before-upgrade",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let output = stdout(&snapshot);
    assert!(output.contains("Created snapshot"));
    assert!(output.contains("for env source"));
    assert!(output.contains("label: before-upgrade"));
    assert!(root.child("ocm-home/snapshots/source").exists());
}

#[test]
fn env_snapshot_create_json_reports_snapshot_metadata() {
    let root = TestDir::new("env-snapshot-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let output = stdout(&snapshot);
    assert!(output.contains("\"envName\": \"source\""));
    assert!(output.contains("\"archivePath\":"));
    assert!(output.contains("\"id\":"));
}

#[test]
fn env_snapshot_show_reports_snapshot_metadata() {
    let root = TestDir::new("env-snapshot-show");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "source", "--port", "19789", "--protect"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "source",
            "--label",
            "before-upgrade",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    let show = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "show", "source", &snapshot_id],
    );
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("snapshotId:"));
    assert!(output.contains("envName: source"));
    assert!(output.contains("label: before-upgrade"));
    assert!(output.contains("gatewayPort: 19789"));
    assert!(output.contains("protected: true"));
}

#[test]
fn env_snapshot_show_json_reports_the_snapshot_shape() {
    let root = TestDir::new("env-snapshot-show-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    let show = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "show", "source", &snapshot_id, "--json"],
    );
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("\"envName\": \"source\""));
    assert!(output.contains("\"archivePath\":"));
    assert!(output.contains("\"createdAt\":"));
}

#[test]
fn env_snapshot_list_reports_env_scoped_snapshots_in_newest_first_order() {
    let root = TestDir::new("env-snapshot-list");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let first = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "first"],
    );
    assert!(first.status.success(), "{}", stderr(&first));

    let second = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "second"],
    );
    assert!(second.status.success(), "{}", stderr(&second));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let output = stdout(&list);
    let second_index = output.find("label=second").unwrap();
    let first_index = output.find("label=first").unwrap();
    assert!(second_index < first_index);
}

#[test]
fn env_snapshot_list_json_supports_the_global_view() {
    let root = TestDir::new("env-snapshot-list-all");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    for name in ["alpha", "beta"] {
        let create = run_ocm(&cwd, &env, &["env", "create", name]);
        assert!(create.status.success(), "{}", stderr(&create));
        let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", name]);
        assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    }

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "--all", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let output = stdout(&list);
    assert!(output.contains("\"envName\": \"alpha\""));
    assert!(output.contains("\"envName\": \"beta\""));
}

#[test]
fn env_snapshot_restore_reverts_state_from_the_selected_snapshot() {
    let root = TestDir::new("env-snapshot-restore");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "source", "--port", "19789", "--protect"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt"),
        "before restore",
    );
    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "source",
            "--label",
            "before-upgrade",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_list = stdout(&list);
    let snapshot_id = snapshot_list
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt"),
        "after drift",
    );

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    let output = stdout(&restore);
    assert!(output.contains("Restored env source from snapshot"));
    assert!(output.contains("label: before-upgrade"));
    assert_eq!(
        fs::read_to_string(root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt"))
            .unwrap(),
        "before restore"
    );
}

#[cfg(unix)]
#[test]
fn env_snapshot_restores_the_complete_durable_root_with_metadata_and_sqlite() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    let root = TestDir::new("env-snapshot-complete-durable-root");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let env_root = root.child("ocm-home/envs/source");
    let dotenv = env_root.join(".openclaw/.env");
    let secret = env_root.join(".openclaw/secrets/telegram/default.token");
    let future_state = env_root.join("durable-future/state.txt");
    let future_non_sqlite_db = env_root.join("durable-future/cache.db");
    let future_link = env_root.join("durable-future/state-link");
    let database_path = env_root.join(".openclaw/state/durable.sqlite");
    write_text(&dotenv, "OPENCLAW_SENTINEL=before\n");
    write_text(&secret, "secret-before\n");
    write_text(&future_state, "future-before\n");
    write_text(&future_non_sqlite_db, "opaque future cache\n");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    symlink("state.txt", &future_link).unwrap();
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    let database = Connection::open(&database_path).unwrap();
    database
        .execute_batch(
            "CREATE TABLE durable_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable_state VALUES ('sentinel', 'before');",
        )
        .unwrap();
    drop(database);

    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap();

    write_text(&dotenv, "OPENCLAW_SENTINEL=after\n");
    write_text(&secret, "secret-after\n");
    fs::remove_file(&future_link).unwrap();
    fs::remove_file(&future_state).unwrap();
    let database = Connection::open(&database_path).unwrap();
    database
        .execute(
            "UPDATE durable_state SET value = 'after' WHERE key = 'sentinel'",
            [],
        )
        .unwrap();
    drop(database);

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));

    assert_eq!(
        fs::read_to_string(&dotenv).unwrap(),
        "OPENCLAW_SENTINEL=before\n"
    );
    assert_eq!(fs::read_to_string(&secret).unwrap(), "secret-before\n");
    assert_eq!(fs::metadata(&secret).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        fs::read_to_string(&future_state).unwrap(),
        "future-before\n"
    );
    assert_eq!(
        fs::read_to_string(&future_non_sqlite_db).unwrap(),
        "opaque future cache\n"
    );
    assert_eq!(fs::read_link(&future_link).unwrap(), Path::new("state.txt"));

    let restored = Connection::open_with_flags(
        &database_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let value: String = restored
        .query_row(
            "SELECT value FROM durable_state WHERE key = 'sentinel'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, "before");
    let integrity: String = restored
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn env_snapshot_restore_does_not_collide_with_or_delete_a_foreign_restore_path() {
    let root = TestDir::new("env-snapshot-foreign-restore-path");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let workspace = root.child("ocm-home/envs/source/.openclaw/workspace/collision-fixture");
    for index in 0..2_048 {
        write_text(
            &workspace.join(format!("entry-{index:04}.txt")),
            "snapshot payload\n",
        );
    }

    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap().to_string();

    let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    command
        .current_dir(&cwd)
        .args(["env", "snapshot", "restore", "source", snapshot_id.as_str()])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let foreign_root = root.child(format!(
        "ocm-home/envs/.source-ocm-restore-{}-1",
        child.id()
    ));
    let foreign_marker = foreign_root.join("owned-by-someone-else.txt");
    write_text(&foreign_marker, "do not delete\n");

    let restore = child.wait_with_output().unwrap();
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(&foreign_marker).unwrap(),
        "do not delete\n"
    );
}

#[test]
fn env_snapshot_restore_recovers_the_displaced_root_when_acceptance_fails() {
    let root = TestDir::new("env-snapshot-acceptance-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "source", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let notes = root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt");
    write_text(&notes, "snapshot-state\n");
    #[cfg(unix)]
    let rejected_read_only_dir =
        root.child("ocm-home/envs/source/.openclaw/future-durable/read-only");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        write_text(&rejected_read_only_dir.join("state.txt"), "snapshot-only\n");
        fs::set_permissions(&rejected_read_only_dir, fs::Permissions::from_mode(0o555)).unwrap();
    }
    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap();
    let artifact = Path::new(snapshot_json["archivePath"].as_str().unwrap());
    let metadata_path = artifact
        .parent()
        .unwrap()
        .join(format!("{snapshot_id}.json"));
    let mut metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["serviceEnabled"] = Value::Bool(true);
    metadata["serviceRunning"] = Value::Bool(true);
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&rejected_read_only_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&rejected_read_only_dir).unwrap();
    }
    write_text(&notes, "pre-restore-current-state\n");
    let launchctl_bin = env.get("OCM_INTERNAL_LAUNCHCTL_BIN").unwrap();
    write_executable_script(
        Path::new(launchctl_bin),
        "#!/bin/sh\ncase \"$1\" in\n  managername) exit 0 ;;\n  print) exit 1 ;;\n  bootstrap) echo 'forced acceptance failure' >&2; exit 1 ;;\n  *) exit 0 ;;\nesac\n",
    );

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(!restore.status.success(), "{}", stdout(&restore));
    assert!(
        stderr(&restore).contains("failed managed-service acceptance"),
        "{}",
        stderr(&restore)
    );
    assert_eq!(
        fs::read_to_string(&notes).unwrap(),
        "pre-restore-current-state\n"
    );
    let operation_namespaces = fs::read_dir(root.child("ocm-home/envs"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("ocm-restore"))
        .count();
    assert_eq!(operation_namespaces, 0);
}

#[test]
fn env_snapshot_restore_remains_compatible_with_legacy_tar_metadata() {
    let root = TestDir::new("env-snapshot-legacy-tar");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let notes = root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt");
    write_text(&notes, "legacy-snapshot\n");

    let snapshot_id = "1700000000-000000001";
    let snapshot_dir = root.child("ocm-home/snapshots/source");
    fs::create_dir_all(&snapshot_dir).unwrap();
    let archive_path = snapshot_dir.join(format!("{snapshot_id}.tar"));
    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            archive_path.to_str().unwrap(),
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let metadata = serde_json::json!({
        "kind": "ocm-env-snapshot",
        "id": snapshot_id,
        "envName": "source",
        "label": "legacy",
        "archivePath": archive_path,
        "sourceRoot": root.child("ocm-home/envs/source"),
        "gatewayPort": 18789,
        "serviceEnabled": false,
        "serviceRunning": false,
        "defaultRuntime": null,
        "defaultLauncher": null,
        "protected": false,
        "createdAt": "2023-11-14T22:13:20Z"
    });
    fs::write(
        snapshot_dir.join(format!("{snapshot_id}.json")),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();

    write_text(&notes, "current-state\n");
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(fs::read_to_string(&notes).unwrap(), "legacy-snapshot\n");
}

#[test]
fn env_snapshot_create_and_restore_quiesce_a_running_managed_gateway() {
    let root = TestDir::new("env-snapshot-cold-lifecycle");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let health = TestHttpServer::serve_bytes_times("/health", "text/plain", b"ok", 20);
    let health_port = url::Url::parse(&health.url()).unwrap().port().unwrap() as u32;

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "source",
            "--port",
            &health_port.to_string(),
            "--launcher",
            "stable",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let started = run_ocm(&cwd, &env, &["service", "start", "source"]);
    assert!(started.status.success(), "{}", stderr(&started));
    write_running_snapshot_service(&root, &cwd, &env, health_port);

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let running_runtime = fs::read(&runtime_path).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    let observer_done = Arc::new(AtomicBool::new(false));
    let observer_stop = Arc::clone(&observer_done);
    let observer = thread::spawn(move || {
        let mut last_running = true;
        while !observer_stop.load(Ordering::Relaxed) {
            let desired_running = fs::read(&registry_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|registry| registry["envs"].as_array().cloned())
                .and_then(|envs| envs.into_iter().find(|entry| entry["name"] == "source"))
                .and_then(|entry| entry["serviceRunning"].as_bool())
                .unwrap_or(last_running);
            if desired_running != last_running {
                if desired_running {
                    fs::write(&runtime_path, &running_runtime).unwrap();
                } else {
                    write_empty_snapshot_service(&runtime_path, &ocm_home);
                }
                last_running = desired_running;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });

    let notes = root.child("ocm-home/envs/source/.openclaw/workspace/notes.txt");
    write_text(&notes, "before snapshot\n");
    fs::write(root.child("launchctl.log"), "").unwrap();
    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "cold"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let create_lifecycle = fs::read_to_string(root.child("launchctl.log")).unwrap();
    let create_stop = create_lifecycle.find("bootout gui/").unwrap();
    let create_start = create_lifecycle.rfind("bootstrap gui/").unwrap();
    assert!(create_stop < create_start, "{create_lifecycle}");

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let list_json: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(list_json[0]["serviceRunning"], true);
    let snapshot_id = list_json[0]["id"].as_str().unwrap().to_string();

    write_text(&notes, "after snapshot\n");
    fs::write(root.child("launchctl.log"), "").unwrap();
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    let restore_lifecycle = fs::read_to_string(root.child("launchctl.log")).unwrap();
    let restore_stop = restore_lifecycle.find("bootout gui/").unwrap();
    let restore_start = restore_lifecycle.rfind("bootstrap gui/").unwrap();
    assert!(restore_stop < restore_start, "{restore_lifecycle}");
    assert_eq!(fs::read_to_string(&notes).unwrap(), "before snapshot\n");

    let shown = run_ocm(&cwd, &env, &["env", "show", "source", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown_json: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown_json["serviceRunning"], true);
    observer_done.store(true, Ordering::Relaxed);
    observer.join().unwrap();
}

#[test]
fn env_snapshot_create_waits_for_delayed_supervisor_stop_acknowledgement() {
    let root = TestDir::new("env-snapshot-delayed-stop-ack");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let health = TestHttpServer::serve_bytes_times("/health", "text/plain", b"ok", 20);
    let health_port = url::Url::parse(&health.url()).unwrap().port().unwrap() as u32;

    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "source",
            "--port",
            &health_port.to_string(),
            "--launcher",
            "stable",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let started = run_ocm(&cwd, &env, &["service", "start", "source"]);
    assert!(started.status.success(), "{}", stderr(&started));
    write_running_snapshot_service(&root, &cwd, &env, health_port);

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let running_runtime = fs::read(&runtime_path).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    let observer_runtime_path = runtime_path.clone();
    let observer_done = Arc::new(AtomicBool::new(false));
    let observer_done_thread = Arc::clone(&observer_done);
    let observer = thread::spawn(move || {
        let mut last_running = true;
        let mut delayed_stop = false;
        while !observer_done_thread.load(Ordering::Relaxed) {
            let desired_running = fs::read(&registry_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|registry| registry["envs"].as_array().cloned())
                .and_then(|envs| envs.into_iter().find(|entry| entry["name"] == "source"))
                .and_then(|entry| entry["serviceRunning"].as_bool())
                .unwrap_or(last_running);
            if desired_running != last_running {
                if desired_running {
                    fs::write(&observer_runtime_path, &running_runtime).unwrap();
                } else {
                    delayed_stop = true;
                    thread::sleep(Duration::from_millis(3_200));
                    write_empty_snapshot_service(&observer_runtime_path, &ocm_home);
                }
                last_running = desired_running;
            }
            thread::sleep(Duration::from_millis(5));
        }
        delayed_stop
    });

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "source",
            "--label",
            "delayed-stop",
        ],
    );

    for _ in 0..200 {
        let running = fs::read(&runtime_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|runtime| runtime["children"].as_array().cloned())
            .is_some_and(|children| !children.is_empty());
        if running {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    observer_done.store(true, Ordering::Relaxed);
    assert!(
        observer.join().unwrap(),
        "fixture never delayed the stop acknowledgement"
    );
    assert!(
        snapshot.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&snapshot),
        stderr(&snapshot)
    );

    let shown = run_ocm(&cwd, &env, &["env", "show", "source", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["serviceEnabled"], true);
    assert_eq!(shown["serviceRunning"], true);
}

#[test]
fn env_snapshot_create_quiesces_desired_service_before_child_is_observable() {
    let root = TestDir::new("env-snapshot-starting-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "source",
            "--port",
            "19845",
            "--launcher",
            "stable",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let started = run_ocm(&cwd, &env, &["service", "start", "source"]);
    assert!(started.status.success(), "{}", stderr(&started));

    write_running_snapshot_service(&root, &cwd, &env, 19845);
    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    let running_runtime = fs::read(&runtime_path).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    write_empty_snapshot_service(&runtime_path, &ocm_home);
    let registry_path = env_registry_path(&env, &cwd).unwrap();
    let observer_done = Arc::new(AtomicBool::new(false));
    let stop_count = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::new(AtomicUsize::new(0));
    let observer_done_thread = Arc::clone(&observer_done);
    let observer_stop_count = Arc::clone(&stop_count);
    let observer_start_count = Arc::clone(&start_count);
    let observer_runtime_path = runtime_path.clone();
    let observer = thread::spawn(move || {
        let mut last_running = true;
        while !observer_done_thread.load(Ordering::Relaxed) {
            let desired_running = fs::read(&registry_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|registry| registry["envs"].as_array().cloned())
                .and_then(|envs| envs.into_iter().find(|entry| entry["name"] == "source"))
                .and_then(|entry| entry["serviceRunning"].as_bool())
                .unwrap_or(last_running);
            if desired_running != last_running {
                if desired_running {
                    fs::write(&observer_runtime_path, &running_runtime).unwrap();
                    observer_start_count.fetch_add(1, Ordering::Relaxed);
                } else {
                    write_empty_snapshot_service(&observer_runtime_path, &ocm_home);
                    observer_stop_count.fetch_add(1, Ordering::Relaxed);
                }
                last_running = desired_running;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });

    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "starting"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while start_count.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    observer_done.store(true, Ordering::Relaxed);
    observer.join().unwrap();
    assert_eq!(stop_count.load(Ordering::Relaxed), 1);
    assert_eq!(start_count.load(Ordering::Relaxed), 1);

    let shown = run_ocm(&cwd, &env, &["env", "show", "source", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["serviceEnabled"], true);
    assert_eq!(shown["serviceRunning"], true);
}

#[test]
fn env_snapshot_create_preserves_disabled_policy_while_stale_child_quiesces() {
    let root = TestDir::new("env-snapshot-disabled-stale-child");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "source",
            "--port",
            "19846",
            "--launcher",
            "stable",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let started = run_ocm(&cwd, &env, &["service", "start", "source"]);
    assert!(started.status.success(), "{}", stderr(&started));
    write_running_snapshot_service(&root, &cwd, &env, 19846);

    let disabled = run_ocm(&cwd, &env, &["service", "uninstall", "source"]);
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    let before = run_ocm(&cwd, &env, &["env", "show", "source", "--json"]);
    assert!(before.status.success(), "{}", stderr(&before));
    let before: Value = serde_json::from_str(&stdout(&before)).unwrap();
    assert_eq!(before["serviceEnabled"], false);
    assert_eq!(before["serviceRunning"], false);

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    let ocm_home = env.get("OCM_HOME").unwrap().clone();
    let observer_runtime_path = runtime_path.clone();
    let observer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        write_empty_snapshot_service(&observer_runtime_path, &ocm_home);
    });
    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "disabled"],
    );
    observer.join().unwrap();
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let shown = run_ocm(&cwd, &env, &["env", "show", "source", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["serviceEnabled"], false);
    assert_eq!(shown["serviceRunning"], false);
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let list: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(list[0]["serviceEnabled"], false);
    assert_eq!(list[0]["serviceRunning"], false);
}

#[test]
fn env_snapshot_create_preserves_already_stopped_enabled_policy() {
    let root = TestDir::new("env-snapshot-already-stopped");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);
    let launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "source",
            "--port",
            "19847",
            "--launcher",
            "stable",
        ],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let installed = run_ocm(&cwd, &env, &["service", "install", "source"]);
    assert!(installed.status.success(), "{}", stderr(&installed));

    fs::write(root.child("launchctl.log"), "").unwrap();
    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "stopped"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let lifecycle = fs::read_to_string(root.child("launchctl.log")).unwrap();
    assert!(!lifecycle.contains("bootout "), "{lifecycle}");
    assert!(!lifecycle.contains("bootstrap "), "{lifecycle}");

    let shown = run_ocm(&cwd, &env, &["env", "show", "source", "--json"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: Value = serde_json::from_str(&stdout(&shown)).unwrap();
    assert_eq!(shown["serviceEnabled"], true);
    assert_eq!(shown["serviceRunning"], false);
}

#[test]
fn env_snapshot_restore_preserves_device_pairing_and_session_state() {
    let root = TestDir::new("env-snapshot-device-pairing-state");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    for (name, port) in [("foreign", "19780"), ("source", "19781")] {
        let create = run_ocm(&cwd, &env, &["env", "create", name, "--port", port]);
        assert!(create.status.success(), "{}", stderr(&create));
    }

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    let foreign_workspace = root.child("ocm-home/envs/foreign/.openclaw/workspace");
    write_text(
        &source_state.join("agents/main/sessions/main.jsonl"),
        &format!("{{\"cwd\":\"{}\"}}\n", foreign_workspace.display()),
    );
    let database_path = source_state.join("state/openclaw.sqlite");
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    let database = Connection::open(&database_path).unwrap();
    database
        .execute_batch(
            "CREATE TABLE device_pairing_paired (
               device_id TEXT PRIMARY KEY,
               token TEXT NOT NULL
             );
             INSERT INTO device_pairing_paired VALUES ('paired-browser', 'must-survive');",
        )
        .unwrap();
    drop(database);

    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap();

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));

    let restored = Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("snapshot restore must retain OpenClaw's shared state database");
    let token: String = restored
        .query_row(
            "SELECT token FROM device_pairing_paired WHERE device_id = 'paired-browser'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(token, "must-survive");
    assert!(
        source_state
            .join("agents/main/sessions/main.jsonl")
            .exists()
    );
}

#[test]
fn env_snapshot_restore_recovers_managed_plugin_payloads() {
    let root = TestDir::new("env-snapshot-plugin-payloads");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let state_root = root.child("ocm-home/envs/source/.openclaw");
    let payloads = [
        (
            "plugins/installs.json",
            "{\"legacy\":{\"source\":\"path\"}}\n",
        ),
        (
            "extensions/path-demo/openclaw.plugin.json",
            "{\"id\":\"path-demo\"}\n",
        ),
        (
            "npm/projects/npm-demo/package-lock.json",
            "{\"lockfileVersion\":3}\n",
        ),
        (
            "npm/projects/npm-demo/node_modules/npm-demo/index.js",
            "module.exports = 'npm-demo';\n",
        ),
        ("git/git-demo/repo/.git/HEAD", "ref: refs/heads/main\n"),
        (
            "git/git-demo/repo/openclaw.plugin.json",
            "{\"id\":\"git-demo\"}\n",
        ),
    ];
    for (path, contents) in payloads {
        write_text(&state_root.join(path), contents);
    }

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    for root_name in ["plugins", "extensions", "npm", "git"] {
        fs::remove_dir_all(state_root.join(root_name)).unwrap();
    }

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));

    for (path, contents) in payloads {
        assert_eq!(
            fs::read_to_string(state_root.join(path)).unwrap(),
            contents,
            "{path}"
        );
    }
}

#[test]
fn env_snapshot_restore_preserves_configured_agent_workspaces_and_includes() {
    let root = TestDir::new("env-snapshot-secondary-workspaces");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        concat!(
            "{\n",
            "  $include: './config/agents.json5',\n",
            "  env: { vars: { SECONDARY_WORKSPACE: 'team/ops' } }\n",
            "}\n"
        ),
    );
    write_text(
        &source_state.join("config/agents.json5"),
        concat!(
            "{ agents: { list: [\n",
            "  { id: 'main', default: true },\n",
            "  { id: 'clawforce' },\n",
            "  { id: 'custom', workspace: '${OPENCLAW_HOME}/.openclaw/${SECONDARY_WORKSPACE}' }\n",
            "] } }\n"
        ),
    );
    write_text(
        &source_state.join("workspace-clawforce/skills/social/SKILL.md"),
        "clawforce skill before upgrade\n",
    );
    write_text(
        &source_state.join("team/ops/IDENTITY.md"),
        "custom workspace before upgrade\n",
    );
    write_text(
        &source_state.join("workspace-attestations/manifest.json"),
        "legacy generated state\n",
    );
    write_text(
        &source_state.join("workspace-cache/cache.json"),
        "unconfigured prefix lookalike\n",
    );

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let list_output = stdout(&list);
    let snapshot_id = list_output
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    fs::remove_dir_all(source_state.join("workspace-clawforce")).unwrap();
    fs::remove_dir_all(source_state.join("team")).unwrap();
    fs::remove_dir_all(source_state.join("config")).unwrap();
    fs::remove_dir_all(source_state.join("workspace-attestations")).unwrap();
    fs::remove_dir_all(source_state.join("workspace-cache")).unwrap();

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(source_state.join("workspace-clawforce/skills/social/SKILL.md"))
            .unwrap(),
        "clawforce skill before upgrade\n"
    );
    assert_eq!(
        fs::read_to_string(source_state.join("team/ops/IDENTITY.md")).unwrap(),
        "custom workspace before upgrade\n"
    );
    assert!(source_state.join("config/agents.json5").exists());
    assert!(source_state.join("workspace-attestations").exists());
    assert!(source_state.join("workspace-cache").exists());
}

#[test]
fn env_snapshot_preserves_keyed_include_order_when_overriding_an_agent() {
    let root = TestDir::new("env-snapshot-keyed-include-order");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        concat!(
            "{\n",
            "  $include: './config/agents.json5',\n",
            "  agents: { entries: { primary: { name: 'override' } } }\n",
            "}\n"
        ),
    );
    write_text(
        &source_state.join("config/agents.json5"),
        "{ agents: { entries: { primary: {}, ops: {} } } }\n",
    );
    write_text(
        &source_state.join("workspace/default.txt"),
        "default workspace\n",
    );
    write_text(
        &source_state.join("workspace-ops/secondary.txt"),
        "secondary workspace\n",
    );

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let list_output = stdout(&list);
    let snapshot_id = list_output
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();

    fs::remove_dir_all(source_state.join("workspace-ops")).unwrap();
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(source_state.join("workspace-ops/secondary.txt")).unwrap(),
        "secondary workspace\n"
    );
}

#[test]
fn env_snapshot_preserves_unknown_agent_workspace_directories() {
    let root = TestDir::new("env-snapshot-blocked-keyed-agent");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        r#"{
          "$include": "./config/agents.json5",
          "env": {}
        }"#,
    );
    write_text(
        &source_state.join("config/agents.json5"),
        r#"{
          "agents": {
            "defaults": { "workspace": "${OPENCLAW_STATE_DIR}/team" },
            "entries": {
              "constructor": {
                "workspace": "${OPENCLAW_STATE_DIR}/ignored"
              },
              "ops": {}
            }
          }
        }"#,
    );
    write_text(
        &source_state.join("team/real.txt"),
        "actual OpenClaw default workspace\n",
    );
    write_text(
        &source_state.join("ignored/ignored.txt"),
        "ignored keyed agent workspace\n",
    );

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let list_output = stdout(&list);
    let snapshot_id = list_output
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();

    fs::remove_dir_all(source_state.join("team")).unwrap();
    fs::remove_dir_all(source_state.join("ignored")).unwrap();
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(source_state.join("team/real.txt")).unwrap(),
        "actual OpenClaw default workspace\n"
    );
    assert_eq!(
        fs::read_to_string(source_state.join("ignored/ignored.txt")).unwrap(),
        "ignored keyed agent workspace\n"
    );
}

#[test]
fn env_snapshot_preserves_all_in_root_workspace_state() {
    let root = TestDir::new("env-snapshot-config-env-precedence");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    let vars_workspace = source_state.join("team/from-vars");
    let active_workspace = source_state.join("team/from-top-level");
    write_text(
        &source_state.join("openclaw.json"),
        &format!(
            concat!(
                "{{\n",
                "  env: {{\n",
                "    vars: {{ WORKSPACE_ROOT: '{}' }},\n",
                "    WORKSPACE_ROOT: '{}'\n",
                "  }},\n",
                "  agents: {{ defaults: {{ workspace: '${{WORKSPACE_ROOT}}' }} }}\n",
                "}}\n"
            ),
            vars_workspace.display(),
            active_workspace.display()
        ),
    );
    write_text(&vars_workspace.join("notes.txt"), "inactive workspace\n");
    write_text(&active_workspace.join("notes.txt"), "active workspace\n");

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    fs::remove_dir_all(source_state.join("team")).unwrap();
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(active_workspace.join("notes.txt")).unwrap(),
        "active workspace\n"
    );
    assert_eq!(
        fs::read_to_string(vars_workspace.join("notes.txt")).unwrap(),
        "inactive workspace\n"
    );
}

#[test]
fn env_snapshot_uses_the_environment_runtime_identity_for_workspace_selection() {
    let root = TestDir::new("env-snapshot-runtime-identity");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    let workspace = source_state.join("team/source-19789");
    write_text(
        &source_state.join("openclaw.json"),
        concat!(
            "{\n",
            "  agents: { defaults: {\n",
            "    workspace: '${OCM_ACTIVE_ENV_ROOT}/.openclaw/team/${OCM_ACTIVE_ENV}-${OPENCLAW_GATEWAY_PORT}'\n",
            "  } }\n",
            "}\n"
        ),
    );
    write_text(&workspace.join("notes.txt"), "runtime workspace\n");
    env.insert("OCM_ACTIVE_ENV".to_string(), "stale".to_string());
    env.insert(
        "OCM_ACTIVE_ENV_ROOT".to_string(),
        root.child("stale-root").display().to_string(),
    );
    env.insert("OPENCLAW_GATEWAY_PORT".to_string(), "1".to_string());

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    fs::remove_dir_all(source_state.join("team")).unwrap();
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "runtime workspace\n"
    );
}

#[test]
fn env_snapshot_records_external_workspace_configuration_without_following_it() {
    let root = TestDir::new("env-snapshot-external-workspace");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let external = root.child("external-workspace");
    write_text(&external.join("notes.txt"), "external data\n");
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        &format!(
            r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
            external.display()
        ),
    );

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    write_text(&external.join("notes.txt"), "external data changed\n");
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let list_json: Value = serde_json::from_str(&stdout(&list)).unwrap();
    let snapshot_id = list_json[0]["id"].as_str().unwrap();
    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(external.join("notes.txt")).unwrap(),
        "external data changed\n"
    );
}

#[test]
fn env_snapshot_removes_partial_artifacts_when_sqlite_snapshot_fails() {
    let root = TestDir::new("env-snapshot-invalid-sqlite");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let database = root.child("ocm-home/envs/source/.openclaw/state/openclaw.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, b"SQLite format 3\0not a valid database").unwrap();

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert_eq!(snapshot.status.code(), Some(1));
    assert!(
        stderr(&snapshot).contains("SQLite"),
        "{}",
        stderr(&snapshot)
    );
    assert_eq!(
        fs::read_to_string(&database).unwrap(),
        "SQLite format 3\0not a valid database"
    );
    assert!(!root.child("ocm-home/snapshots/source").exists());
}

#[test]
fn env_snapshot_restore_json_reports_the_restored_binding_shape() {
    let root = TestDir::new("env-snapshot-restore-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--protect"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_list = stdout(&list);
    let snapshot_id = snapshot_list
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    let restore = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "restore",
            "source",
            &snapshot_id,
            "--json",
        ],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));
    let output = stdout(&restore);
    assert!(output.contains("\"envName\": \"source\""));
    assert!(output.contains("\"snapshotId\":"));
    assert!(output.contains("\"protected\": true"));
}

#[test]
fn env_snapshot_restore_rewrites_openclaw_config_for_the_current_root() {
    let root = TestDir::new("env-snapshot-restore-config");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_root = root.child("ocm-home/envs/source");
    fs::write(
        source_root.join(".openclaw/openclaw.json"),
        format!(
            "{{\n  \"agents\": {{\n    \"defaults\": {{\n      \"workspace\": \"{}\"\n    }}\n  }},\n  \"gateway\": {{\n    \"port\": 19789\n  }}\n}}\n",
            source_root.join(".openclaw/workspace").display()
        ),
    )
    .unwrap();

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    fs::write(
        source_root.join(".openclaw/openclaw.json"),
        "{\n  \"agents\": {\n    \"defaults\": {\n      \"workspace\": \"/tmp/foreign/.openclaw/workspace\"\n    }\n  },\n  \"gateway\": {\n    \"port\": 20000\n  }\n}\n",
    )
    .unwrap();

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));

    let raw = fs::read_to_string(source_root.join(".openclaw/openclaw.json")).unwrap();
    let config: Value = serde_json::from_str(&raw).unwrap();
    let actual_workspace = fs::canonicalize(Path::new(
        config["agents"]["defaults"]["workspace"].as_str().unwrap(),
    ))
    .unwrap();
    let expected_workspace = fs::canonicalize(source_root)
        .unwrap()
        .join(".openclaw/workspace");
    assert_eq!(actual_workspace, expected_workspace);
    assert_eq!(config["gateway"]["port"].as_u64(), Some(19789));
}

#[cfg(unix)]
#[test]
fn env_snapshot_restore_preserves_a_config_symlink_without_following_it() {
    let root = TestDir::new("env-snapshot-restore-config-symlink");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_root = root.child("ocm-home/envs/source");
    let source_config = source_root.join(".openclaw/openclaw.json");
    let external_config = root.child("external/openclaw.json");
    let external_raw = format!(
        "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}},\"gateway\":{{\"port\":19789}}}}\n",
        source_root.join(".openclaw/workspace").display()
    );
    write_text(&external_config, &external_raw);
    if source_config.exists() {
        fs::remove_file(&source_config).unwrap();
    }
    std::os::unix::fs::symlink(&external_config, &source_config).unwrap();

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    fs::remove_file(&source_config).unwrap();
    fs::write(&source_config, "{}\n").unwrap();

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );

    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(fs::read_to_string(&external_config).unwrap(), external_raw);
    assert!(
        fs::symlink_metadata(&source_config)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&source_config).unwrap(), external_config);
}

#[test]
fn env_snapshot_restore_preserves_captured_session_state() {
    let root = TestDir::new("env-snapshot-restore-runtime-repair");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let foreign = run_ocm(&cwd, &env, &["env", "create", "foreign"]);
    assert!(foreign.status.success(), "{}", stderr(&foreign));

    let source = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(source.status.success(), "{}", stderr(&source));

    let source_root = root.child("ocm-home/envs/source");
    fs::create_dir_all(source_root.join(".openclaw/agents/main/agent")).unwrap();
    fs::create_dir_all(source_root.join(".openclaw/agents/main/sessions")).unwrap();
    write_text(
        &source_root.join(".openclaw/agents/main/agent/auth-profiles.json"),
        "{\"ok\":true}",
    );
    write_text(
        &source_root.join(".openclaw/agents/main/sessions/main.jsonl"),
        &format!(
            "{{\"cwd\":\"{}\"}}\n",
            root.child("ocm-home/envs/foreign/.openclaw/workspace")
                .display()
        ),
    );

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "source",
            "--label",
            "before-repair",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    let restore = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "restore", "source", &snapshot_id],
    );
    assert!(restore.status.success(), "{}", stderr(&restore));

    assert!(
        source_root
            .join(".openclaw/agents/main/agent/auth-profiles.json")
            .exists()
    );
    assert!(
        source_root
            .join(".openclaw/agents/main/sessions/main.jsonl")
            .exists()
    );
}

#[test]
fn env_snapshot_remove_deletes_the_named_snapshot() {
    let root = TestDir::new("env-snapshot-remove");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "create",
            "source",
            "--label",
            "before-cleanup",
        ],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    let remove = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "remove", "source", &snapshot_id],
    );
    assert!(remove.status.success(), "{}", stderr(&remove));
    let output = stdout(&remove);
    assert!(output.contains("Removed snapshot"));
    assert!(output.contains("for env source"));
    assert!(output.contains("label: before-cleanup"));

    let list_after = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source"]);
    assert!(list_after.status.success(), "{}", stderr(&list_after));
    assert!(stdout(&list_after).contains("No snapshots."));
}

#[test]
fn env_snapshot_remove_json_reports_removed_snapshot_metadata() {
    let root = TestDir::new("env-snapshot-remove-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let snapshot_id = stdout(&list)
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();

    let remove = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "snapshot",
            "remove",
            "source",
            &snapshot_id,
            "--json",
        ],
    );
    assert!(remove.status.success(), "{}", stderr(&remove));
    let output = stdout(&remove);
    assert!(output.contains("\"envName\": \"source\""));
    assert!(output.contains("\"snapshotId\":"));
    assert!(output.contains("\"archivePath\":"));
}

#[test]
fn env_snapshot_remove_reports_cleanup_warnings_after_logical_removal() {
    let root = TestDir::new("env-snapshot-remove-warning");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let snapshot = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot_json: serde_json::Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    let snapshot_id = snapshot_json["id"].as_str().unwrap();

    let history_root = root.child("ocm-home/upgrade-history");
    fs::create_dir_all(&history_root).unwrap();
    fs::write(
        history_root.join("source"),
        "block linked recovery traversal",
    )
    .unwrap();

    let remove = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "remove", "source", snapshot_id],
    );
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(
        stdout(&remove).contains("warning: linked upgrade recovery cleanup failed"),
        "{}",
        stdout(&remove)
    );

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source"]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert!(stdout(&list).contains("No snapshots."));
}

#[test]
fn env_snapshot_prune_previews_candidates_without_removing_them() {
    let root = TestDir::new("env-snapshot-prune-preview");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let old = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "old"],
    );
    assert!(old.status.success(), "{}", stderr(&old));
    let new = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "new"],
    );
    assert!(new.status.success(), "{}", stderr(&new));

    let prune = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "prune", "source", "--keep", "1"],
    );
    assert!(prune.status.success(), "{}", stderr(&prune));
    let output = stdout(&prune);
    assert!(output.contains("Snapshot prune preview (source): 1 candidate(s)"));
    assert!(output.contains("label=old"));
    assert!(output.contains("Re-run with --yes to remove them."));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let listed = stdout(&list);
    assert!(listed.contains("\"label\": \"old\""));
    assert!(listed.contains("\"label\": \"new\""));
}

#[test]
fn env_snapshot_prune_yes_removes_selected_snapshots() {
    let root = TestDir::new("env-snapshot-prune-apply");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let old = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "old"],
    );
    assert!(old.status.success(), "{}", stderr(&old));
    let new = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "source", "--label", "new"],
    );
    assert!(new.status.success(), "{}", stderr(&new));

    let prune = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "prune", "source", "--keep", "1", "--yes"],
    );
    assert!(prune.status.success(), "{}", stderr(&prune));
    let output = stdout(&prune);
    assert!(output.contains("Pruned 1 snapshot(s)."));
    assert!(output.contains("label=old"));

    let list = run_ocm(&cwd, &env, &["env", "snapshot", "list", "source", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let listed = stdout(&list);
    assert!(!listed.contains("\"label\": \"old\""));
    assert!(listed.contains("\"label\": \"new\""));
}

#[test]
fn env_snapshot_prune_json_supports_the_global_view() {
    let root = TestDir::new("env-snapshot-prune-json-all");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    for name in ["alpha", "beta"] {
        let create = run_ocm(&cwd, &env, &["env", "create", name]);
        assert!(create.status.success(), "{}", stderr(&create));
        let old = run_ocm(
            &cwd,
            &env,
            &["env", "snapshot", "create", name, "--label", "old"],
        );
        assert!(old.status.success(), "{}", stderr(&old));
        let new = run_ocm(
            &cwd,
            &env,
            &["env", "snapshot", "create", name, "--label", "new"],
        );
        assert!(new.status.success(), "{}", stderr(&new));
    }

    let prune = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "prune", "--all", "--keep", "1", "--json"],
    );
    assert!(prune.status.success(), "{}", stderr(&prune));
    let output = stdout(&prune);
    assert!(output.contains("\"apply\": false"));
    assert!(output.contains("\"scope\": \"all\""));
    assert!(output.contains("\"count\": 2"));
    assert!(output.contains("\"envName\": \"alpha\""));
    assert!(output.contains("\"envName\": \"beta\""));
}
