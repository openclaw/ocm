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
