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
