mod support;

use std::fs;
use std::path::Path;

use ocm::env::{CreateEnvironmentOptions, ExportEnvironmentOptions, ImportEnvironmentOptions};
use ocm::store::{create_environment, export_environment, import_environment};
use rusqlite::{Connection, params};

use support::{TestDir, ocm_env};

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
        let imported = import_environment(
            ImportEnvironmentOptions {
                archive: export.archive_path,
                name: Some("copy".into()),
                root: None,
            },
            &env,
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
