mod support;

use std::fs;

use ocm::store::ensure_store;

use crate::support::{TestDir, TestHttpServer, ocm_env, run_ocm, stderr, stdout};

#[test]
fn env_create_rejects_invalid_names() {
    let root = TestDir::new("cli-invalid-env-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "bad/name"]);
    assert_eq!(create.status.code(), Some(1));
    assert!(
        stderr(&create).contains("Environment name must use letters, numbers, '.', '_', or '-'")
    );
}

#[test]
fn launcher_add_rejects_invalid_names() {
    let root = TestDir::new("cli-invalid-launcher-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "bad/name", "--command", "sh"],
    );
    assert_eq!(add.status.code(), Some(1));
    assert!(stderr(&add).contains("Launcher name must use letters, numbers, '.', '_', or '-'"));
}

#[test]
fn runtime_add_rejects_invalid_names() {
    let root = TestDir::new("cli-invalid-runtime-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "bad/name", "--path", "./bin/openclaw"],
    );
    assert_eq!(add.status.code(), Some(1));
    assert!(stderr(&add).contains("Runtime name must use letters, numbers, '.', '_', or '-'"));
}

#[test]
fn runtime_install_rejects_invalid_names() {
    let root = TestDir::new("cli-invalid-installed-runtime-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let install = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "bad/name", "--path", "./bin/openclaw"],
    );
    assert_eq!(install.status.code(), Some(1));
    assert!(stderr(&install).contains("Runtime name must use letters, numbers, '.', '_', or '-'"));
}

#[test]
fn env_create_rejects_invalid_and_empty_port_values() {
    let root = TestDir::new("cli-invalid-port");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let zero = run_ocm(&cwd, &env, &["env", "create", "demo", "--port", "0"]);
    assert_eq!(zero.status.code(), Some(1));
    assert!(stderr(&zero).contains("--port must be a positive integer"));

    let empty = run_ocm(&cwd, &env, &["env", "create", "demo", "--port="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--port requires a value"));
}

#[test]
fn env_create_rejects_empty_and_unknown_launcher_values() {
    let root = TestDir::new("cli-create-launcher-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let empty = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--launcher requires a value"));

    let missing = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "missing"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("launcher \"missing\" does not exist"));
}

#[test]
fn env_export_requires_a_name_and_non_empty_output_values() {
    let root = TestDir::new("cli-export-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "export"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let empty_output = run_ocm(&cwd, &env, &["env", "export", "demo", "--output="]);
    assert_eq!(empty_output.status.code(), Some(1));
    assert!(stderr(&empty_output).contains("--output requires a value"));
}

#[test]
fn env_import_requires_an_archive_and_non_empty_option_values() {
    let root = TestDir::new("cli-import-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_archive = run_ocm(&cwd, &env, &["env", "import"]);
    assert_eq!(missing_archive.status.code(), Some(1));
    assert!(stderr(&missing_archive).contains("archive path is required"));

    let empty_name = run_ocm(&cwd, &env, &["env", "import", "./demo.tar", "--name="]);
    assert_eq!(empty_name.status.code(), Some(1));
    assert!(stderr(&empty_name).contains("--name requires a value"));

    let empty_root = run_ocm(&cwd, &env, &["env", "import", "./demo.tar", "--root="]);
    assert_eq!(empty_root.status.code(), Some(1));
    assert!(stderr(&empty_root).contains("--root requires a value"));
}

#[test]
fn env_snapshot_create_requires_a_name_and_non_empty_label_values() {
    let root = TestDir::new("cli-snapshot-create-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "snapshot", "create"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let empty_label = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "create", "demo", "--label="],
    );
    assert_eq!(empty_label.status.code(), Some(1));
    assert!(stderr(&empty_label).contains("--label requires a value"));
}

#[test]
fn env_snapshot_list_requires_a_name_or_all_and_rejects_conflicts() {
    let root = TestDir::new("cli-snapshot-list-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing = run_ocm(&cwd, &env, &["env", "snapshot", "list"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("environment name is required"));

    let conflicting = run_ocm(&cwd, &env, &["env", "snapshot", "list", "demo", "--all"]);
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(stderr(&conflicting).contains("env snapshot list accepts either <name> or --all"));
}

#[test]
fn env_snapshot_restore_requires_both_name_and_snapshot_id() {
    let root = TestDir::new("cli-snapshot-restore-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "snapshot", "restore"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let missing_snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "restore", "demo"]);
    assert_eq!(missing_snapshot.status.code(), Some(1));
    assert!(stderr(&missing_snapshot).contains("snapshot id is required"));
}

#[test]
fn env_snapshot_remove_requires_both_name_and_snapshot_id() {
    let root = TestDir::new("cli-snapshot-remove-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "snapshot", "remove"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let missing_snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "remove", "demo"]);
    assert_eq!(missing_snapshot.status.code(), Some(1));
    assert!(stderr(&missing_snapshot).contains("snapshot id is required"));

    let extra = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "remove", "demo", "snapshot-1", "extra"],
    );
    assert_eq!(extra.status.code(), Some(1));
    assert!(stderr(&extra).contains("unexpected arguments: extra"));
}

#[test]
fn root_double_dash_requires_an_active_environment() {
    let root = TestDir::new("cli-root-double-dash-active-env");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &["--", "status"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains(
        "no active environment; use \"ocm @<env> -- ...\" or \"ocm env run <name> -- ...\""
    ));
}

#[test]
fn root_at_env_requires_a_target_name() {
    let root = TestDir::new("cli-root-at-env-name");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &["@", "--", "status"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("env shorthand requires a target like @mira"));
}

#[test]
fn root_at_env_requires_double_dash_before_openclaw_arguments() {
    let root = TestDir::new("cli-root-at-env-separator");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &["@demo", "status"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("env shorthand requires -- before OpenClaw arguments"));
}

#[test]
fn env_snapshot_show_requires_both_name_and_snapshot_id() {
    let root = TestDir::new("cli-snapshot-show-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "snapshot", "show"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let missing_snapshot = run_ocm(&cwd, &env, &["env", "snapshot", "show", "demo"]);
    assert_eq!(missing_snapshot.status.code(), Some(1));
    assert!(stderr(&missing_snapshot).contains("snapshot id is required"));

    let extra = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "show", "demo", "snapshot-1", "extra"],
    );
    assert_eq!(extra.status.code(), Some(1));
    assert!(stderr(&extra).contains("unexpected arguments: extra"));
}

#[test]
fn env_snapshot_prune_requires_scope_and_retention_flags() {
    let root = TestDir::new("cli-snapshot-prune-scope-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_scope = run_ocm(&cwd, &env, &["env", "snapshot", "prune"]);
    assert_eq!(missing_scope.status.code(), Some(1));
    assert!(stderr(&missing_scope).contains("environment name is required"));

    let missing_selector = run_ocm(&cwd, &env, &["env", "snapshot", "prune", "demo"]);
    assert_eq!(missing_selector.status.code(), Some(1));
    assert!(
        stderr(&missing_selector).contains("env snapshot prune requires --keep or --older-than")
    );

    let all_missing_selector = run_ocm(&cwd, &env, &["env", "snapshot", "prune", "--all"]);
    assert_eq!(all_missing_selector.status.code(), Some(1));
    assert!(
        stderr(&all_missing_selector)
            .contains("env snapshot prune requires --keep or --older-than")
    );
}

#[test]
fn env_snapshot_prune_validates_scope_and_numeric_values() {
    let root = TestDir::new("cli-snapshot-prune-value-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let conflicting = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "prune", "demo", "--all", "--keep", "1"],
    );
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(stderr(&conflicting).contains("env snapshot prune accepts either <name> or --all"));

    let empty_keep = run_ocm(&cwd, &env, &["env", "snapshot", "prune", "demo", "--keep="]);
    assert_eq!(empty_keep.status.code(), Some(1));
    assert!(stderr(&empty_keep).contains("--keep requires a value"));

    let zero_older = run_ocm(
        &cwd,
        &env,
        &["env", "snapshot", "prune", "demo", "--older-than", "0"],
    );
    assert_eq!(zero_older.status.code(), Some(1));
    assert!(stderr(&zero_older).contains("--older-than must be a positive integer"));
}

#[test]
fn upgrade_batch_requires_explicit_scope_runtime_and_outage_acknowledgement() {
    let root = TestDir::new("cli-upgrade-batch-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_envs = run_ocm(&cwd, &env, &["upgrade", "batch", "--runtime", "target"]);
    assert_eq!(missing_envs.status.code(), Some(1));
    assert!(stderr(&missing_envs).contains("upgrade batch requires --envs"));

    let missing_runtime = run_ocm(&cwd, &env, &["upgrade", "batch", "--envs", "alpha,beta"]);
    assert_eq!(missing_runtime.status.code(), Some(1));
    assert!(stderr(&missing_runtime).contains("upgrade batch requires --runtime"));

    let one_env = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "batch",
            "--runtime",
            "target",
            "--envs",
            "alpha",
            "--accept-fleet-outage",
        ],
    );
    assert_eq!(one_env.status.code(), Some(1));
    assert!(stderr(&one_env).contains("at least two unique environments"));

    let zero_parallel = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "batch",
            "--runtime",
            "target",
            "--envs",
            "alpha,beta",
            "--parallel",
            "0",
            "--accept-fleet-outage",
        ],
    );
    assert_eq!(zero_parallel.status.code(), Some(1));
    assert!(stderr(&zero_parallel).contains("--parallel must be a positive integer"));

    let missing_ack = run_ocm(
        &cwd,
        &env,
        &[
            "upgrade",
            "batch",
            "--runtime",
            "target",
            "--envs",
            "alpha,beta",
        ],
    );
    assert_eq!(missing_ack.status.code(), Some(1));
    assert!(stderr(&missing_ack).contains("--accept-fleet-outage"));
}

#[test]
fn env_doctor_requires_a_name() {
    let root = TestDir::new("cli-env-doctor-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "doctor"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let extra = run_ocm(&cwd, &env, &["env", "doctor", "demo", "extra"]);
    assert_eq!(extra.status.code(), Some(1));
    assert!(stderr(&extra).contains("unexpected arguments: extra"));
}

#[test]
fn env_cleanup_requires_a_name() {
    let root = TestDir::new("cli-env-cleanup-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let missing_name = run_ocm(&cwd, &env, &["env", "cleanup"]);
    assert_eq!(missing_name.status.code(), Some(1));
    assert!(stderr(&missing_name).contains("environment name is required"));

    let extra = run_ocm(&cwd, &env, &["env", "cleanup", "demo", "extra"]);
    assert_eq!(extra.status.code(), Some(1));
    assert!(stderr(&extra).contains("unexpected arguments: extra"));
}

#[test]
fn env_cleanup_rejects_mixed_name_and_all_scope() {
    let root = TestDir::new("cli-env-cleanup-all-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let conflicting = run_ocm(&cwd, &env, &["env", "cleanup", "demo", "--all"]);
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(stderr(&conflicting).contains("env cleanup accepts either <name> or --all"));
}

#[test]
fn runtime_releases_use_the_official_source_and_reject_conflicting_selectors() {
    let root = TestDir::new("cli-runtime-releases-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let server = TestHttpServer::serve_bytes(
        "/openclaw",
        "application/json",
        br#"{"dist-tags":{"latest":"2026.3.24"},"versions":{"2026.3.24":{"version":"2026.3.24","dist":{"tarball":"https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz"}}}}"#,
    );
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_OPENCLAW_RELEASES_URL".to_string(),
        server.url(),
    );

    let official = run_ocm(&cwd, &env, &["runtime", "releases"]);
    assert_eq!(official.status.code(), Some(0));
    assert!(stdout(&official).contains("2026.3.24"));

    let conflicting = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "releases",
            "--manifest-url",
            "https://example.test/releases.json",
            "--version",
            "0.2.0",
            "--channel",
            "stable",
        ],
    );
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(
        stderr(&conflicting)
            .contains("runtime releases accepts only one of --version or --channel")
    );
}

#[test]
fn env_run_rejects_empty_and_unknown_launcher_overrides() {
    let root = TestDir::new("cli-run-launcher-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let empty = run_ocm(&cwd, &env, &["env", "run", "demo", "--launcher="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--launcher requires a value"));

    let missing = run_ocm(&cwd, &env, &["env", "run", "demo", "--launcher", "missing"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("launcher \"missing\" does not exist"));
}

#[test]
fn env_create_rejects_empty_and_unknown_runtime_values() {
    let root = TestDir::new("cli-create-runtime-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let empty = run_ocm(&cwd, &env, &["env", "create", "demo", "--runtime="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--runtime requires a value"));

    let missing = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--runtime", "missing"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("runtime \"missing\" does not exist"));
}

#[test]
fn env_run_rejects_conflicting_runtime_and_launcher_overrides() {
    let root = TestDir::new("cli-run-conflicting-overrides");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("stable"), "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);

    let add_runtime = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add_runtime.status.success(), "{}", stderr(&add_runtime));

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "run",
            "demo",
            "--runtime",
            "stable",
            "--launcher",
            "stable",
            "--",
            "onboard",
        ],
    );
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("env run accepts only one of --runtime or --launcher"));
}

#[test]
fn env_run_rejects_global_daemon_install_during_onboarding() {
    let root = TestDir::new("cli-run-onboard-daemon-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "stable", "--command", "sh"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run = run_ocm(
        &cwd,
        &env,
        &["env", "run", "demo", "--", "onboard", "--install-daemon"],
    );
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains(
        "env onboarding cannot install the OpenClaw daemon because the daemon service is global",
    ));
}

#[test]
fn env_run_rejects_empty_and_unknown_runtime_overrides() {
    let root = TestDir::new("cli-run-runtime-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let empty = run_ocm(&cwd, &env, &["env", "run", "demo", "--runtime="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--runtime requires a value"));

    let missing = run_ocm(&cwd, &env, &["env", "run", "demo", "--runtime", "missing"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("runtime \"missing\" does not exist"));
}

#[test]
fn runtime_add_rejects_empty_and_missing_paths() {
    let root = TestDir::new("cli-runtime-path-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let empty = run_ocm(&cwd, &env, &["runtime", "add", "stable", "--path="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--path requires a value"));

    let missing = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./missing"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("runtime path does not exist:"));
}

#[test]
fn runtime_install_rejects_empty_and_missing_paths() {
    let root = TestDir::new("cli-runtime-install-path-validation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let empty = run_ocm(&cwd, &env, &["runtime", "install", "stable", "--path="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--path requires a value"));

    let missing = run_ocm(
        &cwd,
        &env,
        &["runtime", "install", "stable", "--path", "./missing"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("runtime path does not exist:"));
}

#[test]
fn runtime_install_rejects_empty_and_conflicting_urls() {
    let root = TestDir::new("cli-runtime-install-url-validation");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("stable"), "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);

    let empty = run_ocm(&cwd, &env, &["runtime", "install", "stable", "--url="]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(stderr(&empty).contains("--url requires a value"));

    let missing = run_ocm(&cwd, &env, &["runtime", "install", "stable"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains(
        "runtime install requires --path, --url, --manifest-url, or --version/--channel"
    ));

    let conflicting = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./bin/stable",
            "--url",
            "http://127.0.0.1/stable",
        ],
    );
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(
        stderr(&conflicting)
            .contains("runtime install accepts only one of --path, --url, or --manifest-url")
    );
}

#[test]
fn runtime_install_manifest_requires_a_selector_and_rejects_conflicting_sources() {
    let root = TestDir::new("cli-runtime-install-manifest-validation");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("stable"), "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);

    let missing_selector = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            "https://example.test/releases.json",
        ],
    );
    assert!(
        stderr(&missing_selector)
            .contains("runtime install with --manifest-url requires --version or --channel")
    );
    assert_eq!(missing_selector.status.code(), Some(1));

    let conflicting_selectors = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--manifest-url",
            "https://example.test/releases.json",
            "--version",
            "0.2.0",
            "--channel",
            "stable",
        ],
    );
    assert_eq!(conflicting_selectors.status.code(), Some(1));
    assert!(stderr(&conflicting_selectors).contains(
        "runtime install with --manifest-url accepts only one of --version or --channel"
    ));

    let conflicting = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "install",
            "stable",
            "--path",
            "./bin/stable",
            "--manifest-url",
            "https://example.test/releases.json",
            "--version",
            "0.2.0",
        ],
    );
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(
        stderr(&conflicting)
            .contains("runtime install accepts only one of --path, --url, or --manifest-url")
    );
}

#[test]
fn runtime_update_requires_manifest_backing_and_rejects_ambiguous_defaults() {
    let root = TestDir::new("cli-runtime-update-validation");
    let cwd = root.child("workspace");
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("stable"), "#!/bin/sh\n").unwrap();
    let env = ocm_env(&root);
    let stores = ensure_store(&env, &cwd).unwrap();

    let add = run_ocm(
        &cwd,
        &env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(add.status.success(), "{}", stderr(&add));

    let missing_selector = run_ocm(&cwd, &env, &["runtime", "update", "stable"]);
    assert_eq!(missing_selector.status.code(), Some(1));
    assert!(
        stderr(&missing_selector)
            .contains("runtime \"stable\" is not backed by a release manifest")
    );

    let conflicting = run_ocm(
        &cwd,
        &env,
        &[
            "runtime",
            "update",
            "stable",
            "--version",
            "0.2.0",
            "--channel",
            "stable",
        ],
    );
    assert_eq!(conflicting.status.code(), Some(1));
    assert!(
        stderr(&conflicting).contains("runtime update accepts only one of --version or --channel")
    );

    let explicit_manifest = run_ocm(
        &cwd,
        &env,
        &["runtime", "update", "stable", "--version", "0.2.0"],
    );
    assert_eq!(explicit_manifest.status.code(), Some(1));
    assert!(
        stderr(&explicit_manifest)
            .contains("runtime \"stable\" is not backed by a release manifest")
    );

    fs::write(
        stores.runtimes_dir.join("legacy.json"),
        "{\n  \"kind\": \"ocm-runtime\",\n  \"name\": \"legacy\",\n  \"binaryPath\": \"/tmp/openclaw\",\n  \"sourceKind\": \"installed\",\n  \"sourceManifestUrl\": \"https://example.test/releases.json\",\n  \"releaseVersion\": \"0.2.0\",\n  \"releaseChannel\": \"stable\",\n  \"createdAt\": \"2026-03-25T10:00:00Z\",\n  \"updatedAt\": \"2026-03-25T10:00:00Z\"\n}\n",
    )
    .unwrap();

    let ambiguous_default = run_ocm(&cwd, &env, &["runtime", "update", "legacy"]);
    assert_eq!(ambiguous_default.status.code(), Some(1));
    assert!(stderr(&ambiguous_default).contains(
        "runtime \"legacy\" does not have a stored release selector; pass --version or --channel"
    ));

    let unexpected_name = run_ocm(&cwd, &env, &["runtime", "update", "--all", "stable"]);
    assert_eq!(unexpected_name.status.code(), Some(1));
    assert!(stderr(&unexpected_name).contains("unexpected arguments: stable"));
}
