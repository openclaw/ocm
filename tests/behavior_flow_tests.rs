mod support;

use std::fs;
use std::process::Command;

use crate::support::{
    TestDir, install_fake_launchctl, ocm_env, path_string, run_ocm, stderr, stdout,
    write_executable_script, write_text,
};
use ocm::store::{clean_path, now_utc, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::Value;

fn exported_gateway_port(output: &std::process::Output) -> u32 {
    let script = stdout(output);
    let marker = "export OPENCLAW_GATEWAY_PORT='";
    let start = script.find(marker).expect("gateway port export missing") + marker.len();
    let rest = &script[start..];
    let end = rest
        .find('\'')
        .expect("gateway port export terminator missing");
    rest[..end]
        .parse::<u32>()
        .expect("gateway port export must be numeric")
}

#[test]
fn env_use_prints_activation_exports_for_the_selected_environment() {
    let root = TestDir::new("behavior-env-use");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OPENCLAW_DEV_SOURCE_ROOT".to_string(),
        "/tmp/stale-source".to_string(),
    );
    env.insert(
        "OPENCLAW_BUNDLED_PLUGINS_DIR".to_string(),
        "/tmp/stale-plugins".to_string(),
    );
    env.insert(
        "OPENCLAW_RANDOM_USER_VALUE".to_string(),
        "stale".to_string(),
    );
    env.insert("OPENCLAW_PROFILE".to_string(), "legacy".to_string());
    env.insert("OPENCLAW_DIAGNOSTICS".to_string(), "timeline".to_string());

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--port", "19789"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let use_output = run_ocm(&cwd, &env, &["env", "use", "demo"]);
    assert!(use_output.status.success(), "{}", stderr(&use_output));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let script = stdout(&use_output);
    assert!(script.contains("unset OPENCLAW_PROFILE"));
    assert!(script.contains("unset OPENCLAW_DEV_SOURCE_ROOT"));
    assert!(script.contains("unset OPENCLAW_BUNDLED_PLUGINS_DIR"));
    assert!(script.contains("unset OPENCLAW_RANDOM_USER_VALUE"));
    assert!(!script.contains("unset OPENCLAW_DIAGNOSTICS"));
    assert!(script.contains(&format!("export OPENCLAW_HOME='{}'", env_root.display())));
    assert!(script.contains("export OPENCLAW_SERVICE_REPAIR_POLICY='external'"));
    assert!(script.contains(&format!("export OPENCLAW_GATEWAY_PORT='{}'", 19789)));

    let evaluated = Command::new("bash")
        .args([
            "-c",
            "eval \"$1\"; printf '%s|%s|%s|%s|%s' \"${OPENCLAW_DEV_SOURCE_ROOT-unset}\" \"${OPENCLAW_BUNDLED_PLUGINS_DIR-unset}\" \"${OPENCLAW_RANDOM_USER_VALUE-unset}\" \"$OPENCLAW_DIAGNOSTICS\" \"$OPENCLAW_GATEWAY_PORT\"",
            "bash",
            &script,
        ])
        .env_clear()
        .envs(&env)
        .output()
        .unwrap();
    assert!(evaluated.status.success());
    assert_eq!(
        String::from_utf8(evaluated.stdout).unwrap(),
        "unset|unset|unset|timeline|19789"
    );
}

#[test]
fn env_exec_injects_openclaw_environment_variables() {
    let root = TestDir::new("behavior-env-exec");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let exec_output = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "exec",
            "demo",
            "--",
            "sh",
            "-lc",
            "printf '%s|%s|%s' \"$OPENCLAW_HOME\" \"${OPENCLAW_PROFILE:-unset}\" \"$OPENCLAW_SERVICE_REPAIR_POLICY\"",
        ],
    );
    assert!(exec_output.status.success(), "{}", stderr(&exec_output));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    assert_eq!(
        stdout(&exec_output),
        format!("{}|unset|external", env_root.display())
    );
}

#[test]
fn env_run_uses_the_registered_launcher_and_its_cwd() {
    let root = TestDir::new("behavior-env-run");
    let cwd = root.child("workspace");
    let launcher_dir = cwd.join("launchers/stable");
    fs::create_dir_all(&launcher_dir).unwrap();
    let env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "stable",
            "--command",
            "sh",
            "--cwd",
            "./launchers/stable",
        ],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    let run_output = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "run",
            "demo",
            "--",
            "-lc",
            "printf '%s|%s' \"$PWD\" \"$OPENCLAW_HOME\"",
        ],
    );
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let expected_run_dir = fs::canonicalize(&launcher_dir).unwrap();
    assert_eq!(
        stdout(&run_output),
        format!("{}|{}", expected_run_dir.display(), env_root.display())
    );
}

#[test]
fn supervised_gateway_restart_requires_negotiated_external_handoff() {
    let root = TestDir::new("behavior-supervised-gateway-restart");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "launchd".to_string(),
    );
    install_fake_launchctl(&root, &mut env);

    let invocation_log = root.child("gateway-restart.log");
    let launcher = root.child("openclaw");
    write_executable_script(
        &launcher,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s|%s|%s|%s|%s' \"${{OPENCLAW_SUPERVISOR_MODE-unset}}\" \"${{OPENCLAW_SERVICE_MARKER-unset}}\" \"${{OPENCLAW_SERVICE_KIND-unset}}\" \"${{OPENCLAW_NO_RESPAWN-unset}}\" \"${{OPENCLAW_LAUNCHD_LABEL-unset}}\"\n",
            path_string(&invocation_log)
        ),
    );

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &[
            "launcher",
            "add",
            "stable",
            "--command",
            &path_string(&launcher),
        ],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));
    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "stable"],
    );
    assert!(create.status.success(), "{}", stderr(&create));
    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime_state = |restart_handoff: &str| SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: path_string(&root.child("ocm-home")),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "launcher".to_string(),
            binding_name: "stable".to_string(),
            gateway_state: "running".to_string(),
            restart_handoff: Some(restart_handoff.to_string()),
            restart_count: 0,
            child_port: 18789,
            pid: Some(4242),
            stdout_path: path_string(&root.child("demo.stdout.log")),
            stderr_path: path_string(&root.child("demo.stderr.log")),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
        }],
        children: Vec::new(),
    };
    fs::write(
        &runtime_path,
        serde_json::to_vec(&runtime_state("protocol-v1")).unwrap(),
    )
    .unwrap();

    let restart = run_ocm(&cwd, &env, &["@demo", "--", "gateway", "restart"]);
    assert!(restart.status.success(), "{}", stderr(&restart));
    assert_eq!(stdout(&restart), "external|openclaw|unset|1|unset");
    assert_eq!(
        fs::read_to_string(&invocation_log).unwrap(),
        "gateway restart\n"
    );

    fs::write(
        &runtime_path,
        serde_json::to_vec(&runtime_state("legacy")).unwrap(),
    )
    .unwrap();
    let legacy_restart = run_ocm(&cwd, &env, &["@demo", "--", "gateway", "restart"]);
    assert!(!legacy_restart.status.success());
    assert!(
        stderr(&legacy_restart).contains("has not negotiated external restart handoff protocol v1")
    );
    let profiled_legacy_restart = run_ocm(
        &cwd,
        &env,
        &["@demo", "--", "--profile", "work", "gateway", "restart"],
    );
    assert!(!profiled_legacy_restart.status.success());
    assert!(
        stderr(&profiled_legacy_restart)
            .contains("has not negotiated external restart handoff protocol v1")
    );
    let exec_lifecycle = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "exec",
            "demo",
            "--",
            "openclaw",
            "--no-color",
            "gateway",
            "stop",
        ],
    );
    assert!(!exec_lifecycle.status.success());
    assert!(
        stderr(&exec_lifecycle)
            .contains("env exec cannot safely run OpenClaw gateway lifecycle commands")
    );
    assert_eq!(
        fs::read_to_string(&invocation_log).unwrap(),
        "gateway restart\n"
    );
}

#[test]
fn env_run_onboard_clears_legacy_skip_bootstrap_by_default() {
    let root = TestDir::new("behavior-env-run-onboard-bootstrap");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "noop", "--command", "/usr/bin/true"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "noop"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let config_path = env_root.join(".openclaw/openclaw.json");
    let workspace = env_root.join(".openclaw/workspace");
    write_text(
        &config_path,
        &format!(
            r#"{{
  "agents": {{
    "defaults": {{
      "workspace": "{}",
      "skipBootstrap": true
    }}
  }},
  "gateway": {{
    "mode": "local"
  }}
}}
"#,
            path_string(&workspace)
        ),
    );

    let run_output = run_ocm(&cwd, &env, &["env", "run", "demo", "--", "onboard"]);
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let config: Value = serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
    assert!(config["agents"]["defaults"].get("skipBootstrap").is_none());
}

#[test]
fn env_run_onboard_preserves_explicit_skip_bootstrap() {
    let root = TestDir::new("behavior-env-run-onboard-skip-bootstrap");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "noop", "--command", "/usr/bin/true"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(&cwd, &env, &["env", "create", "demo", "--launcher", "noop"]);
    assert!(create.status.success(), "{}", stderr(&create));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    let config_path = env_root.join(".openclaw/openclaw.json");
    let workspace = env_root.join(".openclaw/workspace");
    write_text(
        &config_path,
        &format!(
            r#"{{
  "agents": {{
    "defaults": {{
      "workspace": "{}",
      "skipBootstrap": true
    }}
  }},
  "gateway": {{
    "mode": "local"
  }}
}}
"#,
            path_string(&workspace)
        ),
    );

    let run_output = run_ocm(
        &cwd,
        &env,
        &["env", "run", "demo", "--", "onboard", "--skip-bootstrap"],
    );
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let config: Value = serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(config["agents"]["defaults"]["skipBootstrap"], true);
}

#[test]
fn env_run_overrides_parent_openclaw_environment_state() {
    let root = TestDir::new("behavior-env-run-parent-env");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "inspect", "--command", "sh"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create_demo = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "inspect"],
    );
    assert!(create_demo.status.success(), "{}", stderr(&create_demo));

    let create_test = run_ocm(
        &cwd,
        &env,
        &["env", "create", "test", "--launcher", "inspect"],
    );
    assert!(create_test.status.success(), "{}", stderr(&create_test));

    let demo_root = clean_path(&root.child("ocm-home/envs/demo"));
    env.insert("OPENCLAW_HOME".to_string(), demo_root.display().to_string());
    env.insert(
        "OPENCLAW_STATE_DIR".to_string(),
        demo_root.join(".openclaw").display().to_string(),
    );
    env.insert(
        "OPENCLAW_CONFIG_PATH".to_string(),
        demo_root
            .join(".openclaw/openclaw.json")
            .display()
            .to_string(),
    );
    env.insert("OCM_ACTIVE_ENV".to_string(), "demo".to_string());
    env.insert(
        "OCM_ACTIVE_ENV_ROOT".to_string(),
        demo_root.display().to_string(),
    );
    env.insert("OPENCLAW_PROFILE".to_string(), "legacy".to_string());
    env.insert(
        "OPENCLAW_SERVICE_REPAIR_POLICY".to_string(),
        "auto".to_string(),
    );

    let run_output = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "run",
            "test",
            "--",
            "-lc",
            "printf '%s|%s|%s|%s|%s|%s' \"$OPENCLAW_HOME\" \"$OPENCLAW_STATE_DIR\" \"$OPENCLAW_CONFIG_PATH\" \"$OCM_ACTIVE_ENV\" \"${OPENCLAW_PROFILE:-unset}\" \"$OPENCLAW_SERVICE_REPAIR_POLICY\"",
        ],
    );
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let test_root = clean_path(&root.child("ocm-home/envs/test"));
    assert_eq!(
        stdout(&run_output),
        format!(
            "{}|{}|{}|{}|{}|{}",
            test_root.display(),
            test_root.join(".openclaw").display(),
            test_root.join(".openclaw/openclaw.json").display(),
            "test",
            "unset",
            "external"
        )
    );
}

#[test]
fn root_double_dash_runs_openclaw_in_the_active_environment() {
    let root = TestDir::new("behavior-root-double-dash");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "inspect", "--command", "sh"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "inspect"],
    );
    assert!(create.status.success(), "{}", stderr(&create));

    env.insert("OCM_ACTIVE_ENV".to_string(), "demo".to_string());
    env.insert(
        "OPENCLAW_HOME".to_string(),
        "/tmp/legacy-openclaw-home".to_string(),
    );
    env.insert("OPENCLAW_PROFILE".to_string(), "legacy".to_string());

    let run_output = run_ocm(
        &cwd,
        &env,
        &[
            "--",
            "-lc",
            "printf '%s|%s|%s' \"$OCM_ACTIVE_ENV\" \"$OPENCLAW_HOME\" \"${OPENCLAW_PROFILE:-unset}\"",
        ],
    );
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let env_root = clean_path(&root.child("ocm-home/envs/demo"));
    assert_eq!(
        stdout(&run_output),
        format!("demo|{}|unset", env_root.display())
    );
}

#[test]
fn root_at_env_runs_openclaw_in_the_named_environment() {
    let root = TestDir::new("behavior-root-at-env");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);

    let add_launcher = run_ocm(
        &cwd,
        &env,
        &["launcher", "add", "inspect", "--command", "sh"],
    );
    assert!(add_launcher.status.success(), "{}", stderr(&add_launcher));

    let create_demo = run_ocm(
        &cwd,
        &env,
        &["env", "create", "demo", "--launcher", "inspect"],
    );
    assert!(create_demo.status.success(), "{}", stderr(&create_demo));

    let create_test = run_ocm(
        &cwd,
        &env,
        &["env", "create", "test", "--launcher", "inspect"],
    );
    assert!(create_test.status.success(), "{}", stderr(&create_test));

    let demo_root = clean_path(&root.child("ocm-home/envs/demo"));
    env.insert("OCM_ACTIVE_ENV".to_string(), "demo".to_string());
    env.insert("OPENCLAW_HOME".to_string(), demo_root.display().to_string());
    env.insert("OPENCLAW_PROFILE".to_string(), "legacy".to_string());

    let run_output = run_ocm(
        &cwd,
        &env,
        &[
            "@test",
            "--",
            "-lc",
            "printf '%s|%s|%s' \"$OCM_ACTIVE_ENV\" \"$OPENCLAW_HOME\" \"${OPENCLAW_PROFILE:-unset}\"",
        ],
    );
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let test_root = clean_path(&root.child("ocm-home/envs/test"));
    assert_eq!(
        stdout(&run_output),
        format!("test|{}|unset", test_root.display())
    );
}

#[test]
fn env_use_auto_assigns_distinct_gateway_ports_for_fresh_envs() {
    let root = TestDir::new("behavior-env-use-auto-port");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create_demo = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create_demo.status.success(), "{}", stderr(&create_demo));

    let create_test = run_ocm(&cwd, &env, &["env", "create", "test"]);
    assert!(create_test.status.success(), "{}", stderr(&create_test));

    let demo_use = run_ocm(&cwd, &env, &["env", "use", "demo"]);
    assert!(demo_use.status.success(), "{}", stderr(&demo_use));
    let demo_port = exported_gateway_port(&demo_use);
    let demo_root = root.child("ocm-home/envs/demo");
    write_text(
        &demo_root.join(".openclaw/openclaw.json"),
        &format!("{{\n  \"gateway\": {{\n    \"port\": {demo_port}\n  }}\n}}\n"),
    );

    let test_use = run_ocm(&cwd, &env, &["env", "use", "test"]);
    assert!(test_use.status.success(), "{}", stderr(&test_use));
    let test_port = exported_gateway_port(&test_use);
    assert!(demo_port.abs_diff(test_port) > 110);
}

#[test]
fn env_use_skips_an_integral_decimal_machine_global_openclaw_port_family() {
    let root = TestDir::new("behavior-env-use-skips-global-openclaw");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let global_root = root.child("home/.openclaw");
    fs::create_dir_all(&global_root).unwrap();
    write_text(
        &global_root.join("openclaw.json"),
        "{\n  \"gateway\": {\n    \"port\": 18789.0\n  }\n}\n",
    );

    let create_demo = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create_demo.status.success(), "{}", stderr(&create_demo));

    let demo_use = run_ocm(&cwd, &env, &["env", "use", "demo"]);
    assert!(demo_use.status.success(), "{}", stderr(&demo_use));
    assert!(exported_gateway_port(&demo_use) >= 18_900);
}

#[test]
fn env_exec_skips_gateway_port_claimed_by_an_initialized_environment() {
    let root = TestDir::new("behavior-env-exec-config-port");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let create_demo = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(create_demo.status.success(), "{}", stderr(&create_demo));

    let demo_root = root.child("ocm-home/envs/demo");
    write_text(
        &demo_root.join(".openclaw/openclaw.json"),
        "{\n  \"gateway\": {\n    \"port\": 18789\n  }\n}\n",
    );

    let create_test = run_ocm(&cwd, &env, &["env", "create", "test"]);
    assert!(create_test.status.success(), "{}", stderr(&create_test));

    let exec_output = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "exec",
            "test",
            "--",
            "sh",
            "-lc",
            "printf '%s' \"$OPENCLAW_GATEWAY_PORT\"",
        ],
    );
    assert!(exec_output.status.success(), "{}", stderr(&exec_output));
    let test_port = stdout(&exec_output)
        .parse::<u32>()
        .expect("gateway port must be numeric");
    assert!(test_port >= 18_900);
    assert!(test_port > 18_789 + 110);
}
