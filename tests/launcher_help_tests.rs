mod support;

use std::fs;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};

#[test]
fn top_level_help_is_clean_and_points_to_topics() {
    let root = TestDir::new("help-root");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains(&format!("OpenClaw Manager v{}", env!("CARGO_PKG_VERSION"))));
    assert!(output.contains(
        "Manage isolated OpenClaw environments, releases, runtimes, launchers, and env supervision."
    ));
    assert!(output.contains("ocm [--color <mode>] <command> [args]"));
    assert!(output.contains("Fast path: create or reuse an env and keep it running"));
    assert!(output.contains("OpenClaw development envs with worktrees and watch mode"));
    assert!(output.contains("Guided setup for release and local-dev flows"));
    assert!(output.contains("Safely update one env or all envs and restart services when needed"));
    assert!(output.contains("Check host software for release and feature readiness"));
    assert!(output.contains("Update the installed ocm binary"));
    assert!(output.contains("Bring an existing plain OpenClaw home into OCM"));
    assert!(output.contains("Inspect and control the explicit OpenClaw adoption flow"));
    assert!(output.contains("--color <mode>"));
    assert!(output.contains("Color policy for pretty output: auto, always, or never"));
    assert!(output.contains("Environment lifecycle, binding, execution, snapshots, and repair"));
    assert!(output.contains("start"));
    assert!(output.contains("dev"));
    assert!(output.contains("upgrade"));
    assert!(output.contains("setup"));
    assert!(output.contains("ocm dev shaks"));
    assert!(output.contains("ocm dev shaks --watch"));
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
    assert!(output.contains("Read env logs"));
    assert!(output.contains(
        "ocm logs <env> [--stream <info|error>] [--tail <count>] [--follow|-f] [--raw] [--json]"
    ));
    assert!(output.contains("--follow, -f"));
    assert!(output.contains("ocm logs mira --follow"));
    assert!(output.contains("ocm logs mira -f"));
    assert!(output.contains("ocm logs mira --stream error --follow"));
    assert!(output.contains("By default OCM shows both stdout and stderr together in time order."));
    assert!(output.contains("Default output shows the last 50 lines."));
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
    assert!(output.contains("Development envs"));
    assert!(output.contains(
        "ocm dev <env> [--repo <path>] [--root <path>] [--port <port>] [--watch] [--force] [--service] [--onboard]"
    ));
    assert!(output.contains("ocm dev shaks --root /tmp/shaks"));
    assert!(output.contains("ocm dev shaks --watch"));
    assert!(output.contains("ocm dev shaks --watch --force"));
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
    assert!(output.contains("Show dev env status"));
    assert!(output.contains("ocm dev status [env] [--raw] [--json]"));
    assert!(output.contains("Only envs created through `ocm dev` appear here."));
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
    assert!(output.contains("Doctor commands"));
    assert!(output.contains("ocm doctor host"));
    assert!(output.contains("Check required software for official releases"));
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
    assert!(output.contains("Check host readiness"));
    assert!(output.contains("ocm doctor host [--raw] [--json]"));
    assert!(output.contains("ocm doctor host --fix git --yes [--json]"));
    assert!(output.contains("--fix <tool>"));
    assert!(output.contains("--yes"));
    assert!(output.contains(
        "Official release installs prefer host Node.js 22.22.3+, 24.15.0+, or 25.9.0+ and npm."
    ));
    assert!(
        output.contains(
            "On supported platforms, OCM can manage a private copy when they are missing."
        )
    );
    assert!(output.contains(
        "Git is the first supported host fix target; OCM will not install Homebrew automatically."
    ));
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
    assert!(output.contains("Start an environment"));
    assert!(output.contains("ocm start [name]"));
    assert!(output.contains("Optional environment name. If omitted, ocm generates a new one."));
    assert!(output.contains("--no-service"));
    assert!(output.contains("--onboard"));
    assert!(output.contains("ocm start mira --channel stable"));
    assert!(output.contains("ocm start luna --command 'pnpm openclaw' --cwd /path/to/openclaw"));
    assert!(output.contains("ocm start mira --onboard"));
    assert!(output.contains("Start installs and starts the env service by default."));
    assert!(output.contains("Start writes the minimum local config by default"));
    assert!(output.contains(
        "Managed services currently support launchd on macOS and systemd --user on Linux."
    ));
    assert!(output.contains(
        "Official release selectors prefer host Node.js 22.22.3+, 24.15.0+, or 25.9.0+ and npm, and OCM can manage a private copy on supported platforms when they are missing."
    ));
    assert!(output.contains(
        "When start creates a new official-release env interactively, it can offer to install git for repo-aware coding workflows."
    ));
    assert!(output.contains(
        "If OCM detects an existing plain OpenClaw home, start keeps the new env fresh and points you at `migrate`"
    ));
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
    assert!(output.contains("Upgrade environments"));
    assert!(output.contains("ocm upgrade simulate <env> --to <version|channel|repo-path>"));
    assert!(output.contains(
        "ocm upgrade <env> [--version <version> | --channel <channel> | --runtime <runtime>]"
    ));
    assert!(output.contains("--to <target>"));
    assert!(output.contains("--dry-run"));
    assert!(output.contains("--no-rollback"));
    assert!(output.contains("ocm upgrade --all"));
    assert!(output.contains("Simulations clone the source env"));
    assert!(output.contains("Channel-tracked runtimes move forward automatically."));
    assert!(output.contains("older targets are rejected before snapshot creation"));
    assert!(output.contains("require both HTTP health and OpenClaw gateway reachability"));
    assert!(output.contains("upgrade history <env>"));
    assert!(output.contains("upgrade rollback <env>"));
    assert!(
        output.contains("An env upgrade will not replace runtime bytes shared with another env")
    );
    assert!(output.contains("pre-upgrade snapshot"));
    assert!(output.contains("OpenClaw update finalization"));
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
    assert!(output.contains("Roll back an upgrade"));
    assert!(output.contains("upgrade rollback <env>"));
    assert!(output.contains("--transaction <id>"));
    assert!(output.contains("--dry-run"));
    assert!(output.contains("pre-rollback safety snapshot"));
    assert!(output.contains("restores the pre-rollback runtime and environment state"));
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
    assert!(output.contains("Guided setup"));
    assert!(output.contains("ocm setup"));
    assert!(output.contains("Interactive setup"));
    assert!(output.contains(
        "Official release choices prefer host Node.js 22.22.3+, 24.15.0+, or 25.9.0+ and npm, and OCM can manage a private copy on supported platforms when they are missing."
    ));
    assert!(output.contains(
        "If git is missing, setup can offer to install it for repo-aware coding workflows."
    ));
    assert!(
        output.contains(
            "If OCM detects an existing plain OpenClaw home, setup points you at `migrate`"
        )
    );
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
    assert!(output.contains("Self commands"));
    assert!(output.contains("Check for or install a newer ocm release"));
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
    assert!(output.contains("Environment commands"));
    assert!(output.contains("destroy"));
    assert!(output.contains("snapshot create"));
    assert!(output.contains("Portability:"));
    assert!(output.contains("ocm help env create"));
    assert!(output.contains("ocm help env snapshot"));
}

#[test]
fn env_destroy_help_describes_preview_first_teardown() {
    let root = TestDir::new("help-env-destroy");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "env", "destroy"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains("Destroy an environment"));
    assert!(output.contains(
        "ocm env destroy <name> [--yes] [--force | --if-state-token <token>] [--raw] [--json]"
    ));
    assert!(output.contains("--if-state-token <token>"));
    assert!(output.contains("ocm env destroy mira --yes"));
    assert!(output.contains("Destroy does not remove shared runtimes or launchers."));
    assert!(output.contains("TTY output uses cards by default."));
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
fn env_clone_and_import_help_describe_target_sandbox_origins() {
    let root = TestDir::new("help-env-sandbox-origin");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let clone = run_ocm(&cwd, &env, &["help", "env", "clone"]);
    assert!(clone.status.success(), "{}", stderr(&clone));
    let clone_output = stdout(&clone);
    assert!(clone_output.contains("--sandbox-origin <url>"));
    assert!(clone_output.contains("removes a copied public MCP app sandbox origin"));
    assert!(clone_output.contains("owned by $include"));

    let import = run_ocm(&cwd, &env, &["help", "env", "import"]);
    assert!(import.status.success(), "{}", stderr(&import));
    let import_output = stdout(&import);
    assert!(import_output.contains("--sandbox-origin <url>"));
    assert!(import_output.contains("removes a copied public MCP app sandbox origin"));
    assert!(import_output.contains("owned by $include"));
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
    assert!(output.contains("Release commands"));
    assert!(output.contains("List published OpenClaw releases"));
    assert!(output.contains("Install a published OpenClaw release as a runtime"));
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
    assert!(output.contains("Run OpenClaw inside an environment"));
    assert!(output.contains(
        "ocm env run <name> [--runtime <name> | --launcher <name>] -- <openclaw args...>"
    ));
    assert!(output.contains("ocm -- status"));
    assert!(output.contains("ocm @mira -- status"));
    assert!(output.contains("`--` is required before OpenClaw arguments."));
    assert!(
        output.contains(
            "If an environment is active, you can also use the root-level `--` shortcut."
        )
    );
    assert!(
        output.contains("For one-shot explicit env runs, use the root-level `@<env>` shortcut.")
    );
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
    assert!(output.contains("TTY output uses grouped cards by default."));

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
    assert!(output.contains(
        "TTY output uses cards for one env and a table when no env is passed or `--all` is used."
    ));

    let service_install = run_ocm(&cwd, &env, &["help", "service", "install"]);
    assert!(
        service_install.status.success(),
        "{}",
        stderr(&service_install)
    );
    let output = stdout(&service_install);
    assert!(output.contains("ocm service install <env> [--raw] [--json]"));
    assert!(output.contains("Use `service start` to start the env after it is enabled."));
    assert!(output.contains("shared OCM background service"));

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
    assert!(output.contains("never rebuilds desired state"));
}

#[test]
fn nested_snapshot_help_is_available() {
    let root = TestDir::new("help-env-snapshot");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let group = run_ocm(&cwd, &env, &["env", "snapshot"]);
    assert!(group.status.success(), "{}", stderr(&group));
    let output = stdout(&group);
    assert!(output.contains("Environment snapshot commands"));
    assert!(output.contains("snapshot prune"));

    let leaf = run_ocm(&cwd, &env, &["env", "snapshot", "create", "--help"]);
    assert!(leaf.status.success(), "{}", stderr(&leaf));
    let output = stdout(&leaf);
    assert!(output.contains("Create an environment snapshot"));
    assert!(output.contains("ocm env snapshot create <name> [--label <label>] [--raw] [--json]"));

    let show = run_ocm(&cwd, &env, &["help", "env", "snapshot", "show"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let output = stdout(&show);
    assert!(output.contains("ocm env snapshot show <name> <snapshot> [--raw] [--json]"));
    assert!(output.contains("TTY output uses grouped cards by default."));

    let list = run_ocm(&cwd, &env, &["help", "env", "snapshot", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let output = stdout(&list);
    assert!(output.contains("ocm env snapshot list <name> [--raw] [--json]"));
    assert!(output.contains("TTY output renders a table by default."));

    let remove = run_ocm(&cwd, &env, &["help", "env", "snapshot", "remove"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    let output = stdout(&remove);
    assert!(output.contains("ocm env snapshot remove <name> <snapshot> [--raw] [--json]"));
    assert!(output.contains(
        "validates the stored environment, snapshot ID, storage kind, and artifact path"
    ));
    assert!(output.contains("later cleanup failures are reported as warnings"));

    let prune = run_ocm(&cwd, &env, &["help", "env", "snapshot", "prune"]);
    assert!(prune.status.success(), "{}", stderr(&prune));
    let output = stdout(&prune);
    assert!(output.contains(
        "ocm env snapshot prune (<name> | --all) [--keep <count>] [--older-than <days>] [--raw] [--yes] [--json]"
    ));
    assert!(
        output.contains("TTY output renders tables for preview and applied removals by default.")
    );
    assert!(output.contains("preserves cleanup warnings for each removed snapshot"));
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
    assert!(output.contains("Install a managed runtime"));
    assert!(output.contains("--manifest-url <url>"));
    assert!(output.contains("Exactly one install source must be provided."));
    assert!(output.contains(
        "Official release installs prefer host Node.js 22.22.3+, 24.15.0+, or 25.9.0+ and npm."
    ));
    assert!(
        output.contains(
            "On supported platforms, OCM can manage a private copy when they are missing."
        )
    );
    assert!(output.contains(
        "Use `ocm doctor host` only if you want a full machine check or an explicit host-tool fix like git."
    ));
    assert!(
        output
            .contains("Bound runtimes are reused only when the selected release already matches.")
    );

    let service = run_ocm(&cwd, &env, &["service", "start", "--help"]);
    assert!(service.status.success(), "{}", stderr(&service));
    let output = stdout(&service);
    assert!(output.contains("Start an env under the background service"));
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
    assert!(output.contains("mutually exclusive"));

    let remove = run_ocm(&cwd, &env, &["help", "runtime", "remove"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(stdout(&remove).contains("Clear every environment binding"));
}

#[test]
fn release_install_help_mentions_doctor_host() {
    let root = TestDir::new("help-release-install");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let help = run_ocm(&cwd, &env, &["help", "release", "install"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let output = stdout(&help);
    assert!(output.contains("ocm release install [<name>] (--version <version> | --channel <channel>) [--description <text>] [--force] [--raw] [--json]"));
    assert!(output.contains(
        "Official release installs prefer host Node.js 22.22.3+, 24.15.0+, or 25.9.0+ and npm."
    ));
    assert!(
        output.contains(
            "On supported platforms, OCM can manage a private copy when they are missing."
        )
    );
    assert!(output.contains(
        "Use `ocm doctor host` only if you want a full machine check or an explicit host-tool fix like git."
    ));
    assert!(
        output
            .contains("Bound runtimes are reused only when the selected release already matches.")
    );
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
    assert!(output.contains("TTY output uses grouped cards by default."));

    let runtime_show = run_ocm(&cwd, &env, &["help", "runtime", "show"]);
    assert!(runtime_show.status.success(), "{}", stderr(&runtime_show));
    let output = stdout(&runtime_show);
    assert!(output.contains("ocm runtime show <name> [--raw] [--json]"));
    assert!(output.contains("TTY output uses grouped cards by default."));
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
    assert!(output.contains("Exact versions accept either `1.2.3` or `v1.2.3`."));
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
    assert!(
        output
            .contains("TTY output uses cards for one runtime and a table for `--all` by default.")
    );
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
fn runtime_which_help_mentions_raw_mode_and_tty_cards() {
    let root = TestDir::new("help-runtime-which");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let which = run_ocm(&cwd, &env, &["help", "runtime", "which"]);
    assert!(which.status.success(), "{}", stderr(&which));
    let output = stdout(&which);
    assert!(output.contains("ocm runtime which <name> [--raw] [--json]"));
    assert!(output.contains("--raw"));
    assert!(output.contains("TTY output uses a grouped card by default."));
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
