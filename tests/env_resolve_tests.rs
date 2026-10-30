mod support;

use std::fs;

use serde_json::Value;

use crate::support::{
    TestDir, TestHttpServer, install_fake_node_and_npm, ocm_env, openclaw_package_tarball, run_ocm,
    sha512_integrity, stderr, stdout, write_executable_script,
};

#[test]
fn env_resolve_reports_the_bound_launcher_without_touching_last_used_at() {
    let root = TestDir::new("env-resolve-launcher");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "fallback",
            "--command",
            "printf launcher",
        ],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "fallback"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let before_show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(before_show.status.success(), "{}", stderr(&before_show));
    assert!(!stdout(&before_show).contains("lastUsedAt:"));

    let resolve = run_ocm(&cwd, &env, &["env", "resolve", "demo"]);
    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let expected_cwd = fs::canonicalize(&cwd).unwrap();
    let resolved = stdout(&resolve);
    assert!(resolved.contains("envName: demo"));
    assert!(resolved.contains("bindingKind: launcher"));
    assert!(resolved.contains("bindingName: fallback"));
    assert!(resolved.contains("command: printf launcher"));
    assert!(resolved.contains(&format!("runDir: {}", expected_cwd.display())));

    let after_show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(after_show.status.success(), "{}", stderr(&after_show));
    assert!(!stdout(&after_show).contains("lastUsedAt:"));
}

#[test]
fn env_resolve_reports_the_bound_runtime_and_forwarded_args() {
    let root = TestDir::new("env-resolve-runtime");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--", "onboard", "status"],
    );
    assert!(resolve.status.success(), "{}", stderr(&resolve));
    let expected_binary_path = fs::canonicalize(&runtime_path).unwrap();
    let expected_cwd = fs::canonicalize(&cwd).unwrap();
    let resolved = stdout(&resolve);
    assert!(resolved.contains("bindingKind: runtime"));
    assert!(resolved.contains("bindingName: stable"));
    assert!(resolved.contains(&format!("binaryPath: {}", expected_binary_path.display())));
    assert!(resolved.contains("runtimeSourceKind: registered"));
    assert!(resolved.contains("forwardedArgs: onboard status --no-install-daemon"));
    assert!(resolved.contains(&format!("runDir: {}", expected_cwd.display())));
}

#[test]
fn env_resolve_json_reports_runtime_resolution_shape() {
    let root = TestDir::new("env-resolve-json");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let runtime_path = bin_dir.join("stable");
    write_executable_script(&runtime_path, "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let resolve = run_ocm(
        &cwd,
        &env,
        &["env", "resolve", "demo", "--json", "--", "onboard"],
    );
    assert!(resolve.status.success(), "{}", stderr(&resolve));

    let expected_binary_path = fs::canonicalize(&runtime_path).unwrap();
    let expected_cwd = fs::canonicalize(&cwd).unwrap();
    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["envName"], "demo");
    assert_eq!(resolved["bindingKind"], "runtime");
    assert_eq!(resolved["bindingName"], "stable");
    assert_eq!(
        resolved["binaryPath"],
        Value::String(expected_binary_path.display().to_string())
    );
    assert_eq!(resolved["runtimeSourceKind"], "registered");
    assert_eq!(
        resolved["forwardedArgs"],
        Value::Array(vec![
            Value::String("onboard".to_string()),
            Value::String("--no-install-daemon".to_string()),
        ])
    );
    assert_eq!(
        resolved["runDir"],
        Value::String(expected_cwd.display().to_string())
    );
}

#[test]
fn env_resolve_normalizes_onboard_after_openclaw_root_options() {
    let root = TestDir::new("env-resolve-onboard-root-options");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let cases: [(&[&str], &[&str]); 3] = [
        (
            &["--log-level", "debug", "onboard"],
            &["--log-level", "debug", "onboard", "--no-install-daemon"],
        ),
        (
            &[
                "--no-color",
                "--profile",
                "work",
                "onboard",
                "--skip-daemon",
            ],
            &[
                "--no-color",
                "--profile",
                "work",
                "onboard",
                "--skip-daemon",
            ],
        ),
        (
            &["--log-level=debug", "status"],
            &["--log-level=debug", "status"],
        ),
    ];
    for (forwarded, expected) in cases {
        let mut args = vec!["env", "resolve", "demo", "--json", "--"];
        args.extend_from_slice(forwarded);
        let resolve = run_ocm(&cwd, &env, &args);
        assert!(resolve.status.success(), "{}", stderr(&resolve));
        let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
        assert_eq!(resolved["forwardedArgs"], serde_json::json!(expected));
    }

    let refused = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "resolve",
            "demo",
            "--",
            "--no-color",
            "onboard",
            "--install-daemon",
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("the daemon service is global"));
}

#[test]
fn env_resolve_forwards_onboard_recommendations_without_daemon_guard() {
    let root = TestDir::new("env-resolve-onboard-recommendations");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let cases: [(&[&str], &[&str]); 5] = [
        (
            &["onboard", "recommendations"],
            &["onboard", "recommendations"],
        ),
        (
            &["onboard", "--json", "recommendations"],
            &["onboard", "--json", "recommendations"],
        ),
        (
            &["--no-color", "onboard", "recommendations", "refresh"],
            &["--no-color", "onboard", "recommendations", "refresh"],
        ),
        (
            &[
                "onboard",
                "recommendations",
                "--agent",
                "main",
                "acknowledge",
            ],
            &[
                "onboard",
                "recommendations",
                "--agent",
                "main",
                "acknowledge",
            ],
        ),
        (
            &["onboard", "--workspace", "recommendations"],
            &[
                "onboard",
                "--workspace",
                "recommendations",
                "--no-install-daemon",
            ],
        ),
    ];
    for (forwarded, expected) in cases {
        let mut args = vec!["env", "resolve", "demo", "--json", "--"];
        args.extend_from_slice(forwarded);
        let resolve = run_ocm(&cwd, &env, &args);
        assert!(resolve.status.success(), "{}", stderr(&resolve));
        let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
        assert_eq!(
            resolved["forwardedArgs"],
            serde_json::json!(expected),
            "{forwarded:?}"
        );
    }
}

#[test]
fn env_resolve_reports_release_backed_runtime_provenance() {
    let root = TestDir::new("env-resolve-release-runtime");
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
    let manifest_body = format!(
        "{{\"dist-tags\":{{\"latest\":\"2026.3.24\"}},\"versions\":{{\"2026.3.24\":{{\"version\":\"2026.3.24\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{}\"}}}}}},\"time\":{{\"2026.3.24\":\"2026-03-25T16:35:52.000Z\"}}}}",
        tarball_server.url(),
        integrity
    );
    let manifest_server =
        TestHttpServer::serve_bytes("/openclaw", "application/json", manifest_body.as_bytes());
    let mut env = ocm_env(&root);
    install_fake_node_and_npm(&root, &mut env, "22.22.3");
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        manifest_server.url(),
    );

    let install = run_ocm(&cwd, &env, &["runtime", "install", "--channel", "stable"]);
    assert!(install.status.success(), "{}", stderr(&install));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let resolve = run_ocm(&cwd, &env, &["env", "resolve", "demo", "--json"]);
    assert!(resolve.status.success(), "{}", stderr(&resolve));

    let resolved: Value = serde_json::from_str(&stdout(&resolve)).unwrap();
    assert_eq!(resolved["bindingKind"], "runtime");
    assert_eq!(resolved["bindingName"], "stable");
    assert_eq!(resolved["runtimeSourceKind"], "installed");
    assert_eq!(resolved["runtimeReleaseVersion"], "2026.3.24");
    assert_eq!(resolved["runtimeReleaseChannel"], "stable");
}
