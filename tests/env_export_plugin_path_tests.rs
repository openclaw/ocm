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
        let name = format!("target-{index}");
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
        let import = run_ocm(
            root.path(),
            &env,
            &["env", "import", &archive, "--name", &name],
        );
        assert!(import.status.success(), "{}", stderr(&import));
        let target = root.child(format!("ocm-home/envs/{name}"));
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
    fs::remove_dir_all(root.child("outside")).unwrap();
    assert!(!target.join("external/value").exists());
}

#[test]
fn state_import_honors_an_explicit_sandbox_origin_without_relocating_paths() {
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
    assert_eq!(actual, expected);
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
}

#[cfg(unix)]
#[test]
fn state_import_cleans_readonly_directories_when_an_explicit_override_fails() {
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
    let retry = run_ocm(
        root.path(),
        &env,
        &["env", "import", "readonly.tar", "--name", "target"],
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
