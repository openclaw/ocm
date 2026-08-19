mod support;

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::{Connection, params};

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
            "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}},\"list\":[{{\"id\":\"main\",\"default\":true}},{{\"id\":\"ops\",\"workspace\":\"{}\"}}]}},\"externalProject\":\"{}\"}}\n",
            source_home.join("workspace").display(),
            source_home.join("team/ops").display(),
            source_home
                .parent()
                .unwrap()
                .join("projects/unrelated")
                .display()
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
            "{{\"cwd\":\"{}\",\"log\":\"{}\"}}\n",
            source_home.join("workspace").display(),
            source_home.join("logs/app.log").display()
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
    assert!(config_text.contains(&imported_state.join("workspace").display().to_string()));
    assert!(
        config_text.contains(
            &source_home
                .parent()
                .unwrap()
                .join("projects/unrelated")
                .display()
                .to_string()
        )
    );
    assert!(!config_text.contains(&source_home.display().to_string()));

    let session_text =
        fs::read_to_string(imported_state.join("agents/main/sessions/main.jsonl")).unwrap();
    let log_text = fs::read_to_string(imported_state.join("logs/app.log")).unwrap();
    let backup_text = fs::read_to_string(imported_state.join("openclaw.json.bak")).unwrap();
    assert!(session_text.contains(&imported_state.join("workspace").display().to_string()));
    assert!(session_text.contains(&imported_state.join("logs/app.log").display().to_string()));
    assert!(!session_text.contains(&source_home.display().to_string()));
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
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&source_home).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("notes.txt"), "external data\n").unwrap();
    fs::write(
        source_home.join("openclaw.json"),
        format!(
            r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
            external.display()
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
        format!(
            r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
            external.display()
        )
    );
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
fn adopt_import_preserves_external_plugin_checkouts_with_an_isolation_warning() {
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
    assert!(
        stderr(&output).contains(
            "warning: imported plugin external-dev could not be isolated inside the env state; OCM preserved but did not copy or modify its external location"
        ),
        "{}",
        stderr(&output)
    );

    let imported_records = read_imported_plugin_install_records(&root, "mira");
    assert_eq!(
        imported_records["external-dev"]["installPath"].as_str(),
        Some(external_plugin.to_string_lossy().as_ref())
    );
    assert_eq!(
        fs::read_to_string(external_plugin.join("marker.txt")).unwrap(),
        "external checkout\n"
    );
    assert!(
        !root
            .child("ocm-home/envs/mira/.openclaw/external-checkout")
            .exists()
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
