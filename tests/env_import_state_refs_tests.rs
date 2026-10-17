mod support;

use std::fs;
use std::path::Path;

use ocm::env::{CreateEnvironmentOptions, ExportEnvironmentOptions, ImportEnvironmentOptions};
use ocm::store::{create_environment, export_environment, import_environment};
use rusqlite::{Connection, params};

use support::{TestDir, ocm_env};

#[cfg(unix)]
#[test]
fn state_import_admits_databases_when_no_root_relocation_is_needed() {
    use std::io::Read;
    use std::os::unix::fs::symlink;
    use support::{run_ocm, stderr, stdout};

    for source_metadata in [true, false] {
        for protected in [true, false] {
            let root = TestDir::new("import-without-root-relocation");
            let mut env = ocm_env(&root);
            let source = root.child("source");
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
            let relative = if protected {
                "skill-workshop/support/history.sqlite"
            } else {
                "state/openclaw.sqlite"
            };
            let state = source.join(".openclaw");
            let database = state.join(relative);
            fs::create_dir_all(database.parent().unwrap()).unwrap();
            let db = Connection::open(&database).unwrap();
            db.execute_batch("CREATE TABLE paths(path TEXT)").unwrap();
            db.execute(
                "INSERT INTO paths VALUES (?1)",
                [source.join("payload").display().to_string()],
            )
            .unwrap();
            drop(db);
            if protected {
                fs::create_dir(state.join("state")).unwrap();
                symlink(
                    "../skill-workshop/support/history.sqlite",
                    state.join("state/openclaw.sqlite"),
                )
                .unwrap();
            }
            let before = fs::read(&database).unwrap();
            let archive = root.child("state.tar");
            let exported = run_ocm(
                root.path(),
                &env,
                &[
                    "env",
                    "export",
                    "source",
                    "--include-state",
                    "--output",
                    archive.to_str().unwrap(),
                ],
            );
            assert!(exported.status.success(), "{}", stderr(&exported));
            let original_archive = fs::read(&archive).unwrap();
            let mut input = tar::Archive::new(original_archive.as_slice());
            let mut output = tar::Builder::new(Vec::new());
            let mut archived_database = Vec::new();
            for entry in input.entries().unwrap() {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().into_owned();
                let mut header = entry.header().clone();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                if path == Path::new("meta/env.json") && !source_metadata {
                    let mut metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    metadata["env"]
                        .as_object_mut()
                        .unwrap()
                        .remove("sourceRoot");
                    metadata["env"]
                        .as_object_mut()
                        .unwrap()
                        .remove("canonicalSourceRoot");
                    bytes = serde_json::to_vec(&metadata).unwrap();
                    header.set_size(bytes.len() as u64);
                    header.set_cksum();
                }
                if path == Path::new("root/.openclaw").join(relative) {
                    archived_database = bytes.clone();
                }
                output.append(&header, bytes.as_slice()).unwrap();
            }
            assert!(!archived_database.is_empty());
            let archive_bytes = output.into_inner().unwrap();
            fs::write(&archive, &archive_bytes).unwrap();
            fs::rename(&source, root.child("offline")).unwrap();
            env.insert(
                "OCM_HOME".into(),
                root.child("fresh-registry").display().to_string(),
            );
            let imported = run_ocm(
                root.path(),
                &env,
                &[
                    "env",
                    "import",
                    archive.to_str().unwrap(),
                    "--name",
                    "restored",
                    "--root",
                    source.to_str().unwrap(),
                    "--json",
                ],
            );
            assert_eq!(
                imported.status.success(),
                !protected,
                "{}",
                stderr(&imported)
            );
            let shown = run_ocm(root.path(), &env, &["env", "show", "restored", "--json"]);
            assert_eq!(shown.status.success(), !protected);
            if protected {
                assert!(stderr(&imported).contains("selects integrity-protected history"));
                assert!(!source.exists());
            } else {
                assert_eq!(
                    fs::read(source.join(".openclaw").join(relative)).unwrap(),
                    archived_database
                );
                let summary: serde_json::Value = serde_json::from_str(&stdout(&imported)).unwrap();
                let rewritten = summary["databasePathsRewritten"].as_object().unwrap();
                assert!(rewritten.values().all(|count| count.as_u64() == Some(0)));
                if !source_metadata {
                    assert!(rewritten.is_empty());
                }
            }
            assert_eq!(
                fs::read(root.child("offline/.openclaw").join(relative)).unwrap(),
                before
            );
            assert_eq!(fs::read(&archive).unwrap(), archive_bytes);
        }
    }
}

#[test]
fn state_import_relocates_registered_database_text_and_preserves_other_values() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let root = TestDir::new("import-state-refs");
        let env = ocm_env(&root);
        let source = create_environment(
            CreateEnvironmentOptions {
                name: "source".into(),
                root: None,
                gateway_port: None,
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            },
            &env,
            root.path(),
        )
        .unwrap();
        let source_root = Path::new(&source.root);
        let state = source_root.join(".openclaw");
        fs::create_dir_all(state.join("state")).unwrap();
        fs::create_dir_all(state.join("custom")).unwrap();
        let agent_path = state.join("custom/history.sqlite");
        let global = Connection::open(state.join("state/openclaw.sqlite")).unwrap();
        global.pragma_update(None, "encoding", encoding).unwrap();
        global.execute_batch("CREATE TABLE agent_databases (agent_id TEXT PRIMARY KEY, path TEXT);
        CREATE TABLE worktrees (id TEXT PRIMARY KEY, repo_root TEXT, path TEXT, provisioned_paths_json TEXT, created_at INTEGER);").unwrap();
        global
            .execute(
                "INSERT INTO agent_databases VALUES ('main', ?1)",
                [agent_path.to_str().unwrap()],
            )
            .unwrap();
        global
            .execute(
                "INSERT INTO worktrees VALUES ('checkout', ?1, ?2, ?3, 42)",
                params![
                    state.join("repository").to_str(),
                    state.join("worktrees/checkout").to_str(),
                    serde_json::json!([state.join("worktrees/checkout/hello.txt")]).to_string()
                ],
            )
            .unwrap();
        let outside = source_root.join("repository-data/history.sqlite");
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        let extra = Connection::open(&outside).unwrap();
        extra
            .execute_batch("CREATE TABLE paths (path TEXT)")
            .unwrap();
        extra
            .execute("INSERT INTO paths VALUES (?1)", [&source.root])
            .unwrap();
        drop(extra);
        global
            .execute(
                "INSERT INTO agent_databases VALUES ('extra', ?1)",
                [outside.to_str().unwrap()],
            )
            .unwrap();
        drop(global);
        let agent = Connection::open(&agent_path).unwrap();
        agent.pragma_update(None, "encoding", encoding).unwrap();
        agent.execute_batch("CREATE TABLE session_nodes (session_key TEXT PRIMARY KEY, entry_json TEXT, snapshot_revision INTEGER);
        CREATE TABLE transcript_events (session_id TEXT, seq INTEGER, event_json TEXT, event_zstd BLOB, created_at INTEGER, PRIMARY KEY(session_id, seq));
        CREATE TABLE session_entry_snapshots (session_key TEXT, field TEXT, value_json TEXT, PRIMARY KEY(session_key, field)) WITHOUT ROWID;
        CREATE TRIGGER snapshot_update AFTER UPDATE ON session_entry_snapshots BEGIN UPDATE session_nodes SET snapshot_revision = snapshot_revision + 1; END;
        CREATE TABLE preserved (id INTEGER PRIMARY KEY, value TEXT, payload BLOB, number REAL, absent TEXT);
        CREATE VIRTUAL TABLE search_history USING fts5(text);
        CREATE VIRTUAL TABLE compact_search USING fts5(text, content='');
        CREATE VIRTUAL TABLE external_search USING fts5(entry_json, content='session_nodes', content_rowid='rowid');").unwrap();
        let records = |base: &Path| {
            let workspace = base.join(".openclaw/worktrees/checkout");
            let entry = format!(
                r#"{{ "spawnedCwd": {}, "worktree": {{"repoRoot":{},"canonicalWorkspaceDir":{}}}, "label":"retained", "updatedAt":1e3 }}"#,
                serde_json::to_string(&workspace).unwrap(),
                serde_json::to_string(&base.join(".openclaw/repository")).unwrap(),
                serde_json::to_string(&workspace).unwrap()
            );
            let event = serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","name":"write","arguments":{"path":workspace.join("hello.txt"),"content":"hello"}}]}}).to_string();
            let snapshot = format!(
                r#"{{ "cwd": {}, "count": 1.000 }}"#,
                serde_json::to_string(&workspace).unwrap()
            );
            (entry, event, snapshot)
        };
        let (entry, event, snapshot) = records(source_root);
        agent
            .execute(
                "INSERT INTO session_nodes VALUES ('agent:main:proof', ?1, 7)",
                [&entry],
            )
            .unwrap();
        agent
            .execute(
                "INSERT INTO transcript_events VALUES ('session', 1, ?1, NULL, 42)",
                [&event],
            )
            .unwrap();
        agent.execute("INSERT INTO session_entry_snapshots VALUES ('agent:main:proof', 'systemPromptReport', ?1)", [&snapshot]).unwrap();
        let untouched = serde_json::json!({"sibling":format!("{}-other/file",source.root), "url":format!("https://example.test{}/file",source.root), "file":format!("file://other-host{}/file",source.root), "escape":"a", "number":"PRECISE_NUMBER"}).to_string().replace("\"PRECISE_NUMBER\"", "1e400");
        let binary = format!("binary {}/do-not-rewrite", source.root).into_bytes();
        agent
            .execute(
                "INSERT INTO preserved VALUES (1, ?1, ?2, 1.25, NULL)",
                params![untouched, binary],
            )
            .unwrap();
        let file_url = url::Url::from_file_path(state.join("worktrees/checkout/hello.txt"))
            .unwrap()
            .to_string();
        agent
            .execute(
                "INSERT INTO preserved VALUES (2, ?1, ?2, 2.5, NULL)",
                params![file_url, binary],
            )
            .unwrap();
        agent
            .execute("INSERT INTO search_history (text) VALUES (?1)", [&entry])
            .unwrap();
        agent
            .execute(
                "INSERT INTO external_search (external_search) VALUES ('rebuild')",
                [],
            )
            .unwrap();
        drop(agent);
        for relative in [
            "agents/other/agent/openclaw-agent.sqlite",
            "memory/main.sqlite",
            "plugins/workboard/workboard.sqlite",
            "custom/unregistered.sqlite",
        ] {
            let path = state.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let db = Connection::open(path).unwrap();
            db.execute_batch("CREATE TABLE paths (value TEXT)").unwrap();
            db.execute("INSERT INTO paths VALUES (?1)", [&source.root])
                .unwrap();
        }
        let before = fs::read(&agent_path).unwrap();
        let export = export_environment(
            ExportEnvironmentOptions {
                name: "source".into(),
                output: Some(root.child("state.tar").display().to_string()),
                include_state: true,
            },
            &env,
            root.path(),
        )
        .unwrap();
        let ordinary = export_environment(
            ExportEnvironmentOptions {
                name: "source".into(),
                output: Some(root.child("ordinary.tar").display().to_string()),
                include_state: false,
            },
            &env,
            root.path(),
        )
        .unwrap();
        fs::rename(source_root, root.child("source-offline")).unwrap();
        // A fresh registry can import below an archive's former source root.
        // Discovery must not project an already-rewritten registry path twice.
        let mut import_env = env.clone();
        import_env.insert(
            "OCM_HOME".into(),
            root.child("fresh-import").display().to_string(),
        );
        let imported = import_environment(
            ImportEnvironmentOptions {
                archive: export.archive_path,
                name: Some("copy".into()),
                root: Some(source_root.join("copy").display().to_string()),
            },
            &import_env,
            root.path(),
        )
        .unwrap();
        let target_state = Path::new(&imported.root).join(".openclaw");
        let expected = records(Path::new(&imported.root));
        let copied = Connection::open(target_state.join("custom/history.sqlite")).unwrap();
        let actual: String = copied
            .query_row("SELECT entry_json FROM session_nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(actual, expected.0);
        let actual: String = copied
            .query_row("SELECT event_json FROM transcript_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(actual, expected.1);
        let actual: String = copied
            .query_row("SELECT value_json FROM session_entry_snapshots", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(actual, expected.2);
        assert_eq!(
            copied
                .query_row("SELECT snapshot_revision FROM session_nodes", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            7
        );
        let actual: (String, Vec<u8>, f64, Option<String>) = copied
            .query_row(
                "SELECT value, payload, number, absent FROM preserved WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(actual, (untouched, binary.clone(), 1.25, None));
        let actual: (String, Vec<u8>) = copied
            .query_row(
                "SELECT value, payload FROM preserved WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            actual,
            (
                url::Url::from_file_path(target_state.join("worktrees/checkout/hello.txt"))
                    .unwrap()
                    .to_string(),
                binary
            )
        );
        let search: String = copied
            .query_row(
                "SELECT text FROM search_history WHERE search_history MATCH 'copy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(search, expected.0);
        assert_eq!(
            copied
                .query_row(
                    "SELECT COUNT(*) FROM external_search WHERE external_search MATCH 'copy'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        let global = Connection::open(target_state.join("state/openclaw.sqlite")).unwrap();
        let actual: (String, String, String, i64) = global
            .query_row(
                "SELECT repo_root, path, provisioned_paths_json, created_at FROM worktrees",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            actual,
            (
                target_state.join("repository").display().to_string(),
                target_state
                    .join("worktrees/checkout")
                    .display()
                    .to_string(),
                serde_json::json!([target_state.join("worktrees/checkout/hello.txt")]).to_string(),
                42
            )
        );
        for relative in [
            "agents/other/agent/openclaw-agent.sqlite",
            "memory/main.sqlite",
            "plugins/workboard/workboard.sqlite",
            "custom/unregistered.sqlite",
        ] {
            let db = Connection::open(target_state.join(relative)).unwrap();
            let value: String = db
                .query_row("SELECT value FROM paths", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                &value,
                if relative.contains("unregistered") {
                    &source.root
                } else {
                    &imported.root
                }
            );
        }
        let extra =
            Connection::open(Path::new(&imported.root).join("repository-data/history.sqlite"))
                .unwrap();
        let path: String = extra
            .query_row("SELECT path FROM paths", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, imported.root);
        let counts = imported
            .database_paths_rewritten
            .unwrap()
            .into_iter()
            .map(|(path, count)| (path.replace('\\', "/"), count))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(counts[".openclaw/state/openclaw.sqlite"], 5);
        assert_eq!(counts[".openclaw/custom/history.sqlite"], 6);
        assert_eq!(counts["repository-data/history.sqlite"], 1);
        assert_eq!(counts.len(), 6);
        let ordinary = import_environment(
            ImportEnvironmentOptions {
                archive: ordinary.archive_path,
                name: Some("ordinary".into()),
                root: None,
            },
            &env,
            root.path(),
        )
        .unwrap();
        assert!(ordinary.database_paths_rewritten.is_none());
        assert!(ordinary.state_paths_rewritten.is_none());
        assert!(
            !Path::new(&ordinary.root)
                .join(".openclaw/state/openclaw.sqlite")
                .exists()
        );
        assert_eq!(
            fs::read(root.child("source-offline/.openclaw/custom/history.sqlite")).unwrap(),
            before
        );
    }
}

#[cfg(unix)]
#[test]
fn state_import_preserves_history_reached_through_database_aliases() {
    use std::io::Read;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use support::{run_ocm, stderr, stdout};

    for (selection, relative, namespace_alias, needs_change) in [
        (
            "global",
            "skill-workshop/support/history.sqlite",
            false,
            true,
        ),
        (
            "agent",
            "skill-workshop/support/history.sqlite",
            false,
            true,
        ),
        (
            "registered",
            "skill-workshop/support/history.sqlite",
            false,
            true,
        ),
        ("none", "skill-workshop/support/history.sqlite", false, true),
        ("global", "logs/config-audit.jsonl", false, true),
        ("global", "custom/history.sqlite", false, true),
        (
            "global",
            "skill-workshop/support/history.sqlite",
            true,
            true,
        ),
        ("global", "logs/config-audit.jsonl", true, true),
        ("global", "audit/system-agent.jsonl", true, true),
        (
            "global",
            "skill-workshop/support/history.sqlite",
            false,
            false,
        ),
        (
            "registered-direct",
            "skill-workshop/support/history.sqlite",
            false,
            false,
        ),
    ] {
        let root = TestDir::new("import-protected-database-alias");
        let env = ocm_env(&root);
        let run = |args: &[&str]| {
            let output = run_ocm(root.path(), &env, args);
            assert!(output.status.success(), "{}", stderr(&output));
            serde_json::from_str::<serde_json::Value>(&stdout(&output)).unwrap()
        };
        let source = root.child("source");
        run(&[
            "env",
            "create",
            "source",
            "--root",
            source.to_str().unwrap(),
            "--json",
        ]);
        let state = source.join(".openclaw");
        let (namespace, suffix) = relative.split_once('/').unwrap();
        if namespace_alias {
            fs::create_dir(source.join("history-store")).unwrap();
            symlink("../history-store", state.join(namespace)).unwrap();
        }
        let history = state.join(relative);
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        let db = Connection::open(&history).unwrap();
        db.execute_batch("CREATE TABLE paths (path TEXT NOT NULL)")
            .unwrap();
        db.execute(
            "INSERT INTO paths VALUES (?1)",
            [if needs_change {
                source.join("payload").display().to_string()
            } else {
                "preserved history".into()
            }],
        )
        .unwrap();
        let registered_ordinary = selection == "global" && relative != "custom/history.sqlite";
        if registered_ordinary {
            let ordinary = state.join("custom/registered.sqlite");
            fs::create_dir_all(ordinary.parent().unwrap()).unwrap();
            let agent = Connection::open(&ordinary).unwrap();
            agent
                .execute_batch("CREATE TABLE paths (path TEXT)")
                .unwrap();
            agent
                .execute(
                    "INSERT INTO paths VALUES (?1)",
                    [source.join("payload").to_str().unwrap()],
                )
                .unwrap();
            db.execute_batch("CREATE TABLE agent_databases (path TEXT)")
                .unwrap();
            db.execute(
                "INSERT INTO agent_databases VALUES (?1)",
                [if needs_change {
                    ordinary.display().to_string()
                } else {
                    "custom/registered.sqlite".into()
                }],
            )
            .unwrap();
        }
        drop(db);
        let selected = match selection {
            "global" => Some(state.join("state/openclaw.sqlite")),
            "agent" => Some(state.join("agents/main/agent/openclaw-agent.sqlite")),
            "registered" => Some(state.join("selected/history.sqlite")),
            _ => None,
        };
        if let Some(selected) = selected {
            fs::create_dir_all(selected.parent().unwrap()).unwrap();
            let prefix = if selection == "agent" {
                "../../.."
            } else {
                ".."
            };
            symlink(format!("{prefix}/{relative}"), selected).unwrap();
        }
        if selection.starts_with("registered") {
            fs::create_dir_all(state.join("state")).unwrap();
            let db = Connection::open(state.join("state/openclaw.sqlite")).unwrap();
            db.execute_batch("CREATE TABLE agent_databases (path TEXT)")
                .unwrap();
            db.execute(
                "INSERT INTO agent_databases VALUES (?1)",
                [if selection == "registered-direct" {
                    relative
                } else {
                    "selected/history.sqlite"
                }],
            )
            .unwrap();
        }
        fs::set_permissions(&history, fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(history.parent().unwrap(), fs::Permissions::from_mode(0o500)).unwrap();
        let before = fs::read(&history).unwrap();
        let archive = root.child("state.tar");
        run(&[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            archive.to_str().unwrap(),
            "--json",
        ]);
        let archive_bytes = fs::read(&archive).unwrap();
        let mut tar = tar::Archive::new(archive_bytes.as_slice());
        let archived_path = if namespace_alias {
            Path::new("root/history-store").join(suffix)
        } else {
            Path::new("root/.openclaw").join(relative)
        };
        let mut archived = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap() == archived_path {
                entry.read_to_end(&mut archived).unwrap();
                break;
            }
        }
        assert!(!archived.is_empty());
        fs::rename(&source, root.child("offline")).unwrap();
        let physical = root.child("physical");
        fs::create_dir(&physical).unwrap();
        symlink(&physical, root.child("alias")).unwrap();
        let target = root.child("alias/imported");
        let output = run_ocm(
            root.path(),
            &env,
            &[
                "env",
                "import",
                archive.to_str().unwrap(),
                "--name",
                "imported",
                "--root",
                target.to_str().unwrap(),
                "--json",
            ],
        );
        let protected = relative != "custom/history.sqlite";
        if protected && selection != "none" {
            assert!(!output.status.success());
            assert!(
                stderr(&output).contains("selects integrity-protected history"),
                "{}",
                stderr(&output)
            );
            assert!(!target.exists());
            let shown = run_ocm(root.path(), &env, &["env", "show", "imported", "--json"]);
            assert!(!shown.status.success());
            let original = root.child("offline/.openclaw").join(relative);
            assert_eq!(fs::read(&original).unwrap(), before);
            assert_eq!(fs::read(&archive).unwrap(), archive_bytes);
            assert_eq!(
                fs::metadata(&original).unwrap().permissions().mode() & 0o777,
                0o400
            );
            assert_eq!(
                fs::metadata(original.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o500
            );
            fs::set_permissions(
                original.parent().unwrap(),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            continue;
        }
        assert!(output.status.success(), "{}", stderr(&output));
        let imported: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
        let copied = target.join(".openclaw").join(relative);
        if protected {
            assert_eq!(
                fs::read(&copied).unwrap(),
                archived,
                "{selection}: {relative}"
            );
            assert!(
                imported["databasePathsRewritten"]
                    .get(format!(".openclaw/{relative}"))
                    .is_none()
            );
        } else {
            let db =
                Connection::open_with_flags(&copied, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            let path: String = db
                .query_row("SELECT path FROM paths", [], |row| row.get(0))
                .unwrap();
            assert_eq!(path, target.join("payload").display().to_string());
            assert_eq!(
                imported["databasePathsRewritten"][format!(".openclaw/{relative}")],
                1
            );
        }
        assert_eq!(
            fs::read(root.child("offline/.openclaw").join(relative)).unwrap(),
            before
        );
        assert_eq!(fs::read(&archive).unwrap(), archive_bytes);
        assert_eq!(
            fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
            0o400
        );
        assert_eq!(
            fs::metadata(copied.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
        run(&["env", "show", "imported", "--json"]);
        for parent in [
            copied.parent().unwrap().to_path_buf(),
            root.child("offline/.openclaw")
                .join(relative)
                .parent()
                .unwrap()
                .to_path_buf(),
        ] {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}
