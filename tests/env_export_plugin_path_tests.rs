mod support;

use std::fs;
use std::path::Path;

use ocm::infra::archive::{EnvArchiveMetadata, extract_env_archive};
use serde_json::{Value, json};

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout, write_text};

#[test]
fn state_export_preserves_plugin_selection_across_environment_identity_changes() {
    for include_scope in [
        None,
        Some("/plugins"),
        Some("/plugins/load"),
        Some("/plugins/load/paths"),
    ] {
        let root = TestDir::new("env-export-plugin-identity");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        env.insert("SYNTHETIC_SECRET".to_string(), "not-for-config".to_string());
        env.insert("OCM_ACTIVE_ENV".to_string(), "stale-name".to_string());
        env.insert("OPENCLAW_GATEWAY_PORT".to_string(), "9999".to_string());
        let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
        assert!(create.status.success(), "{}", stderr(&create));

        let source = root.child("ocm-home/envs/source");
        let templates = json!([
            "${OPENCLAW_STATE_DIR}/plugins/${OCM_ACTIVE_ENV}",
            "${OCM_ACTIVE_ENV_ROOT}/.openclaw/plugins/port-${OPENCLAW_GATEWAY_PORT}",
            "${OPENCLAW_STATE_DIR}/plugins/${PLUGIN_DIRECTORY}"
        ]);
        let selected = ["source", "port-19789", "configured"];
        for directory in selected {
            write_text(
                &source.join(format!(".openclaw/plugins/{directory}/index.js")),
                directory,
            );
        }
        let unrelated = json!({
            "path": templates[0],
            "secret": "${SYNTHETIC_SECRET}"
        });
        let mut config = json!({
            "env": {"PLUGIN_DIRECTORY": "configured", "UNRELATED": templates[0]},
            "plugins": {"load": {"paths": templates}, "entries": {"example": {"config": unrelated}}}
        });
        let owner_relative = if let Some(scope) = include_scope {
            let owned = config.pointer(scope).unwrap().clone();
            *config.pointer_mut(scope).unwrap() = json!({"$include": "config/plugins.json5"});
            write_text(
                &source.join(".openclaw/config/plugins.json5"),
                &owned.to_string(),
            );
            ".openclaw/config/plugins.json5"
        } else {
            ".openclaw/openclaw.json"
        };
        write_text(&source.join(".openclaw/openclaw.json"), &config.to_string());
        let source_config = fs::read(source.join(".openclaw/openclaw.json")).unwrap();
        let source_owner = fs::read(source.join(owner_relative)).unwrap();

        for (archive, include_state) in [("default.tar", false), ("state.tar", true)] {
            let mut args = vec!["env", "export", "source", "--output", archive];
            if include_state {
                args.push("--include-state");
            }
            let export = run_ocm(&cwd, &env, &args);
            assert!(
                export.status.success(),
                "{include_scope:?}: {}",
                stderr(&export)
            );
            let extracted = extract_env_archive::<EnvArchiveMetadata>(
                &cwd.join(archive),
                &root.child(format!("extract-{archive}")),
            )
            .unwrap();
            let owner = read_json(&extracted.root_dir.join(owner_relative));
            let pointer = match include_scope {
                None => "/plugins/load/paths",
                Some("/plugins") => "/load/paths",
                Some("/plugins/load") => "/paths",
                Some("/plugins/load/paths") => "",
                _ => unreachable!(),
            };
            if include_state {
                let expected: Vec<_> = selected
                    .iter()
                    .map(|directory| source.join(format!(".openclaw/plugins/{directory}")))
                    .collect();
                assert_eq!(owner.pointer(pointer).unwrap(), &json!(expected));
            } else {
                assert_eq!(owner.pointer(pointer).unwrap(), &templates);
                assert_eq!(
                    fs::read(extracted.root_dir.join(owner_relative)).unwrap(),
                    source_owner
                );
            }
        }
        assert_eq!(
            fs::read(source.join(".openclaw/openclaw.json")).unwrap(),
            source_config
        );
        assert_eq!(fs::read(source.join(owner_relative)).unwrap(), source_owner);

        fs::rename(&source, root.child("source-offline")).unwrap();
        let import = run_ocm(
            &cwd,
            &env,
            &["env", "import", "state.tar", "--name", "restored"],
        );
        assert!(
            import.status.success(),
            "{include_scope:?}: {}",
            stderr(&import)
        );
        let show = run_ocm(&cwd, &env, &["env", "show", "restored", "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        let summary: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_ne!(summary["gatewayPort"].as_u64(), Some(19789));

        let target = root.child("ocm-home/envs/restored");
        let mut config = read_json(&target.join(".openclaw/openclaw.json"));
        if let Some(scope) = include_scope {
            assert_eq!(
                config.pointer(scope).unwrap()["$include"],
                "config/plugins.json5"
            );
            *config.pointer_mut(scope).unwrap() = read_json(&target.join(owner_relative));
        }
        let expected: Vec<_> = selected
            .iter()
            .map(|directory| target.join(format!(".openclaw/plugins/{directory}")))
            .collect();
        assert_eq!(config["plugins"]["load"]["paths"], json!(expected));
        assert_eq!(config["plugins"]["entries"]["example"]["config"], unrelated);
        assert_eq!(config["env"]["UNRELATED"], templates[0]);
        for (path, content) in expected.iter().zip(selected) {
            assert_eq!(fs::read_to_string(path.join("index.js")).unwrap(), content);
        }
        let export = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "export",
                "restored",
                "--include-state",
                "--output",
                "restored.tar",
            ],
        );
        assert!(
            export.status.success(),
            "{include_scope:?}: {}",
            stderr(&export)
        );
    }
}

#[test]
fn state_export_rejects_plugin_path_rewrites_shared_with_unrelated_config() {
    let root = TestDir::new("env-export-shared-plugin-config");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source.join("plugins/source/index.js"),
        "export default {};",
    );
    let shared = r#"["${OPENCLAW_STATE_DIR}/plugins/${OCM_ACTIVE_ENV}"]"#;
    write_text(&source.join("shared.json"), shared);
    let config = json!({
        "plugins": {
            "load": {"paths": {"$include": "shared.json"}},
            "entries": {"example": {"config": {"$include": "shared.json"}}}
        }
    });
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
    assert!(!export.status.success());
    assert!(stderr(&export).contains("shared with unrelated configuration"));
    assert!(!root.child("state.tar").exists());
    assert_eq!(
        fs::read_to_string(source.join("shared.json")).unwrap(),
        shared
    );
    assert_eq!(read_json(&source.join("openclaw.json")), config);
}

fn read_json(path: &Path) -> Value {
    json5::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn state_export_rejects_caller_relative_plugin_selection_without_changing_default() {
    let root = TestDir::new("export-relative-plugin-config");
    let env = ocm_env(&root);
    let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    write_text(
        &source.join("workspace/plugins/demo/index.js"),
        "export default {};",
    );
    write_text(
        &source.join(".openclaw/openclaw.json"),
        r#"{"plugins":{"load":{"paths":["plugins/demo"]}}}"#,
    );
    for cwd in [source.clone(), source.join("workspace")] {
        let output = cwd.join("state.tar");
        let export = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "export",
                "source",
                "--include-state",
                "--output",
                output.to_str().unwrap(),
            ],
        );
        assert!(!export.status.success());
        assert!(
            stderr(&export).contains("ambiguous relative plugin path"),
            "{}",
            stderr(&export)
        );
        assert!(!output.exists());
    }
    let export = run_ocm(
        root.path(),
        &env,
        &["env", "export", "source", "--output", "default.tar"],
    );
    assert!(export.status.success(), "{}", stderr(&export));
}

#[cfg(unix)]
#[test]
fn state_export_keeps_root_file_entrypoints_and_checks_retained_sibling_links() {
    for absolute in [false, true] {
        let root = TestDir::new("export-root-file-plugin");
        let env = ocm_env(&root);
        let create = run_ocm(root.path(), &env, &["env", "create", "source"]);
        assert!(create.status.success(), "{}", stderr(&create));
        let source = root.child("ocm-home/envs/source");
        write_text(&source.join("entry.mjs"), "import './impl.mjs';");
        write_text(&source.join("actual.mjs"), "export default {};");
        fs::create_dir_all(source.join(".openclaw/memory")).unwrap();
        write_text(&source.join(".openclaw/memory/unused"), "discarded");
        std::os::unix::fs::symlink(
            if absolute {
                source.join("actual.mjs")
            } else {
                "actual.mjs".into()
            },
            source.join("impl.mjs"),
        )
        .unwrap();
        write_text(
            &source.join(".openclaw/openclaw.json"),
            &json!({"plugins":{"load":{"paths":[source.join("entry.mjs")]}}}).to_string(),
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
        assert_eq!(export.status.success(), !absolute, "{}", stderr(&export));
        if absolute {
            assert!(stderr(&export).contains("absolute symlink"));
        } else {
            fs::rename(&source, root.child("source-offline")).unwrap();
            let import = run_ocm(
                root.path(),
                &env,
                &["env", "import", "state.tar", "--name", "target"],
            );
            assert!(import.status.success(), "{}", stderr(&import));
            assert!(root.child("ocm-home/envs/target/impl.mjs").is_file());
            assert!(!root.child("ocm-home/envs/target/.openclaw/memory").exists());
        }
    }
}
