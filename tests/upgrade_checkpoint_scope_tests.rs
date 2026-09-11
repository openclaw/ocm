#![cfg(unix)]
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::Value;
use support::{
    TestDir, create_owned_dev_env, dev_plain, ocm_env, run_ocm, stderr, stdout,
    write_executable_script, write_text,
};

struct Fixture {
    root: TestDir,
    env: BTreeMap<String, String>,
    state: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_independent_root(".openclaw/workspace/projects")
    }

    fn with_independent_root(independent: &str) -> Self {
        let root = TestDir::new("upgrade-independent-project");
        let env = ocm_env(&root);
        for (name, version) in [("old", "2026.9.1"), ("new", "2026.9.2")] {
            let executable = root.child(name);
            write_executable_script(
                &executable,
                &format!(
                    r#"#!/bin/sh
case "$1 $2" in
  '--version ') echo '{version}';;
  'config validate') echo 'Config valid';;
  'doctor --lint') echo '{{"ok":true,"checksRun":1,"checksSkipped":0,"findings":[]}}';;
  'update finalize')
    : > "$OCM_PROOF_READY"
    n=0
    while [ ! -f "$OCM_PROOF_RELEASE" ]; do
      n=$((n+1)); [ "$n" -lt 200 ] || exit 92
      sleep 0.025
    done
    if [ "$OCM_PROOF_FAIL" = 1 ]; then echo 'partial migration failed' >&2; exit 23; fi
    echo '{{"status":"ok","mode":"finalize","postUpdate":{{"doctor":{{"status":"ok"}},"plugins":{{"status":"ok"}}}}}}';;
  'gateway status') echo '{{"rpc":{{"ok":true}}}}';;
  *) echo "unexpected fixture invocation $*" >&2; exit 91;;
esac
"#
                ),
            );
            let result = run_ocm(
                root.path(),
                &env,
                &[
                    "runtime",
                    "add",
                    name,
                    "--path",
                    executable.to_str().unwrap(),
                ],
            );
            assert!(result.status.success(), "{}", stderr(&result));
        }
        let result = run_ocm(
            root.path(),
            &env,
            &["env", "create", "demo", "--runtime", "old"],
        );
        assert!(result.status.success(), "{}", stderr(&result));
        let state = root.child("ocm-home/envs/demo/.openclaw");
        let project = state.parent().unwrap().join(independent).join("example");
        write_text(&state.join("openclaw.json"), "{}\n");
        write_text(&project.join("code"), "before\n");
        write_text(&project.join("deleted"), "before\n");
        write_text(
            &project.join("node_modules/package/index.js"),
            "dependency\n",
        );
        symlink("code", project.join("link")).unwrap();
        write_text(&state.join("workspace/.openclaw/legacy-state"), "legacy\n");
        write_text(&state.join("workspace/IDENTITY.md"), "identity\n");
        write_text(
            &state.join("unknown/node_modules/payload"),
            "runtime-before\n",
        );
        write_text(&state.join("unknown/regular.sock"), "ordinary file\n");
        write_text(&state.join("credentials/synthetic"), "fixture-only\n");
        write_text(
            &state.join("agents/main/sessions/synthetic.jsonl"),
            "session-before\n",
        );
        write_text(
            &state.parent().unwrap().join("unlisted-project/code"),
            "unlisted-before\n",
        );
        fs::set_permissions(
            state.join("credentials/synthetic"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let db = Connection::open(state.join("arbitrary.data")).unwrap();
        db.execute_batch(
            "CREATE TABLE durable(value TEXT); INSERT INTO durable VALUES ('before');",
        )
        .unwrap();
        drop(db);
        let fixture = Self {
            root,
            env,
            state,
            project,
        };
        let result = fixture.run(&["env", "set-independent-paths", "demo", independent]);
        assert!(result.status.success(), "{}", stderr(&result));
        fixture
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        run_ocm(self.root.path(), &self.env, args)
    }

    fn edit_project(&self) -> fs::Metadata {
        write_text(&self.project.join("code"), "current developer work\n");
        fs::set_permissions(self.project.join("code"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(self.project.join("deleted")).unwrap();
        write_text(&self.project.join("created"), "new work\n");
        fs::remove_file(self.project.join("link")).unwrap();
        symlink("created", self.project.join("link")).unwrap();
        fs::metadata(self.project.join("code")).unwrap()
    }

    fn assert_project(&self, expected: &fs::Metadata) {
        assert_eq!(
            fs::read_to_string(self.project.join("code")).unwrap(),
            "current developer work\n"
        );
        let current = fs::metadata(self.project.join("code")).unwrap();
        assert_eq!(
            (
                current.ino(),
                current.mode(),
                current.mtime(),
                current.mtime_nsec()
            ),
            (
                expected.ino(),
                expected.mode(),
                expected.mtime(),
                expected.mtime_nsec()
            )
        );
        assert!(!self.project.join("deleted").exists());
        assert_eq!(
            fs::read_to_string(self.project.join("created")).unwrap(),
            "new work\n"
        );
        assert_eq!(
            fs::read_link(self.project.join("link")).unwrap(),
            PathBuf::from("created")
        );
    }
}

#[test]
fn independent_project_survives_success_failure_interrupt_and_explicit_rollback() {
    for (independent, mode) in [
        ".openclaw/workspace/projects",
        ".openclaw/worktrees",
        "development/checkouts",
    ]
    .into_iter()
    .flat_map(|independent| ["success", "failure", "interrupt"].map(|mode| (independent, mode)))
    {
        let fixture = Fixture::with_independent_root(independent);
        let ready = fixture.root.child("ready");
        let release = fixture.root.child("release");
        let mut child = Command::new(env!("CARGO_BIN_EXE_ocm"))
            .args(["upgrade", "demo", "--runtime", "new", "--json"])
            .current_dir(fixture.root.path())
            .env_clear()
            .envs(&fixture.env)
            .env("OCM_PROOF_READY", &ready)
            .env("OCM_PROOF_RELEASE", &release)
            .env("OCM_PROOF_FAIL", if mode == "failure" { "1" } else { "0" })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() && Instant::now() < deadline && child.try_wait().unwrap().is_none() {
            sleep(Duration::from_millis(10));
        }
        if !ready.exists() {
            write_text(&release, "release");
            let output = child.wait_with_output().unwrap();
            panic!(
                "finalizer not reached: {} {}",
                stdout(&output),
                stderr(&output)
            );
        }
        let expected = fixture.edit_project();
        let db = Connection::open(fixture.state.join("arbitrary.data")).unwrap();
        db.execute_batch(
            "ALTER TABLE durable ADD COLUMN migrated TEXT; UPDATE durable SET value='after';",
        )
        .unwrap();
        drop(db);
        write_text(
            &fixture.state.join("openclaw.json"),
            "{\"migration\":\"partial\"}\n",
        );
        write_text(
            &fixture.state.join("unknown/node_modules/payload"),
            "runtime-after\n",
        );
        fs::remove_file(fixture.state.join("workspace/.openclaw/legacy-state")).unwrap();
        write_text(&fixture.state.join("migration-receipt"), "partial\n");
        write_text(
            &fixture.state.join("credentials/synthetic"),
            "credential-after\n",
        );
        write_text(
            &fixture.state.join("agents/main/sessions/synthetic.jsonl"),
            "session-after\n",
        );
        write_text(
            &fixture
                .state
                .parent()
                .unwrap()
                .join("unlisted-project/code"),
            "unlisted-after\n",
        );
        if mode == "interrupt" {
            // This child belongs exclusively to this fixture.
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        }
        write_text(&release, "release");
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.success(),
            mode == "success",
            "{} {}",
            stdout(&output),
            stderr(&output)
        );
        fixture.assert_project(&expected);
        let receipt: Value = serde_json::from_str(&stdout(&output)).unwrap();
        if mode == "success" {
            let rollback = fixture.run(&["upgrade", "rollback", "demo", "--json"]);
            assert!(
                rollback.status.success(),
                "{} {}",
                stdout(&rollback),
                stderr(&rollback)
            );
        } else {
            assert_eq!(receipt["rollback"], "restored");
        }
        fixture.assert_project(&expected);
        assert_eq!(
            fs::read_to_string(fixture.state.join("openclaw.json")).unwrap(),
            "{}\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.state.join("unknown/node_modules/payload")).unwrap(),
            "runtime-before\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.state.join("unknown/regular.sock")).unwrap(),
            "ordinary file\n"
        );
        assert_eq!(
            fs::metadata(fixture.state.join("credentials/synthetic"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            fixture
                .state
                .join("workspace/.openclaw/legacy-state")
                .exists()
        );
        assert!(!fixture.state.join("migration-receipt").exists());
        assert_eq!(
            fs::read_to_string(fixture.state.join("credentials/synthetic")).unwrap(),
            "fixture-only\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.state.join("agents/main/sessions/synthetic.jsonl")).unwrap(),
            "session-before\n"
        );
        assert_eq!(
            fs::read_to_string(
                fixture
                    .state
                    .parent()
                    .unwrap()
                    .join("unlisted-project/code")
            )
            .unwrap(),
            "unlisted-before\n"
        );
        let db = Connection::open(fixture.state.join("arbitrary.data")).unwrap();
        assert_eq!(
            db.query_row("SELECT value FROM durable", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "before"
        );
        assert!(db.prepare("SELECT migrated FROM durable").is_err());
        let snapshot = fixture.run(&[
            "env",
            "snapshot",
            "show",
            "demo",
            receipt["snapshotId"].as_str().unwrap(),
            "--json",
        ]);
        assert!(snapshot.status.success(), "{}", stderr(&snapshot));
        let snapshot: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
        assert!(
            !PathBuf::from(snapshot["archivePath"].as_str().unwrap())
                .join(independent)
                .exists()
        );
    }
}

#[test]
fn configured_workspace_policy_ignores_unrelated_dangling_home_alias() {
    for redeclare in [false, true] {
        let mut fixture = Fixture::with_independent_root("development/projects");
        let root = fixture.state.parent().unwrap();
        let config = serde_json::json!({
            "agents": {"defaults": {"workspace": root.join("development")}}
        });
        write_text(&fixture.state.join("openclaw.json"), &config.to_string());
        symlink("missing-tool-target", root.join(".old-tool")).unwrap();
        if redeclare {
            let result = fixture.run(&[
                "env",
                "set-independent-paths",
                "demo",
                "development/projects",
            ]);
            assert!(result.status.success(), "{}", stderr(&result));
        }
        write_text(&fixture.root.child("release"), "continue");
        fixture.env.insert(
            "OCM_PROOF_READY".into(),
            fixture.root.child("ready").to_str().unwrap().into(),
        );
        fixture.env.insert(
            "OCM_PROOF_RELEASE".into(),
            fixture.root.child("release").to_str().unwrap().into(),
        );
        let upgrade = fixture.run(&["upgrade", "demo", "--runtime", "new", "--json"]);
        assert!(
            upgrade.status.success(),
            "{} {}",
            stdout(&upgrade),
            stderr(&upgrade)
        );
        let expected = fixture.edit_project();
        write_text(&fixture.state.join("credentials/synthetic"), "after\n");
        let rollback = fixture.run(&["upgrade", "rollback", "demo", "--json"]);
        assert!(
            rollback.status.success(),
            "{} {}",
            stdout(&rollback),
            stderr(&rollback)
        );
        fixture.assert_project(&expected);
        assert_eq!(
            fs::read_to_string(fixture.state.join("credentials/synthetic")).unwrap(),
            "fixture-only\n"
        );
        assert_eq!(
            fs::read_link(root.join(".old-tool")).unwrap(),
            PathBuf::from("missing-tool-target")
        );
    }
}

#[test]
fn excluded_development_directories_are_not_opened_during_upgrade() {
    for independent in [".openclaw/worktrees", "development/checkouts"] {
        let fixture = Fixture::with_independent_root(independent);
        let boundary = fixture.state.parent().unwrap().join(independent);
        let permissions = fs::metadata(&boundary).unwrap().permissions();
        fs::set_permissions(&boundary, fs::Permissions::from_mode(0)).unwrap();
        write_text(&fixture.root.child("release"), "continue");
        let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
            .args(["upgrade", "demo", "--runtime", "new", "--json"])
            .current_dir(fixture.root.path())
            .env_clear()
            .envs(&fixture.env)
            .env("OCM_PROOF_READY", fixture.root.child("ready"))
            .env("OCM_PROOF_RELEASE", fixture.root.child("release"))
            .output()
            .unwrap();
        // Restore access before assertions so failed tests remain cleanable.
        fs::set_permissions(&boundary, permissions).unwrap();
        assert!(
            output.status.success(),
            "{independent}: {} {}",
            stdout(&output),
            stderr(&output)
        );
    }
}

#[test]
fn hidden_home_aliases_do_not_make_runtime_state_independent() {
    let fixture = Fixture::new();
    let root = fixture.state.parent().unwrap();
    fs::rename(&fixture.state, root.join("runtime-state")).unwrap();
    symlink("runtime-state", &fixture.state).unwrap();
    fs::create_dir_all(root.join("tool-data/credentials")).unwrap();
    symlink("tool-data", root.join(".codex")).unwrap();
    for forbidden in [
        "runtime-state/credentials",
        "runtime-state/agents",
        "tool-data",
        "tool-data/credentials",
    ] {
        let result = fixture.run(&["env", "set-independent-paths", "demo", forbidden]);
        assert!(
            !result.status.success(),
            "hidden home alias accepted: {forbidden}"
        );
    }
    let workspace_child = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        "runtime-state/workspace/projects",
    ]);
    assert!(
        workspace_child.status.success(),
        "existing workspace alias refused: {}",
        stderr(&workspace_child)
    );
    fs::create_dir_all(root.join("unrelated-project")).unwrap();
    let unrelated = fixture.run(&["env", "set-independent-paths", "demo", "unrelated-project"]);
    assert!(
        unrelated.status.success(),
        "unrelated home project refused: {}",
        stderr(&unrelated)
    );

    let fixture = Fixture::with_independent_root(".openclaw/worktrees");
    symlink(
        ".openclaw/worktrees/example",
        fixture.state.parent().unwrap().join(".codex"),
    )
    .unwrap();
    let protected = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/worktrees",
    ]);
    assert!(
        !protected.status.success(),
        "managed worktree containing a hidden tool home accepted"
    );
}

#[test]
fn development_locations_require_explicit_safe_directory_boundaries() {
    let fixture = Fixture::new();
    let root = fixture.state.parent().unwrap();
    for independent in [
        ".openclaw/worktrees",
        ".openclaw/worktrees/task",
        "clawrouter-live-test/workspace",
        "other-projects",
    ] {
        fs::create_dir_all(root.join(independent).parent().unwrap()).unwrap();
        // The boundary itself may be absent, as with workspace declarations.
        let result = fixture.run(&["env", "set-independent-paths", "demo", independent]);
        assert!(
            result.status.success(),
            "{independent}: {}",
            stderr(&result)
        );
    }
    let saved = fixture.run(&["env", "show", "demo", "--json"]);
    let saved: Value = serde_json::from_str(&stdout(&saved)).unwrap();
    for forbidden in [
        ".openclaw/agents/main/sessions",
        ".openclaw/credentials",
        ".openclaw/state",
        ".openclaw/logs",
        ".openclaw/unknown",
        ".openclaw/worktrees-other",
        ".codex/projects",
        ".config/tool",
        ".clawdbot/worktrees",
    ] {
        fs::create_dir_all(root.join(forbidden)).unwrap();
        let result = fixture.run(&["env", "set-independent-paths", "demo", forbidden]);
        assert!(
            !result.status.success(),
            "protected namespace accepted: {forbidden}"
        );
        let current = fixture.run(&["env", "show", "demo", "--json"]);
        let current: Value = serde_json::from_str(&stdout(&current)).unwrap();
        assert_eq!(
            current["upgradeIndependentPaths"],
            saved["upgradeIndependentPaths"]
        );
    }
    for independent in [".openclaw/worktrees", "development/checkouts"] {
        fs::create_dir_all(root.join(independent)).unwrap();
        write_text(&root.join(independent).join("file"), "not a directory");
        symlink("file", root.join(independent).join("alias")).unwrap();
        for suffix in ["file", "alias", "alias/child", "../escape"] {
            let path = format!("{independent}/{suffix}");
            let result = fixture.run(&["env", "set-independent-paths", "demo", &path]);
            assert!(!result.status.success(), "unsafe boundary accepted: {path}");
        }
        let child = format!("{independent}/child");
        let overlap = fixture.run(&["env", "set-independent-paths", "demo", independent, &child]);
        assert!(!overlap.status.success(), "overlapping boundaries accepted");
        // A configured whole workspace is protected even at a new location.
        write_text(
            &fixture.state.join("openclaw.json"),
            &serde_json::json!({"agents":{"defaults":{"workspace":root.join(independent)}}})
                .to_string(),
        );
        let workspace = fixture.run(&["env", "set-independent-paths", "demo", independent]);
        assert!(
            !workspace.status.success(),
            "whole workspace accepted: {independent}"
        );
        write_text(&fixture.state.join("openclaw.json"), "{}");
    }
    write_text(&fixture.state.join("worktrees/include.json"), "{}");
    write_text(
        &fixture.state.join("openclaw.json"),
        r#"{"$include":"worktrees/include.json"}"#,
    );
    let include = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/worktrees",
    ]);
    assert!(!include.status.success(), "included configuration excluded");
}

#[test]
fn scope_refuses_configuration_workspace_and_path_escape_exclusions() {
    let fixture = Fixture::new();
    write_text(&fixture.state.join("workspace/projects/include.json"), "{}");
    write_text(
        &fixture.state.join("openclaw.json"),
        "{\"$include\":\"workspace/projects/include.json\"}",
    );
    for path in [
        ".openclaw/workspace",
        ".openclaw",
        "../escape",
        "/absolute",
        ".openclaw/credentials",
        ".openclaw/workspace/IDENTITY.md",
        ".openclaw/workspace/projects",
    ] {
        let result = fixture.run(&["env", "set-independent-paths", "demo", path]);
        assert!(
            !result.status.success(),
            "unsafe exclusion accepted: {path}"
        );
    }
    write_text(&fixture.state.join("openclaw.json"), "{}");
    symlink("projects", fixture.state.join("workspace/alias")).unwrap();
    let result = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/workspace/alias/example",
    ]);
    assert!(!result.status.success(), "symlink ancestor accepted");
    fs::remove_file(fixture.state.join("openclaw.json")).unwrap();
    symlink(
        "workspace/projects/include.json",
        fixture.state.join("openclaw.json"),
    )
    .unwrap();
    let result = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/workspace/projects",
    ]);
    assert!(
        !result.status.success(),
        "symlinked configuration target excluded"
    );
}

#[test]
fn configured_workspace_alias_cannot_hide_a_whole_workspace_exclusion() {
    let fixture = Fixture::new();
    let alias = fixture.state.join("workspace/workspace-alias");
    symlink("projects", &alias).unwrap();
    for workspace in [alias.clone(), alias.join("future-agent")] {
        fs::write(
            fixture.state.join("openclaw.json"),
            serde_json::to_vec(&serde_json::json!({
                "agents": {"defaults": {"workspace": workspace}}
            }))
            .unwrap(),
        )
        .unwrap();
        let result = fixture.run(&[
            "env",
            "set-independent-paths",
            "demo",
            ".openclaw/workspace/projects",
        ]);
        assert!(
            !result.status.success(),
            "configured workspace target excluded: {}",
            workspace.display()
        );
    }
    fs::write(
        fixture.state.join("openclaw.json"),
        serde_json::to_vec(&serde_json::json!({
            "agents": {"defaults": {"workspace": alias}}
        }))
        .unwrap(),
    )
    .unwrap();
    let nested = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/workspace/projects/example",
    ]);
    assert!(
        nested.status.success(),
        "independent child beneath workspace alias refused: {}",
        stderr(&nested)
    );
    fs::remove_file(&alias).unwrap();
    symlink("missing-projects", &alias).unwrap();
    let dangling = fixture.run(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/workspace/missing-projects",
    ]);
    assert!(
        !dangling.status.success(),
        "unresolved workspace alias accepted"
    );
    let upgrade = fixture.run(&["upgrade", "demo", "--runtime", "new", "--json"]);
    assert!(
        !upgrade.status.success(),
        "changed unresolved scope reached upgrade"
    );
    let env = fixture.run(&["env", "show", "demo", "--json"]);
    let env: Value = serde_json::from_str(&stdout(&env)).unwrap();
    assert_eq!(env["defaultRuntime"], "old");
}

#[test]
fn frozen_scope_and_invalid_metadata_never_fall_back_to_whole_root_restore() {
    for independent in [
        ".openclaw/workspace/projects",
        ".openclaw/worktrees",
        "development/checkouts",
    ] {
        frozen_scope_preserves_independent_root(independent);
    }
}

fn frozen_scope_preserves_independent_root(independent: &str) {
    let fixture = Fixture::with_independent_root(independent);
    let ready = fixture.root.child("ready");
    let release = fixture.root.child("release");
    write_text(&release, "continue");
    let output = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .args(["upgrade", "demo", "--runtime", "new", "--json"])
        .current_dir(fixture.root.path())
        .env_clear()
        .envs(&fixture.env)
        .env("OCM_PROOF_READY", ready)
        .env("OCM_PROOF_RELEASE", release)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let receipt: Value = serde_json::from_str(&stdout(&output)).unwrap();
    let id = receipt["snapshotId"].as_str().unwrap();
    let show = fixture.run(&["env", "snapshot", "show", "demo", id, "--json"]);
    let snapshot: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let archive = PathBuf::from(snapshot["archivePath"].as_str().unwrap());
    let snapshot_meta = archive.parent().unwrap().join(format!("{id}.json"));
    let original = fs::read(&snapshot_meta).unwrap();
    let expected = fixture.edit_project();
    let clear = fixture.run(&["env", "set-independent-paths", "demo", "none"]);
    assert!(clear.status.success(), "{}", stderr(&clear));
    let mut invalid: Value = serde_json::from_slice(&original).unwrap();
    invalid.as_object_mut().unwrap().remove("upgradeScope");
    fs::write(&snapshot_meta, serde_json::to_vec(&invalid).unwrap()).unwrap();
    let restore = fixture.run(&["env", "snapshot", "restore", "demo", id]);
    assert!(!restore.status.success(), "missing scope was accepted");
    fixture.assert_project(&expected);
    fs::write(&snapshot_meta, original).unwrap();
    // An artifact with an undeclared captured copy must also be refused.
    write_text(&archive.join(independent).join("injected"), "unsafe");
    let restore = fixture.run(&["env", "snapshot", "restore", "demo", id]);
    assert!(
        !restore.status.success(),
        "conflicting artifact was accepted"
    );
    fixture.assert_project(&expected);
    fs::remove_dir_all(archive.join(independent)).unwrap();
    let restore = fixture.run(&["env", "snapshot", "restore", "demo", id]);
    assert!(restore.status.success(), "{}", stderr(&restore));
    fixture.assert_project(&expected);
}

#[test]
fn full_backup_still_rewinds_declared_independent_content() {
    for independent in [
        ".openclaw/workspace/projects",
        ".openclaw/worktrees",
        "development/checkouts",
    ] {
        full_backup_rewinds_independent_root(independent);
    }
}

fn full_backup_rewinds_independent_root(independent: &str) {
    let fixture = Fixture::with_independent_root(independent);
    let snapshot = fixture.run(&["env", "snapshot", "create", "demo", "--json"]);
    assert!(snapshot.status.success(), "{}", stderr(&snapshot));
    let snapshot: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    assert!(snapshot.get("upgradeScope").is_none());
    fixture.edit_project();
    let restore = fixture.run(&[
        "env",
        "snapshot",
        "restore",
        "demo",
        snapshot["id"].as_str().unwrap(),
    ]);
    assert!(restore.status.success(), "{}", stderr(&restore));
    assert_eq!(
        fs::read_to_string(fixture.project.join("code")).unwrap(),
        "before\n"
    );
    assert!(fixture.project.join("deleted").exists());
    assert!(!fixture.project.join("created").exists());
}

fn source_test_git(path: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    output.stdout
}

impl Fixture {
    fn with_dev_repo() -> Self {
        let mut fixture = Self::new();
        support::install_fake_service_manager(&fixture.root, &mut fixture.env);
        for (path, contents) in [
            (
                "package.json",
                r#"{"name":"openclaw","version":"2026.4.19"}"#,
            ),
            ("scripts/run-node.mjs", "// fixture\n"),
            ("openclaw.mjs", "// fixture\n"),
            ("extensions/codex/openclaw.plugin.json", r#"{"id":"codex"}"#),
            (".gitignore", "node_modules/\n.env\n"),
        ] {
            write_text(&fixture.project.join(path), contents);
        }
        source_test_git(&fixture.project, &["init", "--quiet"]);
        source_test_git(&fixture.project, &["add", "."]);
        for (key, value) in [
            ("user.name", "OCM Tests"),
            ("user.email", "tests@example.com"),
        ] {
            source_test_git(&fixture.project, &["config", key, value]);
        }
        source_test_git(&fixture.project, &["commit", "--quiet", "-m", "fixture"]);
        let bin = fixture.root.child("fake-dev-bin");
        for name in ["pnpm", "node"] {
            write_executable_script(&bin.join(name), "#!/bin/sh\nexit 0\n");
        }
        let path = fixture.env.get("PATH").cloned().unwrap_or_default();
        fixture
            .env
            .insert("PATH".to_string(), format!("{}:{path}", bin.display()));
        for (name, path) in [
            ("OCM_PROOF_READY", "ready"),
            ("OCM_PROOF_RELEASE", "release"),
        ] {
            fixture.env.insert(
                name.to_string(),
                fixture.root.child(path).display().to_string(),
            );
        }
        write_text(&fixture.root.child("release"), "continue");
        fixture
    }

    fn run_ok(&self, args: &[&str]) -> std::process::Output {
        let output = self.run(args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        output
    }

    fn source_scope(&self, include_git: bool) {
        let mut args = vec![
            "env",
            "set-independent-paths",
            "demo",
            ".openclaw/workspace/projects/example/.worktrees",
        ];
        if include_git {
            args.push(".openclaw/workspace/projects/example/.git");
        }
        self.run_ok(&args);
    }

    fn register_child(&self, borrowed: bool) {
        let source = self.project.join(".worktrees/child");
        let repo = if borrowed {
            source_test_git(
                &self.project,
                &["worktree", "add", "--detach", ".worktrees/child"],
            );
            &source
        } else {
            create_owned_dev_env(&self.project, "child", &self.env, self.root.path());
            &self.project
        };
        self.run_ok(&dev_plain(&["child", "--repo", repo.to_str().unwrap()]));
        write_text(
            &self.project.join(".worktrees/child/authored"),
            "current child work\n",
        );
        let child = ocm::store::get_environment("child", &self.env, self.root.path()).unwrap();
        assert!(!PathBuf::from(child.root).starts_with(self.state.parent().unwrap()));
    }

    fn child_witness(&self) -> (Value, Vec<u8>, Vec<u8>, Vec<u8>) {
        let source = self.project.join(".worktrees/child");
        let identity = source_test_git(
            &source,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
                "--show-toplevel",
                "HEAD",
            ],
        );
        let private = PathBuf::from(String::from_utf8_lossy(&identity).lines().next().unwrap());
        let child = ocm::store::get_environment("child", &self.env, self.root.path()).unwrap();
        (
            serde_json::to_value(child).unwrap(),
            fs::read(source.join("authored")).unwrap(),
            identity,
            fs::read(private.join("gitdir")).unwrap(),
        )
    }
}

#[test]
fn restore_and_explicit_rollback_preserve_registered_source_and_git_identity() {
    // capture_git is irrelevant for the separately requested full snapshot.
    for (action, capture_git, current_git, allowed, borrowed) in [
        ("full", false, true, false, false),
        ("restore", false, true, false, false),
        ("restore", true, false, true, false),
        ("rollback", false, true, false, false),
        ("rollback", true, false, false, false),
        ("rollback", true, true, true, false),
        ("full", false, true, false, true),
        ("restore", true, false, true, true),
    ] {
        let fixture = Fixture::with_dev_repo();
        fixture.source_scope(capture_git);
        let capture = if action == "full" {
            fixture.run_ok(&["env", "snapshot", "create", "demo", "--json"])
        } else {
            fixture.run_ok(&["upgrade", "demo", "--runtime", "new", "--json"])
        };
        let capture: Value = serde_json::from_str(&stdout(&capture)).unwrap();
        let id = capture[if action == "full" { "id" } else { "snapshotId" }]
            .as_str()
            .unwrap();
        fixture.register_child(borrowed);
        fixture.source_scope(current_git);
        if action == "full" {
            let mut meta =
                ocm::store::get_environment("demo", &fixture.env, fixture.root.path()).unwrap();
            meta.service_enabled = true;
            meta.service_running = true;
            ocm::store::save_environment(meta, &fixture.env, fixture.root.path()).unwrap();
        }
        let child_before = fixture.child_witness();
        let registry = ocm::store::env_registry_path(&fixture.env, fixture.root.path()).unwrap();
        let registry_before = fs::read(&registry).unwrap();
        let snapshots = ["env", "snapshot", "list", "demo", "--json"];
        let snapshots_before = stdout(&fixture.run_ok(&snapshots));
        let result = if action == "rollback" {
            fixture.run(&["upgrade", "rollback", "demo", "--json"])
        } else {
            fixture.run(&["env", "snapshot", "restore", "demo", id])
        };
        assert_eq!(
            result.status.success(),
            allowed,
            "{action}, borrowed={borrowed}, captured Git={capture_git}, current Git={current_git}: {} {}",
            stdout(&result),
            stderr(&result)
        );
        if !allowed {
            assert!(
                stderr(&result).contains("registered dev source")
                    && stderr(&result).contains("child"),
                "{}",
                stderr(&result)
            );
            if action != "rollback" {
                let direct = ocm::store::restore_env_snapshot(
                    ocm::env::RestoreEnvSnapshotOptions {
                        env_name: "demo".to_string(),
                        snapshot_id: id.to_string(),
                    },
                    &fixture.env,
                    fixture.root.path(),
                )
                .unwrap_err();
                assert!(
                    direct.contains("registered dev source") && direct.contains("child"),
                    "{direct}"
                );
            }
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert_eq!(stdout(&fixture.run_ok(&snapshots)), snapshots_before);
        }
        assert_eq!(fixture.child_witness(), child_before);
        fixture.run_ok(&dev_plain(&["child"]));
    }
}

#[test]
fn restore_keeps_a_missing_borrowed_checkout_reserved() {
    let fixture = Fixture::with_dev_repo();
    let source = fixture.project.join(".worktrees/child");
    write_text(&source.join("captured-state"), "old environment content\n");
    fixture.run_ok(&[
        "env",
        "set-independent-paths",
        "demo",
        ".openclaw/workspace/projects/example/.worktrees/other",
        ".openclaw/workspace/projects/example/.git",
    ]);
    let capture = fixture.run_ok(&["upgrade", "demo", "--runtime", "new", "--json"]);
    let capture: Value = serde_json::from_str(&stdout(&capture)).unwrap();
    let id = capture["snapshotId"].as_str().unwrap();
    fs::remove_dir_all(&source).unwrap();
    fixture.register_child(true);
    let child_before = fixture.child_witness();
    let retained = fixture.root.child("retained-child");
    fs::rename(&source, &retained).unwrap();
    let registry = ocm::store::env_registry_path(&fixture.env, fixture.root.path()).unwrap();
    let registry_before = fs::read(&registry).unwrap();
    let snapshots = ["env", "snapshot", "list", "demo", "--json"];
    let snapshots_before = stdout(&fixture.run_ok(&snapshots));
    let restored = fixture.run(&["env", "snapshot", "restore", "demo", id]);
    assert!(!restored.status.success());
    assert!(
        stderr(&restored).contains("registered dev source"),
        "{}",
        stderr(&restored)
    );
    assert!(
        !source.exists(),
        "restore recreated the missing borrowed checkout"
    );
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
    assert_eq!(stdout(&fixture.run_ok(&snapshots)), snapshots_before);
    fs::rename(&retained, &source).unwrap();
    assert_eq!(fixture.child_witness(), child_before);
}

#[test]
fn restore_protects_its_own_borrowed_git_metadata() {
    let fixture = Fixture::with_dev_repo();
    fixture.register_child(true);
    let source = fixture.project.join(".worktrees/child");
    let capture = fixture.run_ok(&["env", "snapshot", "create", "child", "--json"]);
    let capture: Value = serde_json::from_str(&stdout(&capture)).unwrap();
    let args = [
        "env",
        "snapshot",
        "restore",
        "child",
        capture["id"].as_str().unwrap(),
    ];
    let child = ocm::store::get_environment("child", &fixture.env, fixture.root.path()).unwrap();
    fixture.run_ok(&args);
    assert_eq!(
        ocm::store::get_environment("child", &fixture.env, fixture.root.path())
            .unwrap()
            .dev,
        child.dev
    );
    let identity = source_test_git(
        &source,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    );
    let identity = String::from_utf8(identity).unwrap();
    let mut identity = identity.lines();
    let private = PathBuf::from(identity.next().unwrap());
    let common = identity.next().unwrap();
    let moved = PathBuf::from(child.root).join(".openclaw/workspace/projects/git-private");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(&private, &moved).unwrap();
    fs::write(moved.join("commondir"), format!("{common}\n")).unwrap();
    symlink(&moved, &private).unwrap();
    let child_before = fixture.child_witness();
    let registry = ocm::store::env_registry_path(&fixture.env, fixture.root.path()).unwrap();
    let registry_before = fs::read(&registry).unwrap();
    let restored = fixture.run(&args);
    assert!(!restored.status.success());
    assert!(
        stderr(&restored).contains("registered dev source"),
        "{}",
        stderr(&restored)
    );
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
    assert_eq!(fixture.child_witness(), child_before);
}

#[test]
fn upgrade_requires_safe_rollback_but_respects_no_rollback() {
    let mut fixture = Fixture::with_dev_repo();
    fixture
        .env
        .insert("OCM_PROOF_FAIL".to_string(), "1".to_string());
    fixture.register_child(false);
    fixture.source_scope(false);
    let child_before = fixture.child_witness();
    let registry = ocm::store::env_registry_path(&fixture.env, fixture.root.path()).unwrap();
    let registry_before = fs::read(&registry).unwrap();
    let snapshots = ["env", "snapshot", "list", "demo", "--json"];
    let snapshots_before = stdout(&fixture.run_ok(&snapshots));
    let refused = fixture.run(&["upgrade", "demo", "--runtime", "new", "--json"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("child"), "{}", stderr(&refused));
    assert!(
        !fixture.root.child("ready").exists(),
        "unsafe upgrade reached finalizer"
    );
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
    assert_eq!(stdout(&fixture.run_ok(&snapshots)), snapshots_before);
    assert_eq!(fixture.child_witness(), child_before);

    let failed = fixture.run(&[
        "upgrade",
        "demo",
        "--runtime",
        "new",
        "--no-rollback",
        "--json",
    ]);
    assert!(!failed.status.success());
    let receipt: Value = serde_json::from_str(&stdout(&failed)).unwrap();
    assert_eq!(receipt["outcome"], "failed", "{receipt}");
    assert_eq!(receipt["rollback"], "disabled");
    assert!(
        receipt["note"]
            .as_str()
            .is_some_and(|note| note.contains("openclaw update finalize failed")),
        "{receipt}"
    );
    assert!(
        fixture.root.child("ready").exists(),
        "no-rollback did not reach finalizer"
    );
    let current = ocm::store::get_environment("demo", &fixture.env, fixture.root.path()).unwrap();
    // The failed finalizer runs before the replacement binding is published.
    assert_eq!(current.default_runtime.as_deref(), Some("old"));
    assert_eq!(fixture.child_witness(), child_before);
}

fn check_snapshot_residue_preserves_source(root_link: bool, borrowed: bool) {
    let fixture = Fixture::with_dev_repo();
    fixture.register_child(borrowed);
    fixture.run_ok(&["env", "create", "linked"]);
    let source = fixture.project.join(".worktrees/child");
    let linked = fixture.root.child("ocm-home/envs/linked");
    let link = if root_link {
        linked
    } else {
        linked.join(".openclaw")
    };
    fs::remove_dir_all(&link).unwrap();
    symlink(&source, &link).unwrap();
    let residue = if root_link {
        source.join(".openclaw")
    } else {
        source
    };
    write_text(&residue.join("Cargo.lock"), "authored source lock\n");
    write_text(&residue.join("run/keep"), "authored source data\n");
    let child_before = fixture.child_witness();
    let snapshot = fixture.run_ok(&["env", "snapshot", "create", "linked", "--json"]);
    let snapshot: Value = serde_json::from_str(&stdout(&snapshot)).unwrap();
    fixture.run_ok(&[
        "env",
        "snapshot",
        "restore",
        "linked",
        snapshot["id"].as_str().unwrap(),
    ]);
    let lock_preserved = fs::read(residue.join("Cargo.lock")).ok().as_deref()
        == Some(b"authored source lock\n".as_slice());
    let run_preserved = fs::read(residue.join("run/keep")).ok().as_deref()
        == Some(b"authored source data\n".as_slice());
    assert!(
        lock_preserved && run_preserved,
        "restore followed directory link (root={root_link}): source lock preserved={lock_preserved}, run data preserved={run_preserved}"
    );
    assert_eq!(fixture.child_witness(), child_before);
}

#[test]
fn snapshot_residue_cleanup_preserves_state_directory_links() {
    check_snapshot_residue_preserves_source(false, false);
}

#[test]
fn snapshot_residue_cleanup_preserves_root_directory_links() {
    for borrowed in [false, true] {
        check_snapshot_residue_preserves_source(true, borrowed);
    }
}

#[test]
fn legacy_snapshot_restore_preserves_linked_registered_source() {
    use ocm::infra::archive::{ArchivedEnvMeta, EnvArchiveMetadata, write_env_archive};
    use ocm::store::{
        get_env_snapshot, get_environment, snapshot_archive_path, snapshot_meta_path,
    };

    let fixture = Fixture::with_dev_repo();
    fixture.register_child(false);
    fixture.run_ok(&["env", "create", "linked"]);
    let source = fixture.project.join(".worktrees/child");
    let linked = fixture.root.child("ocm-home/envs/linked");
    let state_link = linked.join(".openclaw");
    fs::remove_dir_all(&state_link).unwrap();
    symlink(&source, &state_link).unwrap();
    let captured = fixture.run_ok(&["env", "snapshot", "create", "linked", "--json"]);
    let captured: Value = serde_json::from_str(&stdout(&captured)).unwrap();
    let id = captured["id"].as_str().unwrap();
    let cwd = fixture.root.path();
    let meta = get_environment("linked", &fixture.env, cwd).unwrap();
    let mut archived: ArchivedEnvMeta =
        serde_json::from_value(serde_json::to_value(&meta).unwrap()).unwrap();
    archived.source_root = Some(meta.root.clone());
    let archive = snapshot_archive_path("linked", id, &fixture.env, cwd).unwrap();
    // Historical snapshots retained this link. Current export validates
    // workspace containment, so use the original archive shape directly.
    write_env_archive(
        &EnvArchiveMetadata {
            kind: "ocm-env-archive".to_string(),
            format_version: 1,
            exported_at: meta.updated_at,
            env: archived,
        },
        &linked,
        &archive,
    )
    .unwrap();
    let mut snapshot = get_env_snapshot("linked", id, &fixture.env, cwd).unwrap();
    snapshot.storage_kind = "tar-archive-v1".to_string();
    snapshot.archive_path = archive.to_str().unwrap().to_string();
    fs::write(
        snapshot_meta_path("linked", id, &fixture.env, cwd).unwrap(),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let config = serde_json::to_vec(&serde_json::json!({
        "gateway": {"port": meta.gateway_port.unwrap() + 100}
    }))
    .unwrap();
    fs::write(source.join("openclaw.json"), &config).unwrap();
    write_text(&source.join("browser/keep"), "authored source data\n");
    let child_before = fixture.child_witness();
    let restored = fixture.run(&["env", "snapshot", "restore", "linked", id]);
    let config_unchanged = fs::read(source.join("openclaw.json")).ok().as_ref() == Some(&config);
    let browser_preserved = fs::read(source.join("browser/keep")).ok().as_deref()
        == Some(b"authored source data\n".as_slice());
    assert!(
        restored.status.success() && config_unchanged && browser_preserved,
        "legacy restore followed state directory link: accepted={}, config unchanged={config_unchanged}, browser preserved={browser_preserved}; {}",
        restored.status.success(),
        stderr(&restored)
    );
    assert_eq!(fs::read_link(&state_link).unwrap(), source);
    assert_eq!(fixture.child_witness(), child_before);
}
