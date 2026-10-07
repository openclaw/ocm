#![cfg(unix)]

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use support::{TestDir, ocm_env, run_ocm, stderr, write_text};

fn check_copy_layout(layout: &str, import: bool) {
    let root = TestDir::new(&format!("state-link-{layout}-{import}"));
    let cwd = root.child("cwd");
    let temporary = root.child("temporary");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&temporary).unwrap();
    let mut env = ocm_env(&root);
    env.insert("TMPDIR".into(), temporary.display().to_string());
    let create = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let source = root.child("ocm-home/envs/source");
    let state = source.join(".openclaw");
    let external = root.child("external");
    write_text(&source.join("workspace/notes.txt"), "source workspace\n");
    let config = serde_json::json!({
        "agents": {"defaults": {"workspace": source.join("workspace")}},
        "gateway": {"port": 18789}
    })
    .to_string();
    write_text(&state.join("openclaw.json"), &config);
    write_text(
        &state.join("agents/main/sessions/session.jsonl"),
        "source session\n",
    );
    write_text(
        &state.join("agents/main/agent/settings.json"),
        "durable settings\n",
    );
    write_text(
        &state.join("extensions/demo/.openclaw-runtime-deps.json"),
        "{}\n",
    );
    write_text(
        &state.join("extensions/demo/node_modules/pkg/index.js"),
        "source dependency\n",
    );

    let mut session = state.join("agents/main/sessions/session.jsonl");
    let mut settings = state.join("agents/main/agent/settings.json");
    let mut dependency = state.join("extensions/demo/node_modules/pkg/index.js");
    let mut config_path = state.join("openclaw.json");
    match layout {
        "directory" => {}
        "agent-link" => {
            fs::rename(state.join("agents/main"), &external).unwrap();
            symlink(&external, state.join("agents/main")).unwrap();
            session = external.join("sessions/session.jsonl");
            settings = external.join("agent/settings.json");
        }
        "agents-link" | "relative-agents-link" | "dangling-agents-link" => {
            fs::rename(state.join("agents"), &external).unwrap();
            let target = match layout {
                "relative-agents-link" => std::path::PathBuf::from("../../../../external"),
                "dangling-agents-link" => root.child("missing"),
                _ => external.clone(),
            };
            symlink(target, state.join("agents")).unwrap();
            session = external.join("main/sessions/session.jsonl");
            settings = external.join("main/agent/settings.json");
        }
        "extensions-link" => {
            fs::rename(state.join("extensions"), &external).unwrap();
            symlink(&external, state.join("extensions")).unwrap();
            dependency = external.join("demo/node_modules/pkg/index.js");
        }
        "state-link" => {
            fs::rename(&state, &external).unwrap();
            symlink(&external, &state).unwrap();
            session = external.join("agents/main/sessions/session.jsonl");
            settings = external.join("agents/main/agent/settings.json");
            dependency = external.join("extensions/demo/node_modules/pkg/index.js");
            config_path = external.join("openclaw.json");
        }
        _ => unreachable!(),
    }

    let registry = root.child("ocm-home/envs.json");
    let registry_before = fs::read(&registry).unwrap();
    let assert_source_unchanged = || {
        assert_eq!(fs::read_to_string(&session).unwrap(), "source session\n");
        assert_eq!(fs::read_to_string(&settings).unwrap(), "durable settings\n");
        assert_eq!(
            fs::read_to_string(&dependency).unwrap(),
            "source dependency\n"
        );
        assert_eq!(fs::read_to_string(&config_path).unwrap(), config);
        assert_eq!(
            fs::read_to_string(source.join("workspace/notes.txt")).unwrap(),
            "source workspace\n"
        );
    };
    let result = if import {
        let archive = root.child("source.ocm-env.tar");
        let export = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "export",
                "source",
                "--output",
                archive.to_str().unwrap(),
            ],
        );
        assert_source_unchanged();
        assert_eq!(fs::read(&registry).unwrap(), registry_before);
        if layout == "state-link" {
            // Export already refuses the default workspace through this parent.
            // An unmodified OCM archive cannot reach import with this layout.
            assert!(!export.status.success());
            assert!(stderr(&export).contains("outside the environment root"));
            assert!(!root.child("ocm-home/envs/target").exists());
            return;
        }
        assert!(export.status.success(), "{}", stderr(&export));
        run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "import",
                archive.to_str().unwrap(),
                "--name",
                "target",
            ],
        )
    } else {
        run_ocm(&cwd, &env, &["env", "clone", "source", "target"])
    };
    assert_source_unchanged();
    let target = root.child("ocm-home/envs/target");
    if matches!(layout, "directory" | "agent-link") {
        assert!(result.status.success(), "{}", stderr(&result));
        assert!(!target.join(".openclaw/agents/main/sessions").exists());
        assert_eq!(
            fs::read_to_string(target.join("workspace/notes.txt")).unwrap(),
            "source workspace\n"
        );
        if layout == "directory" {
            assert_eq!(
                fs::read_to_string(target.join(".openclaw/agents/main/agent/settings.json"))
                    .unwrap(),
                "durable settings\n"
            );
        } else {
            assert!(fs::symlink_metadata(target.join(".openclaw/agents/main")).is_err());
        }
    } else {
        assert!(
            !result.status.success(),
            "unsafe layout was accepted: {layout}"
        );
        assert!(
            stderr(&result).contains("expected a real directory"),
            "{}",
            stderr(&result)
        );
        assert_eq!(fs::read(&registry).unwrap(), registry_before);
        assert!(
            fs::symlink_metadata(&target).is_err(),
            "partial target remains"
        );
    }
    if import {
        assert_eq!(
            fs::read_dir(temporary.join("ocm-env-imports"))
                .unwrap()
                .count(),
            0,
            "import staging remains"
        );
    }
}

#[test]
fn clone_preserves_source_state_across_directory_and_link_layouts() {
    for layout in [
        "directory",
        "agent-link",
        "agents-link",
        "relative-agents-link",
        "dangling-agents-link",
        "extensions-link",
        "state-link",
    ] {
        check_copy_layout(layout, false);
    }
}

#[test]
fn unmodified_export_import_preserves_source_state_across_directory_and_link_layouts() {
    for layout in [
        "directory",
        "agent-link",
        "agents-link",
        "relative-agents-link",
        "dangling-agents-link",
        "extensions-link",
        "state-link",
    ] {
        check_copy_layout(layout, true);
    }
}
