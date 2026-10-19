#[path = "support/config_includes.rs"]
mod config_includes;
mod support;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};

fn install_fake_openclaw_on_path(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) {
    let bin_dir = root.child("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let openclaw = bin_dir.join("openclaw");
    fs::write(&openclaw, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&openclaw).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&openclaw, permissions).unwrap();
    }
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let path = if existing_path.is_empty() {
        bin_dir.display().to_string()
    } else {
        format!("{}:{existing_path}", bin_dir.display())
    };
    env.insert("PATH".to_string(), path);
}

#[test]
fn migrate_help_is_available() {
    let root = TestDir::new("migrate-help");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["help", "migrate"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("Migrate an existing OpenClaw home"));
    assert!(body.contains("ocm migrate <env> [<source-home>]"));
    assert!(body.contains("ocm migrate mira"));
    assert!(body.contains("migrated launcher"));
    assert!(body.contains("Use `adopt inspect` or `adopt plan`"));
}

#[test]
fn adopt_group_help_is_available() {
    let root = TestDir::new("adopt-help-group");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["help", "adopt"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("Adoption commands"));
    assert!(body.contains("ocm adopt inspect"));
    assert!(body.contains("ocm adopt plan --name mira"));
    assert!(body.contains("ocm adopt import --name mira"));
}

#[test]
fn adopt_inspect_defaults_to_the_plain_openclaw_home() {
    let root = TestDir::new("adopt-inspect-default");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["adopt", "inspect", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("\"sourceHome\":"));
    assert!(body.contains(".openclaw"));
    assert!(body.contains("\"exists\": false"));
}

#[test]
fn adopt_inspect_can_use_an_explicit_source_home() {
    let root = TestDir::new("adopt-inspect-explicit");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-openclaw");
    fs::create_dir_all(source_home.join("workspace")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(source_home.join("openclaw.json"), "{}\n").unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "inspect",
            source_home.to_string_lossy().as_ref(),
            "--raw",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("exists: true"));
    assert!(body.contains("configExists: true"));
    assert!(body.contains("workspaceExists: true"));
}

#[test]
fn migrate_legacy_inspect_accepts_leading_json_flag() {
    let root = TestDir::new("migrate-legacy-inspect-json");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-openclaw");
    fs::create_dir_all(source_home.join("workspace")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(source_home.join("openclaw.json"), "{}\n").unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "--json",
            "inspect",
            source_home.to_string_lossy().as_ref(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("\"exists\": true"));
    assert!(body.contains("\"configExists\": true"));
}

#[test]
fn help_adopt_inspect_is_available() {
    let root = TestDir::new("adopt-help-inspect");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["help", "adopt", "inspect"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("Inspect a migration source"));
    assert!(body.contains("ocm adopt inspect [<source-home>] [--raw] [--json]"));
}

#[test]
fn help_adopt_import_is_available() {
    let root = TestDir::new("adopt-help-import");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["help", "adopt", "import"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("Import a plain OpenClaw home"));
    assert!(body.contains(
        "preserve config, auth, sessions, and logs, and clear only live runtime residue like locks, pid files, and sockets."
    ));
    assert!(body.contains(
        "ocm adopt import --name <env> [<source-home>] [--root <path>] [--sandbox-origin <url>] [--raw] [--json]"
    ));
    assert!(body.contains("--sandbox-origin <url>"));
    assert!(body.contains("migrated launcher"));
}

#[test]
fn adopt_plan_reports_the_target_env_and_root() {
    let root = TestDir::new("adopt-plan");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["adopt", "plan", "--name", "mira", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("\"envName\": \"mira\""));
    assert!(body.contains("\"envExists\": false"));
    assert!(body.contains("\"targetRoot\":"));
}

#[test]
fn adopt_plan_fails_when_the_environment_registry_is_corrupt() {
    let root = TestDir::new("adopt-plan-corrupt-registry");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(root.child("ocm-home")).unwrap();
    fs::write(root.child("ocm-home/envs.json"), "{not-json\n").unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["adopt", "plan", "--name", "mira", "--json"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(!stdout(&output).contains("\"envExists\": false"));
    assert!(!stderr(&output).is_empty());
}

#[test]
fn adopt_plan_requires_name_when_only_a_positional_token_is_given() {
    let root = TestDir::new("adopt-plan-requires-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["adopt", "plan", "mira"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--name is required"));
}

#[test]
fn adopt_plan_accepts_an_explicit_target_root() {
    let root = TestDir::new("adopt-plan-root");
    let cwd = root.child("workspace");
    let target_root = root.child("custom-root");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "plan",
            "--name",
            "mira",
            "--root",
            target_root.to_string_lossy().as_ref(),
            "--raw",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("env: mira"));
    assert!(body.contains(&format!("targetRoot: {}", target_root.to_string_lossy())));
}

#[test]
fn migrate_legacy_plan_accepts_leading_raw_flag() {
    let root = TestDir::new("migrate-legacy-plan-raw");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["migrate", "--raw", "plan", "--name", "mira"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("env: mira"));
    assert!(body.contains("envExists: false"));
}

#[test]
fn help_adopt_plan_is_available() {
    let root = TestDir::new("adopt-help-plan");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["help", "adopt", "plan"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("Plan a migration target"));
    assert!(
        body.contains(
            "ocm adopt plan --name <env> [<source-home>] [--root <path>] [--raw] [--json]"
        )
    );
}

#[test]
fn adopt_import_requires_name_when_only_a_positional_token_is_given() {
    let root = TestDir::new("adopt-import-requires-name");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &["adopt", "import", source_home.to_string_lossy().as_ref()],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--name is required"));
}

#[test]
fn migrate_allows_name_flag_with_bare_relative_source_home() {
    let root = TestDir::new("migrate-name-relative-source");
    let cwd = root.child("workspace");
    let source_home = cwd.join("legacy-home");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &["migrate", "--name", "mira", "legacy-home", "--json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("\"name\": \"mira\""));
}

#[test]
fn migrate_plan_alias_requires_name() {
    let root = TestDir::new("migrate-plan-alias-requires-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["migrate", "plan", "mira"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--name is required"));
}

#[test]
fn migrate_import_alias_requires_name() {
    let root = TestDir::new("migrate-import-alias-requires-name");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "import",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--name is required"));
}

#[test]
fn migrate_rejects_alias_after_name_flag_with_clear_error() {
    let root = TestDir::new("migrate-mixed-alias-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &["migrate", "--name", "mira", "import", "./legacy-home"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("mixed migrate syntax"));
}

fn seed_plain_openclaw_home(source_home: &std::path::Path) {
    fs::create_dir_all(source_home.join("workspace")).unwrap();
    fs::create_dir_all(source_home.join("team/ops")).unwrap();
    fs::create_dir_all(source_home.join("logs")).unwrap();
    fs::create_dir_all(source_home.join("run")).unwrap();
    fs::create_dir_all(source_home.join("agents/main/agent")).unwrap();
    fs::create_dir_all(source_home.join("agents/main/sessions")).unwrap();
    fs::write(
        source_home.join("openclaw.json"),
        format!(
            "{}\n",
            serde_json::json!({
                "agents": {
                    "defaults": {"workspace": source_home.join("workspace")},
                    "list": [
                        {"id": "main", "default": true},
                        {"id": "ops", "workspace": source_home.join("team").join("ops")}
                    ]
                },
                "externalProject": source_home.parent().unwrap().join("projects").join("unrelated")
            })
        ),
    )
    .unwrap();
    fs::write(source_home.join("workspace/notes.txt"), "hello\n").unwrap();
    fs::write(
        source_home.join("team/ops/custom.txt"),
        "secondary workspace\n",
    )
    .unwrap();
    fs::write(
        source_home.join("logs/app.log"),
        format!("cwd={}\n", source_home.join("workspace").display()),
    )
    .unwrap();
    fs::write(
        source_home.join("agents/main/agent/auth-profiles.json"),
        "{}\n",
    )
    .unwrap();
    fs::write(
        source_home.join("agents/main/sessions/main.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "cwd": source_home.join("workspace"),
                "log": source_home.join("logs").join("app.log")
            })
        ),
    )
    .unwrap();
    fs::write(
        source_home.join("openclaw.json.bak"),
        format!("backup={}\n", source_home.display()),
    )
    .unwrap();
    fs::write(source_home.join("gateway.pid"), "4242\n").unwrap();
    fs::write(source_home.join("run/live.sock"), "sock\n").unwrap();
}

fn seed_installed_plugin_index(source_home: &std::path::Path, records: serde_json::Value) {
    let database_path = source_home.join("state/openclaw.sqlite");
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    let database = Connection::open(database_path).unwrap();
    database
        .execute_batch(
            "CREATE TABLE installed_plugin_index (\
                index_key TEXT NOT NULL PRIMARY KEY, \
                install_records_json TEXT NOT NULL\
            );",
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO installed_plugin_index (index_key, install_records_json) VALUES (?1, ?2)",
            params!["installed-plugin-index", records.to_string()],
        )
        .unwrap();
}

fn read_imported_plugin_install_records(root: &TestDir, env_name: &str) -> serde_json::Value {
    let database = Connection::open(root.child(format!(
        "ocm-home/envs/{env_name}/.openclaw/state/openclaw.sqlite"
    )))
    .unwrap();
    let records: String = database
        .query_row(
            "SELECT install_records_json FROM installed_plugin_index WHERE index_key = ?1",
            ["installed-plugin-index"],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&records).unwrap()
}

fn assert_imported_plain_openclaw_home(
    imported_state: &std::path::Path,
    source_home: &std::path::Path,
) {
    assert!(imported_state.join("workspace/notes.txt").exists());
    assert_eq!(
        fs::read_to_string(imported_state.join("team/ops/custom.txt")).unwrap(),
        "secondary workspace\n"
    );
    assert!(
        imported_state
            .join("agents/main/agent/auth-profiles.json")
            .exists()
    );
    assert!(imported_state.join("logs/app.log").exists());
    assert!(
        imported_state
            .join("agents/main/sessions/main.jsonl")
            .exists()
    );
    assert!(imported_state.join("openclaw.json.bak").exists());
    assert!(!imported_state.join("gateway.pid").exists());
    assert!(!imported_state.join("run/live.sock").exists());

    let config_text = fs::read_to_string(imported_state.join("openclaw.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_text).unwrap();
    assert_eq!(
        config["agents"]["defaults"]["workspace"],
        serde_json::json!(imported_state.join("workspace"))
    );
    assert_eq!(
        config["externalProject"],
        serde_json::json!(
            source_home
                .parent()
                .unwrap()
                .join("projects")
                .join("unrelated")
        )
    );
    let source_json = serde_json::to_string(source_home).unwrap();
    let source_json_path = &source_json[1..source_json.len() - 1];
    assert!(!config_text.contains(source_json_path));

    let session_text =
        fs::read_to_string(imported_state.join("agents/main/sessions/main.jsonl")).unwrap();
    let log_text = fs::read_to_string(imported_state.join("logs/app.log")).unwrap();
    let backup_text = fs::read_to_string(imported_state.join("openclaw.json.bak")).unwrap();
    let session: serde_json::Value = serde_json::from_str(&session_text).unwrap();
    assert_eq!(
        session["cwd"],
        serde_json::json!(imported_state.join("workspace"))
    );
    assert_eq!(
        session["log"],
        serde_json::json!(imported_state.join("logs").join("app.log")),
        "source={}; target={}; session={session_text}",
        source_home.display(),
        imported_state.display()
    );
    assert!(!session_text.contains(source_json_path));
    assert!(log_text.contains(&imported_state.join("workspace").display().to_string()));
    assert!(!log_text.contains(&source_home.display().to_string()));
    assert!(backup_text.contains(&imported_state.display().to_string()));
    assert!(!backup_text.contains(&source_home.display().to_string()));
}

#[test]
fn migrate_rejects_a_target_nested_inside_the_source_before_creating_an_env() {
    let root = TestDir::new("migrate-overlap-target-in-source");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let target_root = source_home.join("managed");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("migration source and target must not overlap"));
    assert!(!root.child("ocm-home/envs.json").exists());
    assert!(!target_root.exists());
}

#[test]
fn migrate_rejects_a_target_that_contains_the_source_before_mutation() {
    let root = TestDir::new("migrate-overlap-source-in-target");
    let cwd = root.child("workspace");
    let target_root = root.child("legacy-home");
    let source_home = target_root.join(".openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("migration source and target must not overlap"));
    assert!(!root.child("ocm-home/envs.json").exists());
    assert!(source_home.join("workspace/notes.txt").exists());
}

#[test]
fn migrate_normalizes_parent_components_before_checking_target_overlap() {
    let root = TestDir::new("migrate-overlap-parent-components");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let target_root = root.child("missing/../legacy-home");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("migration source and target must not overlap"));
    assert!(source_home.join("workspace/notes.txt").exists());
    assert!(!root.child("ocm-home/envs.json").exists());
}

#[test]
fn migrate_rejects_parent_traversal_through_a_file() {
    let root = TestDir::new("migrate-parent-through-file");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let blocker = root.child("blocker");
    let target_root = blocker.join("../managed");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    fs::write(&blocker, "not a directory\n").unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("parent traversal crosses a non-directory"));
    assert!(source_home.join("workspace/notes.txt").exists());
    assert!(!root.child("managed").exists());
    assert!(!root.child("ocm-home/envs.json").exists());
}

#[cfg(unix)]
#[test]
fn migrate_rejects_a_target_with_state_symlinked_to_the_source() {
    let root = TestDir::new("migrate-overlap-symlinked-state");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let target_root = root.child("managed");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&target_root).unwrap();
    seed_plain_openclaw_home(&source_home);
    std::os::unix::fs::symlink(&source_home, target_root.join(".openclaw")).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("migration source and target must not overlap"));
    assert!(source_home.join("workspace/notes.txt").exists());
    assert!(!root.child("ocm-home/envs.json").exists());
}

#[cfg(unix)]
#[test]
fn migrate_resolves_parent_components_after_following_symlinks() {
    let root = TestDir::new("migrate-overlap-symlink-parent");
    let cwd = root.child("workspace");
    let real_root = root.child("real");
    let source_home = real_root.join(".openclaw");
    let linked_child = real_root.join("child");
    let target_root = root.child("linked-child/../.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&linked_child).unwrap();
    seed_plain_openclaw_home(&source_home);
    std::os::unix::fs::symlink(&linked_child, root.child("linked-child")).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("migration source and target must not overlap"));
    assert!(source_home.join("workspace/notes.txt").exists());
    assert!(!root.child("ocm-home/envs.json").exists());
}

#[cfg(unix)]
#[test]
fn migrate_rejects_parent_traversal_through_a_symlink_to_a_file() {
    let root = TestDir::new("migrate-parent-through-file-symlink");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let blocker = root.child("blocker");
    let linked_blocker = root.child("linked-blocker");
    let target_root = linked_blocker.join("../managed");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    fs::write(&blocker, "not a directory\n").unwrap();
    std::os::unix::fs::symlink(&blocker, &linked_blocker).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("parent traversal crosses a non-directory"));
    assert!(source_home.join("workspace/notes.txt").exists());
    assert!(!root.child("managed").exists());
    assert!(!root.child("ocm-home/envs.json").exists());
}

#[cfg(unix)]
#[test]
fn migrate_does_not_bind_a_non_executable_openclaw_file_from_path() {
    let root = TestDir::new("migrate-non-executable-openclaw");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let bin_dir = root.child("bin");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();
    seed_plain_openclaw_home(&source_home);
    let openclaw = bin_dir.join("openclaw");
    fs::write(&openclaw, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&openclaw).unwrap().permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&openclaw, permissions).unwrap();

    let mut env = ocm_env(&root);
    env.insert("PATH".to_string(), bin_dir.display().to_string());
    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("\"defaultLauncher\": null"));
    assert!(!root.child("ocm-home/launchers/mira.migrated.json").exists());
}

#[test]
fn migrate_preserves_history_and_logs_from_plain_openclaw_home() {
    let root = TestDir::new("migrate-direct");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let body = stdout(&output);
    assert!(body.contains("\"name\": \"mira\""));
    assert!(body.contains("\"sourceName\": \"plain-openclaw\""));
    assert!(body.contains("\"defaultLauncher\": \"mira.migrated\""));

    let imported_state = root.child("ocm-home/envs/mira/.openclaw");
    assert_imported_plain_openclaw_home(&imported_state, &source_home);
}

#[test]
fn migrate_resets_or_replaces_a_copied_public_sandbox_origin() {
    let root = TestDir::new("migrate-sandbox-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let source_reset = root.child("legacy-reset/.openclaw");
    let source_explicit = root.child("legacy-explicit/.openclaw");
    for source_home in [&source_reset, &source_explicit] {
        fs::create_dir_all(source_home.join("workspace")).unwrap();
        fs::write(
            source_home.join("openclaw.json"),
            concat!(
                "{\n",
                "  \"gateway\": { \"port\": 18789 },\n",
                "  \"mcp\": {\n",
                "    \"apps\": {\n",
                "      \"enabled\": true,\n",
                "      \"sandboxPort\": 18790,\n",
                "      \"sandboxOrigin\": \"https://source.example.test\"\n",
                "    }\n",
                "  }\n",
                "}\n"
            ),
        )
        .unwrap();
    }

    let reset = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "reset",
            source_reset.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert!(reset.status.success(), "{}", stderr(&reset));
    assert!(
        stderr(&reset).contains("removed copied MCP app sandbox origin from env reset"),
        "{}",
        stderr(&reset)
    );
    assert!(!stderr(&reset).contains("source.example.test"));
    let reset_config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/reset/.openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    assert!(reset_config["mcp"]["apps"]["sandboxOrigin"].is_null());

    let explicit = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "explicit",
            source_explicit.to_string_lossy().as_ref(),
            "--sandbox-origin",
            "https://target.example.test",
            "--json",
        ],
    );
    assert!(explicit.status.success(), "{}", stderr(&explicit));
    assert!(!stderr(&explicit).contains("removed copied MCP app sandbox origin"));
    let explicit_config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/explicit/.openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        explicit_config["mcp"]["apps"]["sandboxOrigin"].as_str(),
        Some("https://target.example.test")
    );
}

#[test]
fn migrate_rejects_include_owned_sandbox_configuration_before_creating_the_target() {
    let root = TestDir::new("migrate-include-owned-sandbox-origin");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&source_home).unwrap();
    fs::write(
        source_home.join("openclaw.json"),
        "{\n  \"mcp\": { \"apps\": { \"$include\": \"./apps.json5\" } }\n}\n",
    )
    .unwrap();
    fs::write(
        source_home.join("apps.json5"),
        "{ sandboxOrigin: \"https://source.example.test\" }\n",
    )
    .unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "target",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(
            "cannot safely reset mcp.apps.sandboxOrigin because OpenClaw config uses $include at mcp.apps"
        ),
        "{}",
        stderr(&output)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
}

#[test]
fn migrate_validates_the_target_origin_before_reading_the_source_home() {
    let root = TestDir::new("migrate-invalid-origin-preflight");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let missing = root.child("missing/.openclaw");

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "target",
            missing.to_string_lossy().as_ref(),
            "--sandbox-origin",
            "https://target.example.test/apps",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains(
        "--sandbox-origin must be an HTTP(S) origin without a path, query, or credentials"
    ));
    assert!(!stderr(&output).contains("plain OpenClaw home does not exist"));
}

#[test]
fn adopt_import_copies_an_external_workspace_into_the_disposable_environment() {
    let root = TestDir::new("migrate-external-workspace");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let external = root.child("external-workspace");
    let agent_external = root.child("agent-workspace");
    fs::create_dir_all(&agent_external).unwrap();
    fs::write(agent_external.join("notes.txt"), "agent data\n").unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&source_home).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("notes.txt"), "external data\n").unwrap();
    let source_config = serde_json::json!({"agents": {
        "defaults": {"workspace": external},
        "list": [{"id": "ops", "workspace": agent_external}]
    }})
    .to_string();
    fs::write(source_home.join("openclaw.json"), &source_config).unwrap();
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let imported_workspace = root.child("ocm-home/envs/mira/.openclaw/workspaces/default");
    assert_eq!(
        fs::read_to_string(imported_workspace.join("notes.txt")).unwrap(),
        "external data\n"
    );
    let imported_config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/mira/.openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        imported_config["agents"]["defaults"]["workspace"].as_str(),
        Some(imported_workspace.to_string_lossy().as_ref())
    );
    assert_eq!(
        fs::read_to_string(external.join("notes.txt")).unwrap(),
        "external data\n"
    );
    assert_eq!(
        fs::read_to_string(source_home.join("openclaw.json")).unwrap(),
        source_config
    );
    let imported_agent = root.child("ocm-home/envs/mira/.openclaw/workspaces/agents/ops");
    assert_eq!(
        fs::read_to_string(imported_agent.join("notes.txt")).unwrap(),
        "agent data\n"
    );
    assert_eq!(
        imported_config["agents"]["list"][0]["workspace"].as_str(),
        Some(imported_agent.to_str().unwrap())
    );
    fs::write(imported_workspace.join("notes.txt"), "target changed\n").unwrap();
    fs::write(imported_agent.join("notes.txt"), "target agent changed\n").unwrap();
    assert_eq!(
        fs::read_to_string(external.join("notes.txt")).unwrap(),
        "external data\n"
    );
    assert_eq!(
        fs::read_to_string(agent_external.join("notes.txt")).unwrap(),
        "agent data\n"
    );
}

#[test]
fn migration_preserves_shared_workspace_locations() {
    for adopt in [false, true] {
        for layout in [
            "identical",
            "nested",
            "outer-agent",
            "agents-only",
            "internal",
            "home-parent",
        ] {
            let root = TestDir::new("migrate-shared-workspaces");
            let source = root.child("input/plain");
            let outer = match layout {
                "internal" => source.join("workspace"),
                "home-parent" => root.child("input"),
                _ => root.child("input/outer"),
            };
            let relative = if layout == "home-parent" {
                "plain/workspace"
            } else {
                "sub"
            };
            let inner = outer.join(relative);
            let unrelated = root.child("input/unrelated");
            for path in [&source, &inner, &unrelated] {
                fs::create_dir_all(path).unwrap();
            }
            fs::write(inner.join("note.txt"), "shared source").unwrap();
            fs::write(unrelated.join("note.txt"), "separate source").unwrap();
            // This sibling is never admitted by the external-workspace plan.
            fs::write(root.child("input/outside.txt"), "not a workspace").unwrap();
            let (default, twin) = match layout {
                "identical" => (outer.clone(), outer.clone()),
                "outer-agent" => (inner.clone(), outer.clone()),
                _ => (outer.clone(), inner.clone()),
            };
            let agents = if layout == "agents-only" {
                serde_json::json!({"entries": {
                    "zulu": {"workspace": outer},
                    "alpha": {"workspace": outer},
                    "child": {"workspace": inner},
                    "separate": {"workspace": unrelated}
                }})
            } else if layout == "home-parent" {
                // No defaults: main's implicit workspace is inside the plain home.
                serde_json::json!({"list": [
                    {"id": "main", "default": true},
                    {"id": "outer", "workspace": outer},
                    {"id": "separate", "workspace": unrelated}
                ]})
            } else {
                serde_json::json!({"defaults": {"workspace": default}, "list": [
                    {"id": "twin", "workspace": twin},
                    {"id": "separate", "workspace": unrelated}
                ]})
            };
            let mut source_value = serde_json::json!({"agents": agents});
            if layout == "internal" {
                source_value["env"] = serde_json::json!({"WORKSPACE_ROOT": outer});
                source_value["agents"]["defaults"]["workspace"] =
                    serde_json::json!("${WORKSPACE_ROOT}");
            }
            let source_config = source_value.to_string();
            fs::write(source.join("openclaw.json"), &source_config).unwrap();
            let env = ocm_env(&root);
            let args = if adopt {
                vec![
                    "adopt",
                    "import",
                    "--name",
                    "mira",
                    source.to_str().unwrap(),
                ]
            } else {
                vec!["migrate", "mira", source.to_str().unwrap()]
            };
            let output = run_ocm(root.path(), &env, &args);
            assert!(output.status.success(), "{layout}: {}", stderr(&output));
            let state = root.child("ocm-home/envs/mira/.openclaw");
            let config: serde_json::Value =
                serde_json::from_slice(&fs::read(state.join("openclaw.json")).unwrap()).unwrap();
            let agents = &config["agents"];
            let (a, b, separate) = if layout == "agents-only" {
                assert_eq!(
                    agents["entries"]["zulu"]["workspace"],
                    agents["entries"]["alpha"]["workspace"]
                );
                assert_eq!(
                    agents["entries"]["alpha"]["workspace"],
                    state.join("workspaces/agents/alpha").to_str().unwrap()
                );
                (
                    &agents["entries"]["alpha"]["workspace"],
                    &agents["entries"]["child"]["workspace"],
                    &agents["entries"]["separate"]["workspace"],
                )
            } else if layout == "home-parent" {
                (
                    &agents["list"][1]["workspace"],
                    &agents["list"][0]["workspace"],
                    &agents["list"][2]["workspace"],
                )
            } else if layout == "outer-agent" {
                (
                    &agents["list"][0]["workspace"],
                    &agents["defaults"]["workspace"],
                    &agents["list"][1]["workspace"],
                )
            } else {
                (
                    &agents["defaults"]["workspace"],
                    &agents["list"][0]["workspace"],
                    &agents["list"][1]["workspace"],
                )
            };
            let a = if layout == "internal" {
                assert_eq!(a, "${WORKSPACE_ROOT}");
                &config["env"]["WORKSPACE_ROOT"]
            } else {
                a
            };
            let a = Path::new(a.as_str().unwrap());
            let b = Path::new(b.as_str().unwrap());
            let separate = Path::new(separate.as_str().unwrap());
            let expected = if layout == "identical" {
                a.to_path_buf()
            } else {
                a.join(relative)
            };
            assert_eq!(b, expected, "{layout}");
            fs::write(b.join("shared-write.txt"), "visible through both").unwrap();
            assert_eq!(
                fs::read_to_string(expected.join("shared-write.txt")).unwrap(),
                "visible through both"
            );
            assert!(!separate.join("shared-write.txt").exists());
            assert_eq!(
                fs::read_to_string(separate.join("note.txt")).unwrap(),
                "separate source"
            );
            assert!(!inner.join("shared-write.txt").exists());
            assert!(!outer.join("shared-write.txt").exists());
            assert_eq!(
                fs::read_to_string(inner.join("note.txt")).unwrap(),
                "shared source"
            );
            assert_eq!(
                fs::read_to_string(unrelated.join("note.txt")).unwrap(),
                "separate source"
            );
            assert_eq!(
                fs::read_to_string(source.join("openclaw.json")).unwrap(),
                source_config
            );
            if !matches!(layout, "home-parent" | "internal") {
                assert!(!a.join("outside.txt").exists());
                assert!(!state.join("workspaces/default/outside.txt").exists());
            }
            if matches!(layout, "identical" | "nested") {
                assert!(!state.join("workspaces/agents/twin").exists());
                assert_eq!(a, state.join("workspaces/default"));
            }
            if layout == "outer-agent" {
                assert!(!state.join("workspaces/default").exists());
            }
            // Runtime state remains at the existing location even for an outer workspace.
            assert!(state.join("openclaw.json").is_file());
        }
    }
}

#[cfg(unix)]
#[test]
fn migration_preserves_sharing_through_configured_workspace_aliases() {
    for adopt in [false, true] {
        let root = TestDir::new("migrate-shared-alias");
        let source = root.child("plain");
        let outer = root.child("outer");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(outer.join("child")).unwrap();
        fs::write(outer.join("child/note.txt"), "source bytes").unwrap();
        let alias = source.join("workspace");
        std::os::unix::fs::symlink(&outer, &alias).unwrap();
        let config = serde_json::json!({"agents": {
            "defaults": {"workspace": alias},
            "list": [{"id": "twin", "workspace": outer}, {"id": "child", "workspace": alias.join("child")}]
        }}).to_string();
        fs::write(source.join("openclaw.json"), &config).unwrap();
        let env = ocm_env(&root);
        let args = if adopt {
            vec![
                "adopt",
                "import",
                "--name",
                "mira",
                source.to_str().unwrap(),
            ]
        } else {
            vec!["migrate", "mira", source.to_str().unwrap()]
        };
        let output = run_ocm(root.path(), &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
        let state = root.child("ocm-home/envs/mira/.openclaw");
        let imported: serde_json::Value =
            serde_json::from_slice(&fs::read(state.join("openclaw.json")).unwrap()).unwrap();
        let workspace = Path::new(
            imported["agents"]["defaults"]["workspace"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(
            imported["agents"]["list"][0]["workspace"],
            workspace.to_str().unwrap()
        );
        assert_eq!(
            imported["agents"]["list"][1]["workspace"],
            workspace.join("child").to_str().unwrap()
        );
        fs::write(workspace.join("child/note.txt"), "target write").unwrap();
        assert_eq!(
            fs::read_to_string(outer.join("child/note.txt")).unwrap(),
            "source bytes"
        );
        assert_eq!(fs::read_link(&alias).unwrap(), outer);
        assert_eq!(
            fs::read_to_string(source.join("openclaw.json")).unwrap(),
            config
        );
        assert!(!state.join("workspaces/agents").exists());
    }
}

#[cfg(any(unix, windows))]
fn migration_source_inventory(
    root: &Path,
) -> std::collections::BTreeMap<std::path::PathBuf, (u32, Vec<u8>)> {
    fn visit(
        root: &Path,
        path: &Path,
        result: &mut std::collections::BTreeMap<std::path::PathBuf, (u32, Vec<u8>)>,
    ) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let value = if metadata.file_type().is_symlink() {
            format!("link:{}", fs::read_link(path).unwrap().display()).into_bytes()
        } else if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), result);
            }
            b"directory".to_vec()
        } else {
            fs::read(path).unwrap()
        };
        #[cfg(unix)]
        let mode = metadata.permissions().mode();
        #[cfg(windows)]
        let mode = u32::from(metadata.permissions().readonly());
        result.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            (mode, value),
        );
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[cfg(any(unix, windows))]
#[test]
fn migration_preserves_internal_redirect_refusal_under_root_spellings() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;

    #[derive(Clone, Copy, Debug)]
    enum RootSpelling {
        Canonical,
        Alias,
        #[cfg(unix)]
        ParentTraversal,
        #[cfg(windows)]
        Ordinary,
    }
    use RootSpelling::*;
    let mut spellings = vec![(Canonical, Canonical), (Alias, Alias), (Alias, Canonical)];
    #[cfg(unix)]
    spellings.push((ParentTraversal, Canonical));
    #[cfg(windows)]
    spellings.extend([
        (Ordinary, Ordinary),
        (Ordinary, Canonical),
        (Canonical, Ordinary),
    ]);
    for (source_spelling, config_spelling) in spellings.drain(..) {
        for enclosing in [false, true] {
            for redirect in [false, true] {
                for adopt in [false, true] {
                    let root = TestDir::new("migrate-root-spellings");
                    let outer = root.child("outer");
                    let home = outer.join(".openclaw");
                    fs::create_dir_all(home.join("owned")).unwrap();
                    fs::write(home.join("owned/notes.txt"), "source payload").unwrap();
                    symlink_dir("owned", home.join("redirect")).unwrap();
                    let canonical = fs::canonicalize(&home).unwrap();
                    let alias = root.child("home-alias");
                    symlink_dir(&canonical, &alias).unwrap();
                    let alias_target = fs::read_link(&alias).unwrap();
                    let spelling = |form| match form {
                        Canonical => canonical.clone(),
                        Alias => alias.clone(),
                        #[cfg(unix)]
                        ParentTraversal => {
                            let anchor = outer.join("anchor");
                            fs::create_dir_all(&anchor).unwrap();
                            let link = root.child("parent-link");
                            symlink_dir(&anchor, &link).unwrap();
                            link.join("../.openclaw")
                        }
                        #[cfg(windows)]
                        Ordinary => std::path::PathBuf::from(
                            canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap(),
                        ),
                    };
                    let source = spelling(source_spelling);
                    let parent_link_target = fs::read_link(root.child("parent-link")).ok();
                    let configured =
                        spelling(config_spelling).join(if redirect { "redirect" } else { "owned" });
                    let mut config = serde_json::json!({"agents":{"entries":{
                        "inner":{"workspace":configured}
                    }}});
                    if enclosing {
                        config["agents"]["defaults"] = serde_json::json!({"workspace":outer});
                    }
                    fs::write(home.join("openclaw.json"), config.to_string()).unwrap();
                    let before = migration_source_inventory(&outer);
                    let env = ocm_env(&root);
                    let args = if adopt {
                        vec![
                            "adopt",
                            "import",
                            "--name",
                            "mira",
                            source.to_str().unwrap(),
                        ]
                    } else {
                        vec!["migrate", "mira", source.to_str().unwrap()]
                    };
                    let output = run_ocm(root.path(), &env, &args);
                    let scenario = format!(
                        "source={source_spelling:?}, config={config_spelling:?}, enclosing={enclosing}, redirect={redirect}, adopt={adopt}"
                    );
                    let target = root.child("ocm-home/envs/mira");
                    if redirect {
                        assert_eq!(
                            output.status.code(),
                            Some(1),
                            "{scenario}: {}",
                            stdout(&output)
                        );
                        assert!(
                            stderr(&output).contains("through a symlink"),
                            "{scenario}: {}",
                            stderr(&output)
                        );
                        assert!(!target.exists(), "{scenario}");
                        let registry = root.child("ocm-home/envs.json");
                        if registry.exists() {
                            let value: serde_json::Value =
                                serde_json::from_slice(&fs::read(registry).unwrap()).unwrap();
                            assert_eq!(value["envs"], serde_json::json!([]), "{scenario}");
                        }
                    } else {
                        assert!(output.status.success(), "{scenario}: {}", stderr(&output));
                        let imported: serde_json::Value = serde_json::from_slice(
                            &fs::read(target.join(".openclaw/openclaw.json")).unwrap(),
                        )
                        .unwrap();
                        let workspace = Path::new(
                            imported["agents"]["entries"]["inner"]["workspace"]
                                .as_str()
                                .unwrap(),
                        );
                        assert_eq!(
                            fs::read_to_string(workspace.join("notes.txt")).unwrap(),
                            "source payload",
                            "{scenario}"
                        );
                        if enclosing {
                            let outer_copy = Path::new(
                                imported["agents"]["defaults"]["workspace"]
                                    .as_str()
                                    .unwrap(),
                            );
                            assert_eq!(workspace, outer_copy.join(".openclaw/owned"), "{scenario}");
                            fs::write(workspace.join("shared-write.txt"), "target write").unwrap();
                            assert_eq!(
                                fs::read_to_string(
                                    outer_copy.join(".openclaw/owned/shared-write.txt")
                                )
                                .unwrap(),
                                "target write",
                                "{scenario}"
                            );
                        }
                    }
                    assert_eq!(migration_source_inventory(&outer), before, "{scenario}");
                    assert_eq!(fs::read_link(&alias).unwrap(), alias_target, "{scenario}");
                    assert_eq!(
                        fs::read_link(root.child("parent-link")).ok(),
                        parent_link_target,
                        "{scenario}"
                    );
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn migrate_copies_deep_external_workspaces_with_bounded_directory_handles() {
    for overlay in [false, true] {
        // Allow the 111-listener Gateway probe plus fixed overhead, while the
        // directory depth still exceeds the descriptor limit to catch retained handles.
        let root = TestDir::new("migrate-deep-workspace");
        let source = root.child("plain-home");
        let external = root.child("external");
        let relative: std::path::PathBuf = std::iter::repeat_n("d", 320).collect();
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(external.join(&relative)).unwrap();
        fs::write(external.join(&relative).join("notes.txt"), "deep payload\n").unwrap();
        if overlay {
            fs::create_dir_all(source.join("workspaces/default").join(&relative)).unwrap();
        }
        fs::write(
            source.join("openclaw.json"),
            serde_json::json!({
                "agents": {"defaults": {"workspace": external}}
            })
            .to_string(),
        )
        .unwrap();
        let env = ocm_env(&root);
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", "ulimit -n 256 && exec \"$@\"", "migration-limit"])
            .arg(support::ocm_test_binary_path())
            .args(["migrate", "mira"])
            .arg(&source)
            .current_dir(&source)
            .env_clear()
            .envs(&env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "overlay={overlay}: {}",
            stderr(&output)
        );
        assert_eq!(
            fs::read_to_string(
                root.child("ocm-home/envs/mira/.openclaw/workspaces/default")
                    .join(&relative)
                    .join("notes.txt")
            )
            .unwrap(),
            "deep payload\n"
        );
        assert_eq!(
            fs::read_to_string(external.join(&relative).join("notes.txt")).unwrap(),
            "deep payload\n"
        );
    }
}

#[cfg(unix)]
#[test]
fn migrate_rejects_copied_workspace_destination_links_without_changing_originals() {
    for agent in [false, true] {
        for layout in ["parent", "workspace", "nested", "file", "dangling"] {
            let root = TestDir::new("migrate-copied-workspace-destination-link");
            let cwd = root.child("cwd");
            let source = root.child("plain-home");
            let selected = root.child("selected");
            let unrelated = root.child("unrelated");
            for path in [
                &cwd,
                &source.join("workspace"),
                &selected.join("nested"),
                &unrelated,
            ] {
                fs::create_dir_all(path).unwrap();
            }
            fs::write(selected.join("notes.txt"), "selected payload\n").unwrap();
            fs::write(
                selected.join("nested/notes.txt"),
                "nested selected payload\n",
            )
            .unwrap();
            fs::write(unrelated.join("notes.txt"), "unrelated payload\n").unwrap();
            let destination = if agent {
                "workspaces/agents/ops"
            } else {
                "workspaces/default"
            };
            let link = source.join(match layout {
                "parent" => {
                    if agent {
                        "workspaces/agents"
                    } else {
                        "workspaces"
                    }
                }
                _ => destination,
            });
            let link = match layout {
                "nested" => link.join("nested"),
                "file" | "dangling" => link.join("notes.txt"),
                _ => link,
            };
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            let link_target = match layout {
                "file" => unrelated.join("notes.txt"),
                "dangling" => unrelated.join("missing.txt"),
                _ => unrelated.clone(),
            };
            std::os::unix::fs::symlink(&link_target, &link).unwrap();
            let config = if agent {
                serde_json::json!({"agents": {"defaults": {"workspace": source.join("workspace")}, "list": [{"id": "ops", "workspace": selected}]}})
            } else {
                serde_json::json!({"agents": {"defaults": {"workspace": selected}}})
            };
            fs::write(
                source.join("openclaw.json"),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            let before = [
                migration_source_inventory(&source),
                migration_source_inventory(&selected),
                migration_source_inventory(&unrelated),
            ];
            let env = ocm_env(&root);
            let output = run_ocm(
                &cwd,
                &env,
                &["migrate", "mira", source.to_str().unwrap(), "--json"],
            );
            assert_eq!(
                output.status.code(),
                Some(1),
                "agent={agent}, layout={layout}"
            );
            assert!(
                stderr(&output).contains("copied destination symlink"),
                "{}",
                stderr(&output)
            );
            assert_eq!(
                before,
                [
                    migration_source_inventory(&source),
                    migration_source_inventory(&selected),
                    migration_source_inventory(&unrelated)
                ],
                "agent={agent}, layout={layout}"
            );
            assert!(!root.child("ocm-home/envs/mira").exists());
            let registry: serde_json::Value =
                serde_json::from_slice(&fs::read(root.child("ocm-home/envs.json")).unwrap())
                    .unwrap();
            assert_eq!(registry["envs"], serde_json::json!([]));
        }
    }
}

#[cfg(unix)]
#[test]
fn adopt_import_copies_a_symlinked_external_workspace_into_the_disposable_environment() {
    let root = TestDir::new("migrate-symlinked-external-workspace");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let external = root.child("lobster-workspace");
    let linked_workspace = source_home.join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&source_home).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("notes.txt"), "repo-backed data\n").unwrap();
    std::os::unix::fs::symlink(&external, &linked_workspace).unwrap();
    fs::write(
        source_home.join("openclaw.json"),
        format!(
            r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
            linked_workspace.display()
        ),
    )
    .unwrap();
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let imported_workspace = root.child("ocm-home/envs/mira/.openclaw/workspaces/default");
    assert_eq!(
        fs::read_to_string(imported_workspace.join("notes.txt")).unwrap(),
        "repo-backed data\n"
    );
    let imported_config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/mira/.openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        imported_config["agents"]["defaults"]["workspace"].as_str(),
        Some(imported_workspace.to_string_lossy().as_ref())
    );
    assert_eq!(fs::read_link(&linked_workspace).unwrap(), external);
    assert_eq!(
        fs::read_to_string(external.join("notes.txt")).unwrap(),
        "repo-backed data\n"
    );
}

#[cfg(unix)]
#[test]
fn migrate_materializes_a_config_symlink_without_rewriting_its_target() {
    let root = TestDir::new("migrate-config-symlink");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let external_config = root.child("external/openclaw.json");
    let source_config = source_home.join("openclaw.json");
    let external_raw = fs::read_to_string(&source_config).unwrap();
    fs::create_dir_all(external_config.parent().unwrap()).unwrap();
    fs::write(&external_config, &external_raw).unwrap();
    fs::remove_file(&source_config).unwrap();
    std::os::unix::fs::symlink(&external_config, &source_config).unwrap();
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "migrate",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&external_config).unwrap(), external_raw);
    let imported_config = root.child("ocm-home/envs/mira/.openclaw/openclaw.json");
    assert!(
        fs::symlink_metadata(&imported_config)
            .unwrap()
            .file_type()
            .is_file()
    );
    let imported_raw = fs::read_to_string(&imported_config).unwrap();
    assert!(
        imported_raw.contains(
            &root
                .child("ocm-home/envs/mira/.openclaw/workspace")
                .display()
                .to_string()
        )
    );
    assert!(!imported_raw.contains(&source_home.display().to_string()));
}

#[test]
fn adopt_import_preserves_history_and_logs_from_plain_openclaw_home() {
    let root = TestDir::new("adopt-import");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("\"defaultLauncher\": \"mira.migrated\""));

    let imported_state = root.child("ocm-home/envs/mira/.openclaw");
    assert_imported_plain_openclaw_home(&imported_state, &source_home);
}

#[test]
fn adopt_import_preserves_legacy_audit_history_bytes() {
    let root = TestDir::new("adopt-audit-history");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let record = format!(
        "{}\n",
        serde_json::json!({ "configPath": source_home.join("openclaw.json") })
    );
    let mut audit_paths = Vec::new();
    for (directory, basename) in [
        ("logs", "config-audit.jsonl"),
        ("audit", "system-agent.jsonl"),
        ("audit", "crestodian.jsonl"),
    ] {
        for suffix in [
            ".migrated",
            "",
            ".migrated.2",
            ".migrated.184467440737095516160",
            ".migrated.raw",
            ".migrated.2.raw",
            ".migrated.2.raw.doctor-scrub-restore",
            ".migrated.2.raw.doctor-scrub-staging",
            ".migrated.2.raw.doctor-scrub-progress",
        ] {
            audit_paths.push(format!("{directory}/{basename}{suffix}"));
        }
        for generation in ["", ".2"] {
            audit_paths.push(format!(
                "{directory}/.{basename}.doctor-importing{generation}"
            ));
        }
    }
    for relative in &audit_paths {
        let path = source_home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &record).unwrap();
    }
    let ordinary_paths = [
        "logs/config-audit.jsonl.migrated.01",
        "audit/system-agent.jsonl.migrated.1",
        "audit/crestodian.jsonl.migrated.2.raw.other",
        "logs/other.jsonl.migrated",
        "other/logs/config-audit.jsonl.migrated",
    ];
    for relative in ordinary_paths {
        let path = source_home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &record).unwrap();
    }

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let imported_state = root.child("ocm-home/envs/mira/.openclaw");
    assert_imported_plain_openclaw_home(&imported_state, &source_home);
    for relative in &audit_paths {
        assert_eq!(
            fs::read(imported_state.join(relative)).unwrap(),
            record.as_bytes(),
            "import rewrote {relative}"
        );
        assert_eq!(
            fs::read(source_home.join(relative)).unwrap(),
            record.as_bytes()
        );
    }
    let relocated = record.replace(
        &source_home.display().to_string(),
        &imported_state.display().to_string(),
    );
    for relative in ordinary_paths {
        assert_eq!(
            fs::read_to_string(imported_state.join(relative)).unwrap(),
            relocated,
            "ordinary runtime reference was not relocated: {relative}"
        );
    }
}

#[test]
fn adopt_import_preserves_skill_workshop_proposal_integrity() {
    let root = TestDir::new("adopt-workshop-integrity");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);

    let draft = format!(
        "# Proposed skill\nRead {}/workspace/notes.txt\n",
        source_home.display()
    );
    let previous = format!(
        "# Previous skill\nRead {}/workspace/old.txt\n",
        source_home.display()
    );
    let support = format!(
        "Reference: {}/workspace/reference.txt\n",
        source_home.display()
    );
    let hash = |content: &str| {
        Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let proposal = serde_json::json!({
        "schema": "openclaw.skill-workshop.proposal.v1",
        "id": "example",
        "kind": "update",
        "status": "pending",
        "title": "Update example",
        "description": "Preserve reviewed content across adoption",
        "createdAt": "2026-09-01T00:00:00Z",
        "updatedAt": "2026-09-01T00:00:00Z",
        "draftFile": "PROPOSAL.md",
        "draftHash": hash(&draft),
        "supportFiles": [{
            "path": "references/tmp/proof.md",
            "hash": hash(&support),
            "sizeBytes": support.len()
        }, {
            "path": "scripts/package.lock",
            "hash": hash(&support),
            "sizeBytes": support.len()
        }],
        "target": {
            "skillName": "example",
            "skillKey": "example",
            "skillDir": source_home.join("workspace/skills/example"),
            "skillFile": source_home.join("workspace/skills/example/SKILL.md")
        },
        "scan": {"state": "clean", "critical": 0, "warn": 0, "info": 0, "findings": []}
    })
    .to_string();
    let rollback = serde_json::json!({
        "schema": "openclaw.skill-workshop.rollback.v1",
        "proposalId": "example",
        "writtenAt": "2026-09-01T00:00:00Z",
        "action": "update",
        "targetSkillFile": source_home.join("workspace/skills/example/SKILL.md"),
        "previousContent": previous,
        "previousContentHash": hash(&previous)
    })
    .to_string();
    let artifacts = [
        ("proposals/example/PROPOSAL.md", draft.as_str()),
        ("proposals/example/proposal.json", proposal.as_str()),
        ("proposals/example/rollback.json", rollback.as_str()),
        (
            "proposals/example/references/tmp/proof.md",
            support.as_str(),
        ),
        ("proposals/example/scripts/package.lock", support.as_str()),
        (
            "recovery/proposals/example/rollback.json",
            rollback.as_str(),
        ),
    ];
    for (relative, content) in &artifacts {
        let path = source_home.join("skill-workshop").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let imported_state = root.child("ocm-home/envs/mira/.openclaw");
    assert_imported_plain_openclaw_home(&imported_state, &source_home);
    let imported_draft =
        fs::read_to_string(imported_state.join("skill-workshop/proposals/example/PROPOSAL.md"))
            .unwrap();
    let imported_record: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(imported_state.join("skill-workshop/proposals/example/proposal.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        hash(&imported_draft),
        imported_record["draftHash"].as_str().unwrap(),
        "adoption invalidated the stored draftHash"
    );
    let imported_rollback: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(imported_state.join("skill-workshop/proposals/example/rollback.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        hash(imported_rollback["previousContent"].as_str().unwrap()),
        imported_rollback["previousContentHash"].as_str().unwrap(),
        "adoption invalidated the stored previousContentHash"
    );
    for (relative, content) in artifacts {
        assert_eq!(
            fs::read_to_string(imported_state.join("skill-workshop").join(relative))
                .unwrap_or_else(|error| panic!(
                    "import lost Skill Workshop artifact {relative}: {error}"
                )),
            content,
            "import rewrote integrity-protected Skill Workshop artifact {relative}"
        );
        assert_eq!(
            fs::read_to_string(source_home.join("skill-workshop").join(relative)).unwrap(),
            content,
            "import changed source artifact {relative}"
        );
    }

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "mira", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let health: serde_json::Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert!(
        health["issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| { !issue.as_str().unwrap().contains("OpenClaw runtime state") }),
        "workshop provenance must not trigger destructive runtime-state cleanup: {}",
        stdout(&doctor)
    );
}

#[test]
fn adopt_import_relocates_managed_plugin_install_records_into_the_imported_state() {
    let root = TestDir::new("adopt-import-plugin-records");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);

    let managed_hub = source_home.join("extensions/managed-hub");
    let managed_npm = source_home.join("npm/node_modules/example-managed-npm");
    for plugin_dir in [&managed_hub, &managed_npm] {
        fs::create_dir_all(plugin_dir).unwrap();
        fs::write(plugin_dir.join("openclaw.plugin.json"), "{}\n").unwrap();
    }

    seed_installed_plugin_index(
        &source_home,
        serde_json::json!({
            "managed-hub": {
                "source": "clawhub",
                "installPath": managed_hub.display().to_string()
            },
            "managed-npm": {
                "source": "npm",
                "installPath": managed_npm.display().to_string()
            }
        }),
    );

    let env = ocm_env(&root);
    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let imported_records = read_imported_plugin_install_records(&root, "mira");

    assert_eq!(
        imported_records["managed-hub"]["installPath"].as_str(),
        Some(
            root.child("ocm-home/envs/mira/.openclaw/extensions/managed-hub")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(
        imported_records["managed-npm"]["installPath"].as_str(),
        Some(
            root.child("ocm-home/envs/mira/.openclaw/npm/node_modules/example-managed-npm")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert!(
        !imported_records
            .to_string()
            .contains(&source_home.display().to_string())
    );
}

#[cfg(unix)]
#[test]
fn adopt_import_rejects_a_symlinked_plugin_database_without_mutating_its_target() {
    let root = TestDir::new("adopt-import-symlinked-plugin-database");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let external_home = root.child("external-state");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);

    let managed_plugin = source_home.join("extensions/managed-hub");
    fs::create_dir_all(&managed_plugin).unwrap();
    fs::write(managed_plugin.join("openclaw.plugin.json"), "{}\n").unwrap();
    seed_installed_plugin_index(
        &external_home,
        serde_json::json!({
            "managed-hub": {
                "source": "clawhub",
                "installPath": managed_plugin.display().to_string()
            }
        }),
    );

    let external_database = external_home.join("state/openclaw.sqlite");
    let source_database = source_home.join("state/openclaw.sqlite");
    fs::create_dir_all(source_database.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&external_database, &source_database).unwrap();
    let external_before = fs::read(&external_database).unwrap();

    let env = ocm_env(&root);
    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("must be a regular file inside the imported state"),
        "{}",
        stderr(&output)
    );
    assert!(!root.child("ocm-home/envs/mira").exists());
    assert_eq!(fs::read(&external_database).unwrap(), external_before);
}

#[test]
fn adopt_import_rejects_external_plugin_paths_without_mutating_the_source() {
    let root = TestDir::new("adopt-import-external-plugin");
    let cwd = root.child("workspace");
    let source_home = root.child("legacy-home/.openclaw");
    let external_plugin = root.child("external-checkout/example-dev-plugin");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&source_home);
    fs::create_dir_all(&external_plugin).unwrap();
    fs::write(external_plugin.join("marker.txt"), "external checkout\n").unwrap();

    seed_installed_plugin_index(
        &source_home,
        serde_json::json!({
            "external-dev": {
                "source": "path",
                "sourcePath": external_plugin.display().to_string(),
                "installPath": external_plugin.display().to_string()
            }
        }),
    );
    let source_database = source_home.join("state/openclaw.sqlite");
    let source_database_before = fs::read(&source_database).unwrap();

    let env = ocm_env(&root);
    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            source_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(
            "imported OpenClaw plugin path(s) could not be isolated inside the env state: external-dev"
        ),
        "{}",
        stderr(&output)
    );
    assert!(!root.child("ocm-home/envs/mira").exists());
    assert_eq!(fs::read(&source_database).unwrap(), source_database_before);
    assert_eq!(
        fs::read_to_string(external_plugin.join("marker.txt")).unwrap(),
        "external checkout\n"
    );
}

#[test]
fn adopt_import_relocates_managed_plugin_records_from_a_copied_gateway_fixture() {
    let root = TestDir::new("adopt-import-copied-plugin-records");
    let cwd = root.child("workspace");
    let original_home = root.child("personal/.openclaw");
    let copied_home = root.child("copied-state");
    fs::create_dir_all(&cwd).unwrap();
    seed_plain_openclaw_home(&copied_home);

    let copied_hub = copied_home.join("extensions/copied-hub");
    let copied_npm = copied_home.join("npm/node_modules/example-copied-npm");
    for plugin_dir in [&copied_hub, &copied_npm] {
        fs::create_dir_all(plugin_dir).unwrap();
        fs::write(plugin_dir.join("openclaw.plugin.json"), "{}\n").unwrap();
    }

    seed_installed_plugin_index(
        &copied_home,
        serde_json::json!({
            "copied-hub": {
                "source": "clawhub",
                "installPath": original_home.join("extensions/copied-hub").display().to_string()
            },
            "copied-npm": {
                "source": "npm",
                "installPath": original_home
                    .join("npm/node_modules/example-copied-npm")
                    .display()
                    .to_string()
            }
        }),
    );

    let env = ocm_env(&root);
    let output = run_ocm(
        &cwd,
        &env,
        &[
            "adopt",
            "import",
            "--name",
            "mira",
            copied_home.to_string_lossy().as_ref(),
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let imported_records = read_imported_plugin_install_records(&root, "mira");
    assert_eq!(
        imported_records["copied-hub"]["installPath"].as_str(),
        Some(
            root.child("ocm-home/envs/mira/.openclaw/extensions/copied-hub")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(
        imported_records["copied-npm"]["installPath"].as_str(),
        Some(
            root.child("ocm-home/envs/mira/.openclaw/npm/node_modules/example-copied-npm")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert!(
        !imported_records
            .to_string()
            .contains(&original_home.display().to_string())
    );
}

#[test]
fn migrate_requires_name() {
    let root = TestDir::new("migrate-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["migrate"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("migrate requires <env> or --name <env>"));
}

#[test]
fn migrate_rejects_multiple_env_names() {
    let root = TestDir::new("migrate-name-conflict");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["migrate", "mira", "--name", "ember"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("migrate accepts only one env name from <env> or --name <env>")
    );
}

#[test]
fn migration_snapshots_registered_databases_and_preserves_workspace_assets() {
    for adopt in [false, true] {
        let root = TestDir::new("migration-live-sqlite");
        let source = root.child("plain-home");
        let external = root.child("external");
        fs::create_dir_all(source.join("state")).unwrap();
        fs::create_dir_all(source.join("workspaces/default")).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(
            source.join("openclaw.json"),
            serde_json::json!({
                "agents": {"defaults": {"workspace": external}}
            })
            .to_string(),
        )
        .unwrap();
        let primary = source.join("state/openclaw.sqlite");
        let overlay = external.join("notes.db");
        let mut connections = Vec::new();
        for path in [&primary, &overlay] {
            let db = Connection::open(path).unwrap();
            db.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                CREATE TABLE durable (value TEXT); INSERT INTO durable VALUES ('committed');",
            )
            .unwrap();
            db.execute_batch("CREATE TABLE session_nodes (entry_json TEXT);")
                .unwrap();
            db.execute(
                "INSERT INTO session_nodes VALUES (?1)",
                [
                    serde_json::json!({"sessionRoot": source.join("workspaces/default")})
                        .to_string(),
                ],
            )
            .unwrap();
            connections.push(db);
        }
        connections[0]
            .execute_batch("CREATE TABLE agent_databases (path TEXT NOT NULL);")
            .unwrap();
        let earlier = source.join("workspaces/default/notes.db");
        if adopt {
            let earlier_db = Connection::open(&earlier).unwrap();
            earlier_db
                .execute_batch(
                    "CREATE TABLE durable(value); INSERT INTO durable VALUES ('earlier home');",
                )
                .unwrap();
        }
        connections[0]
            .execute(
                "INSERT INTO agent_databases VALUES (?1)",
                [if adopt {
                    earlier.as_path()
                } else {
                    overlay.as_path()
                }
                .to_str()
                .unwrap()],
            )
            .unwrap();
        let opaque = b"SQLite format 3\0opaque workspace fixture";
        fs::write(external.join("fixture.sqlite"), opaque).unwrap();
        fs::write(external.join("fixture.sqlite-wal"), b"opaque sidecar").unwrap();
        // Stale logs from the home must not survive replacement by the overlay.
        if !adopt {
            for suffix in ["-wal", "-shm", "-journal"] {
                fs::write(
                    source.join(format!("workspaces/default/notes.db{suffix}")),
                    b"stale log",
                )
                .unwrap();
            }
        }
        let originals: Vec<_> = [&primary, &overlay]
            .iter()
            .map(|path| {
                (
                    fs::read(path).unwrap(),
                    fs::read(format!("{}-wal", path.display())).unwrap(),
                )
            })
            .collect();
        let env = ocm_env(&root);
        let args = if adopt {
            vec![
                "adopt",
                "import",
                "--name",
                "target",
                source.to_str().unwrap(),
            ]
        } else {
            vec!["migrate", "target", source.to_str().unwrap()]
        };
        let output = run_ocm(&source, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
        for relative in ["state/openclaw.sqlite", "workspaces/default/notes.db"] {
            let target = root.child(format!("ocm-home/envs/target/.openclaw/{relative}"));
            for suffix in ["-wal", "-shm", "-journal"] {
                assert!(!Path::new(&format!("{}{suffix}", target.display())).exists());
            }
            let db = Connection::open(target).unwrap();
            assert_eq!(
                db.query_row("SELECT value FROM durable", [], |row| row
                    .get::<_, String>(0))
                    .unwrap(),
                "committed"
            );
            let entry: String = db
                .query_row("SELECT entry_json FROM session_nodes", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&entry).unwrap()["sessionRoot"],
                serde_json::json!(root.child("ocm-home/envs/target/.openclaw/workspaces/default"))
            );
            assert_eq!(
                db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
        }
        assert_eq!(
            fs::read(
                root.child("ocm-home/envs/target/.openclaw/workspaces/default/fixture.sqlite")
            )
            .unwrap(),
            opaque
        );
        assert_eq!(
            fs::read(
                root.child("ocm-home/envs/target/.openclaw/workspaces/default/fixture.sqlite-wal")
            )
            .unwrap(),
            b"opaque sidecar"
        );
        for (path, (primary_before, wal_before)) in [&primary, &overlay].iter().zip(originals) {
            assert_eq!(fs::read(path).unwrap(), primary_before);
            assert_eq!(
                fs::read(format!("{}-wal", path.display())).unwrap(),
                wal_before
            );
        }
        drop(connections);
    }
}

#[cfg(unix)]
#[test]
fn migration_rejects_registered_history_aliases_to_the_global_database() {
    use std::os::unix::fs::symlink;

    for (adopt, absolute, wal, protected) in [
        (false, false, false, true),
        (true, true, false, true),
        (false, true, true, true),
        (true, false, true, true),
        (true, false, true, false),
    ] {
        let root = TestDir::new("migration-registered-history-alias");
        let env = ocm_env(&root);
        let source = root.child("plain-home");
        let target = root.child("target");
        fs::create_dir_all(source.join("state")).unwrap();
        fs::create_dir_all(source.join("workspace")).unwrap();
        fs::write(source.join("openclaw.json"), "{}").unwrap();
        let plugin = source.join("extensions/demo");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("index.js"), "fixture").unwrap();
        let database = source.join("state/openclaw.sqlite");
        let db = Connection::open(&database).unwrap();
        if wal {
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
                .unwrap();
        }
        db.execute_batch(
            "CREATE TABLE agent_databases(path TEXT);
             CREATE TABLE installed_plugin_index(index_key TEXT PRIMARY KEY, install_records_json TEXT NOT NULL);",
        )
        .unwrap();
        let relative = if protected {
            "skill-workshop/support/history.sqlite"
        } else {
            "custom/history.sqlite"
        };
        let history = source.join(relative);
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        symlink(
            if protected {
                "../../state/openclaw.sqlite"
            } else {
                "../state/openclaw.sqlite"
            },
            &history,
        )
        .unwrap();
        db.execute(
            "INSERT INTO agent_databases VALUES (?1)",
            [if absolute {
                history.display().to_string()
            } else {
                relative.into()
            }],
        )
        .unwrap();
        db.execute(
            "INSERT INTO installed_plugin_index VALUES ('installed-plugin-index', ?1)",
            [serde_json::json!({"demo": {"source": "path", "installPath": plugin}}).to_string()],
        )
        .unwrap();
        let source_files = [
            database.clone(),
            database.with_file_name("openclaw.sqlite-wal"),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect::<Vec<_>>();
        let mut args = if adopt {
            vec![
                "adopt",
                "import",
                "--name",
                "target",
                source.to_str().unwrap(),
            ]
        } else {
            vec!["migrate", "target", source.to_str().unwrap()]
        };
        args.extend(["--root", target.to_str().unwrap(), "--json"]);
        let output = run_ocm(root.path(), &env, &args);
        assert_eq!(output.status.success(), !protected, "{}", stderr(&output));
        let shown = run_ocm(root.path(), &env, &["env", "show", "target", "--json"]);
        assert_eq!(shown.status.success(), !protected);
        if protected {
            assert!(stderr(&output).contains("selects integrity-protected history"));
            assert!(!target.exists());
        } else {
            let copied = Connection::open(target.join(".openclaw").join(relative)).unwrap();
            let records: String = copied
                .query_row(
                    "SELECT install_records_json FROM installed_plugin_index",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let records: serde_json::Value = serde_json::from_str(&records).unwrap();
            assert_eq!(
                records["demo"]["installPath"],
                target
                    .join(".openclaw/extensions/demo")
                    .display()
                    .to_string()
            );
        }
        for (path, bytes) in source_files {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(history.is_symlink());
        drop(db);
    }
}

#[test]
fn migration_rolls_back_an_unreadable_sqlite_snapshot() {
    let root = TestDir::new("migration-invalid-sqlite");
    let source = root.child("plain-home");
    fs::create_dir_all(source.join("state")).unwrap();
    fs::write(source.join("openclaw.json"), "{}").unwrap();
    let database = source.join("state/openclaw.sqlite");
    let invalid = b"SQLite format 3\0invalid database fixture";
    fs::write(&database, invalid).unwrap();
    let env = ocm_env(&root);
    let output = run_ocm(
        &source,
        &env,
        &["migrate", "target", source.to_str().unwrap()],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("SQLite"), "{}", stderr(&output));
    assert!(!root.child("ocm-home/envs/target").exists());
    let listing = run_ocm(&source, &env, &["env", "list", "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&listing)).unwrap(),
        serde_json::json!([])
    );
    assert_eq!(fs::read(&database).unwrap(), invalid);
}

#[test]
fn migration_rejects_an_orphan_overlay_log_for_a_copied_database() {
    for registered_in_home in [false, true] {
        let root = TestDir::new("migration-orphan-sqlite-overlay");
        let source = root.child("plain-home");
        let external = root.child("external");
        fs::create_dir_all(source.join("workspaces/default")).unwrap();
        fs::create_dir_all(&external).unwrap();
        let database = source.join("workspaces/default/notes.db");
        let db = Connection::open(&database).unwrap();
        db.execute_batch("CREATE TABLE durable(value); INSERT INTO durable VALUES(42);")
            .unwrap();
        drop(db);
        let before = fs::read(&database).unwrap();
        fs::create_dir_all(source.join("state")).unwrap();
        let registry = Connection::open(source.join("state/openclaw.sqlite")).unwrap();
        registry
            .execute_batch("CREATE TABLE agent_databases (path TEXT NOT NULL);")
            .unwrap();
        registry
            .execute(
                "INSERT INTO agent_databases VALUES (?1)",
                [if registered_in_home {
                    database.clone()
                } else {
                    external.join("notes.db")
                }
                .to_str()
                .unwrap()],
            )
            .unwrap();
        drop(registry);
        fs::write(external.join("notes.db-wal"), b"unrelated overlay log").unwrap();
        fs::write(
            source.join("openclaw.json"),
            serde_json::json!({
                "agents": {"defaults": {"workspace": external}}
            })
            .to_string(),
        )
        .unwrap();
        let env = ocm_env(&root);
        let output = run_ocm(
            &source,
            &env,
            &["migrate", "target", source.to_str().unwrap()],
        );
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("cannot overlay SQLite sidecar"),
            "{}",
            stderr(&output)
        );
        assert!(!root.child("ocm-home/envs/target").exists());
        assert_eq!(fs::read(database).unwrap(), before);
        assert_eq!(
            fs::read(external.join("notes.db-wal")).unwrap(),
            b"unrelated overlay log"
        );
    }
}

#[test]
fn migrate_relocates_complete_root_references_and_preserves_siblings() {
    let root = TestDir::new("migrate-root-boundaries");
    let source = root.child("legacy/.openclaw");
    seed_plain_openclaw_home(&source);
    let mut env = ocm_env(&root);
    install_fake_openclaw_on_path(&root, &mut env);
    let file = "agents/main/agent/root-references.json";
    let owned = source.join("extensions/demo/index.js");
    let file_url = url::Url::from_file_path(&owned).unwrap();
    let value = serde_json::json!({
        "installPath": source.with_file_name(".openclaw-other").join("plugin"),
        "sourcePath": source.with_file_name(".openclaw other").join("plugin"),
        "rootDir": owned,
        "text": format!("{}/first --input {}/second\n{}/third", source.display(), source.display(), source.display()),
        "url": file_url.as_str(),
        "externalUrl": format!("{};other/plugin.js", url::Url::from_file_path(&source).unwrap()),
        "apostropheUrl": format!("{}'other/plugin.js", url::Url::from_file_path(&source).unwrap()),
        "metadata": 17
    });
    fs::create_dir_all(source.join(file).parent().unwrap()).unwrap();
    fs::write(source.join(file), value.to_string()).unwrap();
    #[cfg(unix)]
    fs::write(
        source.join("agents/main/agent/posix.json"),
        serde_json::json!({"installPath":format!("{}\\other/plugin", source.display())})
            .to_string(),
    )
    .unwrap();
    let output = run_ocm(
        root.path(),
        &env,
        &["migrate", "copy", source.to_str().unwrap(), "--json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let target = root.child("ocm-home/envs/copy/.openclaw");
    let actual: serde_json::Value =
        serde_json::from_slice(&fs::read(target.join(file)).unwrap()).unwrap();
    let mut expected = value;
    expected["rootDir"] = serde_json::json!(target.join("extensions/demo/index.js"));
    expected["text"] = serde_json::json!(format!(
        "{}/first --input {}/second\n{}/third",
        target.display(),
        target.display(),
        target.display()
    ));
    expected["url"] = serde_json::json!(
        url::Url::from_file_path(target.join("extensions/demo/index.js"))
            .unwrap()
            .as_str()
    );
    assert_eq!(actual, expected);
    #[cfg(unix)]
    assert_eq!(
        fs::read(target.join("agents/main/agent/posix.json")).unwrap(),
        fs::read(source.join("agents/main/agent/posix.json")).unwrap()
    );
}

#[test]
fn migration_rebases_retained_sqlite_sessions_to_the_copied_state_root() {
    for (adopt, source_form) in [
        (false, "absolute"),
        (false, "relative"),
        (true, "parent"),
        (false, "alias"),
        (false, "workspace-alias"),
        (true, "nested-link"),
        (false, "direct-link"),
        (false, "target-spelling"),
        (false, "embedded-target"),
    ] {
        #[cfg(not(unix))]
        if matches!(
            source_form,
            "alias"
                | "workspace-alias"
                | "nested-link"
                | "direct-link"
                | "target-spelling"
                | "embedded-target"
        ) {
            continue;
        }
        let root = TestDir::new("migration-sqlite-session-paths");
        let source = root.child("legacy").join(".openclaw");
        seed_plain_openclaw_home(&source);
        let alias = root.child("legacy-alias");
        #[cfg(unix)]
        if matches!(
            source_form,
            "alias" | "nested-link" | "target-spelling" | "embedded-target"
        ) {
            std::os::unix::fs::symlink(source.parent().unwrap(), &alias).unwrap();
        }
        let alias_source = alias.join(".openclaw");
        let workspace_alias = root.child("workspace-alias");
        #[cfg(unix)]
        if source_form == "workspace-alias" {
            std::os::unix::fs::symlink(source.join("workspace"), &workspace_alias).unwrap();
        }
        #[cfg(unix)]
        if matches!(source_form, "nested-link" | "direct-link") {
            fs::create_dir_all(source.join("links")).unwrap();
            std::os::unix::fs::symlink(source.join("workspace"), source.join("links/workspace"))
                .unwrap();
        }
        let stored_workspace = match source_form {
            "alias" | "target-spelling" | "embedded-target" => alias_source.join("workspace"),
            "workspace-alias" => workspace_alias,
            "nested-link" => alias_source.join("links/workspace"),
            "direct-link" => source.join("links/workspace"),
            _ => source.join("workspace"),
        };
        if matches!(source_form, "workspace-alias" | "nested-link") {
            let config_path = source.join("openclaw.json");
            let mut config: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
            config["agents"]["defaults"]["workspace"] = serde_json::json!(stored_workspace);
            fs::write(config_path, config.to_string()).unwrap();
        }
        let legacy_path = source.join("agents/main/sessions/main.jsonl");
        let mut legacy: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&legacy_path).unwrap()).unwrap();
        legacy["cwd"] = serde_json::json!(stored_workspace);
        fs::write(legacy_path, format!("{legacy}\n")).unwrap();
        let spawned_cwd = stored_workspace.join("project");
        let external = root.child("external-project");
        let sibling = source.with_file_name(".openclaw-other").join("workspace");
        let mut entry = serde_json::json!({
            "sessionId": "retained-session",
            "sessionRoot": stored_workspace,
            "spawnedCwd": spawned_cwd,
            "externalPath": external,
            "siblingPath": sibling,
            "label": format!("Authored label mentioning {}", source.display()),
            "systemPrompt": format!("Preserve source text: {}", source.display())
        });
        #[cfg(unix)]
        if source_form == "direct-link" {
            fs::create_dir_all(&external).unwrap();
            std::os::unix::fs::symlink(&external, source.join("links/external")).unwrap();
            std::os::unix::fs::symlink(
                root.child("missing-external"),
                source.join("links/dangling"),
            )
            .unwrap();
            entry["worktree"] = serde_json::json!({
                "repoRoot": source.join("links/external"),
                "canonicalWorkspaceDir": source.join("links/dangling/child")
            });
        }
        let databases = [
            "agents/main/agent/openclaw-agent.sqlite",
            "workspace/custom-agent.db",
            "skill-workshop/retained.db",
        ];
        let authored = format!(
            "console.log({:?});",
            source.join("workspace").display().to_string()
        );
        let authored_hash = Sha256::digest(authored.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        for relative in databases {
            let path = source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let db = Connection::open(path).unwrap();
            db.execute_batch(
                "CREATE TABLE session_nodes (session_key TEXT PRIMARY KEY, entry_json TEXT NOT NULL);",
            )
            .unwrap();
            db.execute(
                "INSERT INTO session_nodes VALUES ('retained', ?1)",
                [entry.to_string()],
            )
            .unwrap();
            db.execute_batch("CREATE TABLE board_widgets (descriptor_json TEXT, sha256 TEXT, granted_sha TEXT, grant_state TEXT);").unwrap();
            db.execute(
                "INSERT INTO board_widgets VALUES (?1, ?2, ?2, 'granted')",
                params![
                    serde_json::json!({"source": authored}).to_string(),
                    authored_hash
                ],
            )
            .unwrap();
        }
        fs::create_dir_all(source.join("state")).unwrap();
        let registry = Connection::open(source.join("state/openclaw.sqlite")).unwrap();
        registry
            .execute_batch("CREATE TABLE agent_databases (path TEXT NOT NULL);")
            .unwrap();
        // Protected history remains copied data. Registering it as active state
        // is rejected by the shared ownership policy before migration writes.
        // Keep both active spellings: their proven whole-state aliases also
        // relocate literal legacy references that are not typed path fields.
        for registered in [
            stored_workspace.join("custom-agent.db"),
            source.join("workspace").join("custom-agent.db"),
        ] {
            registry
                .execute(
                    "INSERT INTO agent_databases VALUES (?1)",
                    [registered.to_str().unwrap()],
                )
                .unwrap();
        }
        drop(registry);
        let originals: Vec<_> = databases
            .iter()
            .chain(["state/openclaw.sqlite"].iter())
            .map(|relative| (*relative, fs::read(source.join(relative)).unwrap()))
            .collect();
        let mut target_root = root.child("ocm-home").join("envs").join("target");
        #[cfg(unix)]
        if matches!(source_form, "target-spelling" | "embedded-target") {
            let outside = root.child("outside");
            fs::create_dir_all(&outside).unwrap();
            if source_form == "target-spelling" {
                std::os::unix::fs::symlink(outside, source.join("escape")).unwrap();
                target_root = source.join("escape/target");
            } else {
                target_root = outside.join(format!("={}/target", source.display()));
            }
            fs::write(
                source.join("logs/app.log"),
                format!(
                    "cwd={}\nuri={}\nmixed={} source={}\nroot={}\nquoted=\"{}\"\nangle=<{}>\n",
                    stored_workspace.display(),
                    url::Url::from_file_path(&stored_workspace).unwrap(),
                    target_root.join(".openclaw/workspace").display(),
                    stored_workspace.display(),
                    url::Url::from_file_path(&alias_source).unwrap(),
                    url::Url::from_file_path(&alias_source).unwrap(),
                    url::Url::from_file_path(&alias_source).unwrap()
                ),
            )
            .unwrap();
        }
        let env = ocm_env(&root);
        let cwd = root.child("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let source_argument = match source_form {
            "relative" => "legacy/.openclaw",
            "parent" => "../legacy/.openclaw",
            "alias" | "target-spelling" | "embedded-target" => alias_source.to_str().unwrap(),
            _ => source.to_str().unwrap(),
        };
        let cwd = if source_form == "parent" {
            cwd.as_path()
        } else {
            root.path()
        };
        let mut args = if adopt {
            vec!["adopt", "import", "--name", "target", source_argument]
        } else {
            vec!["migrate", "target", source_argument]
        };
        if matches!(source_form, "target-spelling" | "embedded-target") {
            args.extend(["--root", target_root.to_str().unwrap()]);
        }
        let output = run_ocm(cwd, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
        let target = target_root.join(".openclaw");
        if matches!(source_form, "target-spelling" | "embedded-target") {
            let log = fs::read_to_string(target.join("logs/app.log")).unwrap();
            assert_eq!(
                log,
                format!(
                    "cwd={}\nuri={}\nmixed={} source={}\nroot={}\nquoted=\"{}\"\nangle=<{}>\n",
                    target.join("workspace").display(),
                    url::Url::from_file_path(target.join("workspace")).unwrap(),
                    target.join("workspace").display(),
                    target.join("workspace").display(),
                    url::Url::from_file_path(&target).unwrap(),
                    url::Url::from_file_path(&target).unwrap(),
                    url::Url::from_file_path(&target).unwrap()
                )
            );
            let legacy: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(target.join("agents/main/sessions/main.jsonl")).unwrap(),
            )
            .unwrap();
            assert_eq!(legacy["cwd"], serde_json::json!(target.join("workspace")));
        } else {
            assert_imported_plain_openclaw_home(&target, &source);
        }
        let mut expected = entry;
        expected["sessionRoot"] = serde_json::json!(target.join("workspace"));
        expected["spawnedCwd"] = serde_json::json!(target.join("workspace").join("project"));
        for relative in &databases[..2] {
            let db = Connection::open(target.join(relative)).unwrap();
            let actual: String = db
                .query_row("SELECT entry_json FROM session_nodes", [], |row| row.get(0))
                .unwrap();
            let actual: serde_json::Value = serde_json::from_str(&actual).unwrap();
            assert_eq!(actual, expected, "{source_form}");
            assert_eq!(
                fs::canonicalize(actual["sessionRoot"].as_str().unwrap()).unwrap(),
                fs::canonicalize(target.join("workspace")).unwrap(),
                "{source_form}"
            );
            let payload: (String, String, String, String) = db
                .query_row(
                    "SELECT descriptor_json, sha256, granted_sha, grant_state FROM board_widgets",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                payload,
                (
                    serde_json::json!({"source": authored}).to_string(),
                    authored_hash.clone(),
                    authored_hash.clone(),
                    "granted".to_string()
                )
            );
            assert_eq!(
                db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
        }
        let registry = Connection::open(target.join("state/openclaw.sqlite")).unwrap();
        let registered: String = registry
            .query_row(
                "SELECT path FROM agent_databases ORDER BY rowid LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            registered,
            target
                .join("workspace")
                .join("custom-agent.db")
                .to_str()
                .unwrap()
        );
        assert_eq!(
            fs::read(target.join(databases[2])).unwrap(),
            fs::read(source.join(databases[2])).unwrap()
        );
        for (relative, before) in originals {
            assert_eq!(fs::read(source.join(relative)).unwrap(), before);
        }
    }
}

#[test]
fn migration_rebases_sessions_through_shared_workspace_projections() {
    for (adopt, home_parent) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = TestDir::new("migration-shared-session-projections");
        let shared = root.child("shared");
        let source = if home_parent {
            shared.join("plain-home")
        } else {
            root.child("plain-home")
        };
        let workspace = if home_parent {
            source.join("workspace")
        } else {
            shared.join("project")
        };
        let other = root.child("other");
        let outside = root.child("uncopied");
        for path in [
            source.join("agents/main/agent"),
            source.join("state"),
            workspace.clone(),
            other.clone(),
            outside.clone(),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(
            source.join("openclaw.json"),
            serde_json::json!({
                "agents": {"defaults": {"workspace": shared}, "list": [
                    {"id":"main", "default":true},
                    {"id":"twin", "workspace":shared},
                    {"id":"nested", "workspace":workspace.clone()},
                    {"id":"other", "workspace":other}
                ]}
            })
            .to_string(),
        )
        .unwrap();
        let agent_path = source
            .join("agents")
            .join("main")
            .join("agent")
            .join("openclaw-agent.sqlite");
        let db = Connection::open(&agent_path).unwrap();
        db.execute_batch("CREATE TABLE schema_meta(meta_key TEXT PRIMARY KEY, role TEXT, agent_id TEXT);
            INSERT INTO schema_meta VALUES ('primary', 'agent', 'main');
            CREATE TABLE session_nodes(session_key TEXT PRIMARY KEY, entry_json TEXT NOT NULL) WITHOUT ROWID;").unwrap();
        // Identical JSON follows the same copied workspace across the shared store.
        // Access principals and session keys do not select another destination.
        let entry = serde_json::json!({
            "sessionRoot": workspace.clone(),
            "spawnedCwd": workspace.join("missing-child"),
            "spawnedWorkspaceDir": url::Url::from_directory_path(workspace.clone()).unwrap().as_str(),
            "owner": {"actor": {"id":"twin"}},
            "label": format!("Authored {}", shared.display()),
            "worktree": {"repoRoot": outside, "canonicalWorkspaceDir": workspace.clone()}
        }).to_string();
        for key in [
            "agent:main:retained",
            "agent:twin:retained",
            "agent:nested:retained",
            "agent:other:retained",
            "legacy",
        ] {
            db.execute(
                "INSERT INTO session_nodes VALUES (?1, ?2)",
                params![key, entry],
            )
            .unwrap();
        }
        db.execute_batch("CREATE TABLE session_entries AS SELECT * FROM session_nodes;")
            .unwrap();
        drop(db);
        let external_database = shared.join("retained.db");
        Connection::open(&external_database)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT); INSERT INTO durable VALUES ('keep');")
            .unwrap();
        let registry_path = source.join("state/openclaw.sqlite");
        let registry = Connection::open(&registry_path).unwrap();
        registry.execute_batch("CREATE TABLE agent_databases(agent_id TEXT, path TEXT, PRIMARY KEY(agent_id,path)) WITHOUT ROWID;").unwrap();
        for agent in ["main", "twin"] {
            registry
                .execute(
                    "INSERT INTO agent_databases VALUES (?1, ?2)",
                    params![agent, external_database.to_str().unwrap()],
                )
                .unwrap();
        }
        registry
            .execute(
                "INSERT INTO agent_databases VALUES ('internal', ?1)",
                [agent_path.to_str().unwrap()],
            )
            .unwrap();
        drop(registry);
        let legacy_path = source.join("agents/main/sessions/retained.jsonl");
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let legacy_header = serde_json::json!({
            "type":"session", "version":3, "id":"retained-legacy-session",
            "timestamp":"2026-10-17T12:00:00.000Z", "cwd":workspace,
            "metadata":{"cwd":workspace}
        });
        let legacy_other = serde_json::json!({"type":"custom", "cwd":workspace});
        fs::write(
            &legacy_path,
            format!("[\"session\"]\n{legacy_header}\n{legacy_other}\n"),
        )
        .unwrap();
        let originals = [
            &agent_path,
            &registry_path,
            &external_database,
            &legacy_path,
        ]
        .map(|path| fs::read(path).unwrap());
        let target_root = root.child("target");
        let mut args = if adopt {
            vec!["adopt", "import", "--name", "target"]
        } else {
            vec!["migrate", "target"]
        };
        args.extend([
            source.to_str().unwrap(),
            "--root",
            target_root.to_str().unwrap(),
        ]);
        let output = run_ocm(root.path(), &ocm_env(&root), &args);
        assert!(output.status.success(), "{}", stderr(&output));
        let target = target_root.join(".openclaw");
        let copied_workspace = target.join("workspaces").join("default");
        let copied_workspace = if home_parent {
            copied_workspace.join("plain-home").join("workspace")
        } else {
            copied_workspace.join("project")
        };
        let legacy =
            fs::read_to_string(target.join("agents/main/sessions/retained.jsonl")).unwrap();
        let events = legacy
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let mut expected_header = legacy_header;
        let mut expected_other = legacy_other;
        if home_parent {
            expected_header["cwd"] = serde_json::json!(&copied_workspace);
            // Only the recognized top-level header location changes projection.
            expected_header["metadata"]["cwd"] = serde_json::json!(target.join("workspace"));
            expected_other["cwd"] = serde_json::json!(target.join("workspace"));
        }
        assert_eq!(
            events,
            vec![
                serde_json::json!(["session"]),
                expected_header,
                expected_other
            ]
        );
        let copied =
            Connection::open(target.join("agents/main/agent/openclaw-agent.sqlite")).unwrap();
        for key in [
            "agent:main:retained",
            "agent:twin:retained",
            "agent:nested:retained",
            "agent:other:retained",
            "legacy",
        ] {
            for table in ["session_nodes", "session_entries"] {
                let raw: String = copied
                    .query_row(
                        &format!("SELECT entry_json FROM {table} WHERE session_key=?1"),
                        [key],
                        |row| row.get(0),
                    )
                    .unwrap();
                let mut expected: serde_json::Value = serde_json::from_str(&entry).unwrap();
                expected["sessionRoot"] = serde_json::json!(&copied_workspace);
                expected["spawnedCwd"] = serde_json::json!(copied_workspace.join("missing-child"));
                expected["spawnedWorkspaceDir"] = serde_json::json!(
                    url::Url::from_directory_path(&copied_workspace)
                        .unwrap()
                        .as_str()
                );
                expected["worktree"]["canonicalWorkspaceDir"] =
                    serde_json::json!(&copied_workspace);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&raw).unwrap(),
                    expected,
                    "{table}: {key}"
                );
            }
        }
        let registry = Connection::open(target.join("state/openclaw.sqlite")).unwrap();
        let internal_database: String = registry
            .query_row(
                "SELECT path FROM agent_databases WHERE agent_id = 'internal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            Path::new(&internal_database),
            target
                .join("agents")
                .join("main")
                .join("agent")
                .join("openclaw-agent.sqlite")
        );
        for agent in ["main", "twin"] {
            let path: String = registry
                .query_row(
                    "SELECT path FROM agent_databases WHERE agent_id=?1",
                    [agent],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                Path::new(&path),
                target
                    .join("workspaces")
                    .join("default")
                    .join("retained.db")
            );
            let copied = Connection::open(path).unwrap();
            assert_eq!(
                copied
                    .query_row("SELECT value FROM durable", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "keep"
            );
        }
        for (path, original) in [
            &agent_path,
            &registry_path,
            &external_database,
            &legacy_path,
        ]
        .into_iter()
        .zip(originals)
        {
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }
}

#[test]
fn migration_rolls_back_a_database_relocation_conflict() {
    let root = TestDir::new("migration-sqlite-relocation-conflict");
    let source = root.child("legacy/.openclaw");
    seed_plain_openclaw_home(&source);
    let target = root.child("ocm-home/envs/target/.openclaw");
    let path = source.join("agents/main/agent/openclaw-agent.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE session_nodes (entry_json TEXT UNIQUE ON CONFLICT REPLACE);")
        .unwrap();
    for workspace in [source.join("workspace"), target.join("workspace")] {
        db.execute(
            "INSERT INTO session_nodes VALUES (?1)",
            [serde_json::json!({"sessionRoot": workspace}).to_string()],
        )
        .unwrap();
    }
    drop(db);
    let before = fs::read(&path).unwrap();
    let env = ocm_env(&root);
    let output = run_ocm(
        root.path(),
        &env,
        &["migrate", "target", source.to_str().unwrap()],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("failed to relocate imported OpenClaw state database"),
        "{}",
        stderr(&output)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
    let listing = run_ocm(root.path(), &env, &["env", "list", "--json"]);
    assert!(listing.status.success(), "{}", stderr(&listing));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&listing)).unwrap(),
        serde_json::json!([])
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn migrate_and_adopt_relocate_config_includes() {
    for adopt in [false, true] {
        let root = TestDir::new("migrate-config-includes");
        let env = ocm_env(&root);
        let source = root.child("source/plain-home");
        let fixture = config_includes::IncludedPluginConfig::seed(&source, &root.child("external"));
        let args = if adopt {
            vec![
                "adopt",
                "import",
                "--name",
                "copied",
                source.to_str().unwrap(),
            ]
        } else {
            vec!["migrate", "copied", source.to_str().unwrap()]
        };
        let migrate = run_ocm(root.path(), &env, &args);
        assert!(migrate.status.success(), "{}", stderr(&migrate));
        fixture.assert_relocated(&root.child("ocm-home/envs/copied/.openclaw"));
        fixture.assert_source_unchanged(&source);
    }
}
