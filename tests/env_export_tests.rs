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
         CREATE TABLE installed_plugin_index (index_key TEXT PRIMARY KEY, install_records_json TEXT);"
    ).unwrap();
    let receipt = json!({"demo": {"source": "npm", "spec": "demo@1.0.0",
        "installPath": source.join("extensions/demo").to_str().unwrap()}});
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

    let secondary_path = source.join("state/nested/history.db");
    fs::create_dir_all(secondary_path.parent().unwrap()).unwrap();
    let secondary = Connection::open(&secondary_path).unwrap();
    secondary
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE history (value TEXT); INSERT INTO history VALUES ('committed');
        BEGIN; INSERT INTO history VALUES ('uncommitted');",
        )
        .unwrap();
    write_text(&source.join("state/note.txt"), "durable note");
    write_text(&source.join("state/opaque.sqlite"), "opaque payload");
    for residue in [
        "gateway.pid",
        "lock",
        "service.sock",
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

    let mut connections = Some((connection, secondary));
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
                "service.sock",
                "locks",
                "tmp",
                "native",
                "gateway-supervisor-restart-handoff.json",
                "openclaw.sqlite-wal",
                "openclaw.sqlite-shm",
                "nested/history.db-wal",
                "nested/history.db-shm",
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
        let history = Connection::open_with_flags(
            target.join("state/nested/history.db"),
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
