use std::path::Path;
use std::{collections::BTreeMap, path::PathBuf};

use ocm::env::{EnvDevMeta, EnvMeta};
use ocm::env::{
    ExecutionBinding, GatewayProcessSpec, resolve_execution_binding, resolve_runtime_run_dir,
};
use ocm::launcher::{
    LauncherMeta, build_launcher_command, resolve_launcher_name, resolve_launcher_run_dir,
};
use time::OffsetDateTime;

fn sample_env(default_runtime: Option<&str>, default_launcher: Option<&str>) -> EnvMeta {
    EnvMeta {
        upgrade_independent_paths: Vec::new(),
        kind: "ocm-env".to_string(),
        name: "demo".to_string(),
        root: "/tmp/demo".to_string(),
        gateway_port: None,
        gateway_port_auto_assigned: false,
        service_enabled: true,
        service_running: true,
        default_runtime: default_runtime.map(str::to_string),
        default_launcher: default_launcher.map(str::to_string),
        dev: None,
        dev_ui_port: None,
        protected: false,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        last_used_at: None,
    }
}

fn sample_launcher(cwd: Option<&str>) -> LauncherMeta {
    LauncherMeta {
        kind: "ocm-launcher".to_string(),
        name: "stable".to_string(),
        command: "openclaw".to_string(),
        cwd: cwd.map(str::to_string),
        description: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn resolve_launcher_name_prefers_the_explicit_override() {
    let env = sample_env(None, Some("stable"));

    let resolved = resolve_launcher_name(&env, Some("nightly".to_string())).unwrap();
    assert_eq!(resolved, "nightly");
}

#[test]
fn resolve_launcher_name_falls_back_to_the_environment_default() {
    let env = sample_env(None, Some("stable"));

    let resolved = resolve_launcher_name(&env, None).unwrap();
    assert_eq!(resolved, "stable");
}

#[test]
fn resolve_launcher_name_uses_launcher_wording_when_unbound() {
    let env = sample_env(None, None);

    let error = resolve_launcher_name(&env, None).unwrap_err();
    assert!(error.contains("has no default launcher"));
    assert!(error.contains("env set-launcher"));
    assert!(error.contains("--launcher"));
}

#[test]
fn build_launcher_command_quotes_forwarded_arguments() {
    let launcher = sample_launcher(None);
    let args = vec!["gateway run".to_string(), "it's-live".to_string()];

    let command = build_launcher_command(&launcher, &args);
    assert_eq!(command, "openclaw 'gateway run' 'it'\"'\"'s-live'");
}

#[test]
fn resolve_launcher_run_dir_prefers_the_launcher_cwd() {
    let launcher = sample_launcher(Some("/tmp/launcher"));

    let run_dir = resolve_launcher_run_dir(&launcher, Path::new("/tmp/fallback"));
    assert_eq!(run_dir, Path::new("/tmp/launcher"));
}

#[test]
fn resolve_launcher_run_dir_falls_back_to_the_calling_cwd() {
    let launcher = sample_launcher(None);

    let run_dir = resolve_launcher_run_dir(&launcher, Path::new("/tmp/fallback"));
    assert_eq!(run_dir, Path::new("/tmp/fallback"));
}

#[test]
fn resolve_execution_binding_prefers_runtime_override() {
    let env = sample_env(Some("stable"), Some("launcher"));

    let resolved = resolve_execution_binding(&env, Some("nightly".to_string()), None).unwrap();
    assert!(matches!(resolved, ExecutionBinding::Runtime(name) if name == "nightly"));
}

#[test]
fn resolve_execution_binding_prefers_runtime_default_before_launcher_default() {
    let env = sample_env(Some("stable"), Some("launcher"));

    let resolved = resolve_execution_binding(&env, None, None).unwrap();
    assert!(matches!(resolved, ExecutionBinding::Runtime(name) if name == "stable"));
}

#[test]
fn resolve_execution_binding_falls_back_to_launcher_default() {
    let env = sample_env(None, Some("launcher"));

    let resolved = resolve_execution_binding(&env, None, None).unwrap();
    assert!(matches!(resolved, ExecutionBinding::Launcher(name) if name == "launcher"));
}

#[test]
fn resolve_execution_binding_falls_back_to_dev_binding() {
    let mut env = sample_env(None, None);
    env.dev = Some(EnvDevMeta::Owned {
        repo_root: "/tmp/openclaw".to_string(),
        worktree_root: "/tmp/openclaw/.worktrees/demo".to_string(),
    });

    let resolved = resolve_execution_binding(&env, None, None).unwrap();
    assert!(matches!(resolved, ExecutionBinding::Dev));
}

#[test]
fn resolve_execution_binding_rejects_conflicting_overrides() {
    let env = sample_env(Some("stable"), Some("launcher"));

    let error = resolve_execution_binding(
        &env,
        Some("stable".to_string()),
        Some("launcher".to_string()),
    )
    .unwrap_err();
    assert!(error.contains("only one of --runtime or --launcher"));
}

#[test]
fn resolve_execution_binding_uses_runtime_and_launcher_wording_when_unbound() {
    let env = sample_env(None, None);

    let error = resolve_execution_binding(&env, None, None).unwrap_err();
    assert!(error.contains("has no default runtime, launcher, or dev binding"));
    assert!(error.contains("env set-runtime"));
    assert!(error.contains("env set-launcher"));
}

#[test]
fn resolve_runtime_run_dir_uses_the_calling_cwd() {
    let run_dir = resolve_runtime_run_dir(Path::new("/tmp/fallback"));
    assert_eq!(run_dir, Path::new("/tmp/fallback"));
}

#[test]
fn gateway_process_spec_shell_arguments_wrap_the_launcher_command() {
    let spec = GatewayProcessSpec {
        env_name: "demo".to_string(),
        binding_kind: "launcher".to_string(),
        binding_name: "stable".to_string(),
        command: Some("openclaw gateway run --port 18789".to_string()),
        binary_path: None,
        runtime_source_kind: None,
        runtime_release_version: None,
        runtime_release_channel: None,
        args: Vec::new(),
        run_dir: PathBuf::from("/tmp/demo"),
        process_env: BTreeMap::new(),
    };

    assert_eq!(
        spec.program_arguments(),
        vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            "openclaw gateway run --port 18789".to_string()
        ]
    );
}

#[test]
fn gateway_process_spec_direct_arguments_preserve_the_binary_and_args() {
    let spec = GatewayProcessSpec {
        env_name: "demo".to_string(),
        binding_kind: "runtime".to_string(),
        binding_name: "managed".to_string(),
        command: None,
        binary_path: Some("/tmp/runtime/node_modules/openclaw/openclaw.mjs".to_string()),
        runtime_source_kind: Some("official".to_string()),
        runtime_release_version: Some("1.2.3".to_string()),
        runtime_release_channel: Some("stable".to_string()),
        args: vec![
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            "18789".to_string(),
        ],
        run_dir: PathBuf::from("/tmp/demo"),
        process_env: BTreeMap::new(),
    };

    assert_eq!(
        spec.program_arguments(),
        vec![
            "/tmp/runtime/node_modules/openclaw/openclaw.mjs".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            "18789".to_string()
        ]
    );
    assert!(spec.restart_handoff_pid_bound());
}

#[test]
fn gateway_process_spec_accepts_only_ocm_managed_node_for_indirect_entrypoints() {
    let mut spec = GatewayProcessSpec {
        env_name: "demo".to_string(),
        binding_kind: "runtime".to_string(),
        binding_name: "managed".to_string(),
        command: None,
        binary_path: Some("/tmp/ocm/toolchains/node/v22/bin/node".to_string()),
        runtime_source_kind: Some("installed".to_string()),
        runtime_release_version: Some("1.2.3".to_string()),
        runtime_release_channel: Some("stable".to_string()),
        args: vec![
            "/tmp/runtime/node_modules/openclaw/openclaw.mjs".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            "18789".to_string(),
        ],
        run_dir: PathBuf::from("/tmp/demo"),
        process_env: BTreeMap::new(),
    };

    assert!(spec.restart_handoff_pid_bound());

    spec.runtime_source_kind = Some("registered".to_string());
    assert!(!spec.restart_handoff_pid_bound());
}

#[test]
fn gateway_process_spec_prefers_direct_program_arguments_when_both_shapes_exist() {
    let spec = GatewayProcessSpec {
        env_name: "demo".to_string(),
        binding_kind: "launcher".to_string(),
        binding_name: "dev".to_string(),
        command: Some("pnpm openclaw gateway run --port 18900".to_string()),
        binary_path: Some("node".to_string()),
        runtime_source_kind: None,
        runtime_release_version: None,
        runtime_release_channel: None,
        args: vec![
            "scripts/run-node.mjs".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            "18900".to_string(),
        ],
        run_dir: PathBuf::from("/tmp/demo"),
        process_env: BTreeMap::new(),
    };

    assert_eq!(
        spec.program_arguments(),
        vec![
            "node".to_string(),
            "scripts/run-node.mjs".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            "18900".to_string()
        ]
    );
    assert!(!spec.restart_handoff_pid_bound());
}
