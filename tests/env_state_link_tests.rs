#![cfg(unix)]

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use serde_json::Value;
use support::{
    TestDir, install_fake_service_manager, ocm_env, run_ocm, stderr, stdout,
    write_executable_script, write_text,
};

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
        assert!(
            !temporary.join("ocm-env-imports").exists(),
            "import staged in the shared temp directory"
        );
        let staging = fs::read_dir(root.child("ocm-home/import-staging"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != ".lock")
            .count();
        assert_eq!(staging, 0, "import staging remains");
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

fn check_checkpoint_layout(layout: &str) {
    let root = TestDir::new(&format!("checkpoint-state-link-{layout}"));
    let mut env = ocm_env(&root);
    install_fake_service_manager(&root, &mut env);
    let finalized = root.child("finalized");
    env.insert("OCM_TEST_FINALIZED".into(), finalized.display().to_string());
    for (name, version) in [("old", "2026.9.1"), ("new", "2026.9.2")] {
        let executable = root.child(name);
        write_executable_script(
            &executable,
            &format!(
                r#"#!/bin/sh
case "$1 $2" in
  '--version ') echo '{version}';;
  'config validate') echo 'Config valid';;
  'doctor --lint') echo '{{"ok":true,"checksRun":1,"checksSkipped":0,"findings":[]}}';;
  'update finalize')
    : > "$OCM_TEST_FINALIZED"
    echo '{{"status":"ok","mode":"finalize","postUpdate":{{"doctor":{{"status":"ok"}},"plugins":{{"status":"ok"}}}}}}';;
  'gateway status') echo '{{"rpc":{{"ok":true}}}}';;
  *) echo "unexpected fixture invocation $*" >&2; exit 91;;
esac
"#
            ),
        );
        let result = run_ocm(
            root.path(),
            &env,
            &[
                "runtime",
                "add",
                name,
                "--path",
                executable.to_str().unwrap(),
            ],
        );
        assert!(result.status.success(), "{}", stderr(&result));
    }
    for name in ["demo", "other"] {
        let create = run_ocm(
            root.path(),
            &env,
            &["env", "create", name, "--runtime", "old"],
        );
        assert!(create.status.success(), "{}", stderr(&create));
    }
    let env_root = root.child("ocm-home/envs/demo");
    let state = env_root.join(".openclaw");
    write_text(&state.join("openclaw.json"), "{}\n");
    write_text(
        &state.join("agents/main/sessions/session.jsonl"),
        "before\n",
    );
    write_text(&state.join("extensions/demo/index.js"), "before\n");
    let external = root.child("external");
    let moved = |relative: &str, target: std::path::PathBuf| {
        fs::rename(state.join(relative), &target).unwrap();
        symlink(&target, state.join(relative)).unwrap();
    };
    match layout {
        "directory" => {}
        "inside-state-link" => {
            fs::rename(&state, env_root.join("state")).unwrap();
            symlink("state", &state).unwrap();
        }
        "state-link" => {
            fs::rename(&state, &external).unwrap();
            symlink(&external, &state).unwrap();
        }
        "dangling-state-link" => {
            fs::remove_dir_all(&state).unwrap();
            symlink(root.child("missing"), &state).unwrap();
        }
        "dangling-chain-state-link" => {
            fs::remove_dir_all(&state).unwrap();
            symlink(root.child("missing"), env_root.join("hop")).unwrap();
            symlink("hop", &state).unwrap();
        }
        "firmlink-state-link" => {
            // macOS reaches the same directory through its data-volume spelling.
            let canonical = fs::canonicalize(&env_root).unwrap();
            let alias = std::path::Path::new("/System/Volumes/Data")
                .join(canonical.strip_prefix("/").unwrap());
            if !alias.is_dir() {
                return;
            }
            fs::rename(&state, env_root.join("state")).unwrap();
            symlink(alias.join("state"), &state).unwrap();
        }
        "agents-link" => moved("agents", external.clone()),
        "relative-agents-link" => {
            fs::rename(state.join("agents"), &external).unwrap();
            symlink("../../../../external", state.join("agents")).unwrap();
        }
        "extensions-link" => moved("extensions", external.clone()),
        _ => unreachable!(),
    }
    let session = state.join("agents/main/sessions/session.jsonl");
    let snapshots = || {
        let list = run_ocm(
            root.path(),
            &env,
            &["env", "snapshot", "list", "demo", "--json"],
        );
        assert!(list.status.success(), "{}", stderr(&list));
        serde_json::from_slice::<Value>(&list.stdout).unwrap()
    };
    let binding_of = |name: &str| {
        let show = run_ocm(root.path(), &env, &["env", "show", name, "--json"]);
        assert!(show.status.success(), "{}", stderr(&show));
        serde_json::from_slice::<Value>(&show.stdout).unwrap()["defaultRuntime"].clone()
    };
    let binding = || binding_of("demo");
    let controls = ["directory", "inside-state-link", "firmlink-state-link"];
    let registry = root.child("ocm-home/envs.json");
    if !controls.contains(&layout) {
        // A refusal must come before a running gateway is stopped.
        let cwd = root.path();
        let mut meta = ocm::store::get_environment("demo", &env, cwd).unwrap();
        meta.service_enabled = true;
        meta.service_running = true;
        ocm::store::save_environment(meta, &env, cwd).unwrap();
    }
    let registry_before = fs::read(&registry).unwrap();

    let snapshot = run_ocm(
        root.path(),
        &env,
        &["env", "snapshot", "create", "demo", "--json"],
    );
    let archive = root.child("state.ocm-env.tar");
    let export = run_ocm(
        root.path(),
        &env,
        &[
            "env",
            "export",
            "demo",
            "--include-state",
            "--output",
            archive.to_str().unwrap(),
        ],
    );
    if controls.contains(&layout) {
        assert!(snapshot.status.success(), "{}", stderr(&snapshot));
        let id = serde_json::from_slice::<Value>(&snapshot.stdout).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        write_text(&session, "after\n");
        let restore = run_ocm(
            root.path(),
            &env,
            &["env", "snapshot", "restore", "demo", &id],
        );
        assert!(restore.status.success(), "{}", stderr(&restore));
        assert_eq!(fs::read_to_string(&session).unwrap(), "before\n");
        assert!(export.status.success(), "{}", stderr(&export));
        let upgrade = run_ocm(root.path(), &env, &["upgrade", "demo", "--runtime", "new"]);
        assert!(upgrade.status.success(), "{}", stderr(&upgrade));
        assert!(finalized.exists());
        assert_eq!(binding(), "new");
        if layout == "directory" {
            // Rollback takes its own safety checkpoint before switching back.
            let recorded = snapshots();
            fs::rename(&state, &external).unwrap();
            symlink(&external, &state).unwrap();
            let rollback = run_ocm(root.path(), &env, &["upgrade", "rollback", "demo"]);
            assert!(!rollback.status.success(), "rollback accepted state link");
            assert!(
                stderr(&rollback).contains("resolves outside the environment root"),
                "{}",
                stderr(&rollback)
            );
            assert_eq!(binding(), "new");
            assert_eq!(snapshots(), recorded);
        }
        return;
    }

    for (command, result) in [("snapshot create", &snapshot), ("export", &export)] {
        assert!(
            !result.status.success(),
            "{command} accepted unsafe layout: {layout}"
        );
        assert!(
            stderr(result).contains("resolves outside the environment root"),
            "{command} {layout}: {}",
            stderr(result)
        );
    }
    assert!(fs::symlink_metadata(&archive).is_err(), "archive written");
    let upgrade = run_ocm(root.path(), &env, &["upgrade", "demo", "--runtime", "new"]);
    assert!(!upgrade.status.success(), "upgrade accepted: {layout}");
    assert!(
        stderr(&upgrade).contains("resolves outside the environment root"),
        "{}",
        stderr(&upgrade)
    );
    // Batch upgrades may record the other environment's checkpoint.
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
    let batch = run_ocm(
        root.path(),
        &env,
        &[
            "upgrade",
            "batch",
            "--envs",
            "demo,other",
            "--runtime",
            "new",
            "--accept-fleet-outage",
        ],
    );
    assert!(!batch.status.success(), "batch upgrade accepted: {layout}");
    assert!(
        format!("{}{}", stdout(&batch), stderr(&batch))
            .contains("resolves outside the environment root"),
        "{}{}",
        stdout(&batch),
        stderr(&batch)
    );
    assert_eq!(binding_of("other"), "old");
    assert!(!finalized.exists(), "upgrade finalized before refusing");
    assert_eq!(binding(), "old");
    assert_eq!(snapshots(), serde_json::json!([]));
    for log in ["launchctl.log", "systemctl.log"] {
        let calls = fs::read_to_string(root.child(log)).unwrap_or_default();
        assert!(
            !calls
                .lines()
                .any(|call| call.contains("bootout") || call.contains(" stop ")),
            "service stopped before refusal: {calls}"
        );
    }
    if !layout.starts_with("dangling") {
        assert_eq!(fs::read_to_string(&session).unwrap(), "before\n");
    }
}

#[test]
fn checkpoints_and_state_export_refuse_state_linked_outside_the_root() {
    for layout in [
        "directory",
        "inside-state-link",
        "state-link",
        "dangling-state-link",
        "dangling-chain-state-link",
        "firmlink-state-link",
        "agents-link",
        "relative-agents-link",
        "extensions-link",
    ] {
        check_checkpoint_layout(layout);
    }
}
