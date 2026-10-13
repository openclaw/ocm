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
    for (included_config, absolute_include) in [(false, false), (true, false), (true, true)] {
        assert_state_round_trip(included_config, absolute_include);
    }
}

fn assert_state_round_trip(included_config: bool, absolute_include: bool) {
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
        if included_config {
            let include = if absolute_include {
                source
                    .join("plugins/settings.json")
                    .to_str()
                    .unwrap()
                    .to_string()
            } else {
                "plugins/settings.json".to_string()
            };
            config["plugins"] = json!({"$include": include});
            write_text(
                &source.join("plugins/settings.json"),
                &json!({"load": {"paths": [alias]}}).to_string(),
            );
        } else {
            config["plugins"] = json!({"load": {"paths": [alias]}});
        }
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

    #[cfg(not(unix))]
    if included_config {
        let include = if absolute_include {
            source
                .join("plugins/settings.json")
                .to_str()
                .unwrap()
                .to_string()
        } else {
            "plugins/settings.json".to_string()
        };
        write_text(
            &source.join("openclaw.json"),
            &json!({"plugins":{"$include":include}}).to_string(),
        );
        write_text(
            &source.join("plugins/settings.json"),
            &json!({"load":{"paths":[source.join("extensions/demo")]}}).to_string(),
        );
    }
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
        if absolute_include && !include_state {
            // Ordinary archives keep their existing absolute-include refusal.
            assert!(!import.status.success());
            assert!(
                stderr(&import).contains("$include path escapes"),
                "{}",
                stderr(&import)
            );
            assert!(!root.child(format!("ocm-home/envs/{name}")).exists());
            continue;
        }
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
            let configured = if included_config {
                let include: Value = serde_json::from_slice(
                    &fs::read(target.join("plugins/settings.json")).unwrap(),
                )
                .unwrap();
                include["load"]["paths"][0].clone()
            } else {
                config["plugins"]["load"]["paths"][0].clone()
            };
            assert_eq!(
                configured,
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
        let global_owners: i64 = restored
            .query_row("SELECT COUNT(*) FROM state_leases", [], |row| row.get(0))
            .unwrap();
        assert_eq!(global_owners, 0);
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

#[test]
fn env_export_include_state_rejects_damaged_canonical_databases() {
    for name in ["openclaw.sqlite", "openclaw-quarantine.sqlite"] {
        let root = TestDir::new("export-damaged-state");
        let env = ocm_env(&root);
        let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let source = root
            .child("ocm-home/envs/source/.openclaw/state")
            .join(name);
        write_text(&source, "damaged database header");
        let default = run_ocm(
            root.path(),
            &env,
            &["env", "export", "source", "--output", "default.tar"],
        );
        assert!(default.status.success(), "{}", stderr(&default));
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
        assert!(!export.status.success());
        assert!(stderr(&export).contains("SQLite"), "{}", stderr(&export));
        assert!(!root.child("state.tar").exists());
        let mut extracted = extract_env_archive::<EnvArchiveMetadata>(
            &root.child("default.tar"),
            &root.child("extracted"),
        )
        .unwrap();
        extracted.metadata.include_state = true;
        write_text(
            &extracted.root_dir.join(".openclaw/state").join(name),
            "damaged database header",
        );
        ocm::infra::archive::write_env_archive(
            &extracted.metadata,
            &extracted.root_dir,
            &root.child("damaged.tar"),
        )
        .unwrap();
        let import = run_ocm(
            root.path(),
            &env,
            &["env", "import", "damaged.tar", "--name", "target"],
        );
        assert!(!import.status.success());
        assert!(stderr(&import).contains("SQLite"), "{}", stderr(&import));
        assert!(!root.child("ocm-home/envs/target").exists());
        assert_eq!(
            fs::read_to_string(&source).unwrap(),
            "damaged database header"
        );
    }
}

#[cfg(unix)]
#[test]
fn env_export_include_state_rejects_absolute_plugin_links_and_keeps_relative_links() {
    use rusqlite::Connection;
    use serde_json::json;
    for kind in [
        "absolute",
        "relative",
        "chained-absolute",
        "chained-relative",
    ] {
        for receipt in [false, true] {
            let root = TestDir::new("export-plugin-link");
            let env = ocm_env(&root);
            let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source/.openclaw");
            write_text(
                &source.join("extensions/actual/package.json"),
                r#"{"name":"demo","version":"1.0.0"}"#,
            );
            let target = match kind {
                "absolute" => source.join("extensions/actual"),
                "relative" => std::path::PathBuf::from("actual"),
                _ => {
                    fs::create_dir_all(source.join("extensions/nested")).unwrap();
                    let bridge = if kind == "chained-absolute" {
                        source.join("extensions/nested")
                    } else {
                        std::path::PathBuf::from("nested")
                    };
                    std::os::unix::fs::symlink(bridge, source.join("extensions/bridge")).unwrap();
                    std::path::PathBuf::from("bridge/../actual")
                }
            };
            std::os::unix::fs::symlink(target, source.join("extensions/link")).unwrap();
            if receipt {
                fs::create_dir_all(source.join("state")).unwrap();
                let db = Connection::open(source.join("state/openclaw.sqlite")).unwrap();
                db.execute_batch("CREATE TABLE installed_plugin_index (index_key TEXT PRIMARY KEY, install_records_json TEXT);").unwrap();
                let record = json!({"demo":{"source":"path","installPath":source.join("extensions/link"),"sourcePath":source.join("extensions/link")}});
                db.execute(
                    "INSERT INTO installed_plugin_index VALUES ('installed-plugin-index', ?1)",
                    [record.to_string()],
                )
                .unwrap();
            } else {
                write_text(
                    &source.join("openclaw.json"),
                    &json!({"plugins":{"load":{"paths":[source.join("extensions/link")]}}})
                        .to_string(),
                );
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
            if kind.contains("absolute") {
                assert!(!export.status.success(), "{kind}, receipt={receipt}");
                assert!(
                    stderr(&export).contains("absolute symlink"),
                    "{}",
                    stderr(&export)
                );
                assert!(!root.child("state.tar").exists());
            } else {
                assert!(export.status.success(), "{}", stderr(&export));
                fs::rename(&source, root.child("source-offline")).unwrap();
                let import = run_ocm(
                    root.path(),
                    &env,
                    &["env", "import", "state.tar", "--name", "target"],
                );
                assert!(import.status.success(), "{}", stderr(&import));
                let target = root.child("ocm-home/envs/target/.openclaw");
                assert_eq!(
                    fs::canonicalize(target.join("extensions/link")).unwrap(),
                    fs::canonicalize(target.join("extensions/actual")).unwrap()
                );
            }
        }
    }
}

#[test]
fn env_export_include_state_requires_selected_plugin_payload_roots() {
    use rusqlite::Connection;
    use serde_json::json;
    for (relative, workspace, selected) in [
        ("extensions/demo", false, true),
        ("custom/demo", false, false),
        ("custom/demo", true, true),
        ("memory/demo", false, false),
        ("memory/demo", true, true),
    ] {
        for receipt in [false, true] {
            let root = TestDir::new("export-plugin-selection");
            let env = ocm_env(&root);
            let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source/.openclaw");
            write_text(
                &source.join(relative).join("package.json"),
                r#"{"name":"demo","version":"1.0.0"}"#,
            );
            let mut config = json!({});
            if workspace {
                config["agents"] = json!({"defaults":{"workspace":source.join(std::path::Path::new(relative).parent().unwrap())}});
            }
            if receipt {
                fs::create_dir_all(source.join("state")).unwrap();
                let db = Connection::open(source.join("state/openclaw.sqlite")).unwrap();
                db.execute_batch("CREATE TABLE installed_plugin_index (index_key TEXT PRIMARY KEY, install_records_json TEXT);").unwrap();
                db.execute(
                    "INSERT INTO installed_plugin_index VALUES ('installed-plugin-index', ?1)",
                    [
                        json!({"demo":{"source":"path","installPath":source.join(relative)}})
                            .to_string(),
                    ],
                )
                .unwrap();
            } else {
                config["plugins"] = json!({"load":{"paths":[source.join(relative)]}});
            }
            write_text(&source.join("openclaw.json"), &config.to_string());
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
            assert_eq!(export.status.success(), selected, "{}", stderr(&export));
            if !selected {
                assert!(
                    stderr(&export).contains("excluded")
                        || stderr(&export).contains("removed during import"),
                    "{}",
                    stderr(&export)
                );
                assert!(!root.child("state.tar").exists());
            } else {
                fs::rename(&source, root.child("source-offline")).unwrap();
                let import = run_ocm(
                    root.path(),
                    &env,
                    &["env", "import", "state.tar", "--name", "target"],
                );
                assert!(import.status.success(), "{}", stderr(&import));
                assert!(
                    root.child("ocm-home/envs/target/.openclaw")
                        .join(relative)
                        .join("package.json")
                        .exists()
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn env_export_include_state_checks_discovered_and_nested_payload_links() {
    use serde_json::json;
    for (mode, relative_link) in ["container", "directory", "file", "global", "workspace"]
        .into_iter()
        .flat_map(|mode| [false, true].map(|relative| (mode, relative)))
    {
        let root = TestDir::new("export-payload-tree");
        let env = ocm_env(&root);
        let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let source = root.child("ocm-home/envs/source");
        let plugin = source.join("payload/demo");
        write_text(
            &plugin.join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        write_text(
            &plugin.join("impl.js"),
            "export default { register() {} };\n",
        );
        let catalog = match mode {
            "global" => source.join(".openclaw/extensions"),
            "workspace" => source.join("workspace/.openclaw/extensions"),
            _ => source.join("catalog"),
        };
        let directory_link = matches!(mode, "container" | "global" | "workspace");
        let configured = if directory_link {
            fs::create_dir_all(&catalog).unwrap();
            let target = if relative_link {
                std::path::PathBuf::from(match mode {
                    "global" => "../../payload/demo",
                    "workspace" => "../../../payload/demo",
                    _ => "../payload/demo",
                })
            } else {
                plugin.clone()
            };
            std::os::unix::fs::symlink(target, catalog.join("demo")).unwrap();
            catalog.clone()
        } else {
            let target = if relative_link {
                std::path::PathBuf::from("impl.js")
            } else {
                plugin.join("impl.js")
            };
            std::os::unix::fs::symlink(target, plugin.join("index.js")).unwrap();
            if mode == "file" {
                write_text(&plugin.join("entry.mjs"), "import './index.js';\n");
                plugin.join("entry.mjs")
            } else {
                plugin.clone()
            }
        };
        write_text(
            &source.join(".openclaw/openclaw.json"),
            &match mode {
                "global" => json!({}),
                "workspace" => {
                    json!({"agents":{"defaults":{"workspace": source.join("workspace")}}})
                }
                _ => json!({"plugins":{"load":{"paths":[configured]}}}),
            }
            .to_string(),
        );
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
        assert_eq!(
            export.status.success(),
            relative_link,
            "{}",
            stderr(&export)
        );
        if !relative_link {
            assert!(stderr(&export).contains("absolute symlink"));
            assert!(!root.child("state.tar").exists());
        } else {
            fs::rename(&source, root.child("source-offline")).unwrap();
            let import = run_ocm(
                root.path(),
                &env,
                &["env", "import", "state.tar", "--name", "target"],
            );
            assert!(import.status.success(), "{}", stderr(&import));
            let target = root.child("ocm-home/envs/target");
            let path = if directory_link {
                target
                    .join(catalog.strip_prefix(&source).unwrap())
                    .join("demo/impl.js")
            } else {
                target.join("payload/demo/index.js")
            };
            assert!(fs::read_to_string(path).unwrap().contains("register"));
        }
    }
}

#[cfg(unix)]
#[test]
fn env_export_include_state_materializes_unchanged_linked_config() {
    let root = TestDir::new("export-linked-config");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source/.openclaw");
    let original = source.join("plugins/settings.json");
    write_text(&original, "{}\n");
    std::os::unix::fs::symlink(&original, source.join("openclaw.json")).unwrap();
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
    assert!(
        fs::symlink_metadata(source.join("openclaw.json"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(original).unwrap(), "{}\n");
    fs::rename(&source, root.child("source-offline")).unwrap();
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "state.tar", "--name", "target"],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    assert!(
        fs::symlink_metadata(root.child("ocm-home/envs/target/.openclaw/openclaw.json"))
            .unwrap()
            .is_file()
    );
}

#[cfg(unix)]
#[test]
fn env_export_include_state_checks_relative_link_traversal_after_cleanup() {
    use serde_json::json;
    for retained in [false, true] {
        let root = TestDir::new("export-link-traversal");
        let env = ocm_env(&root);
        let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let source = root.child("ocm-home/envs/source");
        let state = source.join(".openclaw");
        write_text(&state.join("plugins/demo/index.js"), "export default {};\n");
        fs::create_dir_all(state.join("memory/keep")).unwrap();
        fs::create_dir_all(state.join("extensions")).unwrap();
        std::os::unix::fs::symlink("../memory/../plugins/demo", state.join("extensions/demo"))
            .unwrap();
        let config = if retained {
            json!({"agents":{"defaults":{"workspace":state.join("memory/keep")}}})
        } else {
            json!({})
        };
        write_text(&state.join("openclaw.json"), &config.to_string());
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
        assert_eq!(export.status.success(), retained, "{}", stderr(&export));
        if retained {
            fs::rename(&source, root.child("source-offline")).unwrap();
            let import = run_ocm(
                root.path(),
                &env,
                &["env", "import", "state.tar", "--name", "target"],
            );
            assert!(import.status.success(), "{}", stderr(&import));
            assert!(
                root.child("ocm-home/envs/target/.openclaw/extensions/demo/index.js")
                    .is_file()
            );
        } else {
            assert!(
                stderr(&export).contains("entry removed during import"),
                "{}",
                stderr(&export)
            );
            assert!(!root.child("state.tar").exists());
        }
    }
}
