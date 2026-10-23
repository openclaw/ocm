mod support;

use std::fs;

use crate::support::{TestDir, ocm_env, run_ocm, stderr, stdout};

#[test]
fn init_zsh_prints_an_ocm_use_helper() {
    let root = TestDir::new("shell-init-zsh");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let init = run_ocm(&cwd, &env, &["init", "zsh"]);
    assert!(init.status.success(), "{}", stderr(&init));
    let output = stdout(&init);
    assert!(output.contains("ocm_use() {"));
    assert!(output.contains("--shell zsh"));
    assert!(output.contains("'ocm' env use \"$@\""));
    assert!(output.contains("eval \"$script\""));
}

#[test]
fn init_bash_and_sh_print_shell_specific_posix_helpers() {
    let root = TestDir::new("shell-init-posix");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    for shell in ["bash", "sh"] {
        let init = run_ocm(&cwd, &env, &["init", shell]);
        assert!(init.status.success(), "{}: {}", shell, stderr(&init));
        let output = stdout(&init);
        assert!(output.contains("ocm_use() {"), "{shell}");
        assert!(output.contains(&format!("--shell {shell}")), "{shell}");
        assert!(output.contains("'ocm' env use \"$@\""), "{shell}");
        assert!(output.contains("eval \"$script\""), "{shell}");
    }
}

#[test]
fn init_fish_prints_a_fish_specific_helper() {
    let root = TestDir::new("shell-init-fish");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let init = run_ocm(&cwd, &env, &["init", "fish"]);
    assert!(init.status.success(), "{}", stderr(&init));
    let output = stdout(&init);
    assert!(output.contains("function ocm_use"));
    assert!(output.contains("set -l shell_args --shell fish"));
    assert!(output.contains("'ocm' env use $argv $shell_args"));
    assert!(output.contains("eval $script"));
}

#[test]
fn init_defaults_to_zsh_without_a_shell_hint() {
    let root = TestDir::new("shell-init-default-zsh");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let init = run_ocm(&cwd, &env, &["init"]);
    assert!(init.status.success(), "{}", stderr(&init));
    let output = stdout(&init);
    assert!(output.contains("ocm_use() {"));
    assert!(output.contains("--shell zsh"));
    assert!(output.contains("'ocm' env use \"$@\""));
}

#[test]
fn init_defaults_to_fish_when_shell_env_points_to_fish() {
    let root = TestDir::new("shell-init-default-fish");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert("SHELL".to_string(), "/opt/homebrew/bin/fish".to_string());

    let init = run_ocm(&cwd, &env, &["init"]);
    assert!(init.status.success(), "{}", stderr(&init));
    let output = stdout(&init);
    assert!(output.contains("function ocm_use"));
    assert!(output.contains("set -l shell_args --shell fish"));
    assert!(output.contains("'ocm' env use $argv $shell_args"));
}

#[cfg(unix)]
#[test]
fn posix_helpers_keep_the_selected_shell_and_preserve_the_caller() {
    use std::process::Command;

    use crate::support::ocm_test_binary_path;

    let root = TestDir::new("shell-init-executed");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let binary = ocm_test_binary_path();
    let alias = root.child("ocm = selected's binary");
    std::os::unix::fs::symlink(&binary, &alias).unwrap();
    env.insert("OCM_SELF".to_string(), alias.to_string_lossy().into_owned());
    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(env.get("PATH").unwrap()));
    env.insert(
        "PATH".to_string(),
        std::env::join_paths(paths)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let created = run_ocm(&cwd, &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    for shell in ["bash", "sh"] {
        for hint in ["/bin/bash", "/usr/bin/fish"] {
            env.insert("SHELL".to_string(), hint.to_string());
            let init = run_ocm(&cwd, &env, &["init", shell]);
            assert!(init.status.success(), "{}", stderr(&init));
            let script = format!(
                r#"{}
set -- 'caller one' 'caller two'
before_options=$-
before_shell=$SHELL
readonly SHELL
ocm_use demo || exit 10
[ "$OCM_ACTIVE_ENV" = demo ] || exit 11
[ -n "$OPENCLAW_HOME" ] || exit 12
[ "$-" = "$before_options" ] || exit 13
[ "$SHELL" = "$before_shell" ] || exit 14
[ "$#" = 2 ] && [ "$1" = 'caller one' ] && [ "$2" = 'caller two' ] || exit 15
saved_home=$OPENCLAW_HOME
ocm_use missing
result=$?
[ "$result" = 1 ] || exit 16
[ "$OCM_ACTIVE_ENV" = demo ] && [ "$OPENCLAW_HOME" = "$saved_home" ] || exit 17
[ "$-" = "$before_options" ] && [ "$SHELL" = "$before_shell" ] || exit 18
[ "$#" = 2 ] && [ "$1" = 'caller one' ] && [ "$2" = 'caller two' ] || exit 19
ocm_use demo --shell bash || exit 20
ocm_use demo --shell=sh || exit 21
ocm_use demo --shell
result=$?
[ "$result" = 1 ] || exit 22
ocm_use demo --shell bash --shell sh
result=$?
[ "$result" = 1 ] || exit 26
[ "$OCM_ACTIVE_ENV" = demo ] && [ "$OPENCLAW_HOME" = "$saved_home" ] || exit 23
[ "$-" = "$before_options" ] && [ "$SHELL" = "$before_shell" ] || exit 24
[ "$#" = 2 ] && [ "$1" = 'caller one' ] && [ "$2" = 'caller two' ] || exit 25
printf 'activation and caller controls passed\n'
"#,
                stdout(&init)
            );
            let output = Command::new(shell)
                .args(["-c", &script])
                .current_dir(&cwd)
                .env_clear()
                .envs(&env)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "shell={shell}, hint={hint}, status={}: {} {}",
                output.status,
                stdout(&output),
                stderr(&output)
            );
            assert!(stdout(&output).contains("activation and caller controls passed"));
        }
    }
}

#[test]
fn activation_renders_explicit_include_roots_for_each_supported_shell() {
    let root = TestDir::new("shell-include-roots-render");
    let mut env = ocm_env(&root);
    let created = run_ocm(root.path(), &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));

    for roots in [None, Some(""), Some("/shared roots")] {
        env.remove("OPENCLAW_INCLUDE_ROOTS");
        if let Some(roots) = roots {
            env.insert("OPENCLAW_INCLUDE_ROOTS".into(), roots.into());
        }
        for shell in ["bash", "sh", "zsh", "fish"] {
            let output = run_ocm(root.path(), &env, &["env", "use", "demo", "--shell", shell]);
            assert!(output.status.success(), "{}", stderr(&output));
            let script = stdout(&output);
            match roots {
                None => assert!(!script.contains("OPENCLAW_INCLUDE_ROOTS"), "{script}"),
                Some(value) => {
                    let assignment = if shell == "fish" {
                        format!("set -gx OPENCLAW_INCLUDE_ROOTS '{value}';")
                    } else {
                        format!("export OPENCLAW_INCLUDE_ROOTS='{value}'")
                    };
                    assert!(script.contains(&assignment), "{shell}: {script}");
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn activation_and_init_preserve_authorized_include_access() {
    use std::process::Command;

    use crate::support::{ocm_test_binary_path, path_string};

    let root = TestDir::new("shell-include-roots-executed");
    let shared = root.child("shared one's\\config;$HOME");
    fs::create_dir_all(&shared).unwrap();
    let mut env = ocm_env(&root);
    let binary = ocm_test_binary_path();
    env.insert("OCM_SELF".into(), path_string(&binary));
    env.insert("PROBE_OCM".into(), path_string(&binary));
    env.insert("EXPECTED_ROOTS".into(), path_string(&shared));
    let added = run_ocm(
        root.path(),
        &env,
        &["launcher", "add", "fixture", "--command", "/bin/true"],
    );
    assert!(added.status.success(), "{}", stderr(&added));
    let created = run_ocm(
        root.path(),
        &env,
        &["env", "create", "demo", "--launcher", "fixture"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let include = shared.join("settings.json");
    fs::write(
        &include,
        r#"{"gateway":{"mode":"local","bind":"loopback"}}"#,
    )
    .unwrap();
    let config_path = root.child("ocm-home/envs/demo/.openclaw/openclaw.json");
    let config = serde_json::json!({
        "$include": path_string(&include),
        "env": {"OPENCLAW_INCLUDE_ROOTS": path_string(&shared)}
    })
    .to_string();
    fs::write(&config_path, &config).unwrap();
    env.insert("OPENCLAW_INCLUDE_ROOTS".into(), path_string(&shared));
    let prepared = run_ocm(
        root.path(),
        &env,
        &["start", "demo", "--no-service", "--json"],
    );
    assert!(prepared.status.success(), "{}", stderr(&prepared));
    let config = fs::read_to_string(&config_path).unwrap();
    for shell in ["bash", "sh"] {
        for use_init in [false, true] {
            for authorized in [false, true] {
                env.remove("OPENCLAW_INCLUDE_ROOTS");
                if authorized {
                    env.insert("OPENCLAW_INCLUDE_ROOTS".into(), path_string(&shared));
                }
                env.insert("OPENCLAW_RANDOM_USER_VALUE".into(), "stale".into());
                env.insert("OPENCLAW_SYSTEMD_UNIT".into(), "stale.service".into());
                env.insert("OCM_ACTIVE_ENV".into(), "old".into());
                let activation = if use_init {
                    format!("eval \"$(\"$PROBE_OCM\" init {shell})\"; ocm_use demo")
                } else {
                    format!("eval \"$(\"$PROBE_OCM\" env use demo --shell {shell})\"")
                };
                let roots_check = if authorized {
                    "[ \"$OPENCLAW_INCLUDE_ROOTS\" = \"$EXPECTED_ROOTS\" ]"
                } else {
                    "[ -z \"${OPENCLAW_INCLUDE_ROOTS+x}\" ]"
                };
                let script = format!(
                    r#"{activation} || exit 10
{roots_check} || exit 11
[ "$OCM_ACTIVE_ENV" = demo ] || exit 12
[ -z "${{OPENCLAW_RANDOM_USER_VALUE+x}}" ] || exit 13
[ -z "${{OPENCLAW_SYSTEMD_UNIT+x}}" ] || exit 14
"$PROBE_OCM" start demo --no-service --json
"#
                );
                let output = Command::new(shell)
                    .args(["-c", &script])
                    .current_dir(root.path())
                    .env_clear()
                    .envs(&env)
                    .output()
                    .unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(if authorized { 0 } else { 1 }),
                    "shell={shell}, init={use_init}, authorized={authorized}: {} {}",
                    stdout(&output),
                    stderr(&output)
                );
                if !authorized {
                    assert!(stderr(&output).contains("outside the admitted configuration roots"));
                }
                assert_eq!(fs::read_to_string(&config_path).unwrap(), config);
            }
        }
    }
}
