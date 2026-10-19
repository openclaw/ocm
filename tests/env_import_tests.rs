mod support;

use std::{fs, path::Path};

use serde_json::Value;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout, write_text};

#[cfg(unix)]
#[test]
fn env_import_rejects_linked_archive_structure_before_reading_external_files() {
    for linked_entry in ["root", "meta", "meta/env.json"] {
        let root = TestDir::new("env-import-linked-structure");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        let external = root.child("external");
        fs::create_dir_all(&external).unwrap();
        write_text(&external.join("sentinel.txt"), "external fixture contents");
        let metadata = serde_json::json!({
            "kind": "ocm-env-archive",
            "formatVersion": 1,
            "exportedAt": "2026-09-15T00:00:00Z",
            "env": {
                "name": "source",
                "gatewayPort": 19789,
                "protected": false,
                "createdAt": "2026-09-15T00:00:00Z",
                "updatedAt": "2026-09-15T00:00:00Z"
            }
        });
        let metadata = serde_json::to_vec(&metadata).unwrap();
        fs::write(external.join("env.json"), &metadata).unwrap();
        let archive_path = cwd.join("linked.tar");
        let mut archive = tar::Builder::new(fs::File::create(&archive_path).unwrap());
        if !linked_entry.starts_with("meta") {
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive
                .append_data(&mut header, "meta/env.json", metadata.as_slice())
                .unwrap();
        }
        if linked_entry != "root" {
            let empty_root = root.child("empty-root");
            fs::create_dir_all(&empty_root).unwrap();
            archive.append_dir("root", &empty_root).unwrap();
        }
        let target = if linked_entry == "meta/env.json" {
            external.join("env.json")
        } else {
            external.clone()
        };
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        archive
            .append_link(&mut header, linked_entry, &target)
            .unwrap();
        archive.finish().unwrap();
        drop(archive);

        let imported = run_ocm(
            &cwd,
            &env,
            &["env", "import", "linked.tar", "--name", "target"],
        );
        assert!(!imported.status.success(), "accepted linked {linked_entry}");
        assert!(
            stderr(&imported).contains("archive entry"),
            "{}",
            stderr(&imported)
        );
        assert!(!root.child("ocm-home/envs/target").exists());
        assert_eq!(fs::read(external.join("env.json")).unwrap(), metadata);
        assert_eq!(
            fs::read_to_string(external.join("sentinel.txt")).unwrap(),
            "external fixture contents"
        );
    }
}

#[test]
fn env_import_relocates_absolute_includes_without_the_original_source() {
    for include_state in [false, true] {
        for spelling in ["relative", "absolute", "normalized", "overlapping"] {
            let absolute = spelling != "relative";
            let root = TestDir::new("env-import-config-includes");
            let cwd = root.child("cwd");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
            assert!(create.status.success(), "{}", stderr(&create));
            let source = root.child("ocm-home/envs/source");
            let state = source.join(".openclaw");
            let include_root = if spelling == "normalized" {
                source.join("../source/.openclaw")
            } else {
                state.clone()
            };
            let include = |relative: &str| {
                if absolute {
                    include_root.join(relative).to_string_lossy().into_owned()
                } else {
                    format!("./{relative}")
                }
            };
            let config = serde_json::json!({
                "plugins": {"$include": [include("config/plugins.json5"), include("config/shared.json")]}
            }).to_string();
            let nested = serde_json::json!({
                "$include": if absolute { include("config/shared.json") } else { "./shared.json".into() }
            }).to_string();
            write_text(&state.join("openclaw.json"), &config);
            write_text(&state.join("config/plugins.json5"), &nested);
            write_text(&state.join("config/shared.json"), "{\"enabled\": false}\n");
            #[cfg(unix)]
            if include_state && absolute {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    state.join("config/plugins.json5"),
                    fs::Permissions::from_mode(0o400),
                )
                .unwrap();
                fs::set_permissions(state.join("config"), fs::Permissions::from_mode(0o500))
                    .unwrap();
            }
            let mut args = vec!["env", "export", "source", "--output", "archive.tar"];
            if include_state {
                args.push("--include-state");
            }
            let export = run_ocm(&cwd, &env, &args);
            assert!(export.status.success(), "{}", stderr(&export));
            let archive_before = fs::read(cwd.join("archive.tar")).unwrap();
            let offline = root.child("source-offline");
            fs::rename(&source, &offline).unwrap();
            let target_root = if spelling == "overlapping" {
                source.join("target")
            } else {
                root.child("ocm-home/envs/target")
            };
            let mut import_env = env.clone();
            if spelling == "overlapping" {
                import_env.insert(
                    "OCM_HOME".into(),
                    root.child("import-store").display().to_string(),
                );
            }
            let import = run_ocm(
                &cwd,
                &import_env,
                &[
                    "env",
                    "import",
                    "archive.tar",
                    "--name",
                    "target",
                    "--root",
                    target_root.to_str().unwrap(),
                ],
            );
            assert!(import.status.success(), "{}", stderr(&import));
            let target = target_root.join(".openclaw");
            let imported: Value =
                serde_json::from_str(&fs::read_to_string(target.join("openclaw.json")).unwrap())
                    .unwrap();
            let imported_nested: Value = serde_json::from_str(
                &fs::read_to_string(target.join("config/plugins.json5")).unwrap(),
            )
            .unwrap();
            if absolute {
                assert_eq!(
                    Path::new(imported["plugins"]["$include"][0].as_str().unwrap()),
                    target.join("config/plugins.json5")
                );
                assert_eq!(
                    Path::new(imported["plugins"]["$include"][1].as_str().unwrap()),
                    target.join("config/shared.json")
                );
                assert_eq!(
                    Path::new(imported_nested["$include"].as_str().unwrap()),
                    target.join("config/shared.json")
                );
            } else {
                assert_eq!(imported["plugins"]["$include"][0], "./config/plugins.json5");
                assert_eq!(imported["plugins"]["$include"][1], "./config/shared.json");
                assert_eq!(imported_nested["$include"], "./shared.json");
            }
            assert_eq!(
                fs::read_to_string(target.join("config/shared.json")).unwrap(),
                "{\"enabled\": false}\n"
            );
            assert_eq!(
                fs::read_to_string(offline.join(".openclaw/openclaw.json")).unwrap(),
                config
            );
            assert_eq!(
                fs::read_to_string(offline.join(".openclaw/config/plugins.json5")).unwrap(),
                nested
            );
            assert_eq!(fs::read(cwd.join("archive.tar")).unwrap(), archive_before);
            assert!(!source.join(".openclaw").exists());
            #[cfg(unix)]
            if include_state && absolute {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(target.join("config/plugins.json5"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o400
                );
                for parent in [target.join("config"), offline.join(".openclaw/config")] {
                    assert_eq!(
                        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
                        0o500
                    );
                    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn env_import_preserves_readonly_include_symlink_referents() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = TestDir::new("env-import-include-symlink-mode");
    let cwd = root.child("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let state = source.join(".openclaw");
    write_text(
        &state.join("openclaw.json"),
        r#"{"plugins":{"$include":"./config/alias.json"}}"#,
    );
    let fragment = serde_json::json!({"$include": state.join("shared.json")}).to_string();
    write_text(&state.join("config/fragment.json"), &fragment);
    write_text(&state.join("shared.json"), "{\"enabled\":false}");
    symlink("fragment.json", state.join("config/alias.json")).unwrap();
    fs::set_permissions(
        state.join("config/fragment.json"),
        fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    fs::set_permissions(state.join("config"), fs::Permissions::from_mode(0o500)).unwrap();
    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--include-state",
            "--output",
            "archive.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));
    let archive_before = fs::read(cwd.join("archive.tar")).unwrap();
    let offline = root.child("source-offline");
    fs::rename(&source, &offline).unwrap();
    let import = run_ocm(
        &cwd,
        &env,
        &["env", "import", "archive.tar", "--name", "target"],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let target = root.child("ocm-home/envs/target/.openclaw");
    let alias = target.join("config/alias.json");
    assert!(fs::symlink_metadata(&alias).unwrap().is_file());
    let imported: Value = serde_json::from_str(&fs::read_to_string(&alias).unwrap()).unwrap();
    assert_eq!(
        Path::new(imported["$include"].as_str().unwrap()),
        target.join("shared.json")
    );
    assert_eq!(
        fs::metadata(alias).unwrap().permissions().mode() & 0o777,
        0o400
    );
    let copied_fragment: Value =
        serde_json::from_str(&fs::read_to_string(target.join("config/fragment.json")).unwrap())
            .unwrap();
    assert_eq!(
        Path::new(copied_fragment["$include"].as_str().unwrap()),
        target.join("shared.json")
    );
    assert_eq!(
        fs::read_to_string(offline.join(".openclaw/config/fragment.json")).unwrap(),
        fragment
    );
    for parent in [target.join("config"), offline.join(".openclaw/config")] {
        let referent = parent.join("fragment.json");
        assert_eq!(
            fs::metadata(&referent).unwrap().permissions().mode() & 0o777,
            0o400
        );
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o500
        );
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(fs::read(cwd.join("archive.tar")).unwrap(), archive_before);
    assert!(!source.exists());
}

#[cfg(unix)]
#[test]
fn env_import_keeps_archived_include_resolution_inside_the_config_directory() {
    for include_state in [false, true] {
        for case in ["external", "parent", "symlink"] {
            let root = TestDir::new("env-import-include-containment");
            let cwd = root.child("cwd");
            fs::create_dir_all(&cwd).unwrap();
            let env = ocm_env(&root);
            let original = root.child("original");
            let external = root.child("external");
            write_text(&external.join("settings.json"), "{\"enabled\":false}");
            let archived = root.child("archived");
            let config_dir = archived.join("root/.openclaw");
            fs::create_dir_all(&config_dir).unwrap();
            let directive = match case {
                "external" => external.join("settings.json"),
                "parent" => original.join(".openclaw/../settings.json"),
                "symlink" => {
                    std::os::unix::fs::symlink(&external, config_dir.join("linked")).unwrap();
                    original.join(".openclaw/linked/settings.json")
                }
                _ => unreachable!(),
            };
            write_text(
                &config_dir.join("openclaw.json"),
                &serde_json::json!({
                    "plugins": {"$include": directive}
                })
                .to_string(),
            );
            write_text(
                &archived.join("meta/env.json"),
                &serde_json::json!({
                    "kind": "ocm-env-archive", "formatVersion": 1,
                    "includeState": include_state, "exportedAt": "2026-10-19T00:00:00Z",
                    "env": {"name": "source", "sourceRoot": original,
                        "canonicalSourceRoot": original, "protected": false,
                        "createdAt": "2026-10-19T00:00:00Z", "updatedAt": "2026-10-19T00:00:00Z"}
                })
                .to_string(),
            );
            let mut archive = tar::Builder::new(fs::File::create(cwd.join("archive.tar")).unwrap());
            archive.follow_symlinks(false);
            archive
                .append_dir_all("meta", archived.join("meta"))
                .unwrap();
            archive
                .append_dir_all("root", archived.join("root"))
                .unwrap();
            archive.finish().unwrap();
            drop(archive);
            let imported = run_ocm(
                &cwd,
                &env,
                &["env", "import", "archive.tar", "--name", "target"],
            );
            assert!(!imported.status.success(), "accepted {case}");
            assert!(
                stderr(&imported).contains("config directory"),
                "{}",
                stderr(&imported)
            );
            assert!(!root.child("ocm-home/envs/target").exists());
            assert_eq!(
                fs::read_to_string(external.join("settings.json")).unwrap(),
                "{\"enabled\":false}"
            );
        }
    }
}

#[test]
fn env_import_restores_an_archive_with_a_new_name_and_root() {
    let root = TestDir::new("env-import");
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
        "hello import",
    );
    let source_state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        &serde_json::json!({"agents":{"list":[
            {"id":"main","default":true},
            {"id":"ops","workspace":source_state.join("team/ops")}
        ]}})
        .to_string(),
    );
    write_text(
        &source_state.join("team/ops/custom.txt"),
        "hello custom import",
    );

    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            "./archives/source-backup.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./archives/source-backup.tar",
            "--name",
            "target",
            "--root",
            "./imports/target-root",
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let output = stdout(&import);
    assert!(output.contains("Imported env target from source"));

    let show = run_ocm(&cwd, &env, &["env", "show", "target", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_output = stdout(&show);
    assert!(show_output.contains("\"name\": \"target\""));
    let show_json: Value = serde_json::from_str(&show_output).unwrap();
    assert_ne!(show_json["gatewayPort"].as_u64(), Some(19_789));
    assert_eq!(show_json["serviceEnabled"], false);
    assert_eq!(show_json["serviceRunning"], false);
    assert!(show_output.contains("\"protected\": true"));

    assert_eq!(
        fs::read_to_string(
            root.child("workspace/imports/target-root/.openclaw/workspace/notes.txt")
        )
        .unwrap(),
        "hello import"
    );
    let target_state = root.child("workspace/imports/target-root/.openclaw");
    assert_eq!(
        fs::read_to_string(target_state.join("team/ops/custom.txt")).unwrap(),
        "hello custom import"
    );
    let config: Value =
        serde_json::from_str(&fs::read_to_string(target_state.join("openclaw.json")).unwrap())
            .unwrap();
    let actual_workspace = fs::canonicalize(Path::new(
        config["agents"]["list"][1]["workspace"].as_str().unwrap(),
    ))
    .unwrap();
    let expected_workspace = fs::canonicalize(target_state.join("team/ops")).unwrap();
    assert_eq!(actual_workspace, expected_workspace);
}

#[test]
fn env_import_restores_managed_plugin_payloads_without_runtime_debris() {
    let root = TestDir::new("env-import-plugin-payloads");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    let payloads = [
        (
            "plugins/installs.json",
            "{\"legacy\":{\"source\":\"path\"}}\n",
        ),
        (
            "extensions/path-demo/openclaw.plugin.json",
            "{\"id\":\"path-demo\"}\n",
        ),
        (
            "npm/projects/npm-demo/package-lock.json",
            "{\"lockfileVersion\":3}\n",
        ),
        (
            "npm/projects/npm-demo/node_modules/npm-demo/index.js",
            "module.exports = 'npm-demo';\n",
        ),
        ("git/git-demo/repo/.git/HEAD", "ref: refs/heads/main\n"),
        (
            "git/git-demo/repo/openclaw.plugin.json",
            "{\"id\":\"git-demo\"}\n",
        ),
    ];
    for (path, contents) in payloads {
        write_text(&source_state.join(path), contents);
    }
    write_text(
        &source_state.join("plugin-runtime-deps/demo/node_modules/demo/index.js"),
        "generated runtime dependency\n",
    );

    let archive = root.child("source.ocm-env.tar");
    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            archive.to_string_lossy().as_ref(),
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let target_root = root.child("imports/target");
    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            archive.to_string_lossy().as_ref(),
            "--name",
            "target",
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));

    let target_state = target_root.join(".openclaw");
    for (path, contents) in payloads {
        assert_eq!(
            fs::read_to_string(target_state.join(path)).unwrap(),
            contents,
            "{path}"
        );
    }
    assert!(!target_state.join("plugin-runtime-deps").exists());
}

#[test]
fn env_import_rewrites_identity_bound_workspaces_to_the_imported_data_path() {
    let root = TestDir::new("env-import-runtime-identity");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    write_text(
        &source_state.join("openclaw.json"),
        concat!(
            "{\n",
            "  agents: { defaults: {\n",
            "    workspace: '${OCM_ACTIVE_ENV_ROOT}/.openclaw/team/${OCM_ACTIVE_ENV}-${OPENCLAW_GATEWAY_PORT}'\n",
            "  } }\n",
            "}\n"
        ),
    );
    write_text(
        &source_state.join("team/source-19789/notes.txt"),
        "runtime workspace\n",
    );

    let archive = root.child("source.ocm-env.tar");
    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            archive.to_string_lossy().as_ref(),
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let target_root = root.child("imports/target");
    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            archive.to_string_lossy().as_ref(),
            "--name",
            "target",
            "--root",
            target_root.to_string_lossy().as_ref(),
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));

    let copied_workspace = target_root.join(".openclaw/team/source-19789");
    assert_eq!(
        fs::read_to_string(copied_workspace.join("notes.txt")).unwrap(),
        "runtime workspace\n"
    );
    let config: Value = serde_json::from_str(
        &fs::read_to_string(target_root.join(".openclaw/openclaw.json")).unwrap(),
    )
    .unwrap();
    let configured_workspace = config["agents"]["defaults"]["workspace"].as_str().unwrap();
    assert_eq!(Path::new(configured_workspace), copied_workspace);
}

#[test]
fn env_import_json_reports_the_archive_and_source_name() {
    let root = TestDir::new("env-import-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let export = run_ocm(&cwd, &env, &["env", "export", "source"]);
    assert!(export.status.success(), "{}", stderr(&export));

    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./source.ocm-env.tar",
            "--name",
            "target",
            "--json",
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));
    let output = stdout(&import);
    assert!(output.contains("\"name\": \"target\""));
    assert!(output.contains("\"sourceName\": \"source\""));
    assert!(output.contains("\"archivePath\":"));
}

#[test]
fn env_import_rewrites_openclaw_config_for_the_new_root() {
    let root = TestDir::new("env-import-config-rewrite");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_root = root.child("ocm-home/envs/source");
    fs::write(
        source_root.join(".openclaw/openclaw.json"),
        serde_json::json!({
            "agents":{"defaults":{"workspace":source_root.join(".openclaw/workspace")}},
            "gateway":{"port":19789}
        })
        .to_string(),
    )
    .unwrap();

    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            "./archives/source-config.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./archives/source-config.tar",
            "--name",
            "target",
            "--root",
            "./imports/target-root",
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));

    let raw =
        fs::read_to_string(root.child("workspace/imports/target-root/.openclaw/openclaw.json"))
            .unwrap();
    let config: Value = serde_json::from_str(&raw).unwrap();
    let actual_workspace = fs::canonicalize(Path::new(
        config["agents"]["defaults"]["workspace"].as_str().unwrap(),
    ))
    .unwrap();
    let expected_workspace = fs::canonicalize(root.child("workspace/imports/target-root"))
        .unwrap()
        .join(".openclaw/workspace");
    assert_eq!(actual_workspace, expected_workspace);
    assert_ne!(config["gateway"]["port"].as_u64(), Some(19_789));
}

#[test]
fn env_import_resets_or_replaces_a_copied_public_sandbox_origin() {
    let root = TestDir::new("env-import-sandbox-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        concat!(
            "{\n",
            "  \"gateway\": { \"port\": 19789 },\n",
            "  \"mcp\": {\n",
            "    \"apps\": {\n",
            "      \"enabled\": true,\n",
            "      \"sandboxPort\": 19790,\n",
            "      \"sandboxOrigin\": \"https://source.example.test\"\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    );

    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            "./source-sandbox.ocm-env.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let reset = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./source-sandbox.ocm-env.tar",
            "--name",
            "target-reset",
        ],
    );
    assert!(reset.status.success(), "{}", stderr(&reset));
    assert!(
        stderr(&reset).contains("removed copied MCP app sandbox origin from env target-reset"),
        "{}",
        stderr(&reset)
    );
    assert!(!stderr(&reset).contains("source.example.test"));
    let reset_config: Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/target-reset/.openclaw/openclaw.json"))
            .unwrap(),
    )
    .unwrap();
    assert!(reset_config["mcp"]["apps"]["sandboxOrigin"].is_null());

    let replaced = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./source-sandbox.ocm-env.tar",
            "--name",
            "target-explicit",
            "--sandbox-origin",
            "https://target.example.test",
        ],
    );
    assert!(replaced.status.success(), "{}", stderr(&replaced));
    assert!(!stderr(&replaced).contains("removed copied MCP app sandbox origin"));
    let replaced_config: Value = serde_json::from_str(
        &fs::read_to_string(root.child("ocm-home/envs/target-explicit/.openclaw/openclaw.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        replaced_config["mcp"]["apps"]["sandboxOrigin"].as_str(),
        Some("https://target.example.test")
    );
}

#[test]
fn env_import_validates_the_target_origin_before_reading_the_archive() {
    let root = TestDir::new("env-import-invalid-origin-preflight");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./missing.ocm-env.tar",
            "--sandbox-origin",
            "https://target.example.test/apps",
        ],
    );
    assert_eq!(import.status.code(), Some(1));
    assert!(stderr(&import).contains(
        "--sandbox-origin must be an HTTP(S) origin without a path, query, or credentials"
    ));
    assert!(!stderr(&import).contains("missing.ocm-env.tar"));
}

#[test]
fn env_import_rejects_include_owned_sandbox_configuration_before_creating_the_target() {
    let root = TestDir::new("env-import-include-owned-sandbox-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/openclaw.json"),
        "{\n  \"$include\": \"./base.json5\"\n}\n",
    );
    write_text(
        &root.child("ocm-home/envs/source/.openclaw/base.json5"),
        "{ mcp: { apps: { sandboxOrigin: 'https://source.example.test' } } }\n",
    );
    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            "./include-owned.ocm-env.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./include-owned.ocm-env.tar",
            "--name",
            "target",
        ],
    );
    assert_eq!(import.status.code(), Some(1));
    assert!(
        stderr(&import).contains(
            "cannot safely reset mcp.apps.sandboxOrigin because OpenClaw config uses $include at the config root"
        ),
        "{}",
        stderr(&import)
    );
    assert!(!root.child("ocm-home/envs/target").exists());
}

#[test]
fn env_import_keeps_agent_auth_but_drops_live_runtime_state() {
    let root = TestDir::new("env-import-runtime-cleanup");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let source_state = root.child("ocm-home/envs/source/.openclaw");
    fs::create_dir_all(source_state.join("agents/main/agent")).unwrap();
    fs::create_dir_all(source_state.join("agents/main/sessions")).unwrap();
    fs::create_dir_all(source_state.join("logs")).unwrap();
    write_text(
        &source_state.join("agents/main/agent/auth-profiles.json"),
        "{\"default\":\"ok\"}\n",
    );
    write_text(
        &source_state.join("agents/main/agent/models.json"),
        "{\"primary\":\"gpt-5.4\"}\n",
    );
    write_text(
        &source_state.join("agents/main/sessions/main.jsonl"),
        "{\"type\":\"session\"}\n",
    );
    write_text(&source_state.join("logs/gateway.log"), "copied log\n");
    write_text(&source_state.join("openclaw.json.bak"), "{}\n");

    let export = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            "./archives/source-runtime.tar",
        ],
    );
    assert!(export.status.success(), "{}", stderr(&export));

    let import = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "import",
            "./archives/source-runtime.tar",
            "--name",
            "target",
            "--root",
            "./imports/target-root",
        ],
    );
    assert!(import.status.success(), "{}", stderr(&import));

    let target_state = root.child("workspace/imports/target-root/.openclaw");
    assert!(
        target_state
            .join("agents/main/agent/auth-profiles.json")
            .exists()
    );
    assert!(target_state.join("agents/main/agent/models.json").exists());
    assert!(!target_state.join("agents/main/sessions").exists());
    assert!(!target_state.join("logs").exists());
    assert!(!target_state.join("openclaw.json.bak").exists());
}

#[test]
fn env_import_owns_selected_git_graphs_and_rolls_back_incomplete_graphs() {
    assert_imported_git_graphs(&[
        (false, false, false, ImportGitLayout::Selected),
        (false, true, false, ImportGitLayout::Selected),
        (true, false, false, ImportGitLayout::Selected),
        (true, true, false, ImportGitLayout::Selected),
        (true, false, true, ImportGitLayout::Selected),
    ]);
}

#[test]
fn env_import_maps_original_git_pointers_when_target_overlaps_old_source() {
    assert_imported_git_graphs(&[
        (false, false, false, ImportGitLayout::NestedTarget),
        (true, false, false, ImportGitLayout::NestedTarget),
    ]);
}

#[test]
fn env_import_preserves_an_ordinary_archive_root_checkout() {
    assert_imported_git_graphs(&[(false, false, false, ImportGitLayout::EnvironmentRoot)]);
}

#[derive(Clone, Copy)]
enum ImportGitLayout {
    Selected,
    NestedTarget,
    EnvironmentRoot,
}

fn assert_imported_git_graphs(cases: &[(bool, bool, bool, ImportGitLayout)]) {
    use rusqlite::{Connection, params};
    use sha2::{Digest, Sha256};

    for &(include_state, incomplete, protected_history, layout) in cases {
        let nested = matches!(layout, ImportGitLayout::NestedTarget);
        let root_checkout = matches!(layout, ImportGitLayout::EnvironmentRoot);
        if protected_history && !cfg!(unix) {
            continue;
        }
        let fixture = TestDir::new("import-copied-git");
        let root = fs::canonicalize(fixture.path()).unwrap();
        let env = ocm_env(&fixture);
        let source = root.join("source");
        let target = if nested {
            source.join("projects")
        } else {
            root.join("target")
        };
        let created = run_ocm(
            &root,
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
        // These repositories are selected outside configured workspaces.
        let repository = if root_checkout {
            source.clone()
        } else {
            source.join("projects/repository")
        };
        let checkout = source.join(if include_state {
            ".openclaw/worktrees/task"
        } else {
            "projects/task"
        });
        fs::create_dir_all(&repository).unwrap();
        let git = |path: &Path, args: &[&str]| import_git(path, &env, args);
        git(&repository, &["init", "-q"]);
        write_text(&repository.join("authored.txt"), "authored content\n");
        git(&repository, &["add", "authored.txt"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        git(
            &repository,
            &["worktree", "add", "-qb", "task", checkout.to_str().unwrap()],
        );
        git(
            &repository,
            &["update-ref", "refs/snapshots/archive", "HEAD"],
        );
        let retained_config = source.join(".openclaw/credentials/copied-git.conf");
        if !include_state && !incomplete && !root_checkout {
            fs::create_dir_all(retained_config.parent().unwrap()).unwrap();
            fs::write(&retained_config, "[fixture]\n\tretained = selected\n").unwrap();
            fs::write(
                retained_config.with_file_name("unrelated.cache"),
                "runtime debris",
            )
            .unwrap();
            git(
                &repository,
                &["config", "include.path", retained_config.to_str().unwrap()],
            );
            assert_eq!(
                git(&checkout, &["config", "--get", "fixture.retained"]),
                "selected"
            );
        }
        let private = repository.join(".git/worktrees/task");
        let index = fs::read(private.join("index")).unwrap();
        if incomplete {
            write_text(
                &private.join("commondir"),
                source.join("missing-git").to_str().unwrap(),
            );
        }
        let pointer = fs::read(checkout.join(".git")).unwrap();
        let database = source.join(".openclaw/state/openclaw.sqlite");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let db = Connection::open(&database).unwrap();
        db.execute_batch("CREATE TABLE worktrees (id TEXT PRIMARY KEY, repo_fingerprint TEXT, repo_root TEXT, path TEXT, branch TEXT, base_ref TEXT, snapshot_ref TEXT);").unwrap();
        db.execute("INSERT INTO worktrees VALUES ('bound', 'old-fingerprint', ?1, ?2, 'task', 'HEAD', 'refs/snapshots/archive')", params![repository.to_str(), checkout.to_str()]).unwrap();
        drop(db);
        let database_bytes = fs::read(&database).unwrap();
        let historical = source.join(".openclaw/skill-workshop/snapshot/.git");
        write_text(&historical, "gitdir: /historical/uncopied/storage\n");
        let protected_config = source.join(".openclaw/skill-workshop/support/git-config");
        #[cfg(unix)]
        if protected_history {
            fs::create_dir_all(protected_config.parent().unwrap()).unwrap();
            git(
                &repository,
                &["config", "core.worktree", repository.to_str().unwrap()],
            );
            fs::rename(repository.join(".git/config"), &protected_config).unwrap();
            std::os::unix::fs::symlink(&protected_config, repository.join(".git/config")).unwrap();
        }
        let protected_bytes = protected_history.then(|| fs::read(&protected_config).unwrap());
        let archive = root.join("source.tar");
        let mut args = vec![
            "env",
            "export",
            "source",
            "--output",
            archive.to_str().unwrap(),
        ];
        if include_state {
            args.push("--include-state");
        }
        let exported = run_ocm(&root, &env, &args);
        assert!(exported.status.success(), "{}", stderr(&exported));
        assert_eq!(fs::read(&database).unwrap(), database_bytes);
        let archive_bytes = fs::read(&archive).unwrap();
        let offline = root.join("offline");
        fs::rename(&source, &offline).unwrap();
        let mut import_env = env.clone();
        if nested {
            import_env.insert(
                "OCM_HOME".into(),
                root.join("import-store").display().to_string(),
            );
        }
        let imported = run_ocm(
            &root,
            &import_env,
            &[
                "env",
                "import",
                archive.to_str().unwrap(),
                "--name",
                "target",
                "--root",
                target.to_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(fs::read(&archive).unwrap(), archive_bytes);
        assert_eq!(
            fs::read(offline.join(database.strip_prefix(&source).unwrap())).unwrap(),
            database_bytes
        );
        assert_eq!(
            fs::read(
                offline
                    .join(checkout.strip_prefix(&source).unwrap())
                    .join(".git")
            )
            .unwrap(),
            pointer
        );
        if let Some(bytes) = &protected_bytes {
            assert_eq!(
                fs::read(offline.join(protected_config.strip_prefix(&source).unwrap())).unwrap(),
                *bytes
            );
        }
        if incomplete || protected_history {
            assert!(!imported.status.success(), "{}", stdout(&imported));
            assert!(
                stderr(&imported).contains(if incomplete {
                    "missing-git"
                } else {
                    "integrity-protected history"
                }),
                "{}",
                stderr(&imported)
            );
            assert!(!target.exists(), "incomplete import remained");
            let listed = run_ocm(&root, &import_env, &["env", "list", "--json"]);
            assert!(listed.status.success(), "{}", stderr(&listed));
            assert!(!stdout(&listed).contains(target.to_str().unwrap()));
            continue;
        }
        assert!(imported.status.success(), "{}", stderr(&imported));
        let copied_repository = target.join(repository.strip_prefix(&source).unwrap());
        let copied_checkout = target.join(checkout.strip_prefix(&source).unwrap());
        let common = fs::canonicalize(copied_repository.join(".git")).unwrap();
        git(
            &copied_repository,
            &["worktree", "lock", copied_checkout.to_str().unwrap()],
        );
        assert!(common.join("worktrees/task/locked").exists());
        assert!(
            !offline
                .join(private.strip_prefix(&source).unwrap())
                .join("locked")
                .exists()
        );
        git(
            &copied_repository,
            &["worktree", "unlock", copied_checkout.to_str().unwrap()],
        );
        assert!(!common.join("worktrees/task/locked").exists());
        let actual_common = git(&copied_checkout, &["rev-parse", "--git-common-dir"]);
        assert_eq!(
            fs::canonicalize(copied_checkout.join(actual_common)).unwrap(),
            common
        );
        assert_eq!(
            git(&copied_checkout, &["symbolic-ref", "--short", "HEAD"]),
            "task"
        );
        assert_eq!(
            git(&copied_repository, &["rev-parse", "refs/snapshots/archive"]),
            git(&copied_checkout, &["rev-parse", "HEAD"])
        );
        assert_eq!(
            fs::read(common.join("worktrees/task/index")).unwrap(),
            index
        );
        assert_eq!(
            fs::read_to_string(copied_checkout.join("authored.txt")).unwrap(),
            "authored content\n"
        );
        if !include_state && !root_checkout {
            let config = target.join(retained_config.strip_prefix(&source).unwrap());
            assert_eq!(
                fs::read_to_string(&config).unwrap(),
                "[fixture]\n\tretained = selected\n"
            );
            assert!(!config.with_file_name("unrelated.cache").exists());
            assert_eq!(
                git(&copied_checkout, &["config", "--get", "fixture.retained"]),
                "selected"
            );
        }
        let copied_database = target.join(".openclaw/state/openclaw.sqlite");
        assert_eq!(copied_database.exists(), include_state);
        if include_state {
            let db = Connection::open(&copied_database).unwrap();
            let row: (String, String, String, String, String, String, String) = db.query_row("SELECT id, repo_root, path, repo_fingerprint, branch, base_ref, snapshot_ref FROM worktrees", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))).unwrap();
            assert_eq!(
                fs::canonicalize(&row.1).unwrap(),
                fs::canonicalize(&copied_repository).unwrap()
            );
            assert_eq!(
                fs::canonicalize(&row.2).unwrap(),
                fs::canonicalize(&copied_checkout).unwrap()
            );
            assert!(!row.1.starts_with(r"\\?\"));
            assert!(!row.2.starts_with(r"\\?\"));
            let registered_common = Path::new(&row.1).join(".git");
            let fingerprint =
                Sha256::digest(format!("{}\n", registered_common.display()).as_bytes())[..8]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
            assert_eq!(
                row,
                (
                    "bound".into(),
                    row.1.clone(),
                    row.2.clone(),
                    fingerprint,
                    "task".into(),
                    "HEAD".into(),
                    "refs/snapshots/archive".into()
                )
            );
            assert_eq!(
                db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            assert_eq!(
                fs::read(target.join(historical.strip_prefix(&source).unwrap())).unwrap(),
                b"gitdir: /historical/uncopied/storage\n"
            );
            let summary: Value = serde_json::from_str(&stdout(&imported)).unwrap();
            let database_key = Path::new(".openclaw").join("state").join("openclaw.sqlite");
            assert_eq!(
                summary["databasePathsRewritten"][database_key.to_str().unwrap()],
                2,
                "{summary}"
            );
        }
    }
}

fn import_git_argument(value: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(path) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{path}");
        }
        if let Some(path) = value.strip_prefix(r"\\?\") {
            return path.to_string();
        }
    }
    value.to_string()
}

fn import_git(
    path: &Path,
    env: &std::collections::BTreeMap<String, String>,
    args: &[&str],
) -> String {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(import_git_argument(&path.to_string_lossy()))
        .args(args.iter().map(|value| import_git_argument(value)))
        .env_clear()
        .envs(env)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", stderr(&result));
    stdout(&result).trim().to_string()
}

#[test]
fn env_import_preserves_selected_bare_repository() {
    let fixture = TestDir::new("import-selected-bare-git");
    let root = fs::canonicalize(fixture.path()).unwrap();
    let env = ocm_env(&fixture);
    let source = root.join("source");
    let target = root.join("target");
    let created = run_ocm(
        &root,
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
    let repository = source.join("projects/bare");
    fs::create_dir_all(&repository).unwrap();
    import_git(&repository, &env, &["init", "-q"]);
    import_git(&repository, &env, &["config", "core.bare", "true"]);
    assert_eq!(
        import_git(&repository, &env, &["rev-parse", "--is-bare-repository"]),
        "true"
    );
    let before = fs::read(repository.join(".git/config")).unwrap();
    let archive = root.join("source.tar");
    let exported = run_ocm(
        &root,
        &env,
        &[
            "env",
            "export",
            "source",
            "--output",
            archive.to_str().unwrap(),
        ],
    );
    assert!(exported.status.success(), "{}", stderr(&exported));
    let archive_before = fs::read(&archive).unwrap();
    let offline = root.join("offline");
    fs::rename(&source, &offline).unwrap();
    let imported = run_ocm(
        &root,
        &env,
        &[
            "env",
            "import",
            archive.to_str().unwrap(),
            "--name",
            "target",
            "--root",
            target.to_str().unwrap(),
        ],
    );
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert_eq!(fs::read(archive).unwrap(), archive_before);
    assert_eq!(
        fs::read(offline.join("projects/bare/.git/config")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(target.join("projects/bare/.git/config")).unwrap(),
        before
    );
    assert_eq!(
        import_git(
            &target.join("projects/bare"),
            &env,
            &["rev-parse", "--is-bare-repository"]
        ),
        "true"
    );
}

#[test]
fn env_import_relocates_native_bare_and_ordinary_git_includes() {
    for include_state in [false, true] {
        let fixture = TestDir::new("import-native-bare-git-config");
        let root = fs::canonicalize(fixture.path()).unwrap();
        let env = ocm_env(&fixture);
        let source = root.join("source");
        let target = root.join("target");
        let created = run_ocm(
            &root,
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
        let workspace = source.join(".openclaw/workspace");
        fs::create_dir_all(&workspace).unwrap();
        let settings = workspace.join("settings.ini");
        fs::write(&settings, "[fixture]\n value = retained\n").unwrap();
        let mut original_files = Vec::new();
        for (name, native_bare) in [("bare.git", true), ("ordinary", false)] {
            let repository = workspace.join(name);
            fs::create_dir(&repository).unwrap();
            let args = if native_bare {
                vec!["init", "--bare", "-q"]
            } else {
                vec!["init", "-q"]
            };
            import_git(&repository, &env, &args);
            import_git(
                &repository,
                &env,
                &["config", "include.path", settings.to_str().unwrap()],
            );
            let metadata = if native_bare {
                repository
            } else {
                repository.join(".git")
            };
            for name in ["config", "HEAD"] {
                let path = metadata.join(name);
                original_files.push((
                    path.strip_prefix(&source).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                ));
            }
        }
        original_files.push((
            settings.strip_prefix(&source).unwrap().to_path_buf(),
            fs::read(&settings).unwrap(),
        ));
        let archive = root.join("source.tar");
        let mut args = vec![
            "env",
            "export",
            "source",
            "--output",
            archive.to_str().unwrap(),
        ];
        if include_state {
            args.push("--include-state");
        }
        let exported = run_ocm(&root, &env, &args);
        assert!(exported.status.success(), "{}", stderr(&exported));
        let archive_before = fs::read(&archive).unwrap();
        let offline = root.join("offline");
        fs::rename(&source, &offline).unwrap();
        let imported = run_ocm(
            &root,
            &env,
            &[
                "env",
                "import",
                archive.to_str().unwrap(),
                "--name",
                "target",
                "--root",
                target.to_str().unwrap(),
            ],
        );
        assert!(imported.status.success(), "{}", stderr(&imported));
        assert_eq!(fs::read(&archive).unwrap(), archive_before);
        for (path, bytes) in original_files {
            assert_eq!(fs::read(offline.join(&path)).unwrap(), bytes);
            if path.file_name().unwrap() != "config" {
                assert_eq!(fs::read(target.join(path)).unwrap(), bytes);
            }
        }
        let copied_workspace = target.join(".openclaw/workspace");
        for (name, bare) in [("bare.git", "true"), ("ordinary", "false")] {
            let repository = copied_workspace.join(name);
            assert_eq!(
                import_git(&repository, &env, &["rev-parse", "--is-bare-repository"]),
                bare
            );
            assert_eq!(
                import_git(&repository, &env, &["config", "fixture.value"]),
                "retained"
            );
            assert_eq!(
                Path::new(&import_git(
                    &repository,
                    &env,
                    &["config", "--get", "include.path"]
                )),
                Path::new(&import_git_argument(
                    copied_workspace.join("settings.ini").to_str().unwrap()
                ))
            );
        }
    }
}
