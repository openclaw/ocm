mod support;

use std::{fs, path::Path};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout, write_text};

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

#[test]
fn env_snapshot_restore_preserves_device_pairing_state_while_clearing_foreign_runtime_refs() {
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
        !source_state
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
    assert!(!source_state.join("workspace-attestations").exists());
    assert!(!source_state.join("workspace-cache").exists());
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
fn env_snapshot_ignores_openclaw_blocked_keyed_agents() {
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
    assert!(!source_state.join("ignored").exists());
}

#[test]
fn env_snapshot_uses_openclaw_config_env_precedence_for_workspace_selection() {
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
    assert!(!vars_workspace.exists());
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
fn env_snapshot_rejects_external_workspaces_before_writing_an_archive() {
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
    assert_eq!(snapshot.status.code(), Some(1));
    assert!(
        stderr(&snapshot).contains("outside the environment root"),
        "{}",
        stderr(&snapshot)
    );
    assert!(!root.child("ocm-home/snapshots/source").exists());
    assert_eq!(
        fs::read_to_string(external.join("notes.txt")).unwrap(),
        "external data\n"
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
    write_text(&database, "not a sqlite database\n");

    let snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "create", "source"]);
    assert_eq!(snapshot.status.code(), Some(1));
    assert!(
        stderr(&snapshot).contains("SQLite"),
        "{}",
        stderr(&snapshot)
    );
    assert_eq!(
        fs::read_to_string(&database).unwrap(),
        "not a sqlite database\n"
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
fn env_snapshot_restore_materializes_a_config_symlink_even_without_textual_drift() {
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
            .is_file()
    );
    let restored: Value =
        serde_json::from_str(&fs::read_to_string(&source_config).unwrap()).unwrap();
    assert_eq!(
        restored["agents"]["defaults"]["workspace"].as_str(),
        Some(
            source_root
                .join(".openclaw/workspace")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(restored["gateway"]["port"].as_u64(), Some(19789));
}

#[test]
fn env_snapshot_restore_repairs_foreign_runtime_state_in_the_restored_snapshot() {
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
        !source_root
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
