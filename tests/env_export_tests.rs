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

    let root = TestDir::new("env-export-state");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source/.openclaw");
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
         INSERT INTO agent_database_leases VALUES ('source-owner', 123);"
    ).unwrap();
    write_text(&source.join("openclaw.json"), "{}\n");
    let receipt = json!({"demo": {"source": "npm", "spec": "demo@1.0.0",
        "installPath": source.join("extensions/demo").to_str().unwrap()},
        "recovered": {"source": "npm", "installPath": root.child("former/.openclaw/extensions/recovered")}});
    write_text(
        &source.join("extensions/recovered/index.js"),
        "export default {};",
    );
    #[cfg(unix)]
    let receipt = {
        let mut receipt = receipt;
        let alias = root.child("plugin-alias");
        write_text(
            &source.join("extensions/alias/index.js"),
            "export default {};",
        );
        std::os::unix::fs::symlink(source.join("extensions/alias"), &alias).unwrap();
        receipt["alias"] = json!({"source": "path", "installPath": alias, "sourcePath": alias});
        let config_path = source.join("openclaw.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["plugins"] = json!({"load": {"paths": [alias]}});
        fs::write(config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        receipt
    };
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
    for residue in [
        "gateway.pid",
        "lock",
        "locks/owner",
        "tmp/cache",
        "gateway-supervisor-restart-handoff.json",
        "orphan.db-wal",
        "orphan.db-shm",
        "orphan.db-journal",
    ] {
        write_text(&source.join("state").join(residue), "runtime residue");
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
                "native",
                "gateway-supervisor-restart-handoff.json",
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
            // Import must not rely on the exporting machine still being available.
            drop(connections.take());
            fs::rename(&source, root.child("source-offline")).unwrap();
        }
        let import = run_ocm(&cwd, &env, &["env", "import", &archive, "--name", name]);
        assert!(import.status.success(), "{}", stderr(&import));
        let target = root.child(format!("ocm-home/envs/{name}/.openclaw"));
        let target_db = target.join("state/openclaw.sqlite");
        assert_eq!(target_db.exists(), include_state);
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
        assert_eq!(
            imported_receipt["demo"]["installPath"],
            target.join("extensions/demo").to_str().unwrap()
        );
        assert_eq!(imported_receipt["demo"]["spec"], "demo@1.0.0");
        assert_eq!(
            imported_receipt["recovered"]["installPath"],
            target.join("extensions/recovered").to_str().unwrap()
        );
        #[cfg(unix)]
        {
            let config: Value =
                serde_json::from_slice(&fs::read(target.join("openclaw.json")).unwrap()).unwrap();
            assert_eq!(
                config["plugins"]["load"]["paths"][0],
                target.join("extensions/alias").to_str().unwrap()
            );
        }
        #[cfg(unix)]
        for field in ["installPath", "sourcePath"] {
            assert_eq!(
                imported_receipt["alias"][field],
                target.join("extensions/alias").to_str().unwrap()
            );
        }
        let owners: i64 = restored
            .query_row("SELECT COUNT(*) FROM agent_database_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(owners, 0);
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
fn env_export_include_state_rejects_links_without_copying_their_targets() {
    for linked in ["state", "state/history.db"] {
        let root = TestDir::new("export-state-link");
        let env = ocm_env(&root);
        let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let outside = root.child("outside");
        fs::create_dir_all(&outside).unwrap();
        let db = rusqlite::Connection::open(outside.join("history.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE records (value TEXT); INSERT INTO records VALUES ('unchanged');",
        )
        .unwrap();
        let link = root.child("ocm-home/envs/source/.openclaw").join(linked);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let target = if linked == "state" {
            outside.clone()
        } else {
            outside.join("history.db")
        };
        std::os::unix::fs::symlink(target, link).unwrap();
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
        assert!(!export.status.success(), "linked {linked} was accepted");
        assert!(
            stderr(&export).contains("state symlink"),
            "{}",
            stderr(&export)
        );
        assert!(!root.child("state.tar").exists());
        let value: String = db
            .query_row("SELECT value FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "unchanged");
        let default = run_ocm(
            root.path(),
            &env,
            &["env", "export", "source", "--output", "default.tar"],
        );
        assert!(default.status.success(), "{}", stderr(&default));
        let mut extracted = extract_env_archive::<EnvArchiveMetadata>(
            &root.child("default.tar"),
            &root.child("extracted"),
        )
        .unwrap();
        extracted.metadata.include_state = true;
        let link = extracted.root_dir.join(".openclaw").join(linked);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let target = if linked == "state" {
            outside.clone()
        } else {
            outside.join("history.db")
        };
        std::os::unix::fs::symlink(target, link).unwrap();
        ocm::infra::archive::write_env_archive(
            &extracted.metadata,
            &extracted.root_dir,
            &root.child("linked.tar"),
        )
        .unwrap();
        let import = run_ocm(
            root.path(),
            &env,
            &["env", "import", "linked.tar", "--name", "target"],
        );
        assert!(!import.status.success());
        assert!(
            stderr(&import).contains("state symlink"),
            "{}",
            stderr(&import)
        );
        assert!(!root.child("ocm-home/envs/target").exists());
        let value: String = db
            .query_row("SELECT value FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "unchanged");
    }
}

#[test]
fn env_import_rejects_state_archive_without_source_root() {
    let root = TestDir::new("import-state-metadata");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            "valid.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let mut extracted = extract_env_archive::<EnvArchiveMetadata>(
        &root.child("valid.tar"),
        &root.child("extracted"),
    )
    .unwrap();
    extracted.metadata.env.source_root = None;
    ocm::infra::archive::write_env_archive(
        &extracted.metadata,
        &extracted.root_dir,
        &root.child("invalid.tar"),
    )
    .unwrap();
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "invalid.tar", "--name", "target"],
    );
    assert!(!import.status.success());
    assert!(
        stderr(&import).contains("sourceRoot"),
        "{}",
        stderr(&import)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
}
