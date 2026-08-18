mod support;

use std::fs;

use ocm::env::{EnvMeta, select_prune_candidates};
use ocm::runtime::RuntimeSourceKind;
use ocm::store::{
    ensure_store, env_registry_path, get_environment, get_launcher, get_runtime, now_utc,
};
use time::Duration;

use crate::support::{TestDir, ocm_env, path_string, write_text};

#[test]
fn get_environment_accepts_registry_json_without_last_used_at() {
    let root = TestDir::new("registry-env-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    ensure_store(&env, &cwd).unwrap();
    fs::create_dir_all(root.child("env-root")).unwrap();

    write_text(
        &env_registry_path(&env, &cwd).unwrap(),
        &format!(
            "{{\n  \"kind\": \"ocm-env-registry\",\n  \"envs\": [\n    {{\n      \"kind\": \"ocm-env\",\n      \"name\": \"legacy\",\n      \"root\": \"{}\",\n      \"gatewayPort\": 19789,\n      \"defaultLauncher\": \"stable\",\n      \"protected\": false,\n      \"createdAt\": \"2026-03-20T10:00:00Z\",\n      \"updatedAt\": \"2026-03-20T10:00:00Z\"\n    }}\n  ]\n}}\n",
            path_string(&root.child("env-root"))
        ),
    );

    let meta = get_environment("legacy", &env, &cwd).unwrap();
    assert_eq!(meta.name, "legacy");
    assert_eq!(meta.gateway_port, Some(19789));
    assert_eq!(meta.default_launcher.as_deref(), Some("stable"));
    assert_eq!(meta.last_used_at, None);
}

#[test]
fn get_launcher_accepts_json_without_optional_fields() {
    let root = TestDir::new("launcher-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let stores = ensure_store(&env, &cwd).unwrap();

    write_text(
        &stores.launchers_dir.join("legacy.json"),
        "{\n  \"kind\": \"ocm-launcher\",\n  \"name\": \"legacy\",\n  \"command\": \"openclaw\",\n  \"createdAt\": \"2026-03-20T10:00:00Z\",\n  \"updatedAt\": \"2026-03-20T10:00:00Z\"\n}\n",
    );

    let meta = get_launcher("legacy", &env, &cwd).unwrap();
    assert_eq!(meta.name, "legacy");
    assert_eq!(meta.command, "openclaw");
    assert_eq!(meta.cwd, None);
    assert_eq!(meta.description, None);
}

#[test]
fn get_runtime_accepts_json_without_optional_fields() {
    let root = TestDir::new("runtime-json");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);
    let stores = ensure_store(&env, &cwd).unwrap();

    write_text(
        &stores.runtimes_dir.join("legacy.json"),
        "{\n  \"kind\": \"ocm-runtime\",\n  \"name\": \"legacy\",\n  \"binaryPath\": \"/tmp/openclaw\",\n  \"createdAt\": \"2026-03-20T10:00:00Z\",\n  \"updatedAt\": \"2026-03-20T10:00:00Z\"\n}\n",
    );

    let meta = get_runtime("legacy", &env, &cwd).unwrap();
    assert_eq!(meta.name, "legacy");
    assert_eq!(meta.binary_path, "/tmp/openclaw");
    assert_eq!(meta.source_kind, RuntimeSourceKind::Registered);
    assert_eq!(meta.source_path, None);
    assert_eq!(meta.source_url, None);
    assert_eq!(meta.source_manifest_url, None);
    assert_eq!(meta.source_sha256, None);
    assert_eq!(meta.runtime_sha256, None);
    assert_eq!(meta.release_version, None);
    assert_eq!(meta.release_channel, None);
    assert_eq!(meta.release_selector_kind, None);
    assert_eq!(meta.release_selector_value, None);
    assert_eq!(meta.install_root, None);
    assert_eq!(meta.description, None);
}

#[test]
fn prune_selection_uses_last_used_at_and_skips_protected_envs() {
    let now = now_utc();
    let envs = vec![
        EnvMeta {
            kind: "ocm-env".to_string(),
            name: "old".to_string(),
            root: "/tmp/old".to_string(),
            gateway_port: None,
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            protected: false,
            created_at: now - Duration::days(30),
            updated_at: now - Duration::days(30),
            last_used_at: None,
        },
        EnvMeta {
            kind: "ocm-env".to_string(),
            name: "recently-used".to_string(),
            root: "/tmp/recent".to_string(),
            gateway_port: None,
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            protected: false,
            created_at: now - Duration::days(30),
            updated_at: now - Duration::days(30),
            last_used_at: Some(now - Duration::days(1)),
        },
        EnvMeta {
            kind: "ocm-env".to_string(),
            name: "protected-old".to_string(),
            root: "/tmp/protected".to_string(),
            gateway_port: None,
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            protected: true,
            created_at: now - Duration::days(30),
            updated_at: now - Duration::days(30),
            last_used_at: None,
        },
    ];

    let candidates = select_prune_candidates(&envs, 14);
    let names = candidates
        .into_iter()
        .map(|meta| meta.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["old".to_string()]);
}
