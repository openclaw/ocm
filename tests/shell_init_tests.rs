mod support;

use std::fs;

use ocm::store::get_environment;

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
    assert!(output.contains("eval \"$("));
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
        assert!(output.contains("eval \"$("), "{shell}");
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
fn fish_helper_returns_the_activation_status() {
    use std::process::Command;

    use crate::support::ocm_test_binary_path;

    if Command::new("fish").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("CI").is_none(),
            "fish must be installed when CI is set"
        );
        eprintln!("skipping: fish is not installed");
        return;
    }

    let root = TestDir::new("shell-init-fish-executed");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let binary = ocm_test_binary_path();
    env.insert(
        "OCM_SELF".to_string(),
        binary.to_string_lossy().into_owned(),
    );
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
    let init = run_ocm(&cwd, &env, &["init", "fish"]);
    assert!(init.status.success(), "{}", stderr(&init));
    let helper = stdout(&init);

    let controls = [
        "OPENCLAW_PROFILE",
        "OPENCLAW_GATEWAY_PORT",
        "OPENCLAW_DEV_SOURCE_ROOT",
        "OPENCLAW_BUNDLED_PLUGINS_DIR",
        "OPENCLAW_SERVICE_MARKER",
        "OPENCLAW_SERVICE_KIND",
        "OPENCLAW_SUPERVISOR_MODE",
        "OPENCLAW_NO_RESPAWN",
        "OPENCLAW_LAUNCHD_LABEL",
        "OPENCLAW_SYSTEMD_UNIT",
        "OPENCLAW_WINDOWS_TASK_NAME",
    ];
    let preset = controls
        .iter()
        .map(|key| format!("set -gx {key} stale"))
        .collect::<Vec<_>>()
        .join("; ");
    let erased = controls
        .iter()
        .map(|key| format!("test \"${key}\" != stale; or exit 15"))
        .collect::<Vec<_>>()
        .join("\n");
    for (label, prelude) in [("fresh", ""), ("controls already set", preset.as_str())] {
        let script = format!(
            r#"{prelude}
{helper}
ocm_use demo
set result $status
test $result -eq 0; or begin; echo "{label}: status $result"; exit 10; end
test "$OCM_ACTIVE_ENV" = demo; or exit 11
{erased}
ocm_use demo; and echo chained
ocm env use demo --shell fish | source
test $status -eq 0; or exit 12
ocm_use missing 2>/dev/null
test $status -eq 1; or exit 13
test "$OCM_ACTIVE_ENV" = demo; or exit 14
"#
        );
        let output = Command::new("fish")
            .args(["--no-config", "-c", &script])
            .current_dir(&cwd)
            .env_clear()
            .envs(&env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{label}: status={}: {} {}",
            output.status,
            stdout(&output),
            stderr(&output)
        );
        assert!(stdout(&output).contains("chained"), "{label}");
    }
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

#[cfg(unix)]
#[test]
fn posix_helper_leaves_caller_variables_unchanged() {
    use std::process::Command;

    use crate::support::ocm_test_binary_path;

    let root = TestDir::new("shell-init-caller-variables");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let binary = ocm_test_binary_path();
    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(env.get("PATH").unwrap()));
    env.insert(
        "PATH".to_string(),
        std::env::join_paths(paths)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    for name in ["demo", "other"] {
        let created = run_ocm(&cwd, &env, &["env", "create", name]);
        assert!(created.status.success(), "{}", stderr(&created));
    }

    for (shell, init_shell) in [
        ("bash", "bash"),
        ("sh", "sh"),
        ("zsh", "zsh"),
        ("dash", "sh"),
    ] {
        if !matches!(shell, "bash" | "sh")
            && Command::new(shell).arg("-c").arg(":").output().is_err()
        {
            continue;
        }
        let init = run_ocm(&cwd, &env, &["init", init_shell]);
        assert!(init.status.success(), "{}", stderr(&init));
        let script = format!(
            r#"{}
script=mine
_ocm_script=mine_too
_ocm_arg=mine_three
other=keep
ocm_use demo || exit 10
[ "$OCM_ACTIVE_ENV" = demo ] || exit 11
[ "$script" = mine ] && [ "$_ocm_script" = mine_too ] && [ "$_ocm_arg" = mine_three ] || exit 12
[ "$other" = keep ] || exit 13
ocm_use missing 2>/dev/null
result=$?
[ "$result" = 1 ] || exit 14
[ "$script" = mine ] && [ "$_ocm_script" = mine_too ] && [ "$_ocm_arg" = mine_three ] || exit 15
ocm_use demo --shell=sh || exit 16
[ "$script" = mine ] || exit 17
unset script _ocm_script _ocm_arg
ocm_use demo || exit 18
[ -z "${{script+x}}" ] && [ -z "${{_ocm_script+x}}" ] && [ -z "${{_ocm_arg+x}}" ] || exit 19
readonly _ocm_script=ro script=ro2
ocm_use other || exit 20
[ "$OCM_ACTIVE_ENV" = other ] || exit 21
ocm_use missing 2>/dev/null
result=$?
[ "$result" = 1 ] || exit 22
[ "$script" = ro2 ] && [ "$_ocm_script" = ro ] || exit 23
ocm_use demo --shell=sh || exit 24
[ "$OCM_ACTIVE_ENV" = demo ] || exit 25
printf 'caller variables preserved\n'
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
            "shell={shell}, status={}: {} {}",
            output.status,
            stdout(&output),
            stderr(&output)
        );
        assert!(stdout(&output).contains("caller variables preserved"));
    }
}

#[cfg(unix)]
#[test]
fn posix_helper_discards_the_output_of_a_failed_activation() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let root = TestDir::new("shell-init-failed-activation");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let fake = root.child("failing-ocm");
    fs::write(
        &fake,
        "#!/bin/sh\necho 'FAILED_STDOUT_RAN=1'\necho 'activation failed' >&2\nexit 9\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut env = ocm_env(&root);
    env.insert("OCM_SELF".to_string(), fake.to_string_lossy().into_owned());

    for (shell, init_shell) in [
        ("bash", "bash"),
        ("sh", "sh"),
        ("zsh", "zsh"),
        ("dash", "sh"),
    ] {
        if !matches!(shell, "bash" | "sh")
            && Command::new(shell).arg("-c").arg(":").output().is_err()
        {
            continue;
        }
        let init = run_ocm(&cwd, &env, &["init", init_shell]);
        assert!(init.status.success(), "{}", stderr(&init));
        let script = format!(
            r#"{}
ocm_use demo
result=$?
[ "$result" = 9 ] || exit 10
[ -z "${{FAILED_STDOUT_RAN+x}}" ] || exit 11
ocm_use demo --shell=sh
result=$?
[ "$result" = 9 ] || exit 12
[ -z "${{FAILED_STDOUT_RAN+x}}" ] || exit 13
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
            "shell={shell}, status={}: {} {}",
            output.status,
            stdout(&output),
            stderr(&output)
        );
        assert!(stderr(&output).contains("activation failed"), "{shell}");
    }
}

#[cfg(unix)]
#[test]
fn plain_activation_follows_the_evaluating_shell_instead_of_the_shell_hint() {
    use std::io::ErrorKind;
    use std::process::Command;

    use crate::support::ocm_test_binary_path;

    let root = TestDir::new("shell-activation-evaluator");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let binary = ocm_test_binary_path();
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

    // Without a shell parent, the SHELL hint still selects the dialect.
    env.insert("SHELL".to_string(), "/opt/homebrew/bin/fish".to_string());
    let hinted = run_ocm(&cwd, &env, &["env", "use", "demo"]);
    assert!(hinted.status.success(), "{}", stderr(&hinted));
    assert!(stdout(&hinted).contains("set -gx OCM_ACTIVE_ENV 'demo';"));

    let posix = r#"set -- 'caller one' 'caller two'
before_options=$-
eval "$(ocm env use demo)" || exit 10
[ "$OCM_ACTIVE_ENV" = demo ] || exit 11
[ -n "$OPENCLAW_HOME" ] || exit 12
[ -z "${OPENCLAW_PROFILE+x}" ] || exit 13
[ "$-" = "$before_options" ] || exit 14
[ "$#" = 2 ] && [ "$1" = 'caller one' ] && [ "$2" = 'caller two' ] || exit 15
case "$(ocm env use demo --shell fish)" in *"set -gx OCM_ACTIVE_ENV 'demo';"*) ;; *) exit 16 ;; esac
printf 'activated\n'
"#;
    let fish = r#"ocm env use demo | source
test "$OCM_ACTIVE_ENV" = demo; or exit 11
test -n "$OPENCLAW_HOME"; or exit 12
set -q OPENCLAW_PROFILE; and exit 13
string match -q 'unset *' -- (ocm env use demo --shell zsh)[1]; or exit 16
printf 'activated\n'
"#;
    let mut evaluated = Vec::new();
    for (shell, script) in [
        ("bash", posix),
        ("sh", posix),
        ("zsh", posix),
        ("fish", fish),
    ] {
        let mismatched = if shell == "fish" {
            "/bin/zsh"
        } else {
            "/usr/bin/fish"
        };
        for hint in [mismatched.to_string(), format!("/bin/{shell}")] {
            env.insert("SHELL".to_string(), hint.clone());
            env.insert("OPENCLAW_PROFILE".to_string(), "stale".to_string());
            let mut command = Command::new(shell);
            match shell {
                "zsh" => command.arg("-f"),
                "fish" => command.arg("--no-config"),
                _ => &mut command,
            };
            let output = match command
                .args(["-c", script])
                .current_dir(&cwd)
                .env_clear()
                .envs(&env)
                .output()
            {
                Ok(output) => output,
                // Bash and sh are required; zsh and fish run where installed.
                Err(error)
                    if error.kind() == ErrorKind::NotFound && !matches!(shell, "bash" | "sh") =>
                {
                    break;
                }
                Err(error) => panic!("failed to run {shell}: {error}"),
            };
            assert!(
                output.status.success() && stderr(&output).is_empty(),
                "shell={shell}, hint={hint}, status={}: {} {}",
                output.status,
                stdout(&output),
                stderr(&output)
            );
            assert_eq!(stdout(&output), "activated\n", "shell={shell}, hint={hint}");
            evaluated.push(shell);
        }
    }
    assert!(evaluated.contains(&"bash") && evaluated.contains(&"sh"));
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

#[test]
fn env_use_rejects_unsupported_shells_like_init() {
    let root = TestDir::new("shell-use-unsupported");
    let mut env = ocm_env(&root);
    let created = run_ocm(root.path(), &env, &["env", "create", "demo"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let before = get_environment("demo", &env, root.path()).unwrap();

    for hint in ["/usr/bin/fish", "/bin/zsh"] {
        env.insert("SHELL".to_string(), hint.to_string());
        for shell in ["pwsh", "powershell", "csh", "nu", "bogus", ""] {
            let inline = format!("--shell={shell}");
            for args in [
                vec!["env", "use", "demo", "--shell", shell],
                vec!["env", "use", "demo", inline.as_str()],
            ] {
                let output = run_ocm(root.path(), &env, &args);
                assert_eq!(output.status.code(), Some(1), "{hint} {args:?}");
                assert!(stdout(&output).is_empty(), "{hint} {args:?}");
                assert!(
                    stderr(&output).contains(&format!("ocm: unsupported shell: {shell}\n")),
                    "{hint} {args:?}: {}",
                    stderr(&output)
                );
            }
        }
    }
    assert_eq!(
        get_environment("demo", &env, root.path())
            .unwrap()
            .last_used_at,
        before.last_used_at,
        "a rejected shell must not mark the environment used"
    );

    for shell in ["pwsh", "powershell"] {
        let output = run_ocm(root.path(), &env, &["init", shell]);
        assert_eq!(output.status.code(), Some(1), "{shell}");
        assert!(
            stderr(&output).contains(&format!("unsupported init shell: {shell}")),
            "{shell}"
        );
    }

    for shell in ["bash", "sh", "zsh", "fish", " BASH "] {
        let output = run_ocm(root.path(), &env, &["env", "use", "demo", "--shell", shell]);
        assert!(output.status.success(), "{shell}: {}", stderr(&output));
        assert!(stdout(&output).contains("OCM_ACTIVE_ENV"), "{shell}");
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
