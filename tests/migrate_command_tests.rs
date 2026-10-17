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
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
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
            result.insert(path.strip_prefix(root).unwrap().to_path_buf(), value);
        }
        let mut result = BTreeMap::new();
        visit(root, root, &mut result);
        result
    }

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
                inventory(&source),
                inventory(&selected),
                inventory(&unrelated),
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
                    inventory(&source),
                    inventory(&selected),
                    inventory(&unrelated)
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
