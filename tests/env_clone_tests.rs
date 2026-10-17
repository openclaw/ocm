mod support;

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout, write_text};

fn seed_plugin_registry(path: &Path, folded: bool, records: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = Connection::open(path).unwrap();
    if folded {
        db.execute_batch("CREATE TABLE config_machine_state (state_key TEXT PRIMARY KEY, value_json TEXT NOT NULL)").unwrap();
        db.execute("INSERT INTO config_machine_state VALUES ('plugins.installedIndex', ?1)",
            [serde_json::json!({"revision": 42, "index": {"installRecords": records, "plugins": [], "diagnostics": []}}).to_string()]).unwrap();
    } else {
        db.execute_batch("CREATE TABLE installed_plugin_index (index_key TEXT PRIMARY KEY, install_records_json TEXT NOT NULL)").unwrap();
        db.execute(
            "INSERT INTO installed_plugin_index VALUES ('installed-plugin-index', ?1)",
            [records.to_string()],
        )
        .unwrap();
    }
}

fn read_plugin_registry(path: &Path, folded: bool) -> Value {
    let db = Connection::open(path).unwrap();
    let raw: String = db
        .query_row(
            if folded {
                "SELECT value_json FROM config_machine_state"
            } else {
                "SELECT install_records_json FROM installed_plugin_index"
            },
            [],
            |row| row.get(0),
        )
        .unwrap();
    let document: Value = serde_json::from_str(&raw).unwrap();
    if folded {
        assert_eq!(document["revision"], 42);
        document["index"]["installRecords"].clone()
    } else {
        document
    }
}

#[test]
fn env_clone_rebases_session_paths_and_drops_database_owners() {
    for table in ["session_nodes", "session_entries"] {
        assert_clone_rebases_session_paths_and_drops_database_owners(table);
    }
}

fn assert_clone_rebases_session_paths_and_drops_database_owners(table: &str) {
    let root = TestDir::new("clone-database-owners");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source/.openclaw/state/openclaw.sqlite");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let connection = Connection::open(&source).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
        PRAGMA wal_autocheckpoint=0;
        CREATE TABLE agent_database_leases (
            lease_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, path TEXT NOT NULL,
            owner_pid INTEGER NOT NULL, owner_start_time INTEGER, opened_at INTEGER NOT NULL
        ) STRICT;
        CREATE TABLE durable_fixture (value TEXT NOT NULL) STRICT;
        INSERT INTO durable_fixture VALUES ('keep user state');
        PRAGMA user_version = 42;",
        )
        .unwrap();
    // The test process is live throughout cloning. A missing start identity
    // must still fence source maintenance, but it cannot own the new clone.
    connection.execute(
        "INSERT INTO agent_database_leases VALUES ('live', 'fixture', '/source/agent.sqlite', ?1, NULL, 1)",
        [std::process::id()],
    ).unwrap();
    let source_root = fs::canonicalize(root.child("ocm-home/envs/source")).unwrap();
    let target_root = root.child("ocm-home/envs/target");
    connection
        .execute_batch(
            "CREATE TABLE agent_databases (agent_id TEXT PRIMARY KEY, path TEXT NOT NULL);",
        )
        .unwrap();
    let databases = [
        ".openclaw/agents/main/agent/openclaw-agent.sqlite",
        ".openclaw/private-agent/agent.sqlite",
    ];
    write_text(
        &source_root.join(".openclaw/openclaw.json"),
        &serde_json::json!({"agents":{"entries":{"main":{"agentDir":
            root.child("ocm-home/envs/source/.openclaw/private-agent")
        }}}})
        .to_string(),
    );
    let mut source_databases = Vec::new();
    for (id, relative) in databases.iter().enumerate() {
        let path = source_root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE {table} (session_key TEXT PRIMARY KEY, entry_json TEXT NOT NULL);
             CREATE TABLE preserved (value TEXT NOT NULL, payload BLOB NOT NULL);
             CREATE TABLE board_widgets (descriptor_json TEXT, sha256 TEXT, granted_sha TEXT, grant_state TEXT);",
        ))
        .unwrap();
        let workspace = if id == 0 {
            source_root.join(".openclaw/workspace")
        } else {
            root.child("ocm-home/envs/source/.openclaw/workspace")
        };
        let entry = serde_json::json!({
            "sessionId": "fixture-session",
            "sessionRoot": workspace,
            "spawnedCwd": workspace.join("nested"),
            "external": root.child("external/workspace"),
            "sibling": format!("{}-sibling/workspace", source_root.display()),
            "spawnedWorkspaceDir": workspace,
            "worktree": {"repoRoot": workspace, "canonicalWorkspaceDir": workspace, "branch": workspace},
            "opaque": {"sessionRoot": workspace},
            "nativeRuntimeConsent": {"cwd": workspace},
            "label": format!("retained {}", workspace.display())
        });
        db.execute(
            &format!("INSERT INTO {table} VALUES ('agent:main:fixture', ?1)"),
            [entry.to_string()],
        )
        .unwrap();
        let binary = workspace.to_string_lossy().as_bytes().to_vec();
        db.execute(
            "INSERT INTO preserved VALUES ('keep user state', ?1)",
            [&binary],
        )
        .unwrap();
        let widget_source = format!(
            "export default () => {:?};",
            workspace.display().to_string()
        );
        let descriptor =
            serde_json::json!({"pluginKind":"fixture", "source":widget_source}).to_string();
        let sha = Sha256::digest(widget_source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        db.execute(
            "INSERT INTO board_widgets VALUES (?1, ?2, ?2, 'granted')",
            rusqlite::params![descriptor, sha],
        )
        .unwrap();
        let precise = format!(
            r#"{{ "sessionRoot": {}, "opaque": {{"sessionRoot": {}}}, "number": 1e400 }}"#,
            serde_json::to_string(&workspace).unwrap(),
            serde_json::to_string(&workspace).unwrap()
        );
        db.execute(
            &format!("INSERT INTO {table} VALUES ('precise', ?1)"),
            [&precise],
        )
        .unwrap();
        drop(db);
        connection
            .execute(
                "INSERT INTO agent_databases VALUES (?1, ?2)",
                rusqlite::params![id.to_string(), path.to_str().unwrap()],
            )
            .unwrap();
        source_databases.push((
            path.clone(),
            fs::read(&path).unwrap(),
            entry,
            binary,
            descriptor,
            sha,
            precise,
        ));
    }
    let history = source_root.join(".openclaw/skill-workshop/history.sqlite");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    let db = Connection::open(&history).unwrap();
    db.execute_batch("CREATE TABLE history (path TEXT NOT NULL)")
        .unwrap();
    db.execute(
        "INSERT INTO history VALUES (?1)",
        [source_root.to_str().unwrap()],
    )
    .unwrap();
    drop(db);
    let history_before = fs::read(&history).unwrap();
    let before = fs::read(&source).unwrap();
    let wal_before = fs::read(source.with_extension("sqlite-wal")).unwrap();
    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(
        fs::read(source.with_extension("sqlite-wal")).unwrap(),
        wal_before
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM agent_database_leases", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    let copied =
        Connection::open(root.child("ocm-home/envs/target/.openclaw/state/openclaw.sqlite"))
            .unwrap();
    assert_eq!(
        copied
            .query_row("SELECT COUNT(*) FROM agent_database_leases", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    assert_eq!(
        copied
            .query_row("SELECT value FROM durable_fixture", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "keep user state"
    );
    assert_eq!(
        copied
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        42
    );
    for (relative, (path, before, mut entry, binary, descriptor, sha, precise)) in
        databases.iter().zip(source_databases)
    {
        assert_eq!(fs::read(&path).unwrap(), before);
        let target = target_root.join(relative);
        let db = Connection::open(&target).unwrap();
        let actual: String = db
            .query_row(
                &format!("SELECT entry_json FROM {table} WHERE session_key = 'agent:main:fixture'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        entry["sessionRoot"] = serde_json::json!(target_root.join(".openclaw/workspace"));
        entry["spawnedCwd"] = serde_json::json!(target_root.join(".openclaw/workspace/nested"));
        entry["spawnedWorkspaceDir"] = serde_json::json!(target_root.join(".openclaw/workspace"));
        for field in ["repoRoot", "canonicalWorkspaceDir"] {
            entry["worktree"][field] = serde_json::json!(target_root.join(".openclaw/workspace"));
        }
        let widget: (String, String, String, String) = db
            .query_row(
                "SELECT descriptor_json, sha256, granted_sha, grant_state FROM board_widgets",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(widget, (descriptor, sha.clone(), sha, "granted".into()));
        let original_path = serde_json::to_string(&entry["opaque"]["sessionRoot"]).unwrap();
        let expected_precise = precise.replacen(
            &original_path,
            &serde_json::to_string(&target_root.join(".openclaw/workspace")).unwrap(),
            1,
        );
        assert_eq!(
            db.query_row(
                &format!("SELECT entry_json FROM {table} WHERE session_key = 'precise'"),
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            expected_precise
        );

        assert_eq!(serde_json::from_str::<Value>(&actual).unwrap(), entry);
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            db.query_row("SELECT value, payload FROM preserved", [], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?
            )))
            .unwrap(),
            ("keep user state".into(), binary)
        );
    }
    let registered: Vec<String> = copied
        .prepare("SELECT path FROM agent_databases ORDER BY agent_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        registered,
        databases.map(|relative| target_root.join(relative).display().to_string())
    );
    assert_eq!(fs::read(&history).unwrap(), history_before);
    assert_eq!(
        fs::read(target_root.join(".openclaw/skill-workshop/history.sqlite")).unwrap(),
        history_before
    );
}

#[test]
fn env_clone_relocates_plugin_registry_without_changing_source() {
    check_clone_plugin_registry(false);
}

#[test]
fn env_clone_relocates_folded_plugin_registry_without_changing_source() {
    check_clone_plugin_registry(true);
}

fn check_clone_plugin_registry(folded: bool) {
    let root = TestDir::new("clone-plugin-registry");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source/.openclaw");
    let target = root.child("ocm-home/envs/target/.openclaw");
    let relative = "npm/projects/demo/node_modules/demo";
    write_text(&source.join(relative).join("index.js"), "original");
    let project = root.child("local-plugin-project");
    write_text(&project.join("index.js"), "project original");
    let local = "extensions/local-copy";
    let archive = "extensions/archive-copy";
    let packed = "extensions/npm-pack-copy";
    let linked = "extensions/internal-linked";
    let missing = "npm/node_modules/absent/package";
    let recovered = "extensions/copied-gateway";
    for relative in [local, archive, packed, linked, recovered] {
        write_text(&source.join(relative).join("index.js"), "payload original");
    }
    let archive_source = root.child("local-plugin.tgz");
    write_text(&archive_source, "archive provenance");
    let db_path = source.join("state/openclaw.sqlite");
    let records = serde_json::json!({"demo": {
        "source": "npm",
        "installPath": source.join(relative),
        "sourcePath": source.join(relative)
    }, "local": {
        "source":"path", "sourcePath":project, "installPath":source.join(local), "version":"1.0.0"
    }, "archive": {
        "source":"archive", "sourcePath":archive_source, "installPath":source.join(archive)
    }, "packed": {
        "source":"npm", "artifactKind":"npm-pack", "sourcePath":archive_source,
        "installPath":source.join(packed), "spec":"packed@1.0.0"
    }, "missing": {
        "source":"npm", "installPath":source.join(missing), "spec":"absent@1.0.0"
    }, "linked": {
        "source":"path", "sourcePath":source.join(linked), "installPath":source.join(linked)
    }, "recovered": {
        "source":"npm", "installPath":root.child("former-home/.openclaw").join(recovered)
    }});
    seed_plugin_registry(&db_path, folded, records);
    let before = fs::read(&db_path).unwrap();
    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));
    let copied = read_plugin_registry(&target.join("state/openclaw.sqlite"), folded);
    for field in ["installPath", "sourcePath"] {
        assert_eq!(
            copied["demo"][field],
            target.join(relative).to_str().unwrap()
        );
    }
    assert_eq!(copied["local"]["sourcePath"], project.to_str().unwrap());
    assert_eq!(
        copied["local"]["installPath"],
        target.join(local).to_str().unwrap()
    );
    assert_eq!(copied["local"]["version"], "1.0.0");
    for (id, relative) in [("archive", archive), ("packed", packed)] {
        assert_eq!(copied[id]["sourcePath"], archive_source.to_str().unwrap());
        assert_eq!(
            copied[id]["installPath"],
            target.join(relative).to_str().unwrap()
        );
    }
    assert_eq!(copied["packed"]["artifactKind"], "npm-pack");
    assert_eq!(
        fs::read_to_string(&archive_source).unwrap(),
        "archive provenance"
    );
    assert_eq!(
        copied["missing"]["installPath"],
        target.join(missing).to_str().unwrap()
    );
    assert_eq!(copied["missing"]["spec"], "absent@1.0.0");
    assert!(!source.join(missing).exists());
    assert!(!target.join(missing).exists());
    for field in ["installPath", "sourcePath"] {
        assert_eq!(
            copied["linked"][field],
            target.join(linked).to_str().unwrap()
        );
    }
    assert_eq!(
        copied["recovered"]["installPath"],
        target.join(recovered).to_str().unwrap()
    );
    write_text(&target.join(local).join("index.js"), "clone local update");
    assert_eq!(
        fs::read_to_string(source.join(local).join("index.js")).unwrap(),
        "payload original"
    );
    assert_eq!(
        fs::read_to_string(project.join("index.js")).unwrap(),
        "project original"
    );
    write_text(
        &Path::new(copied["demo"]["installPath"].as_str().unwrap()).join("index.js"),
        "updated",
    );
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert_eq!(
        fs::read_to_string(source.join(relative).join("index.js")).unwrap(),
        "original"
    );
}

#[test]
fn env_clone_rejects_unowned_plugin_paths_without_changing_source() {
    for folded in [false, true] {
        for case in [
            "external",
            "source-only",
            "outside-missing",
            "traversal",
            "target-traversal",
        ] {
            let root = TestDir::new("clone-unowned-plugin");
            let cwd = root.child("workspace");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source/.openclaw");
            let target = root.child("ocm-home/envs/target/.openclaw");
            let external = root.child("external-plugin");
            write_text(&external.join("index.js"), "external original");
            fs::create_dir_all(source.join("extensions")).unwrap();
            let (kind, path) = match case {
                "external" | "source-only" => ("path", external.clone()),
                "outside-missing" => ("npm", root.child("former-home/.openclaw/extensions/demo")),
                "traversal" => ("npm", source.join("extensions/../extensions/demo")),
                "target-traversal" => ("npm", target.join("extensions/../extensions/demo")),
                _ => unreachable!(),
            };
            let db = source.join("state/openclaw.sqlite");
            let field = if case == "source-only" {
                "sourcePath"
            } else {
                "installPath"
            };
            seed_plugin_registry(
                &db,
                folded,
                serde_json::json!({"demo":{"source":kind,(field):path}}),
            );
            let before = fs::read(&db).unwrap();
            let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
            assert!(
                !clone.status.success(),
                "case={case} folded={folded}: {}",
                stdout(&clone)
            );
            assert!(
                stderr(&clone).contains("could not be isolated"),
                "case={case}: {}",
                stderr(&clone)
            );
            assert_eq!(fs::read(&db).unwrap(), before);
            assert_eq!(
                fs::read_to_string(external.join("index.js")).unwrap(),
                "external original"
            );
            assert!(!target.parent().unwrap().exists());
            let shown = run_ocm(&cwd, &env, &["env", "show", "target"]);
            assert!(!shown.status.success());
        }
    }
}

#[cfg(unix)]
#[test]
fn env_clone_rejects_plugin_links_outside_the_copied_state() {
    use std::os::unix::fs::symlink;
    for folded in [false, true] {
        for case in ["source-link", "dangling", "missing-child"] {
            let root = TestDir::new("clone-plugin-link");
            let cwd = root.child("workspace");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source/.openclaw");
            let payload = match case {
                "source-link" => source.join("extensions/actual"),
                "dangling" => root.child("outside/absent"),
                "missing-child" => root.child("outside"),
                _ => unreachable!(),
            };
            if case != "dangling" {
                write_text(&payload.join("index.js"), "source original");
            }
            fs::create_dir_all(source.join("extensions")).unwrap();
            // Existing absolute in-source links escape after copying; dangling
            // outside links must not become trusted merely because the leaf is absent.
            symlink(&payload, source.join("extensions/link")).unwrap();
            let recorded = if case == "missing-child" {
                source.join("extensions/link/absent")
            } else {
                source.join("extensions/link")
            };
            let db = source.join("state/openclaw.sqlite");
            seed_plugin_registry(
                &db,
                folded,
                serde_json::json!({"demo":{"source":"npm","installPath":recorded}}),
            );
            let before = fs::read(&db).unwrap();
            let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
            assert!(!clone.status.success(), "case={case}: {}", stdout(&clone));
            assert!(
                stderr(&clone).contains("could not be isolated"),
                "{}",
                stderr(&clone)
            );
            assert_eq!(fs::read(&db).unwrap(), before);
            if case != "dangling" {
                assert_eq!(
                    fs::read_to_string(payload.join("index.js")).unwrap(),
                    "source original"
                );
            }
            assert!(!root.child("ocm-home/envs/target").exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn env_clone_preserves_owned_relative_plugin_links() {
    use std::os::unix::fs::symlink;
    for folded in [false, true] {
        for missing in [false, true] {
            let root = TestDir::new("clone-owned-plugin-link");
            let cwd = root.child("workspace");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source/.openclaw");
            let target = root.child("ocm-home/envs/target/.openclaw");
            write_text(
                &source.join("extensions/actual/index.js"),
                "source original",
            );
            symlink("actual", source.join("extensions/link")).unwrap();
            let relative = if missing {
                "extensions/link/absent"
            } else {
                "extensions/link"
            };
            let db = source.join("state/openclaw.sqlite");
            seed_plugin_registry(
                &db,
                folded,
                serde_json::json!({"demo":{"source":"npm","installPath":source.join(relative)}}),
            );
            let before = fs::read(&db).unwrap();
            let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
            assert!(clone.status.success(), "{}", stderr(&clone));
            let records = read_plugin_registry(&target.join("state/openclaw.sqlite"), folded);
            assert_eq!(
                records["demo"]["installPath"],
                target.join(relative).to_str().unwrap()
            );
            assert_eq!(target.join(relative).exists(), !missing);
            assert_eq!(
                fs::read_link(target.join("extensions/link")).unwrap(),
                Path::new("actual")
            );
            assert_eq!(fs::read(&db).unwrap(), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn env_clone_relocates_owned_plugin_aliases_and_config() {
    use std::os::unix::fs::symlink;
    for folded in [false, true] {
        for relative in [".openclaw/extensions/linked", "local-plugin"] {
            let root = TestDir::new("clone-owned-plugin-alias");
            let cwd = root.child("workspace");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source");
            let target = root.child("ocm-home/envs/target");
            let payload = source.join(relative);
            write_text(&payload.join("index.js"), "source original");
            let alias = root.child("alias-plugin");
            symlink(&payload, &alias).unwrap();
            let config_path = source.join(".openclaw/openclaw.json");
            let config = serde_json::json!({"plugins":{"load":{"paths":[alias]}}}).to_string();
            fs::write(&config_path, &config).unwrap();
            let db = source.join(".openclaw/state/openclaw.sqlite");
            seed_plugin_registry(
                &db,
                folded,
                serde_json::json!({"demo":{"source":"path","installPath":alias,"sourcePath":alias}}),
            );
            let before = fs::read(&db).unwrap();
            let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
            assert!(
                clone.status.success(),
                "relative={relative}: {}",
                stderr(&clone)
            );
            let records =
                read_plugin_registry(&target.join(".openclaw/state/openclaw.sqlite"), folded);
            for field in ["installPath", "sourcePath"] {
                assert_eq!(
                    records["demo"][field],
                    target.join(relative).to_str().unwrap()
                );
            }
            let copied_config: Value =
                serde_json::from_slice(&fs::read(target.join(".openclaw/openclaw.json")).unwrap())
                    .unwrap();
            assert_eq!(
                copied_config["plugins"]["load"]["paths"][0],
                target.join(relative).to_str().unwrap()
            );
            write_text(&target.join(relative).join("index.js"), "clone update");
            assert_eq!(
                fs::read_to_string(payload.join("index.js")).unwrap(),
                "source original"
            );
            assert_eq!(fs::read_to_string(&config_path).unwrap(), config);
            assert_eq!(fs::read(&db).unwrap(), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn env_clone_rejects_database_links_without_writing_source() {
    use std::os::unix::fs::symlink;
    for folded in [false, true] {
        for case in ["database", "parent", "state-root-explicit-workspace"] {
            let root = TestDir::new("clone-plugin-database-link");
            let cwd = root.child("workspace");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source");
            let state = source.join(".openclaw");
            write_text(&state.join("openclaw.json"), "{}\n");
            let external = root.child("external-state");
            let external_db = if case == "state-root-explicit-workspace" {
                external.join("state/openclaw.sqlite")
            } else {
                external.join("openclaw.sqlite")
            };
            seed_plugin_registry(
                &external_db,
                folded,
                serde_json::json!({"demo":{"source":"npm","installPath":state.join("extensions/demo")}}),
            );
            if case == "state-root-explicit-workspace" {
                let workspace = source.join("custom-workspace");
                write_text(&workspace.join("marker"), "workspace original");
                write_text(
                    &external.join("openclaw.json"),
                    &serde_json::json!({"agents":{"defaults":{"workspace":workspace}}}).to_string(),
                );
                fs::remove_dir_all(&state).unwrap();
                symlink(&external, &state).unwrap();
            } else if case == "parent" {
                symlink(&external, state.join("state")).unwrap();
            } else {
                fs::create_dir_all(state.join("state")).unwrap();
                symlink(&external_db, state.join("state/openclaw.sqlite")).unwrap();
            }
            let before = fs::read(&external_db).unwrap();
            let config_before = fs::read(state.join("openclaw.json")).unwrap();
            let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
            assert!(!clone.status.success(), "case={case}: {}", stdout(&clone));
            if case == "state-root-explicit-workspace" {
                assert!(
                    stderr(&clone).contains("expected a real directory"),
                    "{}",
                    stderr(&clone)
                );
                assert_eq!(
                    fs::read_to_string(source.join("custom-workspace/marker")).unwrap(),
                    "workspace original"
                );
            } else {
                assert!(
                    stderr(&clone).contains("database must be a regular file"),
                    "{}",
                    stderr(&clone)
                );
            }
            assert_eq!(fs::read(&external_db).unwrap(), before);
            assert_eq!(
                fs::read(state.join("openclaw.json")).unwrap(),
                config_before
            );
            assert!(!root.child("ocm-home/envs/target").exists());
        }
    }
}

#[test]
fn env_clone_copies_state_into_a_new_environment() {
    let root = TestDir::new("env-clone");
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
        "hello clone",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));
    assert!(stdout(&clone).contains("Cloned env target from source"));

    let show = run_ocm(&cwd, &env, &["env", "show", "target", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_stdout = stdout(&show);
    let show_json: Value = serde_json::from_str(&show_stdout).unwrap();
    assert!(show_stdout.contains("\"name\": \"target\""));
    let gateway_port = show_json
        .get("gatewayPort")
        .and_then(Value::as_u64)
        .unwrap();
    assert_ne!(gateway_port, 19_789);
    assert!(gateway_port >= 19_790);
    assert!(show_stdout.contains("\"protected\": true"));
    assert_eq!(show_json["serviceEnabled"], false);
    assert_eq!(show_json["serviceRunning"], false);

    assert_eq!(
        fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/workspace/notes.txt"))
            .unwrap(),
        "hello clone"
    );
}

#[test]
fn env_clone_preserves_plugin_payloads_while_clearing_live_runtime_state() {
    let root = TestDir::new("env-clone-plugin-payloads");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
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
            "extensions/path-demo/node_modules/path-dependency/index.js",
            "module.exports = 'path-dependency';\n",
        ),
        (
            "extensions/generated/openclaw.plugin.json",
            "{\"id\":\"generated\"}\n",
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
        write_text(&source_state.join(path), contents);
    }
    write_text(
        &source_state.join("agents/main/sessions/session.jsonl"),
        "{\"message\":\"do not copy\"}\n",
    );
    write_text(&source_state.join("logs/gateway.log"), "do not copy\n");
    write_text(
        &source_state.join("extensions/generated/.openclaw-runtime-deps.json"),
        "{}\n",
    );
    write_text(
        &source_state.join("extensions/generated/.openclaw-install-backups/backup.json"),
        "{}\n",
    );
    write_text(
        &source_state.join("extensions/generated/node_modules/generated/index.js"),
        "module.exports = 'generated';\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let target_state = root.child("ocm-home/envs/target/.openclaw");
    for (path, contents) in payloads {
        assert_eq!(
            fs::read_to_string(target_state.join(path)).unwrap(),
            contents,
            "{path}"
        );
    }
    assert!(!target_state.join("agents/main/sessions").exists());
    assert!(!target_state.join("logs").exists());
    for path in [
        "extensions/generated/.openclaw-runtime-deps.json",
        "extensions/generated/.openclaw-install-backups",
        "extensions/generated/node_modules",
    ] {
        assert!(!target_state.join(path).exists(), "{path}");
    }
}

#[test]
fn env_clone_rewrites_openclaw_config_for_the_new_env_root() {
    let root = TestDir::new("env-clone-config-rewrite");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_root = root.child("ocm-home/envs/source");
    write_text(
        &source_root.join(".openclaw/openclaw.json"),
        &format!(
            concat!(
                "{{\n",
                "  \"agents\": {{\n",
                "    \"defaults\": {{\n",
                "      \"workspace\": \"{}\"\n",
                "    }}\n",
                "  }},\n",
                "  \"gateway\": {{\n",
                "    \"port\": 19789\n",
                "  }}\n",
                "}}\n"
            ),
            source_root.join(".openclaw/workspace").display()
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let config_raw =
        fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap();
    let config: Value = serde_json::from_str(&config_raw).unwrap();
    let expected_workspace = root
        .child("ocm-home/envs/target/.openclaw/workspace")
        .display()
        .to_string();
    assert_eq!(
        config["agents"]["defaults"]["workspace"].as_str(),
        Some(expected_workspace.as_str())
    );
    let cloned_port = config["gateway"]["port"].as_u64().unwrap();
    assert_ne!(cloned_port, 19_789);
    assert!(cloned_port >= 19_790);
}

#[test]
fn env_clone_preserves_configured_custom_workspaces_without_prefix_lookalikes() {
    let root = TestDir::new("env-clone-custom-workspace");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_root = root.child("ocm-home/envs/source");
    let source_state = source_root.join(".openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        &format!(
            concat!(
                r#"{{"agents":{{"#,
                r#""defaults":{{"memorySearch":{{"$include":"./config/memory.json5"}}}},"#,
                r#""list":[{{"id":"main","default":true}},{{"id":"ops","workspace":"{}"}}]"#,
                "}}}}"
            ),
            source_state.join("team/ops").display()
        ),
    );
    write_text(
        &source_state.join("config/memory.json5"),
        "{ enabled: false }\n",
    );
    write_text(
        &source_state.join("team/ops/notes.txt"),
        "custom workspace should survive\n",
    );
    write_text(
        &source_state.join("workspace-cache/cache.json"),
        "unconfigured lookalike\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let target_state = root.child("ocm-home/envs/target/.openclaw");
    assert_eq!(
        fs::read_to_string(target_state.join("team/ops/notes.txt")).unwrap(),
        "custom workspace should survive\n"
    );
    assert!(!target_state.join("workspace-cache").exists());
    let config: Value =
        serde_json::from_str(&fs::read_to_string(target_state.join("openclaw.json")).unwrap())
            .unwrap();
    let expected_workspace = target_state.join("team/ops").display().to_string();
    assert_eq!(
        config["agents"]["list"][1]["workspace"].as_str(),
        Some(expected_workspace.as_str())
    );
    assert!(target_state.join("config/memory.json5").exists());
}

#[test]
fn env_clone_preserves_keyed_workspaces_using_javascript_property_order() {
    let root = TestDir::new("env-clone-keyed-workspace-order");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        r#"{"agents":{"entries":{"10":{},"2":{}}}}"#,
    );
    write_text(
        &source_state.join("workspace/default.txt"),
        "default workspace\n",
    );
    write_text(
        &source_state.join("workspace-10/secondary.txt"),
        "secondary workspace\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let target_state = root.child("ocm-home/envs/target/.openclaw");
    assert_eq!(
        fs::read_to_string(target_state.join("workspace/default.txt")).unwrap(),
        "default workspace\n"
    );
    assert_eq!(
        fs::read_to_string(target_state.join("workspace-10/secondary.txt")).unwrap(),
        "secondary workspace\n"
    );
}

#[test]
fn env_clone_uses_openclaw_config_env_precedence_for_workspace_selection() {
    let root = TestDir::new("env-clone-config-env-precedence");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    let source_vars_workspace = source_state.join("team/from-vars");
    let source_active_workspace = source_state.join("team/from-top-level");
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
            source_vars_workspace.display(),
            source_active_workspace.display()
        ),
    );
    write_text(
        &source_vars_workspace.join("notes.txt"),
        "inactive workspace\n",
    );
    write_text(
        &source_active_workspace.join("notes.txt"),
        "active workspace\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let target_state = root.child("ocm-home/envs/target/.openclaw");
    assert_eq!(
        fs::read_to_string(target_state.join("team/from-top-level/notes.txt")).unwrap(),
        "active workspace\n"
    );
    assert!(!target_state.join("team/from-vars").exists());
    assert!(source_vars_workspace.exists());
    assert!(source_active_workspace.exists());
}

#[test]
fn env_clone_rewrites_identity_bound_workspaces_to_the_copied_data_path() {
    let root = TestDir::new("env-clone-runtime-identity");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
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
    write_text(
        &source_state.join("team/source-19789/notes.txt"),
        "runtime workspace\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let target_state = root.child("ocm-home/envs/target/.openclaw");
    let copied_workspace = target_state.join("team/source-19789");
    assert_eq!(
        fs::read_to_string(copied_workspace.join("notes.txt")).unwrap(),
        "runtime workspace\n"
    );
    let config: Value =
        serde_json::from_str(&fs::read_to_string(target_state.join("openclaw.json")).unwrap())
            .unwrap();
    let configured_workspace = config["agents"]["defaults"]["workspace"].as_str().unwrap();
    assert_eq!(Path::new(configured_workspace), copied_workspace);
}

#[test]
fn env_clone_rejects_external_workspaces_before_creating_the_target() {
    let root = TestDir::new("env-clone-external-workspace");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let external = root.child("external-workspace");
    write_text(
        &external.join("notes.txt"),
        "must not be silently omitted\n",
    );
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        &format!(
            r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
            external.display()
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert_eq!(clone.status.code(), Some(1));
    assert!(
        stderr(&clone).contains("outside the environment root"),
        "{}",
        stderr(&clone)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
    assert_eq!(
        fs::read_to_string(external.join("notes.txt")).unwrap(),
        "must not be silently omitted\n"
    );
}

#[test]
fn env_clone_rewrites_coupled_sandbox_port_and_clears_public_origin() {
    let root = TestDir::new("env-clone-mcp-app-sandbox-rewrite");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"gateway\": { \"port\": 19789 },\n",
            "  \"mcp\": {\n",
            "    \"apps\": {\n",
            "      \"enabled\": true,\n",
            "      \"sandboxPort\": 19790,\n",
            "      \"sandboxOrigin\": \"https://node.example.test:19790\"\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let config_raw =
        fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap();
    let config: Value = serde_json::from_str(&config_raw).unwrap();
    let cloned_gateway_port = config["gateway"]["port"].as_u64().unwrap();
    let expected_sandbox_port = cloned_gateway_port + 1;
    assert_eq!(
        config["mcp"]["apps"]["sandboxPort"].as_u64(),
        Some(expected_sandbox_port)
    );
    assert!(config["mcp"]["apps"]["sandboxOrigin"].is_null());
    let warning = stderr(&clone);
    assert!(
        warning.contains("removed copied MCP app sandbox origin from env target"),
        "{warning}"
    );
    assert!(!warning.contains("node.example.test"), "{warning}");
    assert!(
        warning.contains(&format!("sandbox port {expected_sandbox_port}")),
        "{warning}"
    );

    let source_config_raw =
        fs::read_to_string(root.child("ocm-home/envs/source/.openclaw/openclaw.json")).unwrap();
    let source_config: Value = serde_json::from_str(&source_config_raw).unwrap();
    assert_eq!(
        source_config["mcp"]["apps"]["sandboxOrigin"].as_str(),
        Some("https://node.example.test:19790")
    );
}

#[test]
fn env_clone_reports_a_preserved_custom_sandbox_listener_port_without_echoing_the_origin() {
    let root = TestDir::new("env-clone-custom-mcp-app-sandbox-port");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"gateway\": { \"port\": 19789 },\n",
            "  \"mcp\": { \"apps\": {\n",
            "    \"sandboxPort\": 25000,\n",
            "    \"sandboxOrigin\": \"https://user:secret@source.example.test/apps?token=private\"\n",
            "  }}\n",
            "}\n"
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));
    let warning = stderr(&clone);
    assert!(warning.contains("sandbox port 25000"), "{warning}");
    assert!(!warning.contains("source.example.test"), "{warning}");
    assert!(!warning.contains("secret"), "{warning}");
    assert!(!warning.contains("private"), "{warning}");

    let config: Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(config["mcp"]["apps"]["sandboxPort"].as_u64(), Some(25000));
    assert!(config["mcp"]["apps"]["sandboxOrigin"].is_null());
}

#[test]
fn env_clone_accepts_an_explicit_target_sandbox_origin() {
    let root = TestDir::new("env-clone-explicit-mcp-app-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"gateway\": { \"port\": 19789 },\n",
            "  \"mcp\": {\n",
            "    \"apps\": {\n",
            "      \"enabled\": true,\n",
            "      \"sandboxPort\": 19790,\n",
            "      \"sandboxOrigin\": \"https://source.example.test:19790\"\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    );

    let clone = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "clone",
            "source",
            "target",
            "--sandbox-origin",
            "HTTPS://target.example.test:443/",
        ],
    );
    assert!(clone.status.success(), "{}", stderr(&clone));
    assert!(!stderr(&clone).contains("removed copied MCP app sandbox origin"));

    let config_raw =
        fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap();
    let config: Value = serde_json::from_str(&config_raw).unwrap();
    let cloned_gateway_port = config["gateway"]["port"].as_u64().unwrap();
    assert_eq!(
        config["mcp"]["apps"]["sandboxPort"].as_u64(),
        Some(cloned_gateway_port + 1)
    );
    assert_eq!(
        config["mcp"]["apps"]["sandboxOrigin"].as_str(),
        Some("https://target.example.test")
    );
}

#[test]
fn env_clone_rejects_include_owned_sandbox_configuration_before_copying() {
    let root = TestDir::new("env-clone-include-owned-mcp-app-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        "{\n  \"mcp\": { \"$include\": \"./mcp.json5\" }\n}\n",
    );
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/mcp.json5"),
        "{ apps: { sandboxOrigin: \"https://source.example.test\" } }\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert_eq!(clone.status.code(), Some(1));
    assert!(
        stderr(&clone).contains(
            "cannot safely reset mcp.apps.sandboxOrigin because OpenClaw config uses $include at mcp"
        ),
        "{}",
        stderr(&clone)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
}

#[test]
fn env_clone_rejects_include_owned_agent_workspaces_before_copying() {
    let root = TestDir::new("env-clone-include-owned-agent-workspace");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        "{\n  \"agents\": { \"$include\": \"./agents.json5\" }\n}\n",
    );
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/agents.json5"),
        "{ defaults: { workspace: '~/.openclaw/team/ops' } }\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert_eq!(clone.status.code(), Some(1));
    assert!(
        stderr(&clone).contains(
            "cannot safely rewrite OpenClaw agent workspaces because config uses $include at agents"
        ),
        "{}",
        stderr(&clone)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
}

#[test]
fn env_clone_rejects_an_include_owned_sandbox_origin_value() {
    let root = TestDir::new("env-clone-include-owned-sandbox-origin-value");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"mcp\": { \"apps\": {\n",
            "    \"sandboxOrigin\": { \"$include\": \"./origin.json\" }\n",
            "  }}\n",
            "}\n"
        ),
    );
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/origin.json"),
        "\"https://source.example.test\"\n",
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert_eq!(clone.status.code(), Some(1));
    assert!(
        stderr(&clone).contains(
            "cannot safely reset mcp.apps.sandboxOrigin because OpenClaw config uses $include at mcp.apps.sandboxOrigin"
        ),
        "{}",
        stderr(&clone)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
}

#[test]
fn env_clone_rejects_an_invalid_target_sandbox_origin_without_leaving_a_clone() {
    let root = TestDir::new("env-clone-invalid-mcp-app-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let clone = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "clone",
            "source",
            "target",
            "--sandbox-origin",
            "https://target.example.test/apps",
        ],
    );
    assert_eq!(clone.status.code(), Some(1));
    assert!(stderr(&clone).contains(
        "--sandbox-origin must be an HTTP(S) origin without a path, query, or credentials"
    ));
    assert!(!root.child("ocm-home/envs/target").exists());

    let list = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let listed: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["name"].as_str(), Some("source"));
}

#[test]
fn env_clone_rewrites_a_coupled_loopback_sandbox_origin() {
    let root = TestDir::new("env-clone-mcp-app-loopback-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"gateway\": { \"port\": 19789 },\n",
            "  \"mcp\": {\n",
            "    \"apps\": {\n",
            "      \"enabled\": true,\n",
            "      \"sandboxOrigin\": \"HTTP://LOCALHOST:19790/\"\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let config_raw =
        fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap();
    let config: Value = serde_json::from_str(&config_raw).unwrap();
    let cloned_gateway_port = config["gateway"]["port"].as_u64().unwrap();
    assert!(config["mcp"]["apps"]["sandboxPort"].is_null());
    assert_eq!(
        config["mcp"]["apps"]["sandboxOrigin"].as_str(),
        Some(format!("http://localhost:{}/", cloned_gateway_port + 1).as_str())
    );
}

#[test]
fn env_clone_rewrites_an_integral_number_sandbox_port_and_loopback_origin() {
    let root = TestDir::new("env-clone-mcp-app-integral-number-port");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"gateway\": { \"port\": 19789.0 },\n",
            "  \"mcp\": {\n",
            "    \"apps\": {\n",
            "      \"enabled\": true,\n",
            "      \"sandboxPort\": 19790.0,\n",
            "      \"sandboxOrigin\": \"http://localhost:19790\"\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let config_raw =
        fs::read_to_string(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap();
    let config: Value = serde_json::from_str(&config_raw).unwrap();
    let cloned_gateway_port = config["gateway"]["port"].as_u64().unwrap();
    let expected_sandbox_port = cloned_gateway_port + 1;
    assert_eq!(
        config["mcp"]["apps"]["sandboxPort"].as_u64(),
        Some(expected_sandbox_port)
    );
    assert_eq!(
        config["mcp"]["apps"]["sandboxOrigin"].as_str(),
        Some(format!("http://localhost:{expected_sandbox_port}").as_str())
    );
}

#[test]
fn env_clone_keeps_agent_auth_but_drops_live_runtime_state() {
    let root = TestDir::new("env-clone-clears-runtime-state");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_root = root.child("ocm-home/envs/source");
    let source_state = source_root.join(".openclaw");
    write_text(
        &source_state.join("workspace/notes.txt"),
        "workspace should survive",
    );
    write_text(
        &source_state.join("agents/main/agent/auth-profiles.json"),
        "{\n  \"profiles\": {\"local\": {\"provider\": \"openai-codex\"}}\n}\n",
    );
    write_text(
        &source_state.join("agents/main/agent/models.json"),
        "{\n  \"providers\": {\"openai-codex\": {\"models\": []}}\n}\n",
    );
    write_text(
        &source_state.join("agents/main/sessions/main.jsonl"),
        &format!(
            "{{\"cwd\":\"{}\"}}\n",
            source_state.join("workspace").display()
        ),
    );
    write_text(
        &source_state.join("logs/gateway.log"),
        &format!("root={}\n", source_root.display()),
    );
    write_text(
        &source_state.join("openclaw.json.bak"),
        &format!(
            "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}}}}\n",
            source_state.join("workspace").display()
        ),
    );

    let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(clone.status.success(), "{}", stderr(&clone));

    let target_root = root.child("ocm-home/envs/target");
    let target_state = target_root.join(".openclaw");
    assert_eq!(
        fs::read_to_string(target_state.join("workspace/notes.txt")).unwrap(),
        "workspace should survive"
    );
    assert!(
        target_state
            .join("agents/main/agent/auth-profiles.json")
            .exists()
    );
    assert!(target_state.join("agents/main/agent/models.json").exists());
    assert!(!target_state.join("agents/main/sessions").exists());
    assert!(!target_state.join("logs").exists());
    assert!(!target_state.join("openclaw.json.bak").exists());

    assert_no_source_root_refs(&target_state, &source_root);
}

fn assert_no_source_root_refs(root: &Path, source_root: &Path) {
    visit_files(root, &mut |path| {
        let raw = fs::read_to_string(path).unwrap_or_default();
        assert!(
            !raw.contains(&source_root.display().to_string()),
            "file still references source root: {}",
            path.display()
        );
    });
}

fn visit_files(root: &Path, on_file: &mut dyn FnMut(&Path)) {
    let entries = fs::read_dir(root).unwrap();
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        let metadata = fs::metadata(&path).unwrap();
        if metadata.is_dir() {
            visit_files(&path, on_file);
        } else if metadata.is_file() {
            on_file(&path);
        }
    }
}

#[test]
fn env_clone_rolls_back_an_unreadable_sqlite_snapshot() {
    let root = TestDir::new("clone-invalid-sqlite");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    assert!(
        run_ocm(&cwd, &env, &["env", "create", "source"])
            .status
            .success()
    );
    let database = root.child("ocm-home/envs/source/.openclaw/state/openclaw.sqlite");
    let invalid = b"SQLite format 3\0invalid database fixture";
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, invalid).unwrap();
    let output = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("SQLite"), "{}", stderr(&output));
    assert!(!root.child("ocm-home/envs/target").exists());
    let listing = run_ocm(&cwd, &env, &["env", "list", "--json"]);
    let entries: Value = serde_json::from_str(&stdout(&listing)).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert_eq!(entries[0]["name"], "source");
    assert_eq!(fs::read(&database).unwrap(), invalid);
}

#[test]
fn env_clone_retains_configured_agent_directories() {
    let root = TestDir::new("clone-configured-agent-directories");
    let cwd = root.path();
    let env = ocm_env(&root);
    for (index, relative) in [
        "custom-agent",
        "agents/main/private-agent",
        "agents/main/sessions/private-agent",
        "agents/main/agent",
        "workspace/private-agent",
        "identity/source-5",
        "extensions/demo/node_modules/private-agent",
        "workspace/.openclaw/tmp/private-agent",
    ]
    .iter()
    .enumerate()
    {
        let source_name = format!("source-{index}");
        let target_name = format!("target-{index}");
        let created = run_ocm(cwd, &env, &["env", "create", &source_name]);
        assert!(created.status.success(), "{}", stderr(&created));
        let source = root.child(&format!("ocm-home/envs/{source_name}"));
        let state = source.join(".openclaw");
        let agent_dir = if index == 5 {
            "${OPENCLAW_STATE_DIR}/identity/${OCM_ACTIVE_ENV}".to_string()
        } else {
            state.join(relative).to_str().unwrap().to_string()
        };
        let agents = if index % 2 == 0 {
            serde_json::json!({"entries":{"main":{"agentDir":agent_dir}}})
        } else {
            serde_json::json!({"list":[{"id":"main","agentDir":agent_dir}]})
        };
        write_text(
            &state.join("openclaw.json"),
            &serde_json::json!({"agents":agents}).to_string(),
        );
        write_text(
            &state.join(relative).join("models.json"),
            "{\"providers\":{}}",
        );
        write_text(&state.join("workspace/notes.txt"), "workspace");
        write_text(
            &state.join("agents/main/sessions/stale.jsonl"),
            "runtime debris",
        );
        write_text(
            &state.join("custom-agent-lookalike/stale.txt"),
            "runtime debris",
        );
        write_text(
            &state.join("extensions/demo/.openclaw-runtime-deps.json"),
            "{}",
        );
        write_text(
            &state.join("extensions/demo/node_modules/stale.txt"),
            "runtime debris",
        );
        let config_before = fs::read(state.join("openclaw.json")).unwrap();
        let cloned = run_ocm(cwd, &env, &["env", "clone", &source_name, &target_name]);
        assert!(cloned.status.success(), "{relative}: {}", stderr(&cloned));
        let target = root.child(&format!("ocm-home/envs/{target_name}/.openclaw"));
        assert_eq!(
            fs::read(target.join(relative).join("models.json")).unwrap(),
            b"{\"providers\":{}}"
        );
        assert_eq!(
            fs::read(target.join("workspace/notes.txt")).unwrap(),
            b"workspace"
        );
        assert!(!target.join("agents/main/sessions/stale.jsonl").exists());
        assert!(
            !target
                .join("extensions/demo/node_modules/stale.txt")
                .exists()
        );
        assert!(!target.join("custom-agent-lookalike").exists());
        let config: Value =
            serde_json::from_slice(&fs::read(target.join("openclaw.json")).unwrap()).unwrap();
        let configured = if index % 2 == 0 {
            &config["agents"]["entries"]["main"]["agentDir"]
        } else {
            &config["agents"]["list"][0]["agentDir"]
        };
        assert_eq!(configured, target.join(relative).to_str().unwrap());
        assert_eq!(
            fs::read(state.join("openclaw.json")).unwrap(),
            config_before
        );
        assert_eq!(
            fs::read(state.join(relative).join("models.json")).unwrap(),
            b"{\"providers\":{}}"
        );
        assert!(state.join("agents/main/sessions/stale.jsonl").exists());
    }
}

#[cfg(any(unix, windows))]
#[test]
fn env_clone_rejects_internal_workspace_links_under_root_spellings() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    let root = TestDir::new("clone-internal-workspace-link");
    let env = ocm_env(&root);
    let physical = root.child("physical");
    let alias = root.child("alias");
    fs::create_dir_all(&physical).unwrap();
    symlink_dir(&physical, &alias).unwrap();
    let source = alias.join("source");
    let created = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "create",
            "source",
            "--root",
            source.to_str().unwrap(),
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let state = source.join(".openclaw");
    write_text(&state.join("owned/notes.txt"), "workspace");
    write_text(
        &state.join("agents/main/sessions/old.jsonl"),
        "source session",
    );
    symlink_dir("owned", state.join("redirect")).unwrap();
    let canonical = fs::canonicalize(&source).unwrap();
    let mut roots = vec![
        ("accepted", source.clone()),
        ("canonical", canonical.clone()),
    ];
    #[cfg(windows)]
    {
        roots.push((
            "ordinary-physical",
            std::path::PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap()),
        ));
        roots.push((
            "verbatim-accepted",
            std::path::PathBuf::from(format!(r"\\?\{}", source.display())),
        ));
    }
    for (spelling, configured_root) in roots.drain(..) {
        for (relative, allowed) in [("owned", true), ("redirect", false)] {
            let config = serde_json::json!({"agents": {"defaults": {"workspace": configured_root.join(".openclaw").join(relative)}}}).to_string();
            write_text(&state.join("openclaw.json"), &config);
            let name = format!("{spelling}-{relative}");
            let target = root.child(&format!("ocm-home/envs/{name}"));
            let cloned = run_ocm(root.path(), &env, &["env", "clone", "source", &name]);
            if allowed {
                assert!(cloned.status.success(), "{}", stderr(&cloned));
                assert_eq!(
                    fs::read(target.join(".openclaw/owned/notes.txt")).unwrap(),
                    b"workspace"
                );
                assert!(
                    !target
                        .join(".openclaw/agents/main/sessions/old.jsonl")
                        .exists()
                );
            } else {
                assert!(!cloned.status.success());
                assert!(
                    stderr(&cloned).contains("through a symlink"),
                    "{}",
                    stderr(&cloned)
                );
                assert!(!target.exists());
                let shown = run_ocm(root.path(), &env, &["env", "show", &name]);
                assert!(!shown.status.success());
                assert!(
                    stderr(&shown).contains("does not exist"),
                    "{}",
                    stderr(&shown)
                );
            }
            assert_eq!(
                fs::read(state.join("openclaw.json")).unwrap(),
                config.as_bytes()
            );
            assert_eq!(
                fs::read(state.join("owned/notes.txt")).unwrap(),
                b"workspace"
            );
            assert_eq!(
                fs::read(state.join("agents/main/sessions/old.jsonl")).unwrap(),
                b"source session"
            );
            assert!(state.join("redirect").is_symlink());
        }
    }
}

#[test]
fn env_clone_rejects_external_configured_agent_directories_before_copying() {
    let root = TestDir::new("clone-external-agent-directory");
    let env = ocm_env(&root);
    let cwd = root.path();
    let created = run_ocm(cwd, &env, &["env", "create", "source"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let external = root.child("external-agent");
    write_text(&external.join("models.json"), "external payload");
    let state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &state.join("openclaw.json"),
        &serde_json::json!({"agents":{"entries":{"main":{"agentDir":external}}}}).to_string(),
    );
    let cloned = run_ocm(cwd, &env, &["env", "clone", "source", "target"]);
    assert!(!cloned.status.success());
    assert!(
        stderr(&cloned).contains("outside the environment root"),
        "{}",
        stderr(&cloned)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
    assert_eq!(
        fs::read(external.join("models.json")).unwrap(),
        b"external payload"
    );
    #[cfg(unix)]
    {
        let linked = state.join("linked-agent");
        std::os::unix::fs::symlink(&external, &linked).unwrap();
        write_text(
            &state.join("openclaw.json"),
            &serde_json::json!({"agents":{"list":[{"id":"main","agentDir":linked}]}}).to_string(),
        );
        let cloned = run_ocm(cwd, &env, &["env", "clone", "source", "linked-target"]);
        assert!(!cloned.status.success());
        assert!(
            stderr(&cloned).contains("outside the environment root"),
            "{}",
            stderr(&cloned)
        );
        assert!(!root.child("ocm-home/envs/linked-target").exists());
        assert_eq!(
            fs::read(external.join("models.json")).unwrap(),
            b"external payload"
        );
        assert!(
            fs::symlink_metadata(&linked)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn env_clone_rolls_back_a_database_relocation_failure() {
    let root = TestDir::new("clone-database-relocation-failure");
    let env = ocm_env(&root);
    let cwd = root.path();
    let create = run_ocm(cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source_root = fs::canonicalize(root.child("ocm-home/envs/source")).unwrap();
    let target_root = root.child("ocm-home/envs/target");
    let database = source_root.join(".openclaw/state/openclaw.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let db = Connection::open(&database).unwrap();
    db.execute_batch("CREATE TABLE agent_databases (path TEXT UNIQUE NOT NULL)")
        .unwrap();
    for path in [&source_root, &target_root] {
        db.execute(
            "INSERT INTO agent_databases VALUES (?1)",
            [path.to_str().unwrap()],
        )
        .unwrap();
    }
    drop(db);
    let before = fs::read(&database).unwrap();
    let clone = run_ocm(cwd, &env, &["env", "clone", "source", "target"]);
    assert!(!clone.status.success());
    assert!(
        stderr(&clone).contains("failed to relocate"),
        "{}",
        stderr(&clone)
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert!(!target_root.exists());
    let show = run_ocm(cwd, &env, &["env", "show", "target"]);
    assert!(!show.status.success());
    assert!(
        stderr(&show).contains("does not exist"),
        "{}",
        stderr(&show)
    );
}

#[cfg(unix)]
#[test]
fn env_clone_refuses_active_plugin_history_and_preserves_ordinary_aliases() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    for folded in [false, true] {
        for placement in [
            "protected-alias",
            "protected-unaliased",
            "protected-namespace-alias",
            "protected-isolated",
            "protected-empty",
            "protected-external",
            "ordinary-alias",
        ] {
            let root = TestDir::new("clone-plugin-history");
            let cwd = root.child("workspace");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source/.openclaw");
            let target = root.child("ocm-home/envs/target/.openclaw");
            let protected = placement != "ordinary-alias";
            let parent = if placement == "protected-namespace-alias" {
                "workspace/history-store/support"
            } else if protected {
                "skill-workshop/support"
            } else {
                "workspace/ordinary"
            };
            let relative = Path::new(parent).join("openclaw.sqlite");
            let database = source.join(&relative);
            let plugin_path = match placement {
                "protected-isolated" => target.join("extensions/demo"),
                "protected-external" => root.child("external-plugin"),
                _ => source.join("extensions/demo"),
            };
            let records = if placement == "protected-empty" {
                serde_json::json!({})
            } else {
                serde_json::json!({"demo": {"source": "path", "installPath": plugin_path}})
            };
            write_text(&source.join("extensions/demo/index.js"), "plugin payload");
            // No lease or session tables: only plugin relocation can change this database.
            seed_plugin_registry(&database, folded, records.clone());
            let alias = if placement == "protected-namespace-alias" {
                symlink("workspace/history-store", source.join("skill-workshop")).unwrap();
                "skill-workshop/support"
            } else {
                parent
            };
            if placement != "protected-unaliased" {
                symlink(alias, source.join("state")).unwrap();
            }
            let mode = if protected { 0o440 } else { 0o640 };
            fs::set_permissions(&database, fs::Permissions::from_mode(mode)).unwrap();
            let before = fs::read(&database).unwrap();
            let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
            assert_eq!(fs::read(&database).unwrap(), before);
            if protected && placement != "protected-unaliased" {
                assert!(!clone.status.success(), "{}", stdout(&clone));
                assert!(
                    stderr(&clone).contains("selects integrity-protected history"),
                    "{}",
                    stderr(&clone)
                );
                assert!(!target.parent().unwrap().exists());
                assert!(
                    !run_ocm(&cwd, &env, &["env", "show", "target"])
                        .status
                        .success()
                );
                assert_eq!(
                    fs::metadata(&database).unwrap().permissions().mode() & 0o777,
                    mode
                );
                continue;
            }
            assert!(
                clone.status.success(),
                "placement={placement} folded={folded}: {}",
                stderr(&clone)
            );
            let copied = target.join(&relative);
            let actual = read_plugin_registry(&copied, folded);
            if protected {
                assert_eq!(actual, records);
                assert_eq!(fs::read(&copied).unwrap(), before);
            } else {
                assert_eq!(
                    actual["demo"]["installPath"],
                    target.join("extensions/demo").to_str().unwrap()
                );
                assert_ne!(fs::read(&copied).unwrap(), before);
            }
            assert_eq!(fs::read(&database).unwrap(), before);
            for path in [&database, &copied] {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    mode
                );
            }
            if placement != "protected-unaliased" {
                assert_eq!(
                    fs::read_link(target.join("state")).unwrap(),
                    Path::new(alias)
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn env_clone_refuses_active_history_and_preserves_ordinary_lease_cleanup() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    for (directory, alias, workshop_alias, protected) in [
        ("skill-workshop/support", true, false, true),
        ("skill-workshop/support", false, false, true),
        ("workspace/history-store/support", true, true, true),
        ("workspace/active", true, false, false),
    ] {
        let root = TestDir::new("clone-historical-leases");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let state = root.child("ocm-home/envs/source/.openclaw");
        let relative = Path::new(directory).join("openclaw.sqlite");
        let source = state.join(&relative);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let db = Connection::open(&source).unwrap();
        // No plugin index or session tables: only lease cleanup can change rows.
        db.execute_batch(
            "CREATE TABLE agent_database_leases (
                 lease_id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, path TEXT NOT NULL,
                 owner_pid INTEGER NOT NULL, owner_start_time INTEGER, opened_at INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE durable_fixture (value TEXT);
             INSERT INTO durable_fixture VALUES ('preserved');",
        )
        .unwrap();
        db.execute(
            "INSERT INTO agent_database_leases VALUES ('retained-owner', 'main', ?1, ?2, NULL, 1)",
            rusqlite::params![source.to_str().unwrap(), std::process::id()],
        )
        .unwrap();
        drop(db);
        let mode = if protected { 0o440 } else { 0o600 };
        fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
        let alias_target = if workshop_alias {
            symlink("workspace/history-store", state.join("skill-workshop")).unwrap();
            "skill-workshop/support"
        } else {
            directory
        };
        if alias {
            symlink(alias_target, state.join("state")).unwrap();
        }
        let before = fs::read(&source).unwrap();
        let clone = run_ocm(&cwd, &env, &["env", "clone", "source", "target"]);
        let conflict = protected && alias;
        assert_eq!(clone.status.success(), !conflict, "{}", stderr(&clone));
        if conflict {
            assert!(
                stderr(&clone).contains("selects integrity-protected history"),
                "{}",
                stderr(&clone)
            );
            assert_eq!(fs::read(&source).unwrap(), before);
            assert_eq!(
                fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert!(!root.child("ocm-home/envs/target").exists());
            let show = run_ocm(&cwd, &env, &["env", "show", "target"]);
            assert!(!show.status.success(), "{}", stdout(&show));
            continue;
        }
        let target_state = root.child("ocm-home/envs/target/.openclaw");
        let copied = target_state.join(&relative);
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            mode
        );
        assert_eq!(
            fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
            mode
        );
        if protected {
            assert_eq!(fs::read(&copied).unwrap(), before);
        }
        if alias {
            assert_eq!(
                fs::read_link(target_state.join("state")).unwrap(),
                Path::new(alias_target)
            );
        }
        if workshop_alias {
            assert_eq!(
                fs::read_link(target_state.join("skill-workshop")).unwrap(),
                Path::new("workspace/history-store")
            );
        }
        let db = Connection::open_with_flags(&copied, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let leases: Vec<String> = db
            .prepare("SELECT lease_id FROM agent_database_leases")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            leases,
            if protected {
                vec!["retained-owner"]
            } else {
                vec![]
            }
        );
        assert_eq!(
            db.query_row("SELECT value FROM durable_fixture", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "preserved"
        );
    }
}
