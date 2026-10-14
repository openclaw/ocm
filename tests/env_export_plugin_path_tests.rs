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
fn state_import_relocates_config_and_preserves_archive_state_bytes() {
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
    for path in retained
        .into_iter()
        .chain([".openclaw/state/openclaw.sqlite"])
    {
        assert_eq!(
            fs::read(target.join(path)).unwrap(),
            fs::read(extracted.root_dir.join(path)).unwrap(),
            "{path}"
        );
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
