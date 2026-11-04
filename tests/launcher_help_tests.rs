mod support;

use std::fs;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};

#[test]
fn top_level_help_lists_commands_and_examples() {
    let root = TestDir::new("help-root");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains(&format!("OpenClaw Manager v{}", env!("CARGO_PKG_VERSION"))));
    assert!(output.contains("ocm [--color <mode>] <command> [args]"));
    assert!(output.contains("--color <mode>"));
    assert!(output.contains("ocm dev shaks"));
    assert!(output.contains("ocm dev shaks --no-ui"));
    assert!(output.contains("ocm start"));
    assert!(output.contains("ocm migrate mira"));
    assert!(output.contains("ocm adopt inspect"));
    assert!(output.contains("ocm upgrade mira"));
    assert!(output.contains("ocm help setup"));
    assert!(output.contains("ocm help dev"));
    assert!(output.contains("ocm help adopt"));
    assert!(output.contains("ocm help upgrade"));
    assert!(output.contains("ocm help doctor"));
    assert!(output.contains("ocm help logs"));
    assert!(output.contains("ocm help self"));
    assert!(output.contains("ocm start mira"));
    assert!(output.contains("ocm start mira --channel beta"));
    assert!(
        output.contains(
            "ocm start luna --command 'pnpm openclaw' --cwd /path/to/openclaw --no-service"
        )
    );
    assert!(output.contains("ocm @mira -- onboard"));
    assert!(output.contains("ocm @mira -- status"));
    assert!(output.contains("ocm help start"));
    assert!(output.contains("ocm help env"));
    assert!(output.contains("ocm help release"));
    assert!(output.contains("ocm help runtime install"));
    assert!(output.contains("ocm --color always env list"));
    assert!(!output.contains("ocm help sync"));
    assert!(!output.contains("ocm help manifest"));
    assert!(!output.contains("Reconcile an existing env from an optional ocm.yaml manifest"));
    assert!(!output.contains("Inspect optional ocm.yaml manifests without changing env state"));
    assert!(!output.contains("env snapshot restore <name> <snapshot>"));
    assert!(!output.contains("service restore-global"));
    assert!(!output.contains("service discover"));
}

#[test]
fn logs_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-logs");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "logs"]);
    let via_flag = run_ocm(&cwd, &env, &["logs", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains(
        "ocm logs <env> [--stream <info|error>] [--tail <count>] [--follow|-f] [--raw] [--json]"
    ));
    assert!(output.contains("--follow, -f"));
    assert!(output.contains("ocm logs mira --follow"));
    assert!(output.contains("ocm logs mira -f"));
    assert!(output.contains("ocm logs mira --stream error --follow"));
}

#[test]
fn dev_help_is_available_from_help_and_bare_group() {
    let root = TestDir::new("help-dev-group");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "dev"]);
    let bare = run_ocm(&cwd, &env, &["dev", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(bare.status.success(), "{}", stderr(&bare));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&bare));
    assert!(output.contains(
        "ocm dev <env> [--repo <path>] [--root <path>] [--port <port>] [--watch | --no-watch] [--ui | --no-ui] [--force] [--service] [--onboard]"
    ));
    assert!(output.contains("ocm dev shaks --root /tmp/shaks"));
    assert!(output.contains("ocm dev shaks --no-ui"));
    assert!(output.contains("ocm dev shaks --no-watch"));
    assert!(output.contains("ocm dev shaks --no-watch --no-ui"));
    assert!(output.contains("ocm dev shaks --force"));
    assert!(output.contains(
        "--service uses the background workflow without foreground watching or UI and rejects explicit --watch or --ui."
    ));
    assert!(output.contains("ocm dev shaks --repo /path/to/openclaw --watch --force"));
    assert!(output.contains("ocm dev shaks --service"));
    assert!(output.contains("ocm help dev status"));
}

#[test]
fn dev_status_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-dev-status");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "dev", "status"]);
    let via_flag = run_ocm(&cwd, &env, &["dev", "status", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains("ocm dev status [env] [--raw] [--json]"));
}

#[test]
fn doctor_group_help_is_available_from_help_and_bare_group() {
    let root = TestDir::new("help-doctor-group");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "doctor"]);
    let bare = run_ocm(&cwd, &env, &["doctor"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(bare.status.success(), "{}", stderr(&bare));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&bare));
    assert!(output.contains("ocm doctor host"));
}

#[test]
fn doctor_host_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-doctor-host");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "doctor", "host"]);
    let via_flag = run_ocm(&cwd, &env, &["doctor", "host", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains("ocm doctor host [--raw] [--json]"));
    assert!(output.contains("ocm doctor host --fix git --yes [--json]"));
    assert!(output.contains("--fix <tool>"));
    assert!(output.contains("--yes"));
}

#[test]
fn help_uses_ocm_self_for_root_and_leaf_examples() {
    let root = TestDir::new("help-ocm-self");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert("OCM_SELF".to_string(), "./bin/ocm".to_string());

    let help = run_ocm(&cwd, &env, &["help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains("./bin/ocm [--color <mode>] <command> [args]"));
    assert!(output.contains("./bin/ocm @mira -- onboard"));

    let help = run_ocm(&cwd, &env, &["help", "env", "run"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains(
        "./bin/ocm env run <name> [--runtime <name> | --launcher <name>] -- <openclaw args...>"
    ));
    assert!(output.contains("./bin/ocm env run mira -- onboard"));
}

#[test]
fn start_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-start");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "start"]);
    let via_flag = run_ocm(&cwd, &env, &["start", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains("ocm start [name]"));
    assert!(output.contains("--no-service"));
    assert!(output.contains("--onboard"));
    assert!(output.contains("ocm start mira --channel stable"));
    assert!(output.contains("ocm start luna --command 'pnpm openclaw' --cwd /path/to/openclaw"));
    assert!(output.contains("ocm start mira --onboard"));
}

#[test]
fn upgrade_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-upgrade");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "upgrade"]);
    let via_flag = run_ocm(&cwd, &env, &["upgrade", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(
        output
            .contains("ocm upgrade simulate <env> --to <version|channel|repo-path|owner/repo#PR>")
    );
    assert!(output.contains(
        "ocm upgrade <env> [--version <version> | --channel <channel> | --runtime <runtime>]"
    ));
    assert!(output.contains("--to <target>"));
    assert!(output.contains("--dry-run"));
    assert!(output.contains("--no-rollback"));
    assert!(output.contains("ocm upgrade --all"));
    assert!(output.contains("ocm upgrade batch --runtime <runtime> --envs <env,env,...>"));
    assert!(output.contains("--accept-fleet-outage"));
}

#[test]
fn upgrade_rollback_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-upgrade-rollback");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "upgrade", "rollback"]);
    let via_flag = run_ocm(&cwd, &env, &["upgrade", "rollback", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains("--transaction <id>"));
    assert!(output.contains("--dry-run"));
}

#[test]
fn setup_help_is_available_from_help_and_flag() {
    let root = TestDir::new("help-setup");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "setup"]);
    let via_flag = run_ocm(&cwd, &env, &["setup", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains("ocm setup"));
}

#[test]
fn self_help_is_available_from_help_and_bare_group() {
    let root = TestDir::new("help-self-group");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "self"]);
    let bare = run_ocm(&cwd, &env, &["self"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(bare.status.success(), "{}", stderr(&bare));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&bare));
    assert!(output.contains("ocm self update --check"));
}

#[test]
fn env_group_help_is_available_from_help_and_bare_group() {
    let root = TestDir::new("help-env-group");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "env"]);
    let bare = run_ocm(&cwd, &env, &["env"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(bare.status.success(), "{}", stderr(&bare));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&bare));
    assert!(output.contains("ocm help env create"));
    assert!(output.contains("ocm help env snapshot"));
}

#[test]
fn env_destroy_help_lists_teardown_options() {
    let root = TestDir::new("help-env-destroy");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "env", "destroy"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains(
        "ocm env destroy <name> [--yes] [--force | --if-state-token <token>] [--raw] [--json]"
    ));
    assert!(output.contains("--if-state-token <token>"));
    assert!(output.contains("ocm env destroy mira --yes"));
}

#[test]
fn env_create_help_mentions_release_selectors() {
    let root = TestDir::new("help-env-create-release-selectors");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "env", "create"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains(
        "ocm env create <name> [--root <path>] [--port <port>] [--runtime <name> | --version <version> | --channel <channel>] [--launcher <name>] [--protect] [--raw] [--json]"
    ));
    assert!(output.contains("--version <version>"));
    assert!(output.contains("--channel <channel>"));
    assert!(output.contains("ocm env create rowan --channel stable"));
    assert!(output.contains("ocm env create ember --version 2026.3.24"));
}

#[test]
fn env_clone_and_import_help_list_sandbox_origin_option() {
    let root = TestDir::new("help-env-sandbox-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let clone = run_ocm(&cwd, &env, &["help", "env", "clone"]);
    assert!(clone.status.success(), "{}", stderr(&clone));
    let clone_output = stdout(&clone);
    assert!(clone_output.contains("--sandbox-origin <url>"));

    let import = run_ocm(&cwd, &env, &["help", "env", "import"]);
    assert!(import.status.success(), "{}", stderr(&import));
    let import_output = stdout(&import);
    assert!(import_output.contains("--sandbox-origin <url>"));
}

#[test]
fn env_set_runtime_help_mentions_release_selectors() {
    let root = TestDir::new("help-env-set-runtime-release-selectors");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "env", "set-runtime"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains("ocm env set-runtime <name> <runtime|none>"));
    assert!(
        output.contains("ocm env set-runtime <name> (--version <version> | --channel <channel>)")
    );
    assert!(output.contains("--version <version>"));
    assert!(output.contains("--channel <channel>"));
    assert!(output.contains("ocm env set-runtime mira --channel stable"));
    assert!(output.contains("ocm env set-runtime mira --version 2026.3.24"));
}

#[test]
fn release_group_help_is_available_from_help_and_bare_group() {
    let root = TestDir::new("help-release-group");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "release"]);
    let bare = run_ocm(&cwd, &env, &["release"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(bare.status.success(), "{}", stderr(&bare));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&bare));
    assert!(output.contains("ocm release show 2026.3.24"));
}

#[test]
fn env_run_help_is_available_through_help_keyword_and_flag() {
    let root = TestDir::new("help-env-run");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let via_help = run_ocm(&cwd, &env, &["help", "env", "run"]);
    let via_flag = run_ocm(&cwd, &env, &["env", "run", "--help"]);
    assert!(via_help.status.success(), "{}", stderr(&via_help));
    assert!(via_flag.status.success(), "{}", stderr(&via_flag));

    let output = stdout(&via_help);
    assert_eq!(output, stdout(&via_flag));
    assert!(output.contains(
        "ocm env run <name> [--runtime <name> | --launcher <name>] -- <openclaw args...>"
    ));
    assert!(output.contains("ocm -- status"));
    assert!(output.contains("ocm @mira -- status"));
}

#[test]
fn env_and_service_status_style_help_mentions_raw_mode() {
    let root = TestDir::new("help-status-style");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let env_show = run_ocm(&cwd, &env, &["help", "env", "show"]);
    assert!(env_show.status.success(), "{}", stderr(&env_show));
    let output = stdout(&env_show);
    assert!(output.contains("ocm env show <name> [--raw] [--json]"));

    let env_status = run_ocm(&cwd, &env, &["help", "env", "status"]);
    assert!(env_status.status.success(), "{}", stderr(&env_status));
    let output = stdout(&env_status);
    assert!(output.contains("ocm env status <name> [--raw] [--json]"));
    assert!(output.contains("--raw"));

    let env_resolve = run_ocm(&cwd, &env, &["help", "env", "resolve"]);
    assert!(env_resolve.status.success(), "{}", stderr(&env_resolve));
    let output = stdout(&env_resolve);
    assert!(output.contains(
        "ocm env resolve <name> [--runtime <name> | --launcher <name>] [--raw] [--json] [-- <openclaw args...>]"
    ));

    let env_doctor = run_ocm(&cwd, &env, &["help", "env", "doctor"]);
    assert!(env_doctor.status.success(), "{}", stderr(&env_doctor));
    let output = stdout(&env_doctor);
    assert!(output.contains("ocm env doctor <name> [--raw] [--json]"));

    let service_status = run_ocm(&cwd, &env, &["help", "service", "status"]);
    assert!(
        service_status.status.success(),
        "{}",
        stderr(&service_status)
    );
    let output = stdout(&service_status);
    assert!(output.contains("ocm service status [env] [--raw] [--json]"));

    let service_install = run_ocm(&cwd, &env, &["help", "service", "install"]);
    assert!(
        service_install.status.success(),
        "{}",
        stderr(&service_install)
    );
    let output = stdout(&service_install);
    assert!(output.contains("ocm service install <env> [--raw] [--json]"));

    let refresh_daemon = run_ocm(&cwd, &env, &["help", "service", "refresh-daemon"]);
    assert!(
        refresh_daemon.status.success(),
        "{}",
        stderr(&refresh_daemon)
    );
    let output = stdout(&refresh_daemon);
    assert!(
        output.contains(
            "ocm service refresh-daemon [--acknowledge-gateway-restarts] [--raw] [--json]"
        )
    );
}

#[test]
fn nested_snapshot_help_is_available() {
    let root = TestDir::new("help-env-snapshot");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let group = run_ocm(&cwd, &env, &["env", "snapshot"]);
    assert!(group.status.success(), "{}", stderr(&group));

    let leaf = run_ocm(&cwd, &env, &["env", "snapshot", "create", "--help"]);
    assert!(leaf.status.success(), "{}", stderr(&leaf));
    let output = stdout(&leaf);
    assert!(output.contains("ocm env snapshot create <name> [--label <label>] [--raw] [--json]"));

    let show = run_ocm(&cwd, &env, &["help", "env", "snapshot", "show"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("ocm env snapshot show <name> <snapshot> [--raw] [--json]"));

    let list = run_ocm(&cwd, &env, &["help", "env", "snapshot", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let output = stdout(&list);
    assert!(output.contains("ocm env snapshot list <name> [--raw] [--json]"));

    let remove = run_ocm(&cwd, &env, &["help", "env", "snapshot", "remove"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    let output = stdout(&remove);
    assert!(output.contains("ocm env snapshot remove <name> <snapshot> [--raw] [--json]"));

    let prune = run_ocm(&cwd, &env, &["help", "env", "snapshot", "prune"]);
    assert!(prune.status.success(), "{}", stderr(&prune));
    let output = stdout(&prune);
    assert!(output.contains(
        "ocm env snapshot prune (<name> | --all) [--keep <count>] [--older-than <days>] [--raw] [--yes] [--json]"
    ));
}

#[test]
fn runtime_and_service_leaf_help_are_command_specific() {
    let root = TestDir::new("help-runtime-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let runtime = run_ocm(&cwd, &env, &["help", "runtime", "install"]);
    assert!(runtime.status.success(), "{}", stderr(&runtime));
    let output = stdout(&runtime);
    assert!(output.contains("--manifest-url <url>"));

    let service = run_ocm(&cwd, &env, &["service", "start", "--help"]);
    assert!(service.status.success(), "{}", stderr(&service));
    let output = stdout(&service);
    assert!(output.contains("ocm service start <env> [--raw] [--json]"));
}

#[test]
fn runtime_mutation_help_directs_bound_environments_to_upgrade() {
    let root = TestDir::new("help-runtime-bound-mutations");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    for command in [
        vec!["help", "runtime", "install"],
        vec!["help", "runtime", "build-local"],
        vec!["help", "runtime", "update"],
    ] {
        let help = run_ocm(&cwd, &env, &command);
        assert!(help.status.success(), "{}", stderr(&help));
        assert!(stdout(&help).contains("ocm upgrade <env>"));
    }

    let build_local = run_ocm(&cwd, &env, &["help", "runtime", "build-local"]);
    let output = stdout(&build_local);
    assert!(output.contains("--for-env <env>"));

    let remove = run_ocm(&cwd, &env, &["help", "runtime", "remove"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
}

#[test]
fn release_install_help_lists_release_selectors() {
    let root = TestDir::new("help-release-install");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "release", "install"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains("ocm release install [<name>] (--version <version> | --channel <channel>) [--description <text>] [--force] [--raw] [--json]"));
}

#[test]
fn release_and_runtime_show_help_mentions_raw_mode() {
    let root = TestDir::new("help-release-runtime-show");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let release_show = run_ocm(&cwd, &env, &["help", "release", "show"]);
    assert!(release_show.status.success(), "{}", stderr(&release_show));
    let output = stdout(&release_show);
    assert!(output.contains(
        "ocm release show (<version> | --version <version> | --channel <channel>) [--raw] [--json]"
    ));

    let runtime_show = run_ocm(&cwd, &env, &["help", "runtime", "show"]);
    assert!(runtime_show.status.success(), "{}", stderr(&runtime_show));
    let output = stdout(&runtime_show);
    assert!(output.contains("ocm runtime show <name> [--raw] [--json]"));
}

#[test]
fn self_update_help_mentions_check_and_raw_modes() {
    let root = TestDir::new("help-self-update");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "self", "update"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains("ocm self update [--version <version>] [--check] [--raw] [--json]"));
    assert!(output.contains("--check"));
    assert!(output.contains("--raw"));
}

#[test]
fn runtime_verify_help_mentions_raw_mode() {
    let root = TestDir::new("help-runtime-verify");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let verify = run_ocm(&cwd, &env, &["help", "runtime", "verify"]);
    assert!(verify.status.success(), "{}", stderr(&verify));
    let output = stdout(&verify);
    assert!(output.contains("ocm runtime verify (<name> | --all) [--raw] [--json]"));
}

#[test]
fn runtime_releases_help_mentions_standard_output_modes() {
    let root = TestDir::new("help-runtime-releases");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let releases = run_ocm(&cwd, &env, &["help", "runtime", "releases"]);
    assert!(releases.status.success(), "{}", stderr(&releases));
    let output = stdout(&releases);
    assert!(output.contains(
        "ocm runtime releases [--manifest-url <url>] [--version <version> | --channel <channel>] [--raw] [--json]"
    ));
    assert!(output.contains("--raw"));
    assert!(output.contains("--json"));
}

#[test]
fn runtime_which_help_lists_output_modes() {
    let root = TestDir::new("help-runtime-which");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let which = run_ocm(&cwd, &env, &["help", "runtime", "which"]);
    assert!(which.status.success(), "{}", stderr(&which));
    let output = stdout(&which);
    assert!(output.contains("ocm runtime which <name> [--raw] [--json]"));
    assert!(output.contains("--raw"));
}

#[test]
fn version_flag_uses_the_package_version() {
    let root = TestDir::new("version-flag");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let version = run_ocm(&cwd, &env, &["--version"]);
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(stdout(&version), concat!(env!("CARGO_PKG_VERSION"), "\n"));
}

#[test]
fn unknown_launcher_commands_use_launcher_specific_errors() {
    let root = TestDir::new("launcher-unknown-command");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["launcher", "rename"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("unknown launcher command: rename"));
}

#[test]
fn unknown_self_commands_use_self_specific_errors() {
    let root = TestDir::new("self-unknown-command");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let output = run_ocm(&cwd, &env, &["self", "upgrade"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("unknown self command: upgrade"));
}
