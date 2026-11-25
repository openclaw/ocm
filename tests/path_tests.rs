mod support;

use std::fs;

use ocm::env::{CloneEnvironmentOptions, CreateEnvironmentOptions};
use ocm::launcher::AddLauncherOptions;
use ocm::runtime::AddRuntimeOptions;
use ocm::store::{add_launcher, add_runtime, clone_environment, create_environment};
use ocm::store::{clean_path, resolve_ocm_home, validate_name};

use crate::support::{TestDir, base_env, ocm_env, path_string};

#[test]
fn validate_name_accepts_supported_patterns() {
    assert_eq!(validate_name("env-1", "Environment name").unwrap(), "env-1");
    assert_eq!(validate_name("A_b.c", "Environment name").unwrap(), "A_b.c");
    assert_eq!(validate_name("9demo", "Environment name").unwrap(), "9demo");
}

#[test]
fn validate_name_rejects_empty_or_invalid_values() {
    assert_eq!(
        validate_name("   ", "Environment name").unwrap_err(),
        "Environment name is required"
    );
    assert_eq!(
        validate_name("-demo", "Environment name").unwrap_err(),
        "Environment name must use letters, numbers, '.', '_', or '-'"
    );
    assert_eq!(
        validate_name("demo/slash", "Environment name").unwrap_err(),
        "Environment name must use letters, numbers, '.', '_', or '-'"
    );
}

#[test]
fn resolve_ocm_home_normalizes_relative_override() {
    let root = TestDir::new("resolve-ocm-home");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();

    let home = root.child("home");
    let mut env = base_env(&home);
    env.insert(
        "OCM_HOME".to_string(),
        "./stores/../stores/active".to_string(),
    );

    let resolved = resolve_ocm_home(&env, &cwd).unwrap();
    assert_eq!(resolved, clean_path(&cwd.join("stores/active")));
}

#[test]
fn create_environment_normalizes_relative_custom_root() {
    let root = TestDir::new("custom-root");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let meta = create_environment(
        CreateEnvironmentOptions {
            name: "demo".to_string(),
            root: Some("./env-roots/../env-roots/demo".to_string()),
            gateway_port: None,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            protected: false,
        },
        &env,
        &cwd,
    )
    .unwrap();

    let expected_root = clean_path(&cwd.join("env-roots/demo"));
    assert_eq!(meta.root, path_string(&expected_root));
    assert!(expected_root.exists());
}

#[test]
fn add_launcher_normalizes_relative_cwd() {
    let root = TestDir::new("launcher-cwd");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let meta = add_launcher(
        AddLauncherOptions {
            name: "stable".to_string(),
            command: "sh".to_string(),
            cwd: Some("./launchers/../launchers/stable".to_string()),
            description: None,
        },
        &env,
        &cwd,
    )
    .unwrap();

    let expected_cwd = clean_path(&cwd.join("launchers/stable"));
    assert_eq!(
        meta.cwd.as_deref(),
        Some(path_string(&expected_cwd).as_str())
    );
}

#[test]
fn add_runtime_normalizes_relative_binary_path() {
    let root = TestDir::new("runtime-path");
    let cwd = root.child("workspace");
    let binary_path = cwd.join("bin/openclaw");
    fs::create_dir_all(binary_path.parent().unwrap()).unwrap();
    fs::write(&binary_path, "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);

    let meta = add_runtime(
        AddRuntimeOptions {
            name: "stable".to_string(),
            path: "./bin/./openclaw".to_string(),
            description: None,
        },
        &env,
        &cwd,
    )
    .unwrap();

    let expected_path = clean_path(&cwd.join("bin/openclaw"));
    assert_eq!(meta.binary_path, path_string(&expected_path));
}

#[test]
fn clone_environment_normalizes_relative_custom_root() {
    let root = TestDir::new("clone-custom-root");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    create_environment(
        CreateEnvironmentOptions {
            name: "source".to_string(),
            root: None,
            gateway_port: None,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            protected: false,
        },
        &env,
        &cwd,
    )
    .unwrap();

    let meta = clone_environment(
        CloneEnvironmentOptions {
            source_name: "source".to_string(),
            name: "target".to_string(),
            root: Some("./env-clones/../env-clones/target".to_string()),
            keep_identity: false,
        },
        &env,
        &cwd,
    )
    .unwrap();

    let expected_root = clean_path(&cwd.join("env-clones/target"));
    assert_eq!(meta.root, path_string(&expected_root));
    assert!(expected_root.exists());
}
