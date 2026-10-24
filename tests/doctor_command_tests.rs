mod support;

use std::fs;

use serde_json::Value;

#[cfg(unix)]
use crate::support::write_executable_script;
use crate::support::{
    TestDir, install_fake_git_package_manager, install_fake_node_and_npm, ocm_env, path_string,
    run_ocm, stderr, stdout,
};

#[test]
fn doctor_host_reports_missing_required_tools() {
    let root = TestDir::new("doctor-host-missing-required");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let empty_path = root.child("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    env.insert("PATH".to_string(), path_string(&empty_path));

    let doctor = run_ocm(&cwd, &env, &["doctor", "host", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));

    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], Value::Bool(true));
    assert_eq!(value["officialReleaseReady"], Value::Bool(true));
    assert_eq!(value["requiredIssues"], Value::from(0));

    let checks = value["checks"].as_array().unwrap();
    let expected_recommended_gaps = checks
        .iter()
        .filter(|check| {
            check["level"] == "recommended"
                && check["status"] != "ok"
                && check["status"] != "unsupported"
        })
        .count() as u64;
    assert_eq!(
        value["recommendedGaps"],
        Value::from(expected_recommended_gaps)
    );
    assert!(checks.iter().any(|check| {
        check["name"] == "Node.js"
            && check["level"] == "recommended"
            && check["status"] == "missing"
            && check["detail"]
                .as_str()
                .unwrap_or("")
                .contains("private Node.js toolchain")
    }));
    assert!(checks.iter().any(|check| {
        check["name"] == "npm"
            && check["level"] == "recommended"
            && check["status"] == "missing"
            && check["detail"]
                .as_str()
                .unwrap_or("")
                .contains("private Node.js + npm toolchain")
    }));
}

#[test]
fn doctor_host_reports_ready_official_release_requirements() {
    let root = TestDir::new("doctor-host-ready");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");

    let doctor = run_ocm(&cwd, &env, &["doctor", "host", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));

    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    assert_eq!(value["healthy"], Value::Bool(true));
    assert_eq!(value["officialReleaseReady"], Value::Bool(true));
    assert_eq!(value["requiredIssues"], Value::from(0));

    let checks = value["checks"].as_array().unwrap();
    assert!(checks.iter().any(|check| {
        check["name"] == "Node.js"
            && check["level"] == "recommended"
            && check["status"] == "ok"
            && check["available"] == true
    }));
    assert!(checks.iter().any(|check| {
        check["name"] == "npm"
            && check["level"] == "recommended"
            && check["status"] == "ok"
            && check["available"] == true
    }));
}

#[test]
fn doctor_host_can_fix_git_with_a_supported_package_manager() {
    let root = TestDir::new("doctor-host-fix-git");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let empty_path = root.child("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    env.insert("PATH".to_string(), path_string(&empty_path));
    env.insert(
        "OCM_INTERNAL_HOST_PLATFORM".to_string(),
        "linux".to_string(),
    );
    env.insert(
        "OCM_INTERNAL_HOST_PACKAGE_MANAGER".to_string(),
        "apt-get".to_string(),
    );
    env.insert("OCM_INTERNAL_HOST_IS_ROOT".to_string(), "false".to_string());
    let log_path = install_fake_git_package_manager(&root, &mut env, "apt-get");

    let fix = run_ocm(
        &cwd,
        &env,
        &["doctor", "host", "--fix", "git", "--yes", "--json"],
    );
    assert!(fix.status.success(), "{}", stderr(&fix));

    let value: Value = serde_json::from_str(&stdout(&fix)).unwrap();
    assert_eq!(value["tool"], "git");
    assert_eq!(value["ready"], Value::Bool(true));
    assert_eq!(value["changed"], Value::Bool(true));
    assert_eq!(value["manager"], "apt-get");
    assert!(
        value["version"]
            .as_str()
            .unwrap_or("")
            .contains("git version 2.51.0")
    );

    let log = fs::read_to_string(log_path).unwrap();
    assert!(log.contains("update"));
    assert!(log.contains("install -y git"));

    let doctor = run_ocm(&cwd, &env, &["doctor", "host", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let doctor_json: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    let checks = doctor_json["checks"].as_array().unwrap();
    assert!(checks.iter().any(|check| {
        check["name"] == "git" && check["status"] == "ok" && check["available"] == true
    }));
}

#[test]
fn doctor_host_fix_requires_yes() {
    let root = TestDir::new("doctor-host-fix-yes");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let fix = run_ocm(&cwd, &env, &["doctor", "host", "--fix", "git"]);
    assert_eq!(fix.status.code(), Some(1));
    assert!(stderr(&fix).contains("doctor host --fix git requires --yes"));
}

/// Installs a fake service manager whose binary probe always succeeds and whose
/// session probe (the one the service gate runs) succeeds only while
/// `session-usable` exists in the fixture root.
#[cfg(unix)]
fn install_service_manager_with_session(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
    manager: &str,
) {
    let (binary, override_key) = match manager {
        "launchd" => ("launchctl", "OCM_INTERNAL_LAUNCHCTL_BIN"),
        _ => ("systemctl", "OCM_INTERNAL_SYSTEMCTL_BIN"),
    };
    let bin_dir = root.child("service-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{log}\"\ncase \"$*\" in\n  --version) echo 'systemd 259 (fixture)' ;;\n  help) echo 'usage: launchctl <subcommand>' ;;\n  '--user show-environment'|managername)\n    if [ -f \"{usable}\" ]; then echo 'session ok'; exit 0; fi\n    echo 'Failed to connect to user scope bus' >&2\n    exit 1\n    ;;\n  *) echo \"unexpected: $*\" >&2; exit 1 ;;\nesac\n",
        log = path_string(&root.child("service-manager.log")),
        usable = path_string(&root.child("session-usable")),
    );
    write_executable_script(&bin_dir.join(binary), &script);
    let path = env.get("PATH").cloned().unwrap_or_default();
    env.insert(
        "PATH".to_string(),
        format!("{}:{path}", path_string(&bin_dir)),
    );
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        manager.to_string(),
    );
    env.insert(override_key.to_string(), path_string(&bin_dir.join(binary)));
}

#[cfg(unix)]
fn doctor_background_services(
    cwd: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
) -> (Value, Value) {
    let doctor = run_ocm(cwd, env, &["doctor", "host", "--json"]);
    assert!(doctor.status.success(), "{}", stderr(&doctor));
    let value: Value = serde_json::from_str(&stdout(&doctor)).unwrap();
    let rows = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["category"] == "background-services")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{value}");
    (rows[0].clone(), value["recommendedGaps"].clone())
}

#[cfg(unix)]
fn assert_doctor_background_services_follow_the_service_gate(
    manager: &str,
    name: &str,
    gate_reason: &str,
    version: Option<&str>,
) {
    let root = TestDir::new(&format!("doctor-host-service-session-{manager}"));
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_service_manager_with_session(&root, &mut env, manager);

    fs::write(root.child("session-usable"), "").unwrap();
    let (usable, usable_gaps) = doctor_background_services(&cwd, &env);
    assert_eq!(usable["name"], name);
    assert_eq!(usable["status"], "ok", "{usable}");
    assert_eq!(usable["available"], Value::Bool(true));
    assert_eq!(usable["version"], version.map_or(Value::Null, Value::from));

    fs::remove_file(root.child("session-usable")).unwrap();
    let (unusable, unusable_gaps) = doctor_background_services(&cwd, &env);
    assert_eq!(unusable["name"], name);
    assert_eq!(unusable["level"], "recommended");
    assert_eq!(unusable["status"], "missing", "{unusable}");
    assert_eq!(unusable["available"], Value::Bool(false));
    assert_eq!(
        unusable["version"],
        version.map_or(Value::Null, Value::from)
    );
    assert!(
        unusable["detail"].as_str().unwrap().contains(gate_reason),
        "{unusable}"
    );
    assert!(
        unusable["suggestion"]
            .as_str()
            .unwrap()
            .contains("--no-service"),
        "{unusable}"
    );
    assert_eq!(
        unusable_gaps.as_u64().unwrap(),
        usable_gaps.as_u64().unwrap() + 1
    );

    let start = run_ocm(&cwd, &env, &["start", "demo", "--command", "openclaw"]);
    assert_eq!(start.status.code(), Some(1));
    assert!(stderr(&start).contains(gate_reason), "{}", stderr(&start));
}

#[cfg(unix)]
#[test]
fn doctor_host_reports_systemd_user_services_missing_without_a_usable_session() {
    assert_doctor_background_services_follow_the_service_gate(
        "systemd-user",
        "systemd --user",
        "managed services require a usable systemctl --user session",
        Some("systemd 259 (fixture)"),
    );
}

#[cfg(unix)]
#[test]
fn doctor_host_reports_launchd_services_missing_without_a_usable_session() {
    assert_doctor_background_services_follow_the_service_gate(
        "launchd",
        "launchd",
        "managed services require a usable launchctl session",
        None,
    );
}
