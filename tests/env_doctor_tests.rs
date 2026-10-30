mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::support::{
    TestDir, TestHttpServer, install_fake_node_and_npm, ocm_env, openclaw_package_tarball, run_ocm,
    sha512_integrity, stderr, stdout, write_executable_script,
};

#[test]
fn env_doctor_reports_a_healthy_launcher_bound_environment() {
    let root = TestDir::new("env-doctor-launcher");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "shell", "--command", "printf launcher"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "shell"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "demo"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let output = stdout(&doctor);
    assert!(output.contains("healthy: true"));
    assert!(output.contains("rootStatus: ok"));
    assert!(output.contains("configStatus: absent"));
    assert!(output.contains("launcherStatus: ok"));
    assert!(output.contains("resolutionStatus: ok"));
    assert!(output.contains("resolvedKind: launcher"));
    assert!(output.contains("resolvedName: shell"));
}

fn add_launcher_bound_env(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    name: &str,
    run_dir: &Path,
) -> String {
    let add = run_ocm(
        cwd,
        env,
        &[
            "launcher",
            "add",
            name,
            "--command",
            "echo launcher-ran",
            "--cwd",
            run_dir.to_str().unwrap(),
        ],
    );
    assert!(add.status.success(), "{}", stderr(&add));
    let create = run_ocm(cwd, env, &["env", "create", name, "--launcher", name]);
    assert!(create.status.success(), "{}", stderr(&create));
    let show = run_ocm(cwd, env, &["launcher", "show", name, "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let launcher: Value = serde_json::from_str(&stdout(&show)).unwrap();
    launcher["cwd"].as_str().unwrap().to_string()
}

fn assert_launcher_working_directory_reported(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    name: &str,
    expected_issue: &str,
) {
    let doctor = run_ocm(cwd, env, &["env", "doctor", name, "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], false);
    assert_eq!(value["launcherStatus"], "broken");
    assert_eq!(value["resolutionStatus"], "error");
    assert_eq!(value["issues"], serde_json::json!([expected_issue]));

    let status = run_ocm(cwd, env, &["env", "status", name, "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(value["issue"], expected_issue);

    let run = run_ocm(cwd, env, &["env", "run", name, "--"]);
    assert!(!run.status.success());
    let error = stderr(&run);
    assert!(error.contains(expected_issue), "{error}");
    assert!(!error.contains("failed to run"), "{error}");
    assert!(!stdout(&run).contains("launcher-ran"));
}

#[test]
fn env_doctor_status_and_run_report_a_missing_launcher_working_directory() {
    let root = TestDir::new("env-doctor-launcher-missing-cwd");
    let cwd = root.child("workspace");
    let checkout = cwd.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    let env = ocm_env(&root);
    let recorded = add_launcher_bound_env(&cwd, &env, "dev", &checkout);

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "dev", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], true);
    assert_eq!(value["launcherStatus"], "ok");
    let status = run_ocm(&cwd, &env, &["env", "status", "dev", "--json"]);
    let value: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(value["issue"], Value::Null);
    let run = run_ocm(&cwd, &env, &["env", "run", "dev", "--"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(stdout(&run).contains("launcher-ran"));

    fs::remove_dir_all(&checkout).unwrap();
    assert_launcher_working_directory_reported(
        &cwd,
        &env,
        "dev",
        &format!("launcher \"dev\" working directory does not exist: {recorded}"),
    );
}

#[test]
fn env_doctor_status_and_run_report_a_launcher_working_directory_that_is_a_file() {
    let root = TestDir::new("env-doctor-launcher-file-cwd");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let file = cwd.join("checkout");
    fs::write(&file, "").unwrap();
    let env = ocm_env(&root);
    let recorded = add_launcher_bound_env(&cwd, &env, "dev", &file);

    assert_launcher_working_directory_reported(
        &cwd,
        &env,
        "dev",
        &format!("launcher \"dev\" working directory is not a directory: {recorded}"),
    );
}

#[test]
fn env_doctor_json_reports_runtime_health_issues() {
    let root = TestDir::new("env-doctor-runtime-issues");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    fs::remove_file(&runtime_path).unwrap();

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "demo", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], false);
    assert_eq!(value["rootStatus"], "ok");
    assert_eq!(value["configStatus"], "absent");
    assert_eq!(value["runtimeStatus"], "broken");
    assert_eq!(value["launcherStatus"], "unbound");
    assert_eq!(value["resolutionStatus"], "error");
    assert_eq!(value["resolvedKind"], "runtime");
    assert_eq!(value["resolvedName"], "stable");
    assert_eq!(value["runtimeSourceKind"], "registered");
    assert_eq!(value["runtimeReleaseVersion"], Value::Null);
    assert_eq!(value["runtimeReleaseChannel"], Value::Null);
    let issues = value["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0]
            .as_str()
            .unwrap()
            .contains("runtime \"stable\" binary path does not exist")
    );
}

#[test]
fn env_doctor_keeps_official_runtime_healthy_when_managed_fallback_is_available() {
    let root = TestDir::new("env-doctor-official-runtime-host-issue");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let tarball =
        openclaw_package_tarball("#!/usr/bin/env node\nconsole.log('stable');\n", "2026.3.24");
    let integrity = sha512_integrity(&tarball);
    let tarball_server = TestHttpServer::serve_bytes(
        "/openclaw-2026.3.24.tgz",
        "application/octet-stream",
        &tarball,
    );
    let packument = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let packument_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", packument.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        packument_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let empty_path = root.child("empty-bin");
    fs::create_dir_all(&empty_path).unwrap();
    let mut doctor_env = env.clone();
    doctor_env.insert("PATH".to_string(), empty_path.to_string_lossy().to_string());
    doctor_env.insert(
        "OCM_INTERNAL_NPM_BIN".to_string(),
        root.child("fake-node-bin/npm")
            .to_string_lossy()
            .to_string(),
    );

    let doctor = run_ocm(&cwd, &doctor_env, &["env", "doctor", "demo", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], true);
    assert_eq!(value["configStatus"], "absent");
    assert_eq!(value["runtimeStatus"], "ok");
    assert_eq!(value["runtimeSourceKind"], "installed");
    assert_eq!(value["runtimeReleaseVersion"], "2026.3.24");
    assert_eq!(value["runtimeReleaseChannel"], "stable");
    let issues = value["issues"].as_array().unwrap();
    assert!(issues.is_empty());
}

#[test]
fn env_doctor_reports_env_scoped_config_drift() {
    let root = TestDir::new("env-doctor-config-drift");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create_source = run_ocm(&cwd, &env, &["env", "create", "source", "--port", "19789"]);
    assert!(create_source.status.success(), "{}", stderr(&create_source));
    let create_target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(create_target.status.success(), "{}", stderr(&create_target));

    let source_root = root.child("ocm-home/envs/source");
    fs::write(
        root.child("ocm-home/envs/target/.openclaw/openclaw.json"),
        format!(
            "{{\n  \"agents\": {{\n    \"defaults\": {{\n      \"workspace\": \"{}\"\n    }}\n  }},\n  \"gateway\": {{\n    \"port\": 19789\n  }}\n}}\n",
            source_root.join(".openclaw/workspace").display()
        ),
    )
    .unwrap();

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "target", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], false);
    assert_eq!(value["configStatus"], "drifted");
    let issues = value["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .contains("OpenClaw config contains 1 path(s) under env \"source\" root")
    }));
    assert!(issues.iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .contains("OpenClaw config gateway port 19789 does not match env metadata 19790")
    }));
}

#[test]
fn env_doctor_reports_inferred_env_scoped_config_drift_without_source_metadata() {
    let root = TestDir::new("env-doctor-inferred-config-drift");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create_target = run_ocm(&cwd, &env, &["env", "create", "target", "--port", "19790"]);
    assert!(create_target.status.success(), "{}", stderr(&create_target));

    fs::write(
        root.child("ocm-home/envs/target/.openclaw/openclaw.json"),
        "{\n  \"agents\": {\n    \"defaults\": {\n      \"workspace\": \"/tmp/external-source/.openclaw/workspace\"\n    }\n  },\n  \"memory\": {\n    \"logPath\": \"/tmp/external-source/.openclaw/logs/gateway.log\"\n  },\n  \"gateway\": {\n    \"port\": 19789\n  }\n}\n",
    )
    .unwrap();

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "target", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], false);
    assert_eq!(value["configStatus"], "drifted");
    let issues = value["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .contains("OpenClaw config contains 2 env-scoped path(s) outside the current env root")
    }));
    assert!(issues.iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .contains("OpenClaw config workspace points outside env root")
    }));
    assert!(issues.iter().any(|issue| {
        issue
            .as_str()
            .unwrap()
            .contains("OpenClaw config gateway port 19789 does not match env metadata 19790")
    }));
}

#[test]
fn env_doctor_reports_copied_openclaw_runtime_state_drift() {
    let root = TestDir::new("env-doctor-runtime-state-drift");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let source = run_ocm(&cwd, &env, &["env", "create", "source"]);
    assert!(source.status.success(), "{}", stderr(&source));
    let target = run_ocm(&cwd, &env, &["env", "create", "target"]);
    assert!(target.status.success(), "{}", stderr(&target));

    let target_state = root.child("ocm-home/envs/target/.openclaw/agents/main/sessions");
    fs::create_dir_all(&target_state).unwrap();
    fs::write(
        target_state.join("main.jsonl"),
        format!(
            "{{\"cwd\":\"{}\"}}\n",
            root.child("ocm-home/envs/source/.openclaw/workspace")
                .display()
        ),
    )
    .unwrap();

    let doctor = run_ocm(&cwd, &env, &["env", "doctor", "target", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], false);
    let issues = value["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        issue.as_str().unwrap().contains(
            "OpenClaw runtime state contains 1 copied path reference(s) under env \"source\" root",
        )
    }));
}
