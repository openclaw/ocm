mod support;

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use fs2::FileExt;
use serde_json::Value;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};

#[test]
fn env_cleanup_yes_repairs_env_scoped_config_drift() {
    let root = TestDir::new("env-cleanup-config-repair");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let source = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let source_root = root.child("ocm-home/envs/source");
    let target_config = root.child("ocm-home/envs/target/.openclaw/openclaw.json");
    fs::write(
        &target_config,
        format!(
            "{{\n  \"agents\": {{\n    \"defaults\": {{\n      \"workspace\": \"{}\"\n    }}\n  }},\n  \"gateway\": {{\n    \"port\": 19789\n  }}\n}}\n",
            source_root.join(".openclaw/workspace").display()
        ),
    )
    .unwrap();

    let cleanup = run_ocm(&cwd, &env, &["env", "cleanup", "target", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    let output = stdout(&cleanup);
    assert!(
        output
            .contains("repair-openclaw-config: rewrite env-scoped OpenClaw config paths and ports")
    );

    let repaired = fs::read_to_string(target_config).unwrap();
    assert!(
        repaired.contains(
            &root
                .child("ocm-home/envs/target/.openclaw/workspace")
                .display()
                .to_string()
        )
    );
    assert!(repaired.contains("\"port\": 19790"));
}

#[cfg(unix)]
#[test]
fn env_cleanup_replaces_a_drifted_config_symlink_without_rewriting_its_target() {
    let root = TestDir::new("env-cleanup-config-symlink");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let source = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let source_root = root.child("ocm-home/envs/source");
    let target_config = root.child("ocm-home/envs/target/.openclaw/openclaw.json");
    let external_config = root.child("external/openclaw.json");
    let external_raw = format!(
        "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}},\"gateway\":{{\"port\":19789}}}}\n",
        source_root.join(".openclaw/workspace").display()
    );
    fs::create_dir_all(external_config.parent().unwrap()).unwrap();
    fs::write(&external_config, &external_raw).unwrap();
    if target_config.exists() {
        fs::remove_file(&target_config).unwrap();
    }
    std::os::unix::fs::symlink(&external_config, &target_config).unwrap();

    let cleanup = run_ocm(&cwd, &env, &["env", "cleanup", "target", "--yes"]);

    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    assert_eq!(fs::read_to_string(&external_config).unwrap(), external_raw);
    assert!(
        fs::symlink_metadata(&target_config)
            .unwrap()
            .file_type()
            .is_file()
    );
    let repaired = fs::read_to_string(target_config).unwrap();
    assert!(
        repaired.contains(
            &root
                .child("ocm-home/envs/target/.openclaw/workspace")
                .display()
                .to_string()
        )
    );
    assert!(repaired.contains("\"port\": 19790"));
}

#[test]
fn env_cleanup_waits_for_the_environment_operation_lock() {
    let root = TestDir::new("env-cleanup-operation-lock");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let source = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let source_root = root.child("ocm-home/envs/source");
    let target_config = root.child("ocm-home/envs/target/.openclaw/openclaw.json");
    let drifted = format!(
        "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}},\"gateway\":{{\"port\":19789}}}}\n",
        source_root.join(".openclaw/workspace").display()
    );
    fs::write(&target_config, &drifted).unwrap();

    let lock_path = root.child("ocm-home/locks/environments/target.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    lock.lock_exclusive().unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    command
        .current_dir(&cwd)
        .args(["env", "cleanup", "target", "--yes"])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    sleep(Duration::from_millis(250));

    assert!(
        child.try_wait().unwrap().is_none(),
        "cleanup completed while another environment mutation held the operation lock"
    );
    assert_eq!(fs::read_to_string(&target_config).unwrap(), drifted);

    FileExt::unlock(&lock).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let repaired = fs::read_to_string(target_config).unwrap();
    assert!(repaired.contains("\"port\": 19790"));
}

#[test]
fn env_cleanup_yes_repairs_inferred_env_scoped_config_drift() {
    let root = TestDir::new("env-cleanup-inferred-config-repair");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let target_config = root.child("ocm-home/envs/target/.openclaw/openclaw.json");
    fs::write(
        &target_config,
        "{\n  \"agents\": {\n    \"defaults\": {\n      \"workspace\": \"/tmp/external-source/.openclaw/workspace\"\n    }\n  },\n  \"memory\": {\n    \"logPath\": \"/tmp/external-source/.openclaw/logs/gateway.log\"\n  },\n  \"gateway\": {\n    \"port\": 19789\n  }\n}\n",
    )
    .unwrap();

    let cleanup = run_ocm(&cwd, &env, &["env", "cleanup", "target", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    let output = stdout(&cleanup);
    assert!(
        output
            .contains("repair-openclaw-config: rewrite env-scoped OpenClaw config paths and ports")
    );

    let repaired = fs::read_to_string(target_config).unwrap();
    assert!(
        repaired.contains(
            &root
                .child("ocm-home/envs/target/.openclaw/workspace")
                .display()
                .to_string()
        )
    );
    assert!(
        repaired.contains(
            &root
                .child("ocm-home/envs/target/.openclaw/logs/gateway.log")
                .display()
                .to_string()
        )
    );
    assert!(repaired.contains("\"port\": 19790"));
}

#[test]
fn env_cleanup_yes_clears_copied_openclaw_runtime_state() {
    let root = TestDir::new("env-cleanup-runtime-state-repair");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let source = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let source_root = root.child("ocm-home/envs/source");
    let target_state = root.child("ocm-home/envs/target/.openclaw");
    fs::create_dir_all(target_state.join("agents/main/agent")).unwrap();
    fs::create_dir_all(target_state.join("agents/main/sessions")).unwrap();
    fs::create_dir_all(target_state.join("logs")).unwrap();
    fs::write(
        target_state.join("agents/main/agent/auth-profiles.json"),
        "{\n  \"profiles\": {\"local\": {\"provider\": \"openai-codex\"}}\n}\n",
    )
    .unwrap();
    fs::write(
        target_state.join("agents/main/sessions/main.jsonl"),
        format!(
            "{{\"cwd\":\"{}\"}}\n",
            source_root.join(".openclaw/workspace").display()
        ),
    )
    .unwrap();
    fs::write(
        target_state.join("logs/gateway.log"),
        format!("root={}\n", source_root.display()),
    )
    .unwrap();
    let workshop_content = format!("Reviewed content from {}\n", source_root.display());
    let workshop_artifacts = [
        "skill-workshop/proposals/example/PROPOSAL.md",
        "skill-workshop/proposals/example/references/tmp/proof.md",
        "skill-workshop/proposals/example/scripts/package.lock",
        "skill-workshop/recovery/proposals/example/rollback.json",
    ];
    for relative in workshop_artifacts {
        let path = target_state.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &workshop_content).unwrap();
    }

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "target", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let doctor_value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(doctor_value["healthy"], false);
    let issues = doctor_value["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        let issue = issue.as_str().unwrap();
        issue.contains("OpenClaw runtime state contains") && issue.contains("env \"source\" root")
    }));

    let cleanup = run_ocm(&cwd, &env, &["env", "cleanup", "target", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    let output = stdout(&cleanup);
    assert!(output.contains(
        "reset-openclaw-runtime-state: clear copied OpenClaw runtime state outside config and workspace"
    ));

    assert!(
        target_state
            .join("agents/main/agent/auth-profiles.json")
            .exists()
    );
    assert!(!target_state.join("agents/main/sessions").exists());
    assert!(!target_state.join("logs").exists());
    for relative in workshop_artifacts {
        assert_eq!(
            fs::read_to_string(target_state.join(relative)).unwrap_or_else(|error| panic!(
                "cleanup lost Skill Workshop artifact {relative}: {error}"
            )),
            workshop_content
        );
    }
}

#[test]
fn env_cleanup_yes_clears_inferred_foreign_runtime_state_without_source_metadata() {
    let root = TestDir::new("env-cleanup-inferred-runtime-state-repair");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let target_state = root.child("ocm-home/envs/target/.openclaw");
    fs::create_dir_all(target_state.join("agents/main/agent")).unwrap();
    fs::create_dir_all(target_state.join("agents/main/sessions")).unwrap();
    fs::create_dir_all(target_state.join("logs")).unwrap();
    fs::write(
        target_state.join("agents/main/agent/auth-profiles.json"),
        "{\n  \"profiles\": {\"local\": {\"provider\": \"openai-codex\"}}\n}\n",
    )
    .unwrap();
    fs::write(
        target_state.join("agents/main/sessions/main.jsonl"),
        "{\"cwd\":\"/tmp/external-source/.openclaw/workspace\"}\n",
    )
    .unwrap();
    fs::write(
        target_state.join("logs/gateway.log"),
        "root=/tmp/external-source/.openclaw/logs\n",
    )
    .unwrap();

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "target", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let doctor_value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(doctor_value["healthy"], false);
    let issues = doctor_value["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        let issue = issue.as_str().unwrap();
        issue.contains("OpenClaw runtime state contains")
            && issue.contains("env-scoped path reference(s) outside the current env root")
    }));

    let cleanup = run_ocm(&cwd, &env, &["env", "cleanup", "target", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    let output = stdout(&cleanup);
    assert!(output.contains(
        "reset-openclaw-runtime-state: clear copied OpenClaw runtime state outside config and workspace"
    ));

    assert!(
        target_state
            .join("agents/main/agent/auth-profiles.json")
            .exists()
    );
    assert!(!target_state.join("agents/main/sessions").exists());
    assert!(!target_state.join("logs").exists());
}

#[cfg(any(unix, windows))]
fn fixture_path_spellings(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = vec![path.to_path_buf(), fs::canonicalize(path).unwrap()];
    if cfg!(windows) {
        paths.push(std::path::PathBuf::from(
            paths[1].to_str().unwrap().strip_prefix(r"\\?\").unwrap(),
        ));
        paths.push(std::path::PathBuf::from(format!(r"\\?\{}", path.display())));
    }
    paths
}

#[cfg(any(unix, windows))]
#[test]
fn env_cleanup_preserves_configured_agent_payload_under_root_spellings() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    for spelling in 0..if cfg!(windows) { 4 } else { 2 } {
        for relative in [
            "extensions/demo/node_modules/private-agent",
            "custom/private-agent",
            "agents/main",
            "agents",
        ] {
            let root = TestDir::new("cleanup-configured-agent-payload");
            let cwd = root.path();
            let env = ocm_env(&root);
            let physical = root.child("physical");
            let alias = root.child("alias");
            fs::create_dir_all(&physical).unwrap();
            symlink_dir(&physical, &alias).unwrap();
            let target = alias.join("target");
            let created = run_ocm(cwd, &env, &["env", "create", "source"]);
            assert!(created.status.success(), "{}", stderr(&created));
            let created = run_ocm(
                cwd,
                &env,
                &[
                    "env",
                    "create",
                    "target",
                    "--root",
                    target.to_str().unwrap(),
                ],
            );
            assert!(created.status.success(), "{}", stderr(&created));
            let source_config = root.child("ocm-home/envs/source/.openclaw/openclaw.json");
            crate::support::write_text(&source_config, "{}\n");
            let source_before = fs::read(&source_config).unwrap();
            let state = target.join(".openclaw");
            let agent = state.join(relative.split('/').collect::<std::path::PathBuf>());
            let payload = agent.join("auth-profiles.json");
            // A retained payload may itself quote a source path; it is not runtime
            // drift and must not keep scheduling resets after the stale session goes.
            let auth =
                serde_json::json!({"fixture":true,"source":root.child("ocm-home/envs/source")})
                    .to_string();
            crate::support::write_text(&payload, &auth);
            let configured = fixture_path_spellings(&agent).remove(spelling);
            let config = serde_json::json!({
                "agents":{"entries":{"main":{"agentDir":configured}}}
            })
            .to_string();
            crate::support::write_text(&state.join("openclaw.json"), &config);
            let stale = state.join("agents/main/sessions/stale.jsonl");
            crate::support::write_text(
                &stale,
                &serde_json::json!({"cwd":root.child("ocm-home/envs/source/.openclaw/workspace")})
                    .to_string(),
            );
            let debris = [
                "extensions/demo/node_modules/unselected/index.js",
                "extensions/demo/.openclaw-runtime-deps.json",
                "custom/unselected.txt",
            ];
            for relative in debris {
                crate::support::write_text(&state.join(relative), "debris");
            }
            let preview = run_ocm(cwd, &env, &["env", "cleanup", "target", "--json"]);
            assert!(preview.status.success(), "{}", stderr(&preview));
            let preview: Value = serde_json::from_str(&stdout(&preview)).unwrap();
            assert!(preview["actions"].as_array().unwrap().iter().any(|action| {
                action["kind"] == "reset-openclaw-runtime-state" && action["applied"] == false
            }));
            assert!(stale.exists());
            for relative in debris {
                assert!(state.join(relative).exists());
            }
            assert_eq!(fs::read(&payload).unwrap(), auth.as_bytes());
            assert_eq!(
                fs::read(state.join("openclaw.json")).unwrap(),
                config.as_bytes()
            );

            let cleanup = run_ocm(cwd, &env, &["env", "cleanup", "target", "--yes", "--json"]);
            assert!(cleanup.status.success(), "{}", stderr(&cleanup));
            let cleanup: Value = serde_json::from_str(&stdout(&cleanup)).unwrap();
            assert!(cleanup["actions"].as_array().unwrap().iter().any(|action| {
                action["kind"] == "reset-openclaw-runtime-state" && action["applied"] == true
            }));
            assert!(!stale.exists());
            for relative in debris {
                assert!(!state.join(relative).exists(), "{relative}");
            }
            assert_eq!(fs::read(payload).unwrap(), auth.as_bytes());
            let applied_config = fs::read(state.join("openclaw.json")).unwrap();
            if spelling == 0 {
                assert_eq!(applied_config, config.as_bytes());
            } else {
                // Successful cleanup retains its existing config repair: an
                // alternate physical-root spelling becomes the accepted root.
                let mut expected: Value = serde_json::from_str(&config).unwrap();
                expected["agents"]["entries"]["main"]["agentDir"] = serde_json::json!(agent);
                assert_eq!(
                    serde_json::from_slice::<Value>(&applied_config).unwrap(),
                    expected
                );
            }
            assert_eq!(fs::read(source_config).unwrap(), source_before);
            let after = run_ocm(cwd, &env, &["env", "cleanup", "target", "--json"]);
            assert!(after.status.success(), "{}", stderr(&after));
            let after: Value = serde_json::from_str(&stdout(&after)).unwrap();
            assert!(
                !after["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|action| { action["kind"] == "reset-openclaw-runtime-state" })
            );
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn env_cleanup_refuses_agent_session_collision_before_config_repair_under_root_spellings() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    let root = TestDir::new("cleanup-agent-session-collision");
    let cwd = root.path();
    let env = ocm_env(&root);
    let physical = root.child("physical");
    let alias = root.child("alias");
    fs::create_dir_all(&physical).unwrap();
    symlink_dir(&physical, &alias).unwrap();
    let target = alias.join("target");
    let created = run_ocm(cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let created = run_ocm(
        cwd,
        &env,
        &[
            "env",
            "create",
            "target",
            "--port",
            "19790",
            "--root",
            target.to_str().unwrap(),
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let state = target.join(".openclaw");
    let sessions = state.join("agents/main/sessions");
    let source_config = root.child("ocm-home/envs/source/.openclaw/openclaw.json");
    fs::create_dir_all(&sessions).unwrap();
    for configured in fixture_path_spellings(&sessions) {
        for other_error in ["none", "agent", "workspace"] {
            let mut config = serde_json::json!({
                "agents": {
                    "entries": {"main": {"agentDir": configured}},
                    "defaults": {"workspace": root.child("ocm-home/envs/source/.openclaw/workspace")}
                },
                "gateway": {"port": 19789}
            });
            match other_error {
                "agent" => config["agents"]["entries"]["a"] = serde_json::json!({"agentDir":state}),
                "workspace" => {
                    config["agents"]["defaults"]["workspace"] =
                        serde_json::json!("relative-workspace")
                }
                _ => {}
            }
            let files = [
                (source_config.clone(), "{}\n".to_string()),
                (
                    sessions.join("auth-profiles.json"),
                    "auth fixture".to_string(),
                ),
                (
                    sessions.join("stale.jsonl"),
                    serde_json::json!({
                        "cwd":root.child("ocm-home/envs/source/.openclaw/workspace")
                    })
                    .to_string(),
                ),
                (state.join("openclaw.json"), config.to_string()),
                (
                    state.join("extensions/demo/.openclaw-runtime-deps.json"),
                    "{}".to_string(),
                ),
                (
                    state.join("extensions/demo/node_modules/index.js"),
                    "generated".to_string(),
                ),
            ];
            for (path, contents) in &files {
                crate::support::write_text(path, contents);
            }
            for args in [
                vec!["env", "cleanup", "target", "--json"],
                vec!["env", "cleanup", "target", "--yes", "--json"],
                vec!["env", "cleanup", "--all", "--yes", "--json"],
            ] {
                let cleanup = run_ocm(cwd, &env, &args);
                assert!(!cleanup.status.success());
                assert!(
                    stderr(&cleanup)
                        .contains("configured agent directory overlaps canonical session storage"),
                    "{}",
                    stderr(&cleanup)
                );
                for (path, contents) in &files {
                    assert_eq!(
                        fs::read(path).unwrap(),
                        contents.as_bytes(),
                        "{}",
                        path.display()
                    );
                }
            }
            let doctor = run_ocm(cwd, &env, &["env", "doctor", "target", "--json"]);
            assert!(doctor.status.success(), "{}", stderr(&doctor));
            let doctor: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
            assert_eq!(doctor["healthy"], false);
            assert!(doctor["issues"].as_array().unwrap().iter().any(|issue| {
                issue
                    .as_str()
                    .unwrap()
                    .contains("configured agent directory overlaps canonical session storage")
            }));
        }
    }
}

#[test]
fn env_cleanup_retains_external_agent_directory_policy() {
    let root = TestDir::new("cleanup-external-agent-directory");
    let cwd = root.path();
    let env = ocm_env(&root);
    for name in ["source", "target"] {
        let created = run_ocm(cwd, &env, &["env", "create", name]);
        assert!(created.status.success(), "{}", stderr(&created));
    }
    let state = root.child("ocm-home/envs/target/.openclaw");
    let external = root.child("external-agent/models.json");
    crate::support::write_text(&external, "external payload");
    crate::support::write_text(
        &state.join("openclaw.json"),
        &serde_json::json!({
            "agents":{"entries":{"main":{"agentDir":external.parent().unwrap()}}}
        })
        .to_string(),
    );
    let stale = state.join("agents/main/sessions/stale.jsonl");
    crate::support::write_text(
        &stale,
        &serde_json::json!({"cwd":root.child("ocm-home/envs/source/.openclaw/workspace")})
            .to_string(),
    );
    let cleanup = run_ocm(cwd, &env, &["env", "cleanup", "target", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    assert!(!stale.exists());
    assert_eq!(fs::read(external).unwrap(), b"external payload");
}

fn cleanup_source_fixture(
    root: &TestDir,
    location: &str,
    internal_origin: bool,
    separate_git: bool,
) -> (
    std::collections::BTreeMap<String, String>,
    std::path::PathBuf,
) {
    use crate::support::{path_string, write_text};
    let env = ocm_env(root);
    for name in ["donor", "container"] {
        let created = run_ocm(root.path(), &env, &["env", "create", name]);
        assert!(created.status.success(), "{}", stderr(&created));
    }
    let donor = root.child(if internal_origin {
        "ocm-home/envs/donor/openclaw"
    } else {
        "external-origin"
    });
    write_text(
        &donor.join("package.json"),
        r#"{"name":"openclaw","version":"1.0.0"}"#,
    );
    for path in [
        "scripts/run-node.mjs",
        "scripts/watch-node.mjs",
        "openclaw.mjs",
    ] {
        write_text(&donor.join(path), "console.log('finite source fixture');\n");
    }
    write_text(&donor.join("extensions/.keep"), "fixture\n");
    cleanup_git(&donor, &env, &["init"]);
    cleanup_git(&donor, &env, &["add", "."]);
    cleanup_git(
        &donor,
        &env,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "fixture",
        ],
    );
    let source = root.child(location);
    let metadata = root.child("ocm-home/envs/container/.openclaw/logs/source.git");
    let mut clone = vec!["clone", "--no-hardlinks"];
    let metadata_string = path_string(&metadata);
    if separate_git {
        fs::create_dir_all(metadata.parent().unwrap()).unwrap();
        clone.extend(["--separate-git-dir", &metadata_string]);
    }
    let source_string = path_string(&source);
    let donor_string = path_string(&donor);
    clone.extend([donor_string.as_str(), source_string.as_str()]);
    cleanup_git(root.path(), &env, &clone);
    #[cfg(unix)]
    let selected = {
        let alias = root.child("source-alias");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        alias
    };
    #[cfg(not(unix))]
    let selected = source.clone();
    let registered = run_ocm(
        root.path(),
        &env,
        &[
            "dev",
            "borrower",
            "--repo",
            &path_string(&selected),
            "--no-watch",
            "--no-ui",
        ],
    );
    assert!(registered.status.success(), "{}", stderr(&registered));
    (env, source)
}

fn cleanup_git(
    repo: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
    args: &[&str],
) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_clear()
        .envs(env)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn env_cleanup_refuses_registered_source_deletions_before_any_repair() {
    use crate::support::write_text;
    for (location, separate_git) in [
        ("ocm-home/envs/container/.openclaw/logs/source", false),
        (
            "ocm-home/envs/container/.openclaw/agents/main/sessions/source",
            false,
        ),
        (
            "ocm-home/envs/container/.openclaw/extensions/demo/node_modules/source",
            false,
        ),
        ("external-source", true),
    ] {
        let root = TestDir::new("cleanup-registered-source");
        let (env, source) = cleanup_source_fixture(&root, location, true, separate_git);
        let state = root.child("ocm-home/envs/container/.openclaw");
        let config_path = state.join("openclaw.json");
        // Config repair would execute before runtime reset without a whole-plan guard.
        let config = r#"{"gateway":{"port":1}}"#;
        write_text(&config_path, config);
        let stale = state.join("agents/main/sessions/stale.jsonl");
        write_text(
            &stale,
            &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
                .to_string(),
        );
        let debris = state.join("extensions/demo/.openclaw-runtime-deps.json");
        write_text(&debris, "{}");
        write_text(&state.join("tmp/keep-until-admitted"), "residue");
        write_text(&source.join("uncommitted.txt"), "authored work");
        let registry = ocm::store::env_registry_path(&env, root.path()).unwrap();
        let registry_before = fs::read(&registry).unwrap();
        let source_config = fs::read(if separate_git {
            state.join("logs/source.git/config")
        } else {
            source.join(".git/config")
        })
        .unwrap();
        for args in [
            vec!["env", "cleanup", "container", "--json"],
            vec!["env", "cleanup", "container", "--yes", "--json"],
            vec!["env", "cleanup", "--all", "--yes", "--json"],
            vec!["env", "remove", "container"],
        ] {
            let rejected = run_ocm(root.path(), &env, &args);
            assert!(!rejected.status.success(), "{}", stdout(&rejected));
            assert!(
                stderr(&rejected).contains("registered dev source for env borrower"),
                "{}",
                stderr(&rejected)
            );
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert_eq!(fs::read_to_string(&config_path).unwrap(), config);
            assert_eq!(
                fs::read_to_string(source.join("uncommitted.txt")).unwrap(),
                "authored work"
            );
            assert!(
                stale.exists() && debris.exists() && state.join("tmp/keep-until-admitted").exists()
            );
        }
        assert_eq!(
            fs::read(if separate_git {
                state.join("logs/source.git/config")
            } else {
                source.join(".git/config")
            })
            .unwrap(),
            source_config
        );
        cleanup_git(&source, &env, &["cat-file", "-t", "HEAD"]);
        cleanup_git(&source, &env, &["fsck", "--full"]);
    }
}

#[test]
fn env_cleanup_allows_sources_outside_the_prepared_reset_plan() {
    use crate::support::write_text;
    for mode in ["no-reset", "workspace", "repaired-agent"] {
        let root = TestDir::new("cleanup-source-preservation-control");
        let location = match mode {
            "workspace" => "ocm-home/envs/container/.openclaw/workspace/source",
            "repaired-agent" => "ocm-home/envs/container/.openclaw/custom/agent/source",
            _ => "ocm-home/envs/container/.openclaw/logs/source",
        };
        let (env, source) = cleanup_source_fixture(&root, location, mode != "no-reset", false);
        let state = root.child("ocm-home/envs/container/.openclaw");
        let stale = state.join("agents/main/sessions/stale.jsonl");
        if mode != "no-reset" {
            write_text(
                &stale,
                &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
                    .to_string(),
            );
        }
        if mode == "repaired-agent" {
            write_text(&state.join("openclaw.json"), &serde_json::json!({
                "agents":{"entries":{"main":{"agentDir":root.child("ocm-home/envs/donor/.openclaw/custom/agent")}}}
            }).to_string());
        }
        for args in [
            vec!["env", "cleanup", "container", "--json"],
            vec!["env", "cleanup", "container", "--yes", "--json"],
        ] {
            let result = run_ocm(root.path(), &env, &args);
            assert!(result.status.success(), "{mode}: {}", stderr(&result));
            let result: Value = serde_json::from_str(&stdout(&result)).unwrap();
            assert_eq!(
                result["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|action| action["kind"] == "reset-openclaw-runtime-state"),
                mode != "no-reset"
            );
        }
        assert!(!stale.exists());
        assert!(source.join("package.json").exists());
        cleanup_git(&source, &env, &["cat-file", "-t", "HEAD"]);
        cleanup_git(&source, &env, &["fsck", "--full"]);
        let rejected = run_ocm(root.path(), &env, &["env", "remove", "container"]);
        assert!(!rejected.status.success());
        assert!(
            stderr(&rejected).contains("registered dev source for env borrower"),
            "{}",
            stderr(&rejected)
        );
    }
}

#[cfg(unix)]
#[test]
fn env_cleanup_unlinks_unregistered_alias_without_deleting_borrowed_source() {
    use crate::support::write_text;
    let root = TestDir::new("cleanup-unregistered-source-alias");
    let (env, source) = cleanup_source_fixture(&root, "external-source", true, false);
    let state = root.child("ocm-home/envs/container/.openclaw");
    let alias = state.join("source-link");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    write_text(
        &state.join("agents/main/sessions/stale.jsonl"),
        &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
            .to_string(),
    );
    let cleanup = run_ocm(root.path(), &env, &["env", "cleanup", "container", "--yes"]);
    assert!(cleanup.status.success(), "{}", stderr(&cleanup));
    assert!(!alias.exists());
    cleanup_git(&source, &env, &["cat-file", "-t", "HEAD"]);
    cleanup_git(&source, &env, &["fsck", "--full"]);
}

fn cleanup_legacy_child_fixture(
    root: &TestDir,
    location: Option<&str>,
) -> std::collections::BTreeMap<String, String> {
    use crate::support::{path_string, write_text};
    use ocm::store::{get_environment, save_environment};

    let env = ocm_env(root);
    for name in ["a-parent", "child", "donor"] {
        let created = run_ocm(root.path(), &env, &["env", "create", name]);
        assert!(created.status.success(), "{}", stderr(&created));
    }
    let mut child = get_environment("child", &env, root.path()).unwrap();
    if let Some(location) = location {
        let nested = root
            .child("ocm-home/envs/a-parent/.openclaw")
            .join(location);
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        // Reconstruct a persisted 0.2.44 layout; new root admission stays refused.
        fs::rename(&child.root, &nested).unwrap();
        child.root = path_string(&nested);
    }
    child.protected = true;
    let child = save_environment(child, &env, root.path()).unwrap();
    let state = std::path::Path::new(&child.root).join(".openclaw");
    write_text(&state.join("workspace/keep.txt"), "protected child data\n");
    write_text(
        &state.join("openclaw.json"),
        &serde_json::json!({"gateway":{"port":child.gateway_port}}).to_string(),
    );
    env
}

#[test]
fn env_cleanup_refuses_legacy_child_before_config_binding_or_file_repairs() {
    use crate::support::write_text;
    use ocm::store::{env_registry_path, get_environment, save_environment};

    let root = TestDir::new("cleanup-legacy-child");
    let env = cleanup_legacy_child_fixture(&root, Some("logs/child"));
    let state = root.child("ocm-home/envs/a-parent/.openclaw");
    let config = state.join("openclaw.json");
    write_text(&config, r#"{"gateway":{"port":1}}"#);
    let mut parent = get_environment("a-parent", &env, root.path()).unwrap();
    parent.default_runtime = Some("missing-runtime".into());
    save_environment(parent, &env, root.path()).unwrap();
    let stale = state.join("agents/main/sessions/stale.jsonl");
    write_text(
        &stale,
        &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
            .to_string(),
    );
    let debris = state.join("tmp/keep-until-admitted");
    write_text(&debris, "runtime residue");
    let registry = env_registry_path(&env, root.path()).unwrap();
    let preserved = [
        registry,
        config,
        stale,
        debris,
        state.join("logs/child/.openclaw/openclaw.json"),
        state.join("logs/child/.openclaw/workspace/keep.txt"),
    ];
    let before: Vec<_> = preserved
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    for args in [
        vec!["env", "cleanup", "a-parent", "--json"],
        vec!["env", "cleanup", "a-parent", "--yes", "--json"],
        vec!["env", "cleanup", "--all", "--json"],
        vec!["env", "cleanup", "--all", "--yes", "--json"],
        vec!["env", "remove", "a-parent", "--force"],
    ] {
        let refused = run_ocm(root.path(), &env, &args);
        assert!(!refused.status.success(), "{}", stdout(&refused));
        assert!(
            stderr(&refused).contains("registered environment child"),
            "{}",
            stderr(&refused)
        );
        // a-parent is first in --all: this proves its preflight, not batch rollback.
        for (path, bytes) in preserved.iter().zip(&before) {
            assert_eq!(&fs::read(path).unwrap(), bytes, "{}", path.display());
        }
    }
}

#[test]
fn env_cleanup_allows_legacy_roots_outside_selected_deletions() {
    use crate::support::write_text;
    use ocm::store::{env_registry_path, get_environment};

    for (location, reset) in [
        (Some("logs/child"), false),
        (Some("workspace/child"), true),
        (None, true),
    ] {
        let root = TestDir::new("cleanup-legacy-child-control");
        let env = cleanup_legacy_child_fixture(&root, location);
        let child = get_environment("child", &env, root.path()).unwrap();
        let child_state = std::path::Path::new(&child.root).join(".openclaw");
        let config = fs::read(child_state.join("openclaw.json")).unwrap();
        let registry = env_registry_path(&env, root.path()).unwrap();
        let registry_before = fs::read(&registry).unwrap();
        let stale = root.child("ocm-home/envs/a-parent/.openclaw/agents/main/sessions/stale.jsonl");
        if reset {
            write_text(
                &stale,
                &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
                    .to_string(),
            );
        }
        for args in [
            vec!["env", "cleanup", "a-parent", "--json"],
            vec!["env", "cleanup", "a-parent", "--yes", "--json"],
        ] {
            let output = run_ocm(root.path(), &env, &args);
            assert!(output.status.success(), "{}", stderr(&output));
            let result: Value = serde_json::from_str(&stdout(&output)).unwrap();
            assert_eq!(
                result["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|action| { action["kind"] == "reset-openclaw-runtime-state" }),
                reset
            );
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert_eq!(fs::read(child_state.join("openclaw.json")).unwrap(), config);
            assert_eq!(
                fs::read_to_string(child_state.join("workspace/keep.txt")).unwrap(),
                "protected child data\n"
            );
        }
        assert!(!stale.exists());
    }
}

#[cfg(unix)]
#[test]
fn env_cleanup_preserves_registered_access_links_but_unlinks_unused_aliases() {
    use crate::support::{path_string, write_text};
    use ocm::store::{env_registry_path, get_environment, save_environment};
    use std::os::unix::fs::symlink;

    for registered_link in [false, true] {
        let root = TestDir::new("cleanup-legacy-child-link");
        let env = cleanup_legacy_child_fixture(&root, None);
        let mut child = get_environment("child", &env, root.path()).unwrap();
        let physical = std::path::PathBuf::from(&child.root);
        let state = root.child("ocm-home/envs/a-parent/.openclaw");
        let link = state.join("child-link");
        symlink(&physical, &link).unwrap();
        if registered_link {
            child.root = path_string(&link);
            save_environment(child, &env, root.path()).unwrap();
        }
        let registry = env_registry_path(&env, root.path()).unwrap();
        let before = fs::read(&registry).unwrap();
        let stale = state.join("agents/main/sessions/stale.jsonl");
        write_text(
            &stale,
            &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
                .to_string(),
        );
        for args in [
            vec!["env", "cleanup", "a-parent", "--json"],
            vec!["env", "cleanup", "a-parent", "--yes", "--json"],
        ] {
            let result = run_ocm(root.path(), &env, &args);
            assert_eq!(
                result.status.success(),
                !registered_link,
                "{}",
                stderr(&result)
            );
            if registered_link {
                assert!(stderr(&result).contains("registered environment child"));
                assert_eq!(fs::read_link(&link).unwrap(), physical);
                assert!(stale.exists());
            }
            assert_eq!(fs::read(&registry).unwrap(), before);
            assert_eq!(
                fs::read_to_string(physical.join(".openclaw/workspace/keep.txt")).unwrap(),
                "protected child data\n"
            );
        }
        if !registered_link {
            assert!(fs::symlink_metadata(&link).is_err());
            assert!(!stale.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn env_cleanup_only_refuses_registered_cycles_inside_selected_deletions() {
    use crate::support::write_text;
    use ocm::store::{env_registry_path, get_environment};
    use std::os::unix::fs::symlink;

    for location in [None, Some("logs/child")] {
        let root = TestDir::new("cleanup-registered-root-cycle");
        let env = cleanup_legacy_child_fixture(&root, location);
        let child = get_environment("child", &env, root.path()).unwrap();
        let child_root = std::path::Path::new(&child.root);
        let retained = root.child("retained-child");
        fs::rename(child_root, &retained).unwrap();
        symlink(child_root.file_name().unwrap(), child_root).unwrap();
        let registry = env_registry_path(&env, root.path()).unwrap();
        let registry_before = fs::read(&registry).unwrap();
        let stale = root.child("ocm-home/envs/a-parent/.openclaw/agents/main/sessions/stale.jsonl");
        write_text(
            &stale,
            &serde_json::json!({"cwd":root.child("ocm-home/envs/donor/.openclaw/workspace")})
                .to_string(),
        );
        for apply in [false, true] {
            let mut args = vec!["env", "cleanup", "a-parent", "--json"];
            if apply {
                args.push("--yes");
            }
            let result = run_ocm(root.path(), &env, &args);
            assert_eq!(
                result.status.success(),
                location.is_none(),
                "{}",
                stderr(&result)
            );
            if location.is_some() {
                assert!(stderr(&result).contains("registered environment child"));
            } else {
                let result: Value = serde_json::from_str(&stdout(&result)).unwrap();
                assert!(result["actions"].as_array().unwrap().iter().any(|action| {
                    action["kind"] == "reset-openclaw-runtime-state" && action["applied"] == apply
                }));
            }
            assert_eq!(stale.exists(), location.is_some() || !apply);
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert_eq!(
                fs::read_link(child_root).unwrap(),
                child_root.file_name().unwrap()
            );
            assert_eq!(
                fs::read_to_string(retained.join(".openclaw/workspace/keep.txt")).unwrap(),
                "protected child data\n"
            );
        }
    }
}
