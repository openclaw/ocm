#![cfg(any(target_os = "linux", target_os = "macos"))]
mod support;

use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;
use support::{TestDir, ocm_env, path_string, run_ocm, stderr, stdout, write_executable_script};

fn command(root: &TestDir, env: &BTreeMap<String, String>, args: &[&str]) -> Value {
    let output = run_ocm(root.path(), env, args);
    assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    serde_json::from_str(&stdout(&output)).unwrap()
}

#[test]
fn never_managed_maintenance_needs_no_manager_but_published_state_does() {
    let root = TestDir::new("maintenance-without-manager");
    let mut env = ocm_env(&root);
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "systemd-user".into());
    let manager = root.child("systemctl");
    write_executable_script(&manager, "#!/bin/sh\necho unavailable >&2\nexit 1\n");
    env.insert("OCM_INTERNAL_SYSTEMCTL_BIN".into(), path_string(&manager));
    command(&root, &env, &["env", "create", "source", "--json"]);
    command(
        &root,
        &env,
        &["env", "snapshot", "create", "source", "--json"],
    );
    let registry = root.child("ocm-home/envs.json");
    let before = fs::read(&registry).unwrap();
    for path in [
        root.child("ocm-home/supervisor/state.json"),
        root.child("ocm-home/supervisor/runtime.json"),
        support::managed_service_definition_path(&env, root.path(), "source"),
    ] {
        support::write_text(&path, "{}");
        let output = run_ocm(
            root.path(),
            &env,
            &["env", "snapshot", "create", "source", "--json"],
        );
        assert!(!output.status.success(), "{}", stdout(&output));
        assert!(
            stderr(&output).contains("cannot confirm quiescence"),
            "{}",
            stderr(&output)
        );
        assert_eq!(fs::read(&registry).unwrap(), before);
        fs::remove_file(path).unwrap();
    }
    let snapshots = command(
        &root,
        &env,
        &["env", "snapshot", "list", "source", "--json"],
    );
    assert_eq!(snapshots.as_array().unwrap().len(), 1);
}

#[test]
fn snapshot_maintenance_requires_trustworthy_quiescence_and_restores_intent() {
    for mode in [
        "manager-error",
        "missing",
        "malformed",
        "foreign-store",
        "stale-process",
        "stopped-intent-error",
        "normal",
    ] {
        for capture in [false, true] {
            let root = TestDir::new("maintenance-observation");
            let mut env = ocm_env(&root);
            env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
            support::install_fake_launchctl(&root, &mut env);
            let identity = support::enable_fake_daemon_gateway_admission(&root, &mut env);
            fs::write(
                root.child("identity.json"),
                serde_json::to_vec(&identity).unwrap(),
            )
            .unwrap();
            fs::write(
                root.child("mode"),
                if mode == "stopped-intent-error" {
                    "manager-error"
                } else {
                    mode
                },
            )
            .unwrap();
            write_executable_script(
                &root.child("fake-bin/launchctl"),
                include_str!("fixtures/maintenance_service_manager.py"),
            );
            write_executable_script(&root.child("openclaw"), "#!/bin/sh\nexit 93\n");
            command(
                &root,
                &env,
                &[
                    "launcher",
                    "add",
                    "stable",
                    "--command",
                    &path_string(&root.child("openclaw")),
                    "--json",
                ],
            );
            for name in ["source", "sibling"] {
                command(
                    &root,
                    &env,
                    &["env", "create", name, "--launcher", "stable", "--json"],
                );
            }
            let marker = root.child("ocm-home/envs/source/.openclaw/workspace/marker.txt");
            support::write_text(&marker, "snapshot-state\n");
            let snapshot = command(
                &root,
                &env,
                &["env", "snapshot", "create", "source", "--json"],
            );
            fs::write(&marker, "current-state\n").unwrap();
            for name in ["source", "sibling"] {
                command(&root, &env, &["service", "start", name, "--json"]);
            }
            let before = command(&root, &env, &["service", "status", "source", "--json"]);
            assert_eq!(before["running"], true);
            if mode == "stopped-intent-error" {
                fs::write(root.child("armed"), "armed").unwrap();
                command(&root, &env, &["service", "stop", "source", "--json"]);
            }
            let registry_path = root.child("ocm-home/envs.json");
            let before_registry = fs::read(&registry_path).unwrap();
            let before_plan: Value = serde_json::from_slice(
                &fs::read(root.child("ocm-home/supervisor/state.json")).unwrap(),
            )
            .unwrap();
            fs::write(root.child("launchctl.log"), "").unwrap();
            fs::write(root.child("armed"), "armed").unwrap();
            let args = if capture {
                vec!["env", "snapshot", "create", "source", "--json"]
            } else {
                vec![
                    "env",
                    "snapshot",
                    "restore",
                    "source",
                    snapshot["id"].as_str().unwrap(),
                    "--json",
                ]
            };
            let result = run_ocm(root.path(), &env, &args);
            assert!(root.child("observed-fault").exists(), "{mode}");
            if mode == "normal" {
                assert!(result.status.success(), "{}", stderr(&result));
                assert_eq!(
                    fs::read_to_string(&marker).unwrap(),
                    if capture {
                        "current-state\n"
                    } else {
                        "snapshot-state\n"
                    }
                );
            } else {
                assert!(!result.status.success(), "{mode}: {}", stdout(&result));
                // Compensation preserves policy revision monotonicity while
                // restoring every other target field, including service intent.
                let mut after: Value =
                    serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
                let expected: Value = serde_json::from_slice(&before_registry).unwrap();
                let revision = expected["servicePolicyRevisions"]["source"]
                    .as_u64()
                    .unwrap();
                assert_eq!(
                    after["servicePolicyRevisions"]["source"],
                    revision + if mode == "stopped-intent-error" { 0 } else { 2 }
                );
                after["servicePolicyRevisions"]["source"] =
                    expected["servicePolicyRevisions"]["source"].clone();
                let target = expected["envs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["name"] == "source")
                    .unwrap();
                let restored = after["envs"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|r| r["name"] == "source")
                    .unwrap();
                restored["updatedAt"] = target["updatedAt"].clone();
                assert_eq!(after, expected, "{mode}: {}", stderr(&result));
                assert_eq!(
                    fs::read_to_string(&marker).unwrap(),
                    "current-state\n",
                    "{mode}"
                );
                let snapshots = command(
                    &root,
                    &env,
                    &["env", "snapshot", "list", "source", "--json"],
                );
                assert_eq!(snapshots.as_array().unwrap().len(), 1, "{mode}");
                if mode == "manager-error" || mode == "stopped-intent-error" {
                    assert!(
                        stderr(&result).contains("cannot confirm quiescence"),
                        "{}",
                        stderr(&result)
                    );
                    let runtime: Value = serde_json::from_slice(
                        &fs::read(root.child("ocm-home/supervisor/runtime.json")).unwrap(),
                    )
                    .unwrap();
                    assert!(
                        runtime["children"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|c| c["envName"] == "source")
                    );
                }
            }
            let after_registry: Value =
                serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
            let before_registry: Value = serde_json::from_slice(&before_registry).unwrap();
            let sibling = |value: &Value| {
                value["envs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["name"] == "sibling")
                    .unwrap()
                    .clone()
            };
            assert_eq!(sibling(&after_registry), sibling(&before_registry));
            let after_plan: Value = serde_json::from_slice(
                &fs::read(root.child("ocm-home/supervisor/state.json")).unwrap(),
            )
            .unwrap();
            let sibling_plan = |value: &Value| {
                value["children"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["envName"] == "sibling")
                    .unwrap()
                    .clone()
            };
            assert_eq!(sibling_plan(&before_plan), sibling_plan(&after_plan));
            let log = fs::read_to_string(root.child("launchctl.log")).unwrap();
            assert!(
                !log.contains("bootstrap") && !log.contains("bootout") && !log.contains("unload"),
                "{mode}: {log}"
            );
        }
    }
}
