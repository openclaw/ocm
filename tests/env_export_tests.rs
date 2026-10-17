mod support;

use std::fs;

use ocm::infra::archive::{EnvArchiveMetadata, extract_env_archive};

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout, write_text};

#[test]
fn env_export_writes_the_default_archive_path() {
    let root = TestDir::new("env-export-default");
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
        "hello export",
    );

    let export = run_ocm(&cwd, &env, &["env", "export", "source"]);
    assert!(export.status.success(), "{}", stderr(&export));
    assert!(stdout(&export).contains("Exported env source"));
    assert!(stdout(&export).contains("state included: false"));

    let archive_path = cwd.join("source.ocm-env.tar");
    assert!(archive_path.exists());

    let extract_dir = root.child("extract-default");
    let extracted = extract_env_archive::<EnvArchiveMetadata>(&archive_path, &extract_dir).unwrap();
    assert_eq!(extracted.metadata.env.name, "source");
    assert_eq!(extracted.metadata.env.gateway_port, Some(19789));
    assert_eq!(
        fs::read_to_string(extracted.root_dir.join(".openclaw/workspace/notes.txt")).unwrap(),
        "hello export"
    );
}

#[test]
fn env_export_json_reports_the_custom_archive_path() {
    let root = TestDir::new("env-export-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            "./archives/source-backup.tar",
            "--json",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let output = stdout(&export);
    assert!(output.contains("\"name\": \"source\""));
    assert!(output.contains("\"archivePath\":"));
    assert!(output.contains("\"includeState\": false"));
    assert!(output.contains("/archives/source-backup.tar"));
    assert!(cwd.join("archives/source-backup.tar").exists());
}

#[test]
fn env_export_include_state_round_trips_committed_sqlite_state() {
    use rusqlite::{Connection, OpenFlags};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    fn board_rows(db: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
        let mut query = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    let root = TestDir::new("env-export-state");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source/.openclaw");
    let board_path = source.join("agents/main/agent/openclaw-agent.sqlite");
    fs::create_dir_all(board_path.parent().unwrap()).unwrap();
    let board = Connection::open(&board_path).unwrap();
    board.execute_batch(
        "CREATE TABLE board_tabs (tab_id TEXT PRIMARY KEY, title TEXT, revision INTEGER);
         CREATE TABLE board_widgets (name TEXT PRIMARY KEY, content_kind TEXT, descriptor_json TEXT,
            sha256 TEXT, granted_sha TEXT, grant_state TEXT, manifest TEXT, title TEXT, revision INTEGER);"
    ).unwrap();
    let title = format!("Saved example: {}", source.join("workspace").display());
    board
        .execute("INSERT INTO board_tabs VALUES ('main', ?1, 7)", [&title])
        .unwrap();
    // Registered content hashes the authored source; plugin and MCP descriptors
    // hash their serialized descriptor. All can contain paths as content.
    let authored_source = json!({"type":"text", "text":title}).to_string();
    let descriptors = [
        (
            "registered",
            "plugin",
            json!({"pluginKind":"canvas", "source":authored_source}).to_string(),
        ),
        (
            "plugin",
            "plugin",
            json!({"pluginKind":"notes", "props":{"text":title}}).to_string(),
        ),
        (
            "mcp",
            "mcp-app",
            json!({"serverName":"notes", "resourceUri":"ui://notes", "toolResult":{"text":title}})
                .to_string(),
        ),
    ];
    for (name, kind, descriptor) in &descriptors {
        let hash_input = if *name == "registered" {
            &authored_source
        } else {
            descriptor
        };
        let checksum = Sha256::digest(hash_input.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let granted = *name != "plugin";
        board
            .execute(
                "INSERT INTO board_widgets VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 3)",
                rusqlite::params![
                    name,
                    kind,
                    descriptor,
                    checksum,
                    granted.then_some(&checksum),
                    if granted { "granted" } else { "none" },
                    json!({"contentOwner":name, "grantSemanticsVersion":2}).to_string(),
                    title
                ],
            )
            .unwrap();
    }
    let original_tabs = board_rows(&board, "board_tabs");
    let original_widgets = board_rows(&board, "board_widgets");
    drop(board);
    let original_board_bytes = fs::read(&board_path).unwrap();
    let database = source.join("state/openclaw.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA wal_autocheckpoint=0;
         CREATE TABLE sessions (id TEXT PRIMARY KEY, content TEXT);
         INSERT INTO sessions VALUES ('session-1', 'committed conversation');
         CREATE TABLE installed_plugin_index (index_key TEXT PRIMARY KEY, install_records_json TEXT);
         CREATE TABLE agent_database_leases (lease_id TEXT PRIMARY KEY, owner_pid INTEGER);
         INSERT INTO agent_database_leases VALUES ('source-owner', 123);
         CREATE TABLE state_leases (scope TEXT, lease_key TEXT, owner TEXT, expires_at INTEGER);
         INSERT INTO state_leases VALUES ('core:plugin-lifecycle', 'global', 'source-owner', 9999999999999);"
    ).unwrap();
    write_text(&source.join("openclaw.json"), "{}\n");
    let receipt = json!({"demo": {"source": "npm", "spec": "demo@1.0.0",
        "installPath": source.join("extensions/demo").to_str().unwrap()},
        "recovered": {"source": "npm", "installPath": root.child("former/.openclaw/extensions/recovered")}});
    write_text(
        &source.join("extensions/recovered/index.js"),
        "export default {};",
    );
    connection
        .execute(
            "INSERT INTO installed_plugin_index VALUES ('installed-plugin-index', ?1)",
            [receipt.to_string()],
        )
        .unwrap();
    assert!(
        database
            .with_extension("sqlite-wal")
            .metadata()
            .unwrap()
            .len()
            > 0
    );
    write_text(
        &source.join("extensions/demo/package.json"),
        r#"{"name":"demo","version":"1.0.0"}"#,
    );

    let source_config = fs::read(source.join("openclaw.json")).unwrap();
    let source_include = fs::read(source.join("plugins/settings.json")).ok();
    let secondary_path = source.join("state/history.socket/records.db");
    fs::create_dir_all(secondary_path.parent().unwrap()).unwrap();
    let secondary = Connection::open(&secondary_path).unwrap();
    secondary
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE history (value TEXT); INSERT INTO history VALUES ('committed');
        BEGIN; INSERT INTO history VALUES ('uncommitted');",
        )
        .unwrap();
    let journal_named = Connection::open(source.join("state/activity-journal")).unwrap();
    journal_named.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records (value TEXT); INSERT INTO records VALUES ('durable database');").unwrap();
    write_text(
        &source.join("state/bundle-wal/notes.txt"),
        "durable directory",
    );
    write_text(
        &source.join("state/service.sock"),
        "durable socket-named file",
    );
    write_text(&source.join("state/note.txt"), "durable note");
    write_text(&source.join("state/opaque.sqlite"), "opaque payload");
    write_text(&source.join("state/event-journal"), "{\"durable\":true}");
    let orphan_path = root.child("orphan.db");
    let orphan = Connection::open(&orphan_path).unwrap();
    orphan.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE value (data TEXT); INSERT INTO value VALUES ('orphan');").unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = orphan_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        fs::copy(
            std::path::PathBuf::from(sidecar),
            source.join(format!("state/orphan.db{suffix}")),
        )
        .unwrap();
    }
    fs::write(
        source.join("state/orphan.db-journal"),
        [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7],
    )
    .unwrap();
    #[cfg(target_os = "linux")]
    let non_utf8 = {
        use std::os::unix::ffi::OsStringExt;
        let name = std::ffi::OsString::from_vec(b"history-\xff.db".to_vec());
        let path = source.join("state").join(&name);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records (value TEXT); INSERT INTO records VALUES ('non-UTF-8');").unwrap();
        (name, Some(db))
    };
    #[cfg(target_os = "linux")]
    let (non_utf8_name, mut non_utf8_connection) = non_utf8;
    for residue in [
        "gateway.pid",
        "lock",
        "locks/owner",
        "tmp/cache",
        "gateway-supervisor-restart-handoff.json",
    ] {
        write_text(&source.join(residue), "runtime residue");
    }
    #[cfg(unix)]
    let _socket = {
        let short_root = tempfile::tempdir_in("/tmp").unwrap();
        let path = short_root.path().join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        fs::rename(path, source.join("state/native")).unwrap();
        (short_root, listener)
    };

    let mut connections = Some((connection, secondary, journal_named));
    let mut default_members = Vec::new();
    for (name, include_state) in [("default", false), ("with-state", true)] {
        let archive = format!("{name}.tar");
        let mut args = vec!["env", "export", "source", "--output", &archive, "--json"];
        if include_state {
            args.push("--include-state");
        }
        let export = run_ocm(&cwd, &env, &args);
        assert!(export.status.success(), "{}", stderr(&export));
        let summary: Value = serde_json::from_str(&stdout(&export)).unwrap();
        assert_eq!(summary["includeState"], include_state);
        let mut archive_reader = tar::Archive::new(fs::File::open(cwd.join(&archive)).unwrap());
        let members: Vec<_> = archive_reader
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().into_owned())
            .collect();
        if include_state {
            let without_state: Vec<_> = members
                .iter()
                .filter(|path| !path.starts_with("root/.openclaw/state"))
                .cloned()
                .collect();
            assert_eq!(without_state, default_members);
            for residue in [
                "gateway.pid",
                "lock",
                "locks",
                "tmp",
                "gateway-supervisor-restart-handoff.json",
            ] {
                assert!(!members.contains(&std::path::Path::new("root/.openclaw").join(residue)));
            }
            for residue in [
                "native",
                "openclaw.sqlite-wal",
                "openclaw.sqlite-shm",
                "history.socket/records.db-wal",
                "history.socket/records.db-shm",
                "orphan.db-wal",
                "orphan.db-shm",
                "orphan.db-journal",
            ] {
                assert!(
                    !members.contains(&std::path::Path::new("root/.openclaw/state").join(residue)),
                    "{residue}"
                );
            }
        } else {
            default_members = members;
        }
        let extracted = extract_env_archive::<EnvArchiveMetadata>(
            &cwd.join(&archive),
            &root.child(format!("extract-{name}")),
        )
        .unwrap();
        assert_eq!(extracted.metadata.include_state, include_state);
        assert_eq!(
            extracted
                .root_dir
                .join(".openclaw/state/openclaw.sqlite")
                .exists(),
            include_state
        );
        if include_state {
            let source_db = &connections.as_ref().unwrap().0;
            let unchanged: String = source_db
                .query_row(
                    "SELECT install_records_json FROM installed_plugin_index",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(&unchanged).unwrap(), receipt);
            let owners: i64 = source_db
                .query_row("SELECT COUNT(*) FROM agent_database_leases", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(owners, 1);
            assert_eq!(
                fs::read(source.join("openclaw.json")).unwrap(),
                source_config
            );
            let global_owners: i64 = source_db
                .query_row("SELECT COUNT(*) FROM state_leases", [], |row| row.get(0))
                .unwrap();
            assert_eq!(global_owners, 1);
            assert_eq!(
                fs::read(source.join("plugins/settings.json")).ok(),
                source_include
            );
            #[cfg(target_os = "linux")]
            {
                for suffix in ["-wal", "-shm", "-journal"] {
                    let mut name = non_utf8_name.clone();
                    name.push(suffix);
                    assert!(
                        !extracted
                            .root_dir
                            .join(".openclaw/state")
                            .join(name)
                            .exists()
                    );
                }
                drop(non_utf8_connection.take());
            }
            // Import must not rely on the exporting machine still being available.
            drop(connections.take());
            fs::rename(&source, root.child("source-offline")).unwrap();
        }
        let import = run_ocm(&cwd, &env, &["env", "import", &archive, "--name", name]);
        assert!(import.status.success(), "{}", stderr(&import));
        let target = root.child(format!("ocm-home/envs/{name}/.openclaw"));
        let target_db = target.join("state/openclaw.sqlite");
        assert_eq!(target_db.exists(), include_state);
        let board = Connection::open_with_flags(
            target.join("agents/main/agent/openclaw-agent.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(board_rows(&board, "board_tabs"), original_tabs);
        assert_eq!(board_rows(&board, "board_widgets"), original_widgets);
        for (name, _, _) in &descriptors {
            let (descriptor, checksum): (String, String) = board
                .query_row(
                    "SELECT descriptor_json, sha256 FROM board_widgets WHERE name = ?1",
                    [name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let hash_input = if *name == "registered" {
                serde_json::from_str::<Value>(&descriptor).unwrap()["source"]
                    .as_str()
                    .unwrap()
                    .to_string()
            } else {
                descriptor
            };
            assert_eq!(
                Sha256::digest(hash_input.as_bytes())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
                checksum
            );
        }
        assert_eq!(
            board
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let original = if include_state {
            root.child("source-offline/agents/main/agent/openclaw-agent.sqlite")
        } else {
            board_path.clone()
        };
        assert_eq!(fs::read(original).unwrap(), original_board_bytes);
        if !include_state {
            continue;
        }
        assert_eq!(
            fs::read_to_string(target.join("state/note.txt")).unwrap(),
            "durable note"
        );
        assert_eq!(
            fs::read_to_string(target.join("state/opaque.sqlite")).unwrap(),
            "opaque payload"
        );
        assert_eq!(
            fs::read_to_string(target.join("state/service.sock")).unwrap(),
            "durable socket-named file"
        );
        assert_eq!(
            fs::read_to_string(target.join("state/bundle-wal/notes.txt")).unwrap(),
            "durable directory"
        );
        let journal_named = Connection::open_with_flags(
            target.join("state/activity-journal"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let value: String = journal_named
            .query_row("SELECT value FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "durable database");
        assert_eq!(
            fs::read_to_string(target.join("state/event-journal")).unwrap(),
            "{\"durable\":true}"
        );
        #[cfg(target_os = "linux")]
        {
            let db = Connection::open_with_flags(
                target.join("state").join(&non_utf8_name),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let value: String = db
                .query_row("SELECT value FROM records", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, "non-UTF-8");
        }
        let history = Connection::open_with_flags(
            target.join("state/history.socket/records.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let values: Vec<String> = history
            .prepare("SELECT value FROM history")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(values, ["committed"]);
        assert!(!target_db.with_extension("sqlite-wal").exists());
        assert!(!target_db.with_extension("sqlite-shm").exists());
        let restored =
            Connection::open_with_flags(&target_db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let content: String = restored
            .query_row(
                "SELECT content FROM sessions WHERE id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content, "committed conversation");
        let integrity: String = restored
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let raw: String = restored
            .query_row(
                "SELECT install_records_json FROM installed_plugin_index",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let imported_receipt: Value = serde_json::from_str(&raw).unwrap();
        let mut expected_receipt = receipt.clone();
        expected_receipt["demo"]["installPath"] = serde_json::json!(target.join("extensions/demo"));
        assert_eq!(imported_receipt, expected_receipt);
        assert_eq!(
            fs::read(target.join("openclaw.json")).unwrap(),
            source_config
        );
        let owners: i64 = restored
            .query_row("SELECT COUNT(*) FROM agent_database_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(owners, 1);
        let global_owners: i64 = restored
            .query_row("SELECT COUNT(*) FROM state_leases", [], |row| row.get(0))
            .unwrap();
        assert_eq!(global_owners, 1);
    }
}

#[test]
fn env_export_help_describes_include_state() {
    let root = TestDir::new("env-export-help");
    let output = run_ocm(root.path(), &ocm_env(&root), &["env", "export", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("--include-state"));
    assert!(stdout(&output).contains("SQLite"));
}

#[cfg(unix)]
#[test]
fn env_export_include_state_retains_the_installation_and_snapshots_sqlite_anywhere() {
    use rusqlite::{Connection, OpenFlags};
    let root = TestDir::new("export-whole-installation");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let files = [
        ".openclaw/custom/impl.mjs",
        ".openclaw/memory/unused",
        ".openclaw/agents/main/sessions/history.jsonl",
        ".openclaw/logs/saved.log",
        ".openclaw/backups/settings.json",
        "legacy/data.txt",
        ".openclaw/extensions/demo/.openclaw-install-stage-old",
        ".openclaw/extensions/opaque/node_modules/value",
        ".openclaw/state/projects/demo/Cargo.lock",
        ".openclaw/state/Cargo.lock",
        ".openclaw/state/tmp/notes.txt",
        ".openclaw/workspace/tmp/notes.txt",
    ];
    for path in files {
        write_text(&source.join(path), "retained data");
    }
    write_text(
        &source.join(".openclaw/custom/impl.mjs"),
        "export const value = 42;",
    );
    write_text(
        &source.join("entry.mjs"),
        "import { value } from './.openclaw/custom/impl.mjs'; if (value !== 42) throw Error('missing payload');",
    );
    write_text(
        &source.join(".openclaw/openclaw.json"),
        &serde_json::json!({"plugins":{"load":{"paths":[source.join("entry.mjs")]}}}).to_string(),
    );
    let databases = [
        "records",
        ".openclaw/custom/data.db",
        ".openclaw/agents/main/sessions/history.sqlite",
        ".openclaw/state/history/run/records.db",
        "legacy/.openclaw/tmp",
        ".openclaw/plugin-runtime-deps",
        ".openclaw/extensions/demo/.openclaw-pnpm-store",
        ".openclaw/extensions/filedeps/node_modules",
        ".openclaw/extensions/opaque/.openclaw-runtime-deps-copy-data",
    ];
    let connections: Vec<_> = databases.iter().map(|path| {
        fs::create_dir_all(source.join(path).parent().unwrap()).unwrap();
        let db = Connection::open(source.join(path)).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE entries(value TEXT); INSERT INTO entries VALUES ('committed'); BEGIN; INSERT INTO entries VALUES ('uncommitted');").unwrap();
        db
    }).collect();
    let debris = [
        ".openclaw/gateway.pid",
        ".openclaw/locks/owner",
        ".openclaw/team/.openclaw/tmp/cache.bin",
        ".openclaw/run/process",
        ".openclaw/bundled-plugin-runtime-deps/cache",
        ".openclaw/.openclaw-pnpm-store/cache",
        ".openclaw/.openclaw-npm-cache/cache",
        ".openclaw/extensions/demo/.openclaw-runtime-deps.json",
        ".openclaw/extensions/filedeps/.openclaw-runtime-deps.json",
        ".openclaw/extensions/demo/node_modules/rebuilt.js",
        ".openclaw/extensions/demo/.openclaw-runtime-deps-copy-old/node_modules/rebuilt.js",
    ];
    for path in debris {
        write_text(&source.join(path), "runtime debris");
    }
    let export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            "state.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    drop(connections);
    fs::rename(&source, root.child("source-offline")).unwrap();
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "state.tar", "--name", "target"],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let target = root.child("ocm-home/envs/target");
    for path in files {
        assert!(target.join(path).is_file(), "{path}");
    }
    for path in debris {
        assert!(!target.join(path).exists(), "{path}");
    }
    for path in databases {
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!target.join(format!("{path}{suffix}")).exists());
        }
        let db = Connection::open_with_flags(target.join(path), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let values: Vec<String> = db
            .prepare("SELECT value FROM entries")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(values, ["committed"], "{path}");
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
    let executed = std::process::Command::new("node")
        .arg(target.join("entry.mjs"))
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
}

#[test]
fn env_export_import_retains_configured_agent_directories() {
    let root = TestDir::new("export-configured-agent-directories");
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
        let state = root.child(&format!("ocm-home/envs/{source_name}/.openclaw"));
        let agent_dir = if index == 5 {
            "${OPENCLAW_STATE_DIR}/identity/${OCM_ACTIVE_ENV}".to_string()
        } else {
            state.join(relative).to_str().unwrap().to_string()
        };
        write_text(
            &state.join("openclaw.json"),
            &serde_json::json!({"agents":{"entries":{"main":{"agentDir":agent_dir}}}}).to_string(),
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
            &state.join("extensions/demo/.openclaw-runtime-deps.json"),
            "{}",
        );
        write_text(
            &state.join("extensions/demo/node_modules/stale.txt"),
            "runtime debris",
        );
        let config_before = fs::read(state.join("openclaw.json")).unwrap();
        let archive = root.child(&format!("{source_name}.tar"));
        let exported = run_ocm(
            cwd,
            &env,
            &[
                "env",
                "export",
                &source_name,
                "--output",
                archive.to_str().unwrap(),
            ],
        );
        assert!(
            exported.status.success(),
            "{relative}: {}",
            stderr(&exported)
        );
        let extract_dir = root.child(&format!("extract-{index}"));
        let extracted = extract_env_archive::<EnvArchiveMetadata>(&archive, &extract_dir).unwrap();
        assert_eq!(
            fs::read(
                extracted
                    .root_dir
                    .join(".openclaw")
                    .join(relative)
                    .join("models.json")
            )
            .unwrap(),
            b"{\"providers\":{}}"
        );
        let imported = run_ocm(
            cwd,
            &env,
            &[
                "env",
                "import",
                archive.to_str().unwrap(),
                "--name",
                &target_name,
            ],
        );
        assert!(
            imported.status.success(),
            "{relative}: {}",
            stderr(&imported)
        );
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
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(target.join("openclaw.json")).unwrap()).unwrap();
        assert_eq!(
            config["agents"]["entries"]["main"]["agentDir"],
            target.join(relative).to_str().unwrap()
        );
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
