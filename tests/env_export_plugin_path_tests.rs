mod support;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout, write_text};
use std::fs;

#[test]
fn state_export_copies_configuration_verbatim_without_resolving_it() {
    let root = TestDir::new("export-config-files");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let cases = [
        // Runtime-only expressions, includes, and malformed config are opaque files.
        r#"{"plugins":{"load":{"paths":["${PLUGIN_DIRECTORY}","./entry.mjs"]}},"$include":"missing.json"}"#,
        "{ incomplete configuration",
    ];
    for (index, config) in cases.iter().enumerate() {
        write_text(&source.join(".openclaw/openclaw.json"), config);
        write_text(
            &source.join(".env"),
            "PLUGIN_DIRECTORY=./.openclaw/custom\n",
        );
        write_text(
            &source.join(".openclaw/.env"),
            "UNRESOLVED=${OTHER_VALUE}\n",
        );
        write_text(&source.join("entry.mjs"), "export default {};\n");
        write_text(
            &source.join(".openclaw/custom/dependency.mjs"),
            "export const value = 42;\n",
        );
        let archive = format!("config-{index}.tar");

        let export = run_ocm(
            root.path(),
            &env,
            &[
                "env",
                "export",
                "source",
                "--include-state",
                "--output",
                &archive,
                "--json",
            ],
        );
        assert!(export.status.success(), "{}", stderr(&export));
        let summary: serde_json::Value = serde_json::from_str(&stdout(&export)).unwrap();
        assert_eq!(summary["includeState"], true);
        assert!(summary.get("notIncludedPluginPaths").is_none());
        let extracted =
            ocm::infra::archive::extract_env_archive::<ocm::infra::archive::EnvArchiveMetadata>(
                &root.child(&archive),
                &root.child(format!("extracted-{index}")),
            )
            .unwrap();
        let target = extracted.root_dir;
        for path in [
            ".openclaw/openclaw.json",
            ".env",
            ".openclaw/.env",
            "entry.mjs",
            ".openclaw/custom/dependency.mjs",
        ] {
            assert_eq!(
                fs::read(source.join(path)).unwrap(),
                fs::read(target.join(path)).unwrap(),
                "{path}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn state_export_preserves_links_without_copying_external_targets() {
    use std::os::unix::fs::symlink;
    let root = TestDir::new("export-linked-files");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    write_text(&source.join(".openclaw/state/data/value"), "owned");
    write_text(&root.child("outside/value"), "external");
    symlink(source.join(".openclaw/state/data"), source.join("history")).unwrap();
    symlink(root.child("outside"), source.join("external")).unwrap();
    symlink("missing", source.join("dangling")).unwrap();
    symlink("loop", source.join("loop")).unwrap();
    for (link, target) in [("agents", "agent-store"), ("extensions", "plugin-store")] {
        write_text(
            &source.join(target).join("retained.json"),
            "{\"retained\":true}\n",
        );
        symlink(format!("../{target}"), source.join(".openclaw").join(link)).unwrap();
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
            "links.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    fs::rename(&source, root.child("source-offline")).unwrap();
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "links.tar", "--name", "target"],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let target = root.child("ocm-home/envs/target");
    assert_eq!(
        fs::read_to_string(target.join("history/value")).unwrap(),
        "owned"
    );
    assert_eq!(
        fs::read_link(target.join("external")).unwrap(),
        root.child("outside")
    );
    assert_eq!(
        fs::read_link(target.join("dangling")).unwrap(),
        std::path::Path::new("missing")
    );
    assert_eq!(
        fs::read_link(target.join("loop")).unwrap(),
        std::path::Path::new("loop")
    );
    for (link, destination) in [("agents", "agent-store"), ("extensions", "plugin-store")] {
        assert_eq!(
            fs::read_link(target.join(".openclaw").join(link)).unwrap(),
            std::path::Path::new(&format!("../{destination}"))
        );
        assert_eq!(
            fs::read(target.join(".openclaw").join(link).join("retained.json")).unwrap(),
            b"{\"retained\":true}\n"
        );
    }
    fs::remove_dir_all(root.child("outside")).unwrap();
    assert!(!target.join("external/value").exists());
}

#[test]
fn state_import_honors_an_explicit_sandbox_origin_and_retains_unresolved_plugin_paths() {
    let root = TestDir::new("state-import-origin");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let config = serde_json::json!({"plugins":{"load":{"paths":["${UNRESOLVED_PLUGIN}"]}}, "mcp":{"apps":{"sandboxOrigin":"https://old.example.test"}}});
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        &config.to_string(),
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
    assert!(export.status.success(), "{}", stderr(&export));
    let import = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "import",
            "state.tar",
            "--name",
            "target",
            "--sandbox-origin",
            "https://new.example.test",
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let mut expected = config;
    expected["mcp"]["apps"]["sandboxOrigin"] = "https://new.example.test".into();
    let actual: serde_json::Value = serde_json::from_slice(
        &fs::read(root.child("ocm-home/envs/target/.openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(actual["plugins"], expected["plugins"]);
    assert_eq!(actual["mcp"], expected["mcp"]);
}

#[cfg(unix)]
#[test]
fn state_import_preserves_private_directory_modes() {
    use std::os::unix::fs::PermissionsExt;
    let root = TestDir::new("export-directory-modes");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    write_text(
        &source.join(".openclaw/state/private/data"),
        "private state",
    );
    write_text(&source.join(".openclaw/state/shared/data"), "shared data");
    fs::set_permissions(
        source.join(".openclaw/state/shared"),
        fs::Permissions::from_mode(0o1777),
    )
    .unwrap();
    for path in [".openclaw/state", ".openclaw/state/private"] {
        fs::set_permissions(source.join(path), fs::Permissions::from_mode(0o700)).unwrap();
    }
    write_text(&source.join(".openclaw/openclaw.json"), &serde_json::json!({"agents":{"defaults":{"workspace":source.join(".openclaw/workspace")}}}).to_string());
    fs::set_permissions(source.join(".openclaw"), fs::Permissions::from_mode(0o500)).unwrap();
    let export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            "modes.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "modes.tar", "--name", "target"],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let target = root.child("ocm-home/envs/target");
    for path in ["", ".openclaw/state", ".openclaw/state/private"] {
        assert_eq!(
            fs::metadata(target.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "{path}"
        );
    }
    assert_eq!(
        fs::read(target.join(".openclaw/state/private/data")).unwrap(),
        b"private state"
    );
    assert_eq!(
        fs::metadata(target.join(".openclaw/state/shared"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o1777
    );
    assert_eq!(
        fs::metadata(target.join(".openclaw"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o500
    );
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(target.join(".openclaw/openclaw.json")).unwrap()).unwrap();
    assert_eq!(
        config["agents"]["defaults"]["workspace"],
        target.join(".openclaw/workspace").to_str().unwrap()
    );
    for directory in [source.join(".openclaw"), target.join(".openclaw")] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn state_import_rejects_invalid_config_and_preserves_readonly_directories() {
    use std::os::unix::fs::PermissionsExt;
    let root = TestDir::new("state-import-rollback");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    write_text(
        &source.join(".openclaw/state/readonly/data"),
        "retained data",
    );
    write_text(&source.join(".openclaw/openclaw.json"), "{ invalid");
    fs::set_permissions(
        source.join(".openclaw/state/readonly"),
        fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            "readonly.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    assert_eq!(
        fs::metadata(root.child("readonly.tar"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let import = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "import",
            "readonly.tar",
            "--name",
            "target",
            "--sandbox-origin",
            "https://new.example.test",
        ],
    );
    assert!(!import.status.success());
    assert!(!root.child("ocm-home/envs/target").exists());
    write_text(&source.join(".openclaw/openclaw.json"), "{}\n");
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
    let retry = run_ocm(
        root.path(),
        &env,
        &["env", "import", "valid.tar", "--name", "target"],
    );
    assert!(retry.status.success(), "{}", stderr(&retry));
    let restored = root.child("ocm-home/envs/target/.openclaw/state/readonly");
    assert_eq!(
        fs::metadata(&restored).unwrap().permissions().mode() & 0o777,
        0o500
    );
    for directory in [restored, source.join(".openclaw/state/readonly")] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn state_import_relocates_config_and_retains_other_state_bytes() {
    use ocm::infra::archive::{EnvArchiveMetadata, extract_env_archive};
    use serde_json::{Value, json};

    let root = TestDir::new("state-import-relocate");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let config = json!({
        "agents": {
            "defaults": {"workspace": source.join(".openclaw/workspace")},
            "list": [{"id": "research", "workspace": source.join(".openclaw/workspace-research"),
                "agentDir": source.join(".openclaw/agents/research/agent")}]
        },
        "plugins": {"load": {"paths": [source.join("entry.mjs"), "${PLUGIN_DIRECTORY}"]}},
        "gateway": {"port": 19789},
        "mcp": {"apps": {"sandboxPort": 19790, "sandboxOrigin": "https://source.example.test"}},
        "extra": {"$include": "./config/extra.json"}
    });
    write_text(&source.join(".openclaw/openclaw.json"), &config.to_string());
    write_text(
        &source.join(".openclaw/config/extra.json"),
        "{\"enabled\":true}\n",
    );
    let retained = [
        ".env",
        ".openclaw/.env",
        "entry.mjs",
        ".openclaw/config/extra.json",
        ".openclaw/workspace/notes.txt",
        ".openclaw/workspace-research/notes.txt",
        ".openclaw/agents/research/agent/settings.json",
        ".openclaw/agents/research/sessions/history.jsonl",
        ".openclaw/logs/saved.log",
        ".openclaw/backups/settings.json",
        ".openclaw/state/opaque.bin",
    ];
    for path in &retained[0..3] {
        write_text(&source.join(path), "PLUGIN_DIRECTORY=./plugins\n");
    }
    for path in &retained[4..] {
        write_text(
            &source.join(path),
            &format!("unchanged {}\n", source.display()),
        );
    }
    let database = source.join(".openclaw/state/openclaw.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE receipts (path TEXT);")
        .unwrap();
    connection
        .execute(
            "INSERT INTO receipts VALUES (?1)",
            [source.join("entry.mjs").to_str().unwrap()],
        )
        .unwrap();
    drop(connection);
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
    let extracted = extract_env_archive::<EnvArchiveMetadata>(
        &root.child("state.tar"),
        &root.child("extracted"),
    )
    .unwrap();
    fs::rename(&source, root.child("source-offline")).unwrap();
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "state.tar", "--name", "target", "--json"],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let target = root.child("ocm-home/envs/target");
    let actual: Value =
        serde_json::from_slice(&fs::read(target.join(".openclaw/openclaw.json")).unwrap()).unwrap();
    assert_eq!(
        actual["agents"]["defaults"]["workspace"],
        target.join(".openclaw/workspace").to_str().unwrap()
    );
    assert_eq!(
        actual["agents"]["list"][0]["workspace"],
        target
            .join(".openclaw/workspace-research")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        actual["agents"]["list"][0]["agentDir"],
        target
            .join(".openclaw/agents/research/agent")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        actual["plugins"]["load"]["paths"][0],
        target.join("entry.mjs").to_str().unwrap()
    );
    assert_eq!(actual["plugins"]["load"]["paths"][1], "${PLUGIN_DIRECTORY}");
    assert_eq!(actual["extra"]["$include"], "./config/extra.json");
    assert!(actual["mcp"]["apps"].get("sandboxOrigin").is_none());
    let show = run_ocm(root.path(), &env, &["env", "show", "target", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let meta: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(actual["gateway"]["port"], meta["gatewayPort"]);
    assert_eq!(
        actual["mcp"]["apps"]["sandboxPort"].as_u64(),
        meta["gatewayPort"].as_u64().map(|port| port + 1)
    );
    let copied_database =
        rusqlite::Connection::open(target.join(".openclaw/state/openclaw.sqlite")).unwrap();
    let receipt: String = copied_database
        .query_row("SELECT path FROM receipts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(receipt, target.join("entry.mjs").display().to_string());
    for path in retained {
        let archived = fs::read(extracted.root_dir.join(path)).unwrap();
        let expected = if matches!(
            path,
            ".openclaw/agents/research/agent/settings.json"
                | ".openclaw/agents/research/sessions/history.jsonl"
                | ".openclaw/logs/saved.log"
                | ".openclaw/backups/settings.json"
                | ".openclaw/state/opaque.bin"
        ) {
            String::from_utf8(archived)
                .unwrap()
                .replace(source.to_str().unwrap(), target.to_str().unwrap())
                .into_bytes()
        } else {
            archived
        };
        assert_eq!(fs::read(target.join(path)).unwrap(), expected, "{path}");
    }
    let summary: Value = serde_json::from_str(&stdout(&import)).unwrap();
    assert_eq!(summary["configPathsRewritten"], true);
}

#[cfg(unix)]
#[test]
fn state_import_rejects_a_config_link_to_a_fifo_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let root = TestDir::new("state-import-config-fifo");
    let mut env = ocm_env(&root);
    fs::create_dir_all(root.child("tmp")).unwrap();
    env.insert("TMPDIR".into(), root.child("tmp").display().to_string());
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let fifo = root.child("external-config");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    symlink(
        &fifo,
        root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
    )
    .unwrap();
    let export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            "fifo.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let mut child = Command::new(support::ocm_test_binary_path())
        .current_dir(root.path())
        .env_clear()
        .envs(&env)
        .args(["env", "import", "fifo.tar", "--name", "target"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!timed_out, "import blocked reading a config FIFO");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("must be a regular file"),
        "{}",
        stderr(&output)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
    fs::remove_file(root.child("ocm-home/envs/source/.openclaw/openclaw.json")).unwrap();
    let retry = run_ocm(root.path(), &env, &["env", "create", "target"]);
    assert!(retry.status.success(), "{}", stderr(&retry));
}

#[test]
fn state_import_relocates_plugin_receipts_and_runtime_references_only() {
    use ocm::infra::archive::{EnvArchiveMetadata, extract_env_archive};
    use rusqlite::{Connection, OpenFlags};
    use serde_json::{Value, json};

    for legacy in [false, true] {
        let root = TestDir::new("state-import-plugin-paths");
        let env = ocm_env(&root);
        let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let source = root.child("ocm-home/envs/source");
        let installed = source.join(".openclaw/extensions/demo");
        let package = source.join("packages/demo");
        let external = root.child("external-plugin");
        let lookalike = source.with_file_name("source-other").join("plugin");
        let separator_sibling = source
            .with_file_name(if cfg!(windows) {
                "source-other"
            } else {
                "source\\other"
            })
            .join("plugin");
        write_text(&installed.join("index.js"), "export default {};\n");
        write_text(&package.join("package.json"), "{\"name\":\"demo\"}\n");
        let reentered = source.join("../source/plugin");
        let records = json!({
            "reentered": {"source":"npm", "installPath":reentered},
            "demo": {"source":"path", "installPath":installed, "sourcePath":package,
                "version":"1.0.0", "integrity":"unchanged", "unknown":{"enabled":true,"path":source.join("historical-reference")}},
            "external": {"source":"path", "installPath":external, "sourcePath":external},
            "lookalike": {"source":"npm", "installPath":lookalike},
            "punctuation-sibling": {"source":"path", "installPath":source.with_file_name("source=other").join("plugin")},
            "backslash-sibling": {"source":"path", "installPath":separator_sibling},
            "spaced-sibling": {"source":"path", "installPath":source.with_file_name("source other").join("plugin"), "sourcePath":source.with_file_name("source other").join("plugin")},
            "spaced-escape": {"source":"path", "installPath":source.join("a b/../../outside")},
            "missing": {"source":"npm", "installPath":source.join(".openclaw/extensions/missing")}
        });
        let plugins = json!([{
            "id": "demo", "rootDir": installed, "source": installed.join("index.js"), "setupSource": installed.join("setup.js"),
            "manifestPath": installed.join("openclaw.plugin.json"),
            "packageJson": {"path": installed.join("package.json"),
                "fileSignature": {"mtimeMs": "PRECISE_TIMESTAMP"}}
        }, {"id":"reentered", "rootDir":reentered, "setupSource":reentered}, {"id":"external-setup", "setupSource":source.join("../outside/setup.js")}]);
        let plugins_raw = plugins
            .to_string()
            .replace("\"PRECISE_TIMESTAMP\"", "1791960884534.1313");
        let diagnostics_raw =
            json!([{"message": format!("loaded from {}", installed.display())}]).to_string();
        let database = source.join(".openclaw/state/openclaw.sqlite");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let db = Connection::open(&database).unwrap();
        db.execute_batch(
            "CREATE TABLE durable (value TEXT); INSERT INTO durable VALUES ('session bytes');
            CREATE TABLE agent_database_leases (lease_id TEXT, owner_pid INTEGER);
            INSERT INTO agent_database_leases VALUES ('retained-owner', 123);",
        )
        .unwrap();
        let (select, _initial_document) = if legacy {
            db.execute_batch("CREATE TABLE installed_plugin_index (index_key TEXT PRIMARY KEY, install_records_json TEXT, updated_at INTEGER, plugins_json TEXT, diagnostics_json TEXT, workspace_dir TEXT);").unwrap();
            db.execute(
                "INSERT INTO installed_plugin_index (index_key, install_records_json, updated_at) VALUES ('installed-plugin-index', ?1, 42)",
                [records.to_string()],
            )
            .unwrap();
            db.execute(
                "INSERT INTO installed_plugin_index (index_key, install_records_json, updated_at) VALUES ('unrelated', ?1, 43)",
                [source.display().to_string()],
            )
            .unwrap();
            db.execute("UPDATE installed_plugin_index SET plugins_json=?1, diagnostics_json=?2, workspace_dir=?3 WHERE index_key='installed-plugin-index'",
                [&plugins_raw, &diagnostics_raw, source.join("../source/workspace").to_str().unwrap()]).unwrap();
            (
                "SELECT install_records_json FROM installed_plugin_index WHERE index_key='installed-plugin-index'",
                records.clone(),
            )
        } else {
            db.execute_batch("CREATE TABLE config_machine_state (state_key TEXT PRIMARY KEY, value_json TEXT, updated_at_ms INTEGER);").unwrap();
            let document = json!({"revision":17,"index":{"installRecords":records, "plugins":plugins,
                    "workspaceDir":source, "diagnostics":[{"message":format!("loaded from {}", installed.display())}]},"unknown":"retained"});
            db.execute(
                "INSERT INTO config_machine_state VALUES ('plugins.installedIndex', ?1, 42)",
                [document
                    .to_string()
                    .replace("\"PRECISE_TIMESTAMP\"", "1791960884534.1313")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO config_machine_state VALUES ('unrelated', ?1, 43)",
                [source.display().to_string()],
            )
            .unwrap();
            (
                "SELECT value_json FROM config_machine_state WHERE state_key='plugins.installedIndex'",
                document,
            )
        };
        let receipt_table = if legacy {
            "installed_plugin_index"
        } else {
            "config_machine_state"
        };
        db.execute_batch(&format!("CREATE TRIGGER unrelated_change AFTER UPDATE ON {receipt_table} BEGIN UPDATE durable SET value='changed by trigger'; END;")).unwrap();
        let original_document: String = db.query_row(select, [], |row| row.get(0)).unwrap();
        let expected_document: Value = serde_json::from_str(&original_document).unwrap();
        let receipt_column = if legacy {
            "install_records_json"
        } else {
            "value_json"
        };
        db.execute_batch(&format!("CREATE UNIQUE INDEX receipt_value ON {receipt_table}({receipt_column}); CREATE TABLE receipt_history (original TEXT REFERENCES {receipt_table}({receipt_column}) ON UPDATE CASCADE);")).unwrap();
        db.execute(
            "INSERT INTO receipt_history VALUES (?1)",
            [&original_document],
        )
        .unwrap();
        drop(db);
        #[cfg(unix)]
        if legacy {
            fs::rename(&database, database.with_file_name("index.sqlite")).unwrap();
            std::os::unix::fs::symlink("index.sqlite", &database).unwrap();
        }
        let runtime_file = ".openclaw/agents/main/agent/paths.json";
        let mut runtime = json!({"rootDir":installed, "package":package,
            "command":format!("node {}/index.js", installed.display()),
            "shell":format!("cd {}; pwd", source.display()),
            "quotedExternal":format!("cat '{} other/plugin'", source.display()),
            "quotedOwned":format!("cat '{}/a b/file'", source.display()),
            "quotedBackslash":format!("cat '{}/a\\' {}/other", source.display(), source.display()),
            "bracketedLog":format!("[INFO] loaded from {}/plugin", source.display()),
            "quotedEscape":format!("cat '{}/a b/../../outside'", source.display()),
            "quotedReferences":format!("paths '{} and {}/plugin'", external.display(), source.display()),
            "relativeArgument":format!("sh -c '{}/run ../../../input'", source.display()),
            "quotedScript":format!("sh -c '{}/run --input {}/input'", source.display(), source.display()),
            "multilineEscape":format!("cat '{}/a\nb/../../outside'", source.display()),
            "encoded":json!({"rootDir":installed}).to_string(),
            "normalized":format!("{}/sub/../index.js", installed.display()),
            "newlineSiblingPath":format!("{}\nother/plugin", source.display()),
            "absoluteCommand": {"command":format!("{}/run --input {}/input", source.display(), source.display())},
            "text":format!("{}/run --input {}/input\n{}/second", source.display(), source.display(), source.display()),
            "externalCommand":format!("node {}=other/plugin.js", source.display()),
            "escapedRoot":format!("node {}/../outside/index.js", source.display()),
            "external":external, "lookalike":lookalike});
        let url_cases = json!({
            "apostropheProseSibling":format!("Don't open {}'other/plugin", source.display()),
            "apostropheProseUriSibling":format!("Don't open {}'other/plugin", url::Url::from_file_path(&source).unwrap()),
            "decodedIncompleteProse":format!("{{}}\n\"node {}/plugin", source.display()),
            "mixedUrlAndPlainEscape":format!("{}/a b/../../outside {}/plugin", url::Url::from_file_path(&source).unwrap(), source.display()),
            "decodedProse":format!("{{}}\n{}/plugin", source.display()),
            "multipleUrlEscape":format!("{}/a b/../../outside\n{}/plugin", url::Url::from_file_path(&source).unwrap(), url::Url::from_file_path(&source).unwrap()),
            "sameLineUrlEscape":format!("{}/a b/../../outside {}/plugin", url::Url::from_file_path(&source).unwrap(), url::Url::from_file_path(&source).unwrap()),
            "quotedMultipleUrlEscape":format!("echo \"{}/a b/../../outside\n{}/plugin\"", url::Url::from_file_path(&source).unwrap(), url::Url::from_file_path(&source).unwrap()),
            "prefixedUrl":format!("echo \"url={}\"; pwd", url::Url::from_file_path(installed.join("index.js")).unwrap()),
            "urlCommand":format!("open \"{}\"; pwd", url::Url::from_file_path(installed.join("index.js")).unwrap()),
            "fileUrl":url::Url::from_file_path(installed.join("index.js")).unwrap().to_string(),
            "apostropheFileUrl":format!("{}'other/plugin.js", url::Url::from_file_path(&source).unwrap()),
            "quotedFileUrl":format!("cat '{}'", url::Url::from_file_path(installed.join("index.js")).unwrap()),
            "proseUrlEscape":format!("echo \"loaded from {}/a b/../../outside\"", url::Url::from_file_path(&source).unwrap()),
            "reenteredUrl":format!("{}/../source/plugin?q=keep%20bytes#part", url::Url::from_file_path(&source).unwrap()),
            "backslashFileUrl":format!("{}\\other/plugin", url::Url::from_file_path(&source).unwrap()),
            "angleUrlEscape":format!("<{}/a b/../../outside>", url::Url::from_file_path(&source).unwrap()),
            "spacedLabelUrlEscape":format!("echo \"url = {}/a b/../../outside\"", url::Url::from_file_path(&source).unwrap()),
            "colonLabelUrlEscape":format!("echo \"url: {}/a b/../../outside\"", url::Url::from_file_path(&source).unwrap()),
            "uriLeadingProse":format!("{}/plugin please keep these words", url::Url::from_file_path(&source).unwrap()),
            "encodedRootUrl":format!("{}/plugin?q=alpha%20beta#keep", url::Url::from_file_path(&source).unwrap()).replacen("/source/", "/%73ource/", 1),
            "url":format!("{}/a b/../../outside", url::Url::from_file_path(&source).unwrap()),
            "prefixedUrlEscape":format!("echo \"url={}/a b/../../outside\"", url::Url::from_file_path(&source).unwrap()),
            "quotedUrlEscape":format!("cat '{}/a b/../../outside'", url::Url::from_file_path(&source).unwrap()),
            "angleSibling":format!("{}>other/plugin", url::Url::from_file_path(&source).unwrap()),
            "quotedAngleSibling":format!("cat '{}>other/plugin'", url::Url::from_file_path(&source).unwrap()),
            "wrappedRootUrl":format!("<{}>", url::Url::from_file_path(&source).unwrap()),
            "sameLineUrls":format!("{}/first {}/second", url::Url::from_file_path(&source).unwrap(), url::Url::from_file_path(&source).unwrap()),
            "uriTextWithPlainReference":format!("{}/first {}/second", url::Url::from_file_path(&source).unwrap(), source.display()),
            "quotedSpacedFileUrl":format!("cat '{} other/plugin.js'", url::Url::from_file_path(&source).unwrap()),
            "rootUrls":format!("{}\n{}", url::Url::from_file_path(&source).unwrap(), url::Url::from_file_path(&source).unwrap()),
            "multipleUrls":format!("{}/first\n{}/second", url::Url::from_file_path(&source).unwrap(), url::Url::from_file_path(&source).unwrap()),
            "spacedFileUrl":format!("{} other/plugin.js", url::Url::from_file_path(&source).unwrap()),
            "mixedUrlAndPath":format!("echo \"url={} {}/a b/../../outside\"", url::Url::from_file_path(installed.join("index.js")).unwrap(), source.display()),
            "externalFileUrl":format!("{};other/plugin.js", url::Url::from_file_path(&source).unwrap()),
            "upperFileUrl":url::Url::from_file_path(installed.join("index.js")).unwrap().to_string().replacen("file:", "FILE:", 1),
            "shortFileUrl":format!("file:{}", url::Url::from_file_path(installed.join("index.js")).unwrap().path()),
            "localFileUrl":format!("file://localhost{}", url::Url::from_file_path(installed.join("index.js")).unwrap().path()),
        });
        runtime
            .as_object_mut()
            .unwrap()
            .extend(url_cases.as_object().unwrap().clone());
        runtime["quotedTerminalRoot"] = json!(format!(
            "echo \"{}/a b/../../outside {}\"",
            url::Url::from_file_path(&source).unwrap(),
            source.display()
        ));
        runtime["lineTerminalRoot"] = json!(format!(
            "{}/a b/../../outside {}\n",
            url::Url::from_file_path(&source).unwrap(),
            source.display()
        ));
        runtime["adjacentMixedReferences"] = json!(format!(
            "{}/one,{}/two",
            url::Url::from_file_path(&source).unwrap(),
            source.display()
        ));
        runtime["urlControlText"] = json!(["\t", "\u{b}", "\u{c}", "\u{85}"].map(|separator| {
            let uri = url::Url::from_file_path(&source).unwrap();
            format!("loaded {uri}/plugin{separator}ready")
        }));
        runtime["separatedRootUrls"] = json!([" ", "\t", "\r", "\n"].map(|separator| {
            let uri = url::Url::from_file_path(&source).unwrap();
            format!("{uri}{separator}{uri}/plugin")
        }));
        runtime["labelledRootUrls"] = json!(format!(
            "{} second:{}",
            url::Url::from_file_path(&source).unwrap(),
            url::Url::from_file_path(&source).unwrap()
        ));
        write_text(&source.join(runtime_file), &runtime.to_string());
        write_text(
            &source.join(".openclaw/logs/root.txt"),
            &format!("{}\n", source.display()),
        );
        let surrogate_file = ".openclaw/agents/main/sessions/surrogate.jsonl";
        let opaque_document = |path: &std::path::Path| {
            let encoded = serde_json::to_string(path).unwrap();
            let uri = url::Url::from_file_path(path).unwrap();
            format!(
                "{{\"opaque\":\"\\ud800\",\"mixed\":\"\\ud800 {}\",\"url\":\"\\ud800 {uri}\",\"urlBefore\":\"{uri} \\ud800\",\"urls\":\"{uri},{uri}\",\"path\":{encoded}}}\n",
                &encoded[1..encoded.len() - 1]
            )
        };
        let surrogate = opaque_document(&installed);
        write_text(&source.join(surrogate_file), &surrogate);
        let partial_file = ".openclaw/agents/main/sessions/partial.jsonl";
        let partial = format!("{}\n{{\"unfinished\":", json!({"path": installed}));
        write_text(&source.join(partial_file), &partial);
        let recovered_files = [
            ".openclaw/agents/main/sessions/recovered.jsonl",
            ".openclaw/agents/main/sessions/recovered.jsonl.deleted.2026-10-14T00-00-00.000Z",
            ".openclaw/agents/main/sessions/recovered.jsonl.reset.2026-10-14T00-00-00.000Z",
            ".openclaw/agents/main/sessions/recovered.jsonl.bak.2026-10-14T00-00-00.000Z",
            ".openclaw/agents/main/sessions/recovered.jsonl.migrated.1",
            ".openclaw/agents/main/sessions/recovered.jsonl.pre-doctor-branch-repair-2026-10-14T00-00-00-000Z.bak",
        ];
        let recovered_record = |base: &std::path::Path| {
            serde_json::json!({"cwd":base.join(".openclaw/workspace")}).to_string()
        };
        let broken_record = format!(
            "{{\"incomplete\":{}\n",
            serde_json::to_string(&source).unwrap()
        );
        for recovered_file in recovered_files {
            write_text(
                &source.join(recovered_file),
                &format!(
                    "{{\"unfinished\":\n{}\n{}{}\n",
                    recovered_record(&source),
                    broken_record,
                    recovered_record(&source)
                ),
            );
        }
        let jsonl_file = ".openclaw/agents/main/sessions/commands.jsonl";
        write_text(
            &source.join(jsonl_file),
            &(runtime.to_string() + "\n" + &runtime.to_string() + "\n"),
        );
        let untouched = [
            ".env",
            ".openclaw/workspace/notes.txt",
            ".openclaw/skill-workshop/draft.json",
            ".openclaw/logs/config-audit.jsonl",
            "legacy/data.txt",
        ];
        for file in untouched {
            write_text(&source.join(file), &format!("keep {}\n", source.display()));
        }
        fs::write(
            source.join(".openclaw/state/opaque.bin"),
            [0xff, 0x00, 0xfe],
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&database, fs::Permissions::from_mode(0o400)).unwrap();
            fs::set_permissions(
                database.parent().unwrap(),
                fs::Permissions::from_mode(0o500),
            )
            .unwrap();
            fs::set_permissions(source.join(runtime_file), fs::Permissions::from_mode(0o400))
                .unwrap();
        }
        #[cfg(not(unix))]
        for file in [&database, &source.join(runtime_file)] {
            let mut permissions = fs::metadata(file).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(file, permissions).unwrap();
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
        let archive = extract_env_archive::<EnvArchiveMetadata>(
            &root.child("state.tar"),
            &root.child("archive"),
        )
        .unwrap();
        fs::rename(&source, root.child("source-offline")).unwrap();
        #[cfg(unix)]
        let target = root.child("imports/copy-\"quoted\"#mark%name");
        #[cfg(not(unix))]
        let target = root.child("imports/copy#mark%name");
        let import = run_ocm(
            root.path(),
            &env,
            &[
                "env",
                "import",
                "state.tar",
                "--name",
                "copy",
                "--root",
                target.to_str().unwrap(),
                "--json",
            ],
        );
        assert!(import.status.success(), "{}", stderr(&import));
        let summary: Value = serde_json::from_slice(&import.stdout).unwrap();
        let target = std::path::PathBuf::from(summary["root"].as_str().unwrap());
        let db = Connection::open_with_flags(
            target.join(".openclaw/state/openclaw.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let raw: String = db.query_row(select, [], |row| row.get(0)).unwrap();
        let actual: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            db.query_row("SELECT original FROM receipt_history", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            raw
        );
        let mut expected = expected_document;
        let expected_records = if legacy {
            &mut expected
        } else {
            &mut expected["index"]["installRecords"]
        };
        expected_records["reentered"]["installPath"] = json!(target.join("plugin"));
        expected_records["demo"]["installPath"] = json!(target.join(".openclaw/extensions/demo"));
        expected_records["demo"]["sourcePath"] = json!(target.join("packages/demo"));
        expected_records["demo"]["unknown"]["path"] = json!(target.join("historical-reference"));
        expected_records["missing"]["installPath"] =
            json!(target.join(".openclaw/extensions/missing"));
        if !legacy {
            expected["index"]["plugins"][1]["rootDir"] = json!(target.join("plugin"));
            expected["index"]["plugins"][1]["setupSource"] = json!(target.join("plugin"));
            expected["index"]["plugins"][0]["rootDir"] =
                json!(target.join(".openclaw/extensions/demo"));
            expected["index"]["plugins"][0]["source"] =
                json!(target.join(".openclaw/extensions/demo").join("index.js"));
            expected["index"]["plugins"][0]["setupSource"] =
                json!(target.join(".openclaw/extensions/demo").join("setup.js"));
            expected["index"]["plugins"][0]["manifestPath"] = json!(
                target
                    .join(".openclaw/extensions/demo")
                    .join("openclaw.plugin.json")
            );
            expected["index"]["plugins"][0]["packageJson"]["path"] = json!(
                target
                    .join(".openclaw/extensions/demo")
                    .join("package.json")
            );
            expected["index"]["workspaceDir"] = json!(target);
            expected["index"]["diagnostics"][0]["message"] = json!(format!(
                "loaded from {}",
                target.join(".openclaw/extensions/demo").display()
            ));
        }
        let source_encoded = serde_json::to_string(source.to_str().unwrap()).unwrap();
        let target_encoded = serde_json::to_string(target.to_str().unwrap()).unwrap();
        let source_encoded = &source_encoded[1..source_encoded.len() - 1];
        let target_encoded = &target_encoded[1..target_encoded.len() - 1];
        assert_eq!(
            raw.replace(target_encoded, source_encoded),
            original_document.replace(
                &serde_json::to_string(&reentered).unwrap(),
                &serde_json::to_string(&source.join("plugin")).unwrap()
            )
        );
        if legacy {
            let stored_plugins: String = db.query_row("SELECT plugins_json FROM installed_plugin_index WHERE index_key='installed-plugin-index'", [], |row| row.get(0)).unwrap();
            assert_eq!(
                stored_plugins.replace(target_encoded, source_encoded),
                plugins_raw.replace(
                    &serde_json::to_string(&reentered).unwrap(),
                    &serde_json::to_string(&source.join("plugin")).unwrap()
                )
            );
            assert!(stored_plugins.contains("1791960884534.1313"));
            let parsed: Value = serde_json::from_str(&stored_plugins).unwrap();
            assert_eq!(parsed[1]["rootDir"], json!(target.join("plugin")));
            assert_eq!(parsed[1]["setupSource"], json!(target.join("plugin")));
            assert_eq!(
                parsed[2]["setupSource"],
                json!(source.join("../outside/setup.js"))
            );
            assert_eq!(
                parsed[0]["rootDir"],
                json!(target.join(".openclaw/extensions/demo"))
            );
            assert_eq!(
                parsed[0]["source"],
                json!(target.join(".openclaw/extensions/demo").join("index.js"))
            );
            assert_eq!(
                parsed[0]["setupSource"],
                json!(target.join(".openclaw/extensions/demo").join("setup.js"))
            );
            assert_eq!(
                parsed[0]["manifestPath"],
                json!(
                    target
                        .join(".openclaw/extensions/demo")
                        .join("openclaw.plugin.json")
                )
            );
            assert_eq!(db.query_row("SELECT workspace_dir FROM installed_plugin_index WHERE index_key='installed-plugin-index'", [], |row| row.get::<_, String>(0)).unwrap(), target.join("workspace").to_str().unwrap());
            let diagnostics: String = db.query_row("SELECT diagnostics_json FROM installed_plugin_index WHERE index_key='installed-plugin-index'", [], |row| row.get(0)).unwrap();
            assert_eq!(
                diagnostics,
                diagnostics_raw.replace(source_encoded, target_encoded)
            );
        } else {
            assert!(raw.contains("1791960884534.1313"));
        }
        assert_eq!(actual, expected);
        let unrelated_query = if legacy {
            "SELECT install_records_json, updated_at FROM installed_plugin_index WHERE index_key='unrelated'"
        } else {
            "SELECT value_json, updated_at_ms FROM config_machine_state WHERE state_key='unrelated'"
        };
        assert_eq!(
            db.query_row(unrelated_query, [], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?
            )))
            .unwrap(),
            (target.display().to_string(), 43)
        );
        let timestamp_query = if legacy {
            "SELECT updated_at FROM installed_plugin_index WHERE index_key='installed-plugin-index'"
        } else {
            "SELECT updated_at_ms FROM config_machine_state WHERE state_key='plugins.installedIndex'"
        };
        assert_eq!(
            db.query_row(timestamp_query, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
        assert_eq!(
            db.query_row("SELECT value FROM durable", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "session bytes"
        );
        assert_eq!(
            db.query_row(
                "SELECT lease_id, owner_pid FROM agent_database_leases",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
            ("retained-owner".to_string(), 123)
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let mut expected_runtime = runtime;
        expected_runtime["rootDir"] = json!(target.join(".openclaw/extensions/demo"));
        expected_runtime["package"] = json!(target.join("packages/demo"));
        expected_runtime["command"] = json!(format!(
            "node {}/index.js",
            target.join(".openclaw/extensions/demo").display()
        ));
        expected_runtime["encoded"] =
            json!(json!({"rootDir":target.join(".openclaw/extensions/demo")}).to_string());
        // A bare root followed by punctuation is not a complete root component.
        assert_eq!(
            expected_runtime["shell"],
            json!(format!("cd {}; pwd", source.display()))
        );
        expected_runtime["quotedReferences"] = json!(format!(
            "paths '{} and {}/plugin'",
            external.display(),
            target.display()
        ));
        // Free-form values use literal root boundaries; their command syntax
        // and filesystem resolution are outside this text relocation contract.
        for field in [
            "quotedEscape",
            "multilineEscape",
            "escapedRoot",
            "relativeArgument",
        ] {
            expected_runtime[field] = json!(
                expected_runtime[field]
                    .as_str()
                    .unwrap()
                    .replace(source.to_str().unwrap(), target.to_str().unwrap())
            );
        }
        expected_runtime["quotedScript"] = json!(format!(
            "sh -c '{}/run --input {}/input'",
            target.display(),
            target.display()
        ));
        expected_runtime["quotedBackslash"] = json!(format!(
            "cat '{}/a\\' {}/other",
            target.display(),
            target.display()
        ));
        expected_runtime["bracketedLog"] =
            json!(format!("[INFO] loaded from {}/plugin", target.display()));
        expected_runtime["quotedOwned"] = json!(format!("cat '{}/a b/file'", target.display()));
        let expected_url =
            url::Url::from_file_path(target.join(".openclaw/extensions/demo").join("index.js"))
                .unwrap();
        expected_runtime["text"] = json!(format!(
            "{}/run --input {}/input\n{}/second",
            target.display(),
            target.display(),
            target.display()
        ));
        expected_runtime["mixedUrlAndPath"] = json!(format!(
            "echo \"url={} {}/a b/../../outside\"",
            expected_url,
            target.display()
        ));
        expected_runtime["prefixedUrl"] = json!(format!("echo \"url={}\"; pwd", expected_url));
        expected_runtime["quotedFileUrl"] = json!(format!("cat '{}'", expected_url));
        expected_runtime["urlCommand"] = json!(format!("open \"{}\"; pwd", expected_url));
        expected_runtime["decodedIncompleteProse"] =
            json!(format!("{{}}\n\"node {}/plugin", target.display()));
        expected_runtime["mixedUrlAndPlainEscape"] = json!(format!(
            "{}/a b/../../outside {}/plugin",
            url::Url::from_file_path(&source).unwrap(),
            target.display()
        ));
        expected_runtime["quotedTerminalRoot"] = json!(format!(
            "echo \"{}/a b/../../outside {}\"",
            url::Url::from_file_path(&source).unwrap(),
            target.display()
        ));
        expected_runtime["lineTerminalRoot"] = json!(format!(
            "{}/a b/../../outside {}\n",
            url::Url::from_file_path(&source).unwrap(),
            target.display()
        ));
        expected_runtime["adjacentMixedReferences"] = json!(format!(
            "{}/one,{}/two",
            url::Url::from_file_path(&target).unwrap(),
            target.display()
        ));
        expected_runtime["urlControlText"] =
            json!(["\t", "\u{b}", "\u{c}", "\u{85}"].map(|separator| {
                let uri = url::Url::from_file_path(&target).unwrap();
                format!("loaded {uri}/plugin{separator}ready")
            }));
        expected_runtime["separatedRootUrls"] = json!([" ", "\t", "\r", "\n"].map(|separator| {
            let uri = url::Url::from_file_path(&target).unwrap();
            format!("{uri}{separator}{uri}/plugin")
        }));
        expected_runtime["labelledRootUrls"] = json!(format!(
            "{} second:{}",
            url::Url::from_file_path(&target).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["decodedProse"] = json!(format!("{{}}\n{}/plugin", target.display()));
        expected_runtime["multipleUrlEscape"] = json!(format!(
            "{}/a b/../../outside\n{}/plugin",
            url::Url::from_file_path(&source).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["sameLineUrlEscape"] = json!(format!(
            "{}/a b/../../outside {}/plugin",
            url::Url::from_file_path(&source).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["quotedMultipleUrlEscape"] = json!(format!(
            "echo \"{}/a b/../../outside\n{}/plugin\"",
            url::Url::from_file_path(&source).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["reenteredUrl"] = json!(format!(
            "{}/plugin?q=keep%20bytes#part",
            url::Url::from_file_path(&target).unwrap()
        ));
        if cfg!(windows) {
            expected_runtime["backslashFileUrl"] = json!(format!(
                "{}\\other/plugin",
                url::Url::from_file_path(&target).unwrap()
            ));
        }
        expected_runtime["uriLeadingProse"] = json!(format!(
            "{}/plugin please keep these words",
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["encodedRootUrl"] = json!(format!(
            "{}/plugin?q=alpha%20beta#keep",
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["wrappedRootUrl"] =
            json!(format!("<{}>", url::Url::from_file_path(&target).unwrap()));
        expected_runtime["sameLineUrls"] = json!(format!(
            "{}/first {}/second",
            url::Url::from_file_path(&target).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["uriTextWithPlainReference"] = json!(format!(
            "{}/first {}/second",
            url::Url::from_file_path(&target).unwrap(),
            target.display()
        ));
        expected_runtime["rootUrls"] = json!(format!(
            "{}\n{}",
            url::Url::from_file_path(&target).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["multipleUrls"] = json!(format!(
            "{}/first\n{}/second",
            url::Url::from_file_path(&target).unwrap(),
            url::Url::from_file_path(&target).unwrap()
        ));
        expected_runtime["fileUrl"] = json!(expected_url.as_str());
        expected_runtime["upperFileUrl"] = json!(expected_url.as_str());
        expected_runtime["shortFileUrl"] = json!(expected_url.as_str());
        expected_runtime["localFileUrl"] = json!(expected_url.as_str());
        expected_runtime["normalized"] = json!(format!(
            "{}/sub/../index.js",
            target.join(".openclaw/extensions/demo").display()
        ));
        expected_runtime["absoluteCommand"]["command"] = json!(format!(
            "{}/run --input {}/input",
            target.display(),
            target.display()
        ));
        let actual_runtime: Value =
            serde_json::from_slice(&fs::read(target.join(runtime_file)).unwrap()).unwrap();
        assert_eq!(actual_runtime, expected_runtime);
        let surrogate_copy = fs::read_to_string(target.join(surrogate_file)).unwrap();
        assert_eq!(
            surrogate_copy,
            opaque_document(&target.join(".openclaw/extensions/demo"))
        );
        assert_eq!(
            fs::read_to_string(target.join(partial_file)).unwrap(),
            format!(
                "{}\n{{\"unfinished\":",
                json!({"path": target.join(".openclaw/extensions/demo")})
            )
        );
        assert_eq!(
            fs::read_to_string(target.join(".openclaw/logs/root.txt")).unwrap(),
            format!("{}\n", target.display())
        );
        for recovered_file in recovered_files {
            assert_eq!(
                fs::read_to_string(target.join(recovered_file)).unwrap(),
                format!(
                    "{{\"unfinished\":\n{}\n{}{}\n",
                    recovered_record(&target),
                    broken_record,
                    recovered_record(&target)
                )
            );
        }
        for line in fs::read_to_string(target.join(jsonl_file)).unwrap().lines() {
            assert_eq!(
                serde_json::from_str::<Value>(line).unwrap(),
                expected_runtime
            );
        }
        for file in untouched.into_iter().chain([
            ".openclaw/state/opaque.bin",
            ".openclaw/extensions/demo/index.js",
            "packages/demo/package.json",
        ]) {
            assert_eq!(
                fs::read(target.join(file)).unwrap(),
                fs::read(archive.root_dir.join(file)).unwrap(),
                "{file}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(target.join(".openclaw/state/openclaw.sqlite"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o400
            );
            assert_eq!(
                fs::metadata(target.join(".openclaw/state"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o500
            );
            assert_eq!(
                fs::metadata(target.join(runtime_file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o400
            );
            if legacy {
                assert_eq!(
                    fs::read_link(target.join(".openclaw/state/openclaw.sqlite")).unwrap(),
                    std::path::Path::new("index.sqlite")
                );
            }
            for base in [target.clone(), root.child("source-offline")] {
                fs::set_permissions(
                    base.join(".openclaw/state"),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
        }
        #[cfg(not(unix))]
        {
            for file in [
                target.join(".openclaw/state/openclaw.sqlite"),
                target.join(runtime_file),
            ] {
                assert!(fs::metadata(file).unwrap().permissions().readonly());
            }
            drop(db);
            for base in [target.clone(), root.child("source-offline")] {
                for file in [
                    base.join(".openclaw/state/openclaw.sqlite"),
                    base.join(runtime_file),
                ] {
                    let mut permissions = fs::metadata(&file).unwrap().permissions();
                    permissions.set_readonly(false);
                    fs::set_permissions(file, permissions).unwrap();
                }
            }
        }
        let summary: Value = serde_json::from_str(&stdout(&import)).unwrap();
        assert_eq!(summary["statePathsRewritten"], true);
    }
    assert_portable_archive_source_identities();
}

#[cfg(unix)]
#[test]
fn state_import_refuses_to_rewrite_a_database_outside_the_copy() {
    let root = TestDir::new("state-import-external-database");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let external = root.child("external.sqlite");
    let db = rusqlite::Connection::open(&external).unwrap();
    db.execute_batch(
        "CREATE TABLE config_machine_state (state_key TEXT PRIMARY KEY, value_json TEXT);",
    )
    .unwrap();
    db.execute("INSERT INTO config_machine_state VALUES ('plugins.installedIndex', ?1)",
        [serde_json::json!({"index":{"installRecords":{"demo":{"source":"path","installPath":source.join(".openclaw/extensions/demo")}}}}).to_string()]).unwrap();
    drop(db);
    let before = fs::read(&external).unwrap();
    fs::create_dir_all(source.join(".openclaw/state")).unwrap();
    std::os::unix::fs::symlink(&external, source.join(".openclaw/state/openclaw.sqlite")).unwrap();
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
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "state.tar", "--name", "copy"],
    );
    assert!(!import.status.success());
    assert!(
        stderr(&import).contains("inside the imported state"),
        "{}",
        stderr(&import)
    );
    assert_eq!(fs::read(&external).unwrap(), before);
    assert!(!root.child("ocm-home/envs/copy").exists());
    let show = run_ocm(root.path(), &env, &["env", "show", "copy"]);
    assert!(!show.status.success());
}

#[test]
fn state_import_refuses_a_receipt_conflict_without_replacing_other_rows() {
    let root = TestDir::new("state-import-receipt-conflict");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let target = root.child("ocm-home/envs/copy");
    let database = source.join(".openclaw/state/openclaw.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch("CREATE TABLE config_machine_state (state_key TEXT PRIMARY KEY, value_json TEXT UNIQUE ON CONFLICT REPLACE);").unwrap();
    let receipt = |base: &std::path::Path| {
        serde_json::json!({"index":{"installRecords":{"demo":{"installPath":base.join(".openclaw/extensions/demo")}}}}).to_string()
    };
    db.execute(
        "INSERT INTO config_machine_state VALUES ('plugins.installedIndex', ?1)",
        [receipt(&source)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO config_machine_state VALUES ('unrelated', ?1)",
        [receipt(&target)],
    )
    .unwrap();
    drop(db);
    let before = fs::read(&database).unwrap();
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
    let import = run_ocm(
        root.path(),
        &env,
        &["env", "import", "state.tar", "--name", "copy"],
    );
    assert!(!import.status.success());
    assert!(
        stderr(&import).contains("UNIQUE constraint failed"),
        "{}",
        stderr(&import)
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    assert!(!target.exists());
    assert!(
        !run_ocm(root.path(), &env, &["env", "show", "copy"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn state_import_bounds_directory_handles_for_deep_runtime_state() {
    // Allow the 111-listener Gateway probe plus fixed overhead, while the
    // directory depth still exceeds the descriptor limit to catch retained handles.
    let root = TestDir::new("state-import-deep-runtime");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let target = root.child("ocm-home/envs/copy");
    let relative: std::path::PathBuf = std::iter::once(".openclaw/agents/main/sessions")
        .chain(std::iter::repeat_n("d", 320))
        .collect();
    write_text(
        &source.join(&relative).join("path.json"),
        &serde_json::json!({"cwd": source.join(".openclaw/workspace")}).to_string(),
    );
    write_text(
        &source.join(&relative).join("notes.txt"),
        "unchanged payload\n",
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
            "deep.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", "ulimit -n 256 && exec \"$@\"", "import-limit"])
        .arg(support::ocm_test_binary_path())
        .args(["env", "import", "deep.tar", "--name", "copy"])
        .current_dir(root.path())
        .env_clear()
        .envs(&env)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let actual: serde_json::Value =
        serde_json::from_slice(&fs::read(target.join(&relative).join("path.json")).unwrap())
            .unwrap();
    assert_eq!(
        actual["cwd"],
        serde_json::json!(target.join(".openclaw/workspace"))
    );
    assert_eq!(
        fs::read_to_string(target.join(&relative).join("notes.txt")).unwrap(),
        "unchanged payload\n"
    );
}

#[cfg(unix)]
#[test]
fn state_archive_relocates_both_root_identities_after_the_source_disappears() {
    use ocm::infra::archive::{EnvArchiveMetadata, extract_env_archive};
    use rusqlite::Connection;
    use serde_json::{Value, json};
    use std::os::unix::{fs::PermissionsExt, fs::symlink};

    for nested_alias in [false, true] {
        let root = TestDir::new("state-archive-root-identities");
        let env = ocm_env(&root);
        let fixture = fs::canonicalize(root.path()).unwrap();
        let physical_parent = fixture.join("physical");
        let canonical = physical_parent.join("source");
        fs::create_dir_all(&canonical).unwrap();
        let alias = if nested_alias {
            canonical.join("alias")
        } else {
            fixture.join("alias")
        };
        if !nested_alias {
            symlink(&physical_parent, &alias).unwrap();
        }
        let lexical = alias.join("source");
        let create = run_ocm(
            root.path(),
            &env,
            &[
                "env",
                "create",
                "source",
                "--root",
                lexical.to_str().unwrap(),
            ],
        );
        assert!(create.status.success(), "{}", stderr(&create));
        if nested_alias {
            // Register the initially empty root, then retain its files behind
            // an alias whose spelling is below its canonical source root.
            for entry in fs::read_dir(&lexical).unwrap() {
                let entry = entry.unwrap();
                fs::rename(entry.path(), canonical.join(entry.file_name())).unwrap();
            }
            fs::remove_dir_all(&alias).unwrap();
            symlink(&physical_parent, &alias).unwrap();
        }
        let state = canonical.join(".openclaw");
        fs::create_dir_all(state.join("custom-workspace")).unwrap();
        let config = json!({
            "plugin": {"$include": "extra.json"},
            "agents": {"defaults": {"workspace": canonical.join(".openclaw/custom-workspace")},
                "entries": {"main": {"agentDir": lexical.join(".openclaw/private-agent")}}}
        })
        .to_string();
        write_text(&state.join("openclaw.json"), &config);
        let include =
            json!({"plugin": {"note": canonical.join("included-provenance")}}).to_string();
        write_text(&state.join("extra.json"), &include);
        let custom = state.join("private-agent/custom.sqlite");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        let global = state.join("state/openclaw.sqlite");
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        let registry = Connection::open(&global).unwrap();
        registry
            .execute_batch("CREATE TABLE agent_databases (path TEXT);")
            .unwrap();
        registry
            .execute(
                "INSERT INTO agent_databases VALUES (?1)",
                [canonical
                    .join(".openclaw/private-agent/custom.sqlite")
                    .to_str()
                    .unwrap()],
            )
            .unwrap();
        drop(registry);
        let db = Connection::open(&custom).unwrap();
        db.execute_batch("CREATE TABLE session_nodes (session_key TEXT PRIMARY KEY, entry_json TEXT, payload BLOB);
            CREATE TABLE board_widgets (name TEXT PRIMARY KEY, descriptor_json TEXT);
            CREATE TABLE board_tabs (title TEXT);").unwrap();
        let external = fixture.join("external");
        let sibling = fixture.join("physical/source-sibling");
        for (name, path) in [
            ("lexical", &lexical),
            ("canonical", &canonical),
            ("external", &external),
            ("sibling", &sibling),
        ] {
            let row = json!({"sessionRoot": path.join(".openclaw/custom-workspace"), "spawnedCwd": path.join(".openclaw/custom-workspace")}).to_string();
            db.execute(
                "INSERT INTO session_nodes VALUES (?1, ?2, ?3)",
                rusqlite::params![name, row, row.as_bytes()],
            )
            .unwrap();
        }
        let authored = json!({"path": canonical.join("board-authored"), "command": format!("cat {}/fixture", lexical.display())}).to_string();
        db.execute("INSERT INTO board_widgets VALUES ('main', ?1)", [&authored])
            .unwrap();
        db.execute("INSERT INTO board_tabs VALUES (?1)", [&authored])
            .unwrap();
        drop(db);
        let runtime = json!({"paths": [lexical.join("active"), canonical.join("active")],
            "command": format!("cat {}/first {}/second", lexical.display(), canonical.display()),
            "urls": format!("{} {}", url::Url::from_file_path(lexical.join("active")).unwrap(), url::Url::from_file_path(canonical.join("active")).unwrap())}).to_string();
        write_text(
            &state.join("agents/main/sessions/legacy.jsonl"),
            &(runtime + "\n"),
        );
        for file in [
            "custom-workspace/notes.json",
            "skill-workshop/receipt.json",
            "logs/config-audit.jsonl",
        ] {
            write_text(&state.join(file), &authored);
        }
        fs::write(state.join("opaque.bin"), [0xff, 0, 0x81]).unwrap();
        let protected = [
            "openclaw.json",
            "extra.json",
            "custom-workspace/notes.json",
            "skill-workshop/receipt.json",
            "logs/config-audit.jsonl",
            "opaque.bin",
            "state/openclaw.sqlite",
            "private-agent/custom.sqlite",
        ];
        let before: Vec<_> = protected
            .iter()
            .map(|path| fs::read(state.join(path)).unwrap())
            .collect();
        let archive = fixture.join("state.tar");
        let export = run_ocm(
            root.path(),
            &env,
            &[
                "env",
                "export",
                "source",
                "--include-state",
                "--output",
                archive.to_str().unwrap(),
                "--json",
            ],
        );
        assert!(export.status.success(), "{}", stderr(&export));
        assert_eq!(
            fs::metadata(&archive).unwrap().permissions().mode() & 0o077,
            0
        );
        for (path, bytes) in protected.iter().zip(&before) {
            assert_eq!(fs::read(state.join(path)).unwrap(), *bytes, "source {path}");
        }
        let extracted =
            extract_env_archive::<EnvArchiveMetadata>(&archive, &fixture.join("extracted"))
                .unwrap();
        assert_eq!(extracted.metadata.format_version, 1);
        assert_eq!(
            extracted.metadata.env.source_root.as_deref(),
            lexical.to_str()
        );
        assert_eq!(
            extracted.metadata.env.canonical_source_root.as_deref(),
            canonical.to_str()
        );
        for (path, bytes) in protected[..6].iter().zip(&before) {
            assert_eq!(
                fs::read(extracted.root_dir.join(".openclaw").join(path)).unwrap(),
                *bytes,
                "archive {path}"
            );
        }
        if !nested_alias {
            // An optional alias identity cannot turn an absent or relative
            // original root into ownership of arbitrary configuration strings.
            for (index, invalid_root) in [
                None,
                Some(""),
                Some("relative"),
                Some(r"C:relative"),
                Some(r"\root-relative"),
                Some(r"\\server"),
            ]
            .into_iter()
            .enumerate()
            {
                let mut metadata = extracted.metadata.clone();
                metadata.env.source_root = invalid_root.map(str::to_string);
                let invalid_archive = fixture.join(format!("invalid-{index}.tar"));
                ocm::infra::archive::write_env_archive(
                    &metadata,
                    &extracted.root_dir,
                    &invalid_archive,
                )
                .unwrap();
                let invalid_target = fixture.join("invalid-import");
                let rejected = run_ocm(
                    root.path(),
                    &env,
                    &[
                        "env",
                        "import",
                        invalid_archive.to_str().unwrap(),
                        "--name",
                        "invalid",
                        "--root",
                        invalid_target.to_str().unwrap(),
                    ],
                );
                assert!(!rejected.status.success());
                assert!(
                    stderr(&rejected).contains("absolute sourceRoot"),
                    "{}",
                    stderr(&rejected)
                );
                assert!(!invalid_target.exists());
            }
        }
        fs::remove_dir_all(&canonical).unwrap();
        if !nested_alias {
            fs::remove_file(&alias).unwrap();
        }
        assert!(!lexical.exists() && !canonical.exists());
        // The nested destination also catches cascading replacement of an
        // already relocated value by the shorter canonical source prefix.
        let target = if nested_alias {
            canonical.join("restored")
        } else {
            fixture.join("restored")
        };
        let import = run_ocm(
            root.path(),
            &env,
            &[
                "env",
                "import",
                archive.to_str().unwrap(),
                "--name",
                "restored",
                "--root",
                target.to_str().unwrap(),
                "--json",
            ],
        );
        assert!(import.status.success(), "{}", stderr(&import));
        let imported_state = target.join(".openclaw");
        let config: Value =
            serde_json::from_slice(&fs::read(imported_state.join("openclaw.json")).unwrap())
                .unwrap();
        assert_eq!(
            config["agents"]["defaults"]["workspace"],
            target.join(".openclaw/custom-workspace").to_str().unwrap()
        );
        assert_eq!(
            config["agents"]["entries"]["main"]["agentDir"],
            target.join(".openclaw/private-agent").to_str().unwrap()
        );
        // Active include values follow the root config's import relocation;
        // export and unrelated authored provenance remain byte-preserving.
        let included: Value =
            serde_json::from_slice(&fs::read(imported_state.join("extra.json")).unwrap()).unwrap();
        assert_eq!(
            included["plugin"]["note"],
            target.join("included-provenance").to_str().unwrap()
        );
        for (path, bytes) in protected[2..6].iter().zip(&before[2..6]) {
            assert_eq!(
                fs::read(imported_state.join(path)).unwrap(),
                *bytes,
                "import {path}"
            );
        }
        let db = Connection::open(imported_state.join("private-agent/custom.sqlite")).unwrap();
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        for (name, original, expected) in [
            ("lexical", &lexical, &target),
            ("canonical", &canonical, &target),
            ("external", &external, &external),
            ("sibling", &sibling, &sibling),
        ] {
            let (raw, blob): (String, Vec<u8>) = db
                .query_row(
                    "SELECT entry_json, payload FROM session_nodes WHERE session_key = ?1",
                    [name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let value: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                value["spawnedCwd"],
                expected
                    .join(".openclaw/custom-workspace")
                    .to_str()
                    .unwrap(),
                "{name}"
            );
            let original = json!({"sessionRoot": original.join(".openclaw/custom-workspace"), "spawnedCwd": original.join(".openclaw/custom-workspace")}).to_string();
            assert_eq!(blob, original.as_bytes());
        }
        for (table, column) in [
            ("board_widgets", "descriptor_json"),
            ("board_tabs", "title"),
        ] {
            assert_eq!(
                db.query_row(&format!("SELECT {column} FROM {table}"), [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
                authored
            );
        }
        let registry = Connection::open(imported_state.join("state/openclaw.sqlite")).unwrap();
        assert_eq!(
            registry
                .query_row("SELECT path FROM agent_databases", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            target
                .join(".openclaw/private-agent/custom.sqlite")
                .to_str()
                .unwrap()
        );
        let legacy: Value = serde_json::from_str(
            &fs::read_to_string(imported_state.join("agents/main/sessions/legacy.jsonl")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            legacy["paths"],
            json!([target.join("active"), target.join("active")])
        );
        assert_eq!(
            legacy["command"],
            format!("cat {}/first {}/second", target.display(), target.display())
        );
        let url = url::Url::from_file_path(target.join("active"))
            .unwrap()
            .to_string();
        assert_eq!(legacy["urls"], format!("{url} {url}"));
    }
}

// Called by the existing relocation test selected by native Windows CI as well
// as the full POSIX suite. The payload is portable; only source syntax differs.
fn assert_portable_archive_source_identities() {
    use ocm::infra::archive::{EnvArchiveMetadata, extract_env_archive, write_env_archive};
    use serde_json::Value;

    let root = TestDir::new("archive-source-host-syntax");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    write_text(
        &source.join(".openclaw/openclaw.json"),
        r#"{"marker":"relative-literal"}"#,
    );
    write_text(
        &source.join(".openclaw/workspace/notes.txt"),
        "portable bytes\n",
    );
    let archive = root.child("original.tar");
    let export = run_ocm(
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
    assert!(export.status.success(), "{}", stderr(&export));
    let extracted =
        extract_env_archive::<EnvArchiveMetadata>(&archive, &root.child("extracted")).unwrap();
    let default_archive = root.child("default.tar");
    let default_export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            default_archive.to_str().unwrap(),
        ],
    );
    assert!(
        default_export.status.success(),
        "{}",
        stderr(&default_export)
    );
    let default_extracted = extract_env_archive::<EnvArchiveMetadata>(
        &default_archive,
        &root.child("default-extracted"),
    )
    .unwrap();
    assert!(!default_extracted.metadata.include_state);
    assert_eq!(
        default_extracted.metadata.env.canonical_source_root,
        Some(fs::canonicalize(&source).unwrap().display().to_string())
    );
    assert_eq!(
        fs::read(default_extracted.root_dir.join(".openclaw/openclaw.json")).unwrap(),
        fs::read(source.join(".openclaw/openclaw.json")).unwrap()
    );
    for (label, source_root, canonical) in [
        ("posix", "/portable/source", "/portable/physical"),
        ("drive", r"C:\portable\source", r"\\?\C:\portable\physical"),
        (
            "unc",
            r"\\server\share\source",
            r"\\?\UNC\server\share\physical",
        ),
    ] {
        for extended in [false, true] {
            let mut metadata = extracted.metadata.clone();
            metadata.env.source_root = Some(source_root.to_string());
            metadata.env.canonical_source_root = extended.then(|| canonical.to_string());
            #[cfg(windows)]
            let native_records = if label == "posix" {
                Vec::new()
            } else {
                let ordinary = if label == "drive" {
                    r"C:\portable\physical"
                } else {
                    r"\\server\share\physical"
                };
                let rows = vec![
                    ("lexical", format!(r"{source_root}\active")),
                    ("canonical-extended", format!(r"{canonical}\active")),
                    ("canonical-node", format!(r"{ordinary}\active")),
                    ("sibling", format!(r"{ordinary}-sibling\active")),
                ];
                let database = extracted.root_dir.join(".openclaw/state/openclaw.sqlite");
                fs::create_dir_all(database.parent().unwrap()).unwrap();
                let db = rusqlite::Connection::open(database).unwrap();
                db.execute_batch("DROP TABLE IF EXISTS paths; CREATE TABLE paths (id TEXT PRIMARY KEY, path TEXT, payload BLOB);").unwrap();
                for (id, value) in &rows {
                    db.execute(
                        "INSERT INTO paths VALUES (?1, ?2, ?3)",
                        rusqlite::params![id, value, value.as_bytes()],
                    )
                    .unwrap();
                }
                rows
            };
            let name = format!("{label}-{extended}");
            let archive = root.child(format!("{name}.tar"));
            write_env_archive(&metadata, &extracted.root_dir, &archive).unwrap();
            let target = root.child(format!("import-{name}"));
            let import = run_ocm(
                root.path(),
                &env,
                &[
                    "env",
                    "import",
                    archive.to_str().unwrap(),
                    "--name",
                    &name,
                    "--root",
                    target.to_str().unwrap(),
                    "--json",
                ],
            );
            assert!(import.status.success(), "{name}: {}", stderr(&import));
            let config: Value =
                serde_json::from_slice(&fs::read(target.join(".openclaw/openclaw.json")).unwrap())
                    .unwrap();
            assert_eq!(config["marker"], "relative-literal");
            #[cfg(windows)]
            if !native_records.is_empty() {
                let db = rusqlite::Connection::open(target.join(".openclaw/state/openclaw.sqlite"))
                    .unwrap();
                for (id, original) in native_records {
                    let (value, payload): (String, Vec<u8>) = db
                        .query_row(
                            "SELECT path, payload FROM paths WHERE id = ?1",
                            [id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .unwrap();
                    let relocated = id == "lexical" || (extended && id.starts_with("canonical-"));
                    let expected = if relocated {
                        target.join("active").to_str().unwrap().to_string()
                    } else {
                        original.clone()
                    };
                    assert_eq!(value, expected, "{name}: {id}");
                    assert_eq!(payload, original.as_bytes());
                }
            }
            assert_eq!(
                fs::read(target.join(".openclaw/workspace/notes.txt")).unwrap(),
                b"portable bytes\n"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn state_import_preserves_workspaces_reached_through_other_live_aliases() {
    use serde_json::{Value, json};
    use std::os::unix::fs::symlink;

    let root = TestDir::new("archive-other-live-alias");
    let env = ocm_env(&root);
    let fixture = fs::canonicalize(root.path()).unwrap();
    let physical = fixture.join("physical");
    let external = fixture.join("external");
    fs::create_dir_all(&physical).unwrap();
    fs::create_dir_all(external.join("source")).unwrap();
    let accepted = fixture.join("accepted");
    let alternate = fixture.join("alternate");
    let outside = fixture.join("outside-alias");
    symlink(&physical, &accepted).unwrap();
    symlink(&physical, &alternate).unwrap();
    symlink(&external, &outside).unwrap();
    let source = accepted.join("source");
    let create = run_ocm(
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
    assert!(create.status.success(), "{}", stderr(&create));
    let state = physical.join("source/.openclaw");
    let original = format!(
        "keep {}/source/authored and {}/authored\n",
        physical.display(),
        source.display()
    );
    write_text(&state.join("custom-workspace/notes.txt"), &original);
    let external_path = outside.join("source/data.txt");
    write_text(&external_path, "external bytes");
    let config = json!({"agents":{"defaults":{"workspace":alternate.join("source/.openclaw/custom-workspace")}}, "plugin":{"path":external_path}}).to_string();
    write_text(&state.join("openclaw.json"), &config);
    let archive = fixture.join("state.tar");
    let export = run_ocm(
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
    assert!(export.status.success(), "{}", stderr(&export));
    // The accepted alias now points at unrelated data. Only the recorded
    // canonical source can establish ownership through the other live alias.
    fs::remove_file(&accepted).unwrap();
    symlink(&external, &accepted).unwrap();
    let target = fixture.join("imported");
    let import = run_ocm(
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
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let imported: Value =
        serde_json::from_slice(&fs::read(target.join(".openclaw/openclaw.json")).unwrap()).unwrap();
    assert_eq!(
        imported["agents"]["defaults"]["workspace"],
        target.join(".openclaw/custom-workspace").to_str().unwrap()
    );
    assert_eq!(imported["plugin"]["path"], external_path.to_str().unwrap());
    assert_eq!(
        fs::read_to_string(target.join(".openclaw/custom-workspace/notes.txt")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(state.join("custom-workspace/notes.txt")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(state.join("openclaw.json")).unwrap(),
        config
    );
    assert_eq!(fs::read_to_string(external_path).unwrap(), "external bytes");
}
