mod support;

use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ocm::store::{now_utc, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::Value;

use crate::support::{
    TestDir, install_fake_service_manager, ocm_env, path_string, run_ocm, run_ocm_with_stdin,
    stderr, stdout, write_executable_script,
};

fn init_openclaw_repo(root: &TestDir) -> PathBuf {
    let repo = root.child("repo/openclaw");
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::create_dir_all(repo.join("extensions/codex")).unwrap();
    fs::write(
        repo.join("package.json"),
        r#"{"name":"openclaw","version":"2026.4.19"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("extensions/codex/openclaw.plugin.json"),
        r#"{"id":"codex"}"#,
    )
    .unwrap();
    fs::write(repo.join("scripts/run-node.mjs"), "console.log('run');\n").unwrap();
    fs::write(repo.join("openclaw.mjs"), "console.log('openclaw');\n").unwrap();
    fs::write(
        repo.join("scripts/watch-node.mjs"),
        "console.log('watch');\n",
    )
    .unwrap();
    fs::write(repo.join(".gitignore"), ".env\nnode_modules/\n").unwrap();

    let init = Command::new("git").arg("init").arg(&repo).output().unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let email = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "config",
            "user.email",
            "tests@example.com",
        ])
        .output()
        .unwrap();
    assert!(
        email.status.success(),
        "{}",
        String::from_utf8_lossy(&email.stderr)
    );
    let name = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "config",
            "user.name",
            "OCM Tests",
        ])
        .output()
        .unwrap();
    assert!(
        name.status.success(),
        "{}",
        String::from_utf8_lossy(&name.stderr)
    );
    let add = Command::new("git")
        .args(["-C", &path_string(&repo), "add", "."])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("git")
        .args(["-C", &path_string(&repo), "commit", "-m", "init"])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );

    repo
}

fn init_nested_openclaw_repo(path: &Path) {
    fs::create_dir_all(path.join("scripts")).unwrap();
    fs::write(
        path.join("package.json"),
        r#"{"name":"openclaw","version":"2026.4.19"}"#,
    )
    .unwrap();
    fs::write(path.join("scripts/run-node.mjs"), "console.log('run');\n").unwrap();
    fs::write(path.join("SENTINEL"), "preserve me\n").unwrap();
    let init = Command::new("git").arg("init").arg(path).output().unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
}

fn commit_nested_openclaw_repo(path: &Path) {
    for (key, value) in [
        ("user.email", "tests@example.com"),
        ("user.name", "OCM Tests"),
    ] {
        let configure = Command::new("git")
            .args(["-C", &path_string(path), "config", key, value])
            .output()
            .unwrap();
        assert!(
            configure.status.success(),
            "{}",
            String::from_utf8_lossy(&configure.stderr)
        );
    }
    let add = Command::new("git")
        .args(["-C", &path_string(path), "add", "."])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("git")
        .args(["-C", &path_string(path), "commit", "-m", "init"])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
}

fn add_test_submodule(root: &TestDir, repo: &Path) {
    let submodule = root.child("repo/submodule");
    fs::create_dir_all(&submodule).unwrap();
    let init = Command::new("git")
        .arg("init")
        .arg(&submodule)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    for (key, value) in [
        ("user.email", "tests@example.com"),
        ("user.name", "OCM Tests"),
    ] {
        let configure = Command::new("git")
            .args(["-C", &path_string(&submodule), "config", key, value])
            .output()
            .unwrap();
        assert!(
            configure.status.success(),
            "{}",
            String::from_utf8_lossy(&configure.stderr)
        );
    }
    fs::write(submodule.join("content.txt"), "submodule\n").unwrap();
    fs::write(submodule.join(".gitignore"), ".env\nnode_modules/\n").unwrap();
    let add = Command::new("git")
        .args(["-C", &path_string(&submodule), "add", "."])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("git")
        .args(["-C", &path_string(&submodule), "commit", "-m", "init"])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let add = Command::new("git")
        .args(["-c", "protocol.file.allow=always", "-C", &path_string(repo)])
        .args(["submodule", "add"])
        .arg(&submodule)
        .arg("vendor/submodule")
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("git")
        .args(["-C", &path_string(repo), "commit", "-am", "add submodule"])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
}

fn init_test_submodule(worktree_root: &Path) {
    let init = Command::new("git")
        .args(["-c", "protocol.file.allow=always", "-C"])
        .arg(worktree_root)
        .args(["submodule", "update", "--init"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
}

fn git_worktree_paths(repo: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .args([
            "-C",
            &path_string(repo),
            "worktree",
            "list",
            "--porcelain",
            "-z",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect()
}

fn prepend_fake_bin(env: &mut std::collections::BTreeMap<String, String>, bin_dir: &Path) {
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let combined_path = if existing_path.is_empty() {
        path_string(bin_dir)
    } else {
        format!("{}:{existing_path}", path_string(bin_dir))
    };
    env.insert("PATH".to_string(), combined_path);
}

fn install_fake_dev_runners(root: &TestDir, env: &mut std::collections::BTreeMap<String, String>) {
    let bin_dir = root.child("fake-dev-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let pnpm_log = root.child("pnpm.log");
    let node_log = root.child("node.log");
    let pnpm = format!(
        "#!/bin/sh\nprintf '%s|%s|%s|%s|bundled=%s|devroot=%s\\n' \"$PWD\" \"$OPENCLAW_CONFIG_PATH\" \"$OPENCLAW_GATEWAY_PORT\" \"$*\" \"$OPENCLAW_BUNDLED_PLUGINS_DIR\" \"$OPENCLAW_DEV_SOURCE_ROOT\" >> \"{}\"\n",
        path_string(&pnpm_log)
    );
    let node = format!(
        "#!/bin/sh\nprintf '%s|%s|%s|%s|bundled=%s|devroot=%s\\n' \"$PWD\" \"$OPENCLAW_CONFIG_PATH\" \"$OPENCLAW_GATEWAY_PORT\" \"$*\" \"$OPENCLAW_BUNDLED_PLUGINS_DIR\" \"$OPENCLAW_DEV_SOURCE_ROOT\" >> \"{}\"\nif [ -n \"$OCM_TEST_NODE_STDOUT\" ]; then printf '%s\\n' \"$OCM_TEST_NODE_STDOUT\"; fi\nif [ -n \"$OCM_TEST_NODE_STDERR\" ]; then printf '%s\\n' \"$OCM_TEST_NODE_STDERR\" >&2; fi\n",
        path_string(&node_log)
    );
    write_executable_script(&bin_dir.join("pnpm"), &pnpm);
    write_executable_script(&bin_dir.join("node"), &node);
    prepend_fake_bin(env, &bin_dir);
}

fn source_watch_override_path(root: &TestDir, name: &str) -> PathBuf {
    root.child(format!("ocm-home/source-watch/{name}.json"))
}

fn source_watch_lock_path(root: &TestDir, name: &str) -> PathBuf {
    root.child(format!("ocm-home/source-watch/{name}.lock"))
}

#[cfg(unix)]
fn install_blocking_fake_dev_runners(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) -> (PathBuf, PathBuf, PathBuf) {
    install_fake_dev_runners(root, env);
    let started = root.child("source-watch.started");
    let release = root.child("source-watch.release");
    let log = root.child("source-watch.log");
    let node = format!(
        "#!/bin/sh\nprintf 'started\\n' >> \"{}\"\nprintf 'ready\\n' > \"{}\"\nwhile [ ! -f \"{}\" ]; do /bin/sleep 0.05; done\n",
        path_string(&log),
        path_string(&started),
        path_string(&release),
    );
    write_executable_script(&root.child("fake-dev-bin/node"), &node);
    (started, release, log)
}

#[cfg(unix)]
fn install_failing_fake_dev_runners(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) -> PathBuf {
    install_fake_dev_runners(root, env);
    let started = root.child("source-watch.started");
    let node = format!(
        "#!/bin/sh\nprintf 'ready\\n' > \"{}\"\nexit 23\n",
        path_string(&started),
    );
    write_executable_script(&root.child("fake-dev-bin/node"), &node);
    started
}

#[cfg(unix)]
fn install_stubborn_fake_dev_runners(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) -> (PathBuf, PathBuf) {
    install_fake_dev_runners(root, env);
    let started = root.child("source-watch.started");
    let descendant_pid = root.child("source-watch-descendant.pid");
    let node = format!(
        "#!/bin/sh\ntrap '' TERM\n/bin/sleep 300 &\nprintf '%s\\n' \"$!\" > \"{}\"\nprintf 'ready\\n' > \"{}\"\nwhile :; do /bin/sleep 1; done\n",
        path_string(&descendant_pid),
        path_string(&started),
    );
    write_executable_script(&root.child("fake-dev-bin/node"), &node);
    (started, descendant_pid)
}

#[cfg(unix)]
fn install_orphaning_fake_dev_runners(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    install_fake_dev_runners(root, env);
    let started = root.child("source-watch.started");
    let descendant_pid = root.child("source-watch-descendant.pid");
    let stdin_kind = root.child("source-watch.stdin-kind");
    let node_args = root.child("source-watch.node-args");
    let node = format!(
        "#!/bin/sh\nif [ -t 0 ]; then printf 'tty\\n'; else printf 'pipe\\n'; fi > \"{}\"\nprintf '%s\\n' \"$*\" > \"{}\"\n/bin/sleep 300 &\nprintf '%s\\n' \"$!\" > \"{}\"\nprintf 'ready\\n' > \"{}\"\nexit 23\n",
        path_string(&stdin_kind),
        path_string(&node_args),
        path_string(&descendant_pid),
        path_string(&started),
    );
    write_executable_script(&root.child("fake-dev-bin/node"), &node);
    (started, descendant_pid, stdin_kind, node_args)
}

#[cfg(unix)]
fn install_interactive_fake_dev_runners(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) -> (PathBuf, PathBuf) {
    install_fake_dev_runners(root, env);
    let started = root.child("source-watch.started");
    let received = root.child("source-watch.received");
    let node = format!(
        "#!/bin/sh\nprintf 'ready\\n' > \"{}\"\nIFS= read -r line\nprintf '%s\\n' \"$line\" > \"{}\"\n",
        path_string(&started),
        path_string(&received),
    );
    write_executable_script(&root.child("fake-dev-bin/node"), &node);
    (started, received)
}

#[cfg(unix)]
fn spawn_ocm_with_controlling_pty(
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    args: &[&str],
) -> (std::process::Child, File) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let opened = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        opened,
        0,
        "failed opening test PTY: {}",
        std::io::Error::last_os_error()
    );
    let master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };

    let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    command
        .current_dir(cwd)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::from(slave))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    (command.spawn().unwrap(), master)
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_is_alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    !process_is_alive(pid)
}

#[cfg(unix)]
fn wait_for_process_stop(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut status = 0;
        let waited = unsafe {
            libc::waitpid(
                pid as libc::pid_t,
                &mut status,
                libc::WNOHANG | libc::WUNTRACED,
            )
        };
        if waited == pid as libc::pid_t && libc::WIFSTOPPED(status) {
            return true;
        }
        assert!(
            waited >= 0,
            "failed waiting for process stop: {}",
            std::io::Error::last_os_error()
        );
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn service_env(root: &TestDir) -> std::collections::BTreeMap<String, String> {
    let mut env = ocm_env(root);
    install_fake_service_manager(root, &mut env);
    env
}

#[test]
fn dev_command_provisions_worktree_bootstraps_config_and_runs_gateway() {
    let root = TestDir::new("dev-command-run");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    let config_path = PathBuf::from(show_json["configPath"].as_str().unwrap());
    let workspace_dir = PathBuf::from(show_json["workspaceDir"].as_str().unwrap());

    assert_eq!(show_json["devRepoRoot"], path_string(&repo));
    assert!(worktree_root.starts_with(repo.join(".worktrees")));
    assert!(worktree_root.join(".git").exists());

    let config: Value = serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(config["gateway"]["mode"], "local");
    assert_eq!(config["gateway"]["bind"], "loopback");
    assert_eq!(
        config["agents"]["defaults"]["workspace"],
        path_string(&workspace_dir)
    );
    assert!(config["agents"]["defaults"].get("skipBootstrap").is_none());
    assert!(config["agents"].get("entries").is_none());
    assert!(config["agents"].get("list").is_none());
    assert!(workspace_dir.exists());

    let pnpm_log = fs::read_to_string(root.child("pnpm.log")).unwrap();
    assert!(pnpm_log.contains("|install"));
    assert!(pnpm_log.contains("openclaw gateway run --port"));
    assert!(pnpm_log.contains(&path_string(&worktree_root)));
    assert!(pnpm_log.contains(&path_string(&config_path)));
    assert!(pnpm_log.contains(&format!(
        "|bundled={}",
        path_string(&worktree_root.join("extensions"))
    )));
    assert!(pnpm_log.contains(&format!("|devroot={}", path_string(&worktree_root))));
}

#[test]
fn dev_command_rejects_an_unregistered_clone_at_the_managed_path() {
    let root = TestDir::new("dev-command-unregistered-clone");
    let repo = init_openclaw_repo(&root);
    let worktree_root = repo.join(".worktrees/demo");
    init_nested_openclaw_repo(&worktree_root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(!run.status.success());
    assert!(
        stderr(&run).contains("is not registered to this OpenClaw checkout"),
        "{}",
        stderr(&run)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[test]
fn dev_command_recreates_an_exactly_registered_missing_worktree() {
    let root = TestDir::new("dev-command-missing-worktree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let first = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(first.status.success(), "{}", stderr(&first));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::remove_dir_all(&worktree_root).unwrap();

    let second = run_ocm(&cwd, &env, &["dev", "demo"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(worktree_root.join(".git").exists());
    let canonical_worktree_root = fs::canonicalize(&worktree_root).unwrap();
    assert_eq!(
        git_worktree_paths(&repo)
            .into_iter()
            .filter(|path| fs::canonicalize(path).ok().as_ref() == Some(&canonical_worktree_root))
            .count(),
        1
    );
}

#[test]
fn dev_command_rejects_a_stale_registration_replaced_by_an_unrelated_clone() {
    let root = TestDir::new("dev-command-stale-replacement");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let first = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(first.status.success(), "{}", stderr(&first));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::remove_dir_all(&worktree_root).unwrap();
    init_nested_openclaw_repo(&worktree_root);

    let second = run_ocm(&cwd, &env, &["dev", "demo"]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("registered worktree is not a valid OpenClaw checkout"),
        "{}",
        stderr(&second)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[cfg(unix)]
#[test]
fn dev_command_rejects_a_symlink_alias_to_another_registered_worktree() {
    let root = TestDir::new("dev-command-symlink-alias");
    let repo = init_openclaw_repo(&root);
    let other_worktree = repo.join(".worktrees/other");
    let add = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "worktree",
            "add",
            "--detach",
            &path_string(&other_worktree),
        ])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    fs::write(other_worktree.join("SENTINEL"), "preserve me\n").unwrap();
    std::os::unix::fs::symlink("other", repo.join(".worktrees/demo")).unwrap();
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(!run.status.success());
    assert!(
        stderr(&run).contains("is not registered to this OpenClaw checkout"),
        "{}",
        stderr(&run)
    );
    assert_eq!(
        fs::read_to_string(other_worktree.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[test]
fn env_remove_preserves_an_untracked_dev_worktree() {
    let root = TestDir::new("dev-command-dirty-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::write(worktree_root.join("SENTINEL"), "preserve me\n").unwrap();
    let hide_untracked = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "config",
            "status.showUntrackedFiles",
            "no",
        ])
        .output()
        .unwrap();
    assert!(
        hide_untracked.status.success(),
        "{}",
        String::from_utf8_lossy(&hide_untracked.stderr)
    );

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo", "--force"]);
    assert!(!remove.status.success());
    assert!(stderr(&remove).contains("contains modified or untracked files"));
    assert_eq!(
        fs::read_to_string(worktree_root.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
    let still_registered = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(
        still_registered.status.success(),
        "{}",
        stderr(&still_registered)
    );
}

#[test]
fn env_remove_preserves_ignored_local_files() {
    let root = TestDir::new("dev-command-ignored-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::write(worktree_root.join(".env"), "TOKEN=preserve-me\n").unwrap();

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo", "--force"]);
    assert!(!remove.status.success());
    assert!(stderr(&remove).contains("contains ignored local files"));
    assert_eq!(
        fs::read_to_string(worktree_root.join(".env")).unwrap(),
        "TOKEN=preserve-me\n"
    );
}

#[test]
fn env_remove_discards_installed_node_modules() {
    let root = TestDir::new("dev-command-node-modules-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::create_dir_all(worktree_root.join("node_modules/pkg")).unwrap();
    fs::write(worktree_root.join("node_modules/pkg/package.json"), "{}\n").unwrap();

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(!worktree_root.exists());
}

#[test]
fn dev_command_rejects_another_worktrees_git_backlink() {
    let root = TestDir::new("dev-command-wrong-backlink");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let first = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(first.status.success(), "{}", stderr(&first));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    let other_worktree = repo.join(".worktrees/other");
    let add = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "worktree",
            "add",
            "--detach",
            &path_string(&other_worktree),
        ])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );

    fs::remove_dir_all(&worktree_root).unwrap();
    fs::create_dir_all(worktree_root.join("scripts")).unwrap();
    fs::write(
        worktree_root.join("package.json"),
        r#"{"name":"openclaw","version":"2026.4.19"}"#,
    )
    .unwrap();
    fs::write(
        worktree_root.join("scripts/run-node.mjs"),
        "console.log('run');\n",
    )
    .unwrap();
    fs::copy(other_worktree.join(".git"), worktree_root.join(".git")).unwrap();
    fs::write(worktree_root.join("SENTINEL"), "preserve me\n").unwrap();

    let second = run_ocm(&cwd, &env, &["dev", "demo"]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("registered worktree is not a valid OpenClaw checkout"),
        "{}",
        stderr(&second)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[test]
fn env_remove_accepts_a_clean_worktree_with_an_initialized_submodule() {
    let root = TestDir::new("dev-command-clean-submodule");
    let repo = init_openclaw_repo(&root);
    add_test_submodule(&root, &repo);

    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    init_test_submodule(&worktree_root);

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(!worktree_root.exists());
}

#[test]
fn env_remove_accepts_a_missing_worktree_with_a_non_git_repo_path() {
    let root = TestDir::new("dev-command-non-git-repo-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    fs::remove_dir_all(&repo).unwrap();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("SENTINEL"), "preserve me\n").unwrap();

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo"]);
    assert!(!show.status.success());
    assert_eq!(
        fs::read_to_string(repo.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[test]
fn env_remove_accepts_a_clean_registered_worktree_without_openclaw_markers() {
    let root = TestDir::new("dev-command-markerless-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    let remove_markers = Command::new("git")
        .args(["-C", &path_string(&worktree_root), "rm"])
        .args(["package.json", "scripts/run-node.mjs"])
        .output()
        .unwrap();
    assert!(
        remove_markers.status.success(),
        "{}",
        String::from_utf8_lossy(&remove_markers.stderr)
    );
    let commit = Command::new("git")
        .args([
            "-C",
            &path_string(&worktree_root),
            "commit",
            "-m",
            "remove markers",
        ])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(!worktree_root.exists());
}

#[test]
fn env_remove_preserves_ignored_files_inside_initialized_submodules() {
    let root = TestDir::new("dev-command-ignored-submodule");
    let repo = init_openclaw_repo(&root);
    add_test_submodule(&root, &repo);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    init_test_submodule(&worktree_root);
    let ignored_file = worktree_root.join("vendor/submodule/.env");
    fs::write(&ignored_file, "preserve me\n").unwrap();

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo", "--force"]);
    assert!(!remove.status.success());
    assert!(
        stderr(&remove).contains("contains ignored local files"),
        "{}",
        stderr(&remove)
    );
    assert_eq!(fs::read_to_string(ignored_file).unwrap(), "preserve me\n");
}

#[test]
fn dev_command_supports_relative_worktree_links() {
    let root = TestDir::new("dev-command-relative-links");
    let repo = init_openclaw_repo(&root);
    let configure = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "config",
            "worktree.useRelativePaths",
            "true",
        ])
        .output()
        .unwrap();
    assert!(
        configure.status.success(),
        "{}",
        String::from_utf8_lossy(&configure.stderr)
    );
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(remove.status.success(), "{}", stderr(&remove));
    assert!(!repo.join(".worktrees/demo").exists());
}

#[test]
fn env_remove_refuses_an_unrelated_replacement_checkout() {
    let root = TestDir::new("dev-command-replacement-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    let detach = Command::new("git")
        .args([
            "-C",
            &path_string(&repo),
            "worktree",
            "remove",
            &path_string(&worktree_root),
        ])
        .output()
        .unwrap();
    assert!(
        detach.status.success(),
        "{}",
        String::from_utf8_lossy(&detach.stderr)
    );
    init_nested_openclaw_repo(&worktree_root);

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo", "--force"]);
    assert!(!remove.status.success());
    assert!(
        stderr(&remove).contains("refusing to remove worktree path not registered"),
        "{}",
        stderr(&remove)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[test]
fn env_remove_refuses_a_clean_replacement_at_a_stale_registered_path() {
    let root = TestDir::new("dev-command-stale-replacement-remove");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::remove_dir_all(&worktree_root).unwrap();
    init_nested_openclaw_repo(&worktree_root);
    commit_nested_openclaw_repo(&worktree_root);

    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo", "--force"]);
    assert!(!remove.status.success());
    assert!(
        stderr(&remove).contains("checkout identity does not match"),
        "{}",
        stderr(&remove)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[test]
fn dev_command_can_onboard_then_watch() {
    let root = TestDir::new("dev-command-watch");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--onboard",
            "--watch",
        ],
    );
    assert!(run.status.success(), "{}", stderr(&run));

    let pnpm_log = fs::read_to_string(root.child("pnpm.log")).unwrap();
    assert!(pnpm_log.contains("|install"));
    assert!(pnpm_log.contains("openclaw onboard --mode local --no-install-daemon"));

    let node_log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(node_log.contains("scripts/watch-node.mjs"));
    assert!(node_log.contains("gateway run --port"));
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[test]
fn dev_status_reports_dev_envs() {
    let root = TestDir::new("dev-status");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(run.status.success(), "{}", stderr(&run));

    let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let summary: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(summary["envName"], "demo");
    assert_eq!(summary["repoRoot"], path_string(&repo));
    assert!(
        summary["worktreeRoot"]
            .as_str()
            .unwrap()
            .contains("/.worktrees/demo")
    );
    assert!(summary["gatewayPort"].as_u64().unwrap() > 0);
    assert_eq!(
        summary["gatewayUrl"],
        format!(
            "http://127.0.0.1:{}",
            summary["gatewayPort"].as_u64().unwrap()
        )
    );
    assert!(
        summary["statusCommand"]
            .as_str()
            .unwrap()
            .contains("service status demo")
    );
    assert!(
        summary["logsCommand"]
            .as_str()
            .unwrap()
            .contains("logs demo --follow")
    );
}

#[test]
fn dev_command_accepts_a_custom_env_root() {
    let root = TestDir::new("dev-command-custom-root");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let custom_root = cwd.join("env-roots/demo");

    let run = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--root",
            "./env-roots/demo",
        ],
    );
    assert!(run.status.success(), "{}", stderr(&run));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let resolved_root = fs::canonicalize(&custom_root).unwrap();
    assert_eq!(show_json["root"], path_string(&resolved_root));
    assert_eq!(show_json["openclawHome"], path_string(&resolved_root));
    assert_eq!(
        show_json["configPath"],
        path_string(&resolved_root.join(".openclaw/openclaw.json"))
    );
}

#[test]
fn dev_command_allows_reusing_the_same_explicit_port() {
    let root = TestDir::new("dev-command-reuse-same-port");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let first = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--port",
            "21901",
        ],
    );
    assert!(first.status.success(), "{}", stderr(&first));

    let second = run_ocm(&cwd, &env, &["dev", "demo", "--port", "21901"]);
    assert!(second.status.success(), "{}", stderr(&second));

    let changed = run_ocm(&cwd, &env, &["dev", "demo", "--port", "21902"]);
    assert!(!changed.status.success(), "{}", stdout(&changed));
    assert!(
        stderr(&changed)
            .contains("dev cannot change the port for existing env demo; current port is 21901"),
        "{}",
        stderr(&changed)
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["gatewayPort"], 21901);
}

#[test]
fn dev_command_reuses_the_saved_repo_for_new_envs() {
    let root = TestDir::new("dev-command-saved-repo");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let first = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(first.status.success(), "{}", stderr(&first));

    let second = run_ocm(&cwd, &env, &["dev", "preview"]);
    assert!(second.status.success(), "{}", stderr(&second));

    let show = run_ocm(&cwd, &env, &["env", "show", "preview", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["devRepoRoot"], path_string(&repo));
    assert!(
        show_json["devWorktreeRoot"]
            .as_str()
            .unwrap()
            .contains("/.worktrees/preview")
    );
}

#[test]
fn dev_command_prompts_for_the_repo_when_it_is_not_known_yet() {
    let root = TestDir::new("dev-command-prompt-repo");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm_with_stdin(
        &cwd,
        &env,
        &["dev", "demo"],
        &format!("{}\n", path_string(&repo)),
    );
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(stdout(&run).contains("OpenClaw repo path"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["devRepoRoot"], path_string(&repo));
}

#[test]
fn dev_command_can_start_a_background_service() {
    let root = TestDir::new("dev-command-service");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(
        &cwd,
        &env,
        &["dev", "demo", "--repo", &path_string(&repo), "--service"],
    );
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(stdout(&run).contains("service status demo"));
    assert!(stdout(&run).contains("logs demo --follow"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["serviceEnabled"], true);
    assert_eq!(show_json["serviceRunning"], true);

    let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let status_json: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(status_json["bindingKind"], "dev");
    assert_eq!(status_json["bindingName"], "dev");
    assert_eq!(status_json["desiredRunning"], true);

    let pnpm_log = fs::read_to_string(root.child("pnpm.log")).unwrap();
    assert!(pnpm_log.contains("|install"));
    assert!(stdout(&run).contains("http://127.0.0.1:"));
}

#[test]
fn dev_command_rejects_watch_plus_service() {
    let root = TestDir::new("dev-command-watch-service");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--watch", "--service"]);
    assert!(!run.status.success());
    assert!(stderr(&run).contains("dev cannot combine --watch with --service"));
}

fn create_runtime_backed_env(cwd: &Path, env: &std::collections::BTreeMap<String, String>) {
    let bin_dir = cwd.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable_script(&bin_dir.join("stable"), "#!/bin/sh\nexit 0\n");

    let runtime = run_ocm(
        cwd,
        env,
        &["runtime", "add", "stable", "--path", "./bin/stable"],
    );
    assert!(runtime.status.success(), "{}", stderr(&runtime));

    let created = run_ocm(
        cwd,
        env,
        &[
            "env",
            "create",
            "demo",
            "--runtime",
            "stable",
            "--port",
            "21901",
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
}

#[test]
fn dev_watch_force_takes_over_runtime_env_without_rebinding() {
    let root = TestDir::new("dev-command-runtime-watch-force");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    env.insert(
        "OCM_TEST_NODE_STDOUT".to_string(),
        "source watch stdout".to_string(),
    );
    env.insert(
        "OCM_TEST_NODE_STDERR".to_string(),
        "source watch stderr".to_string(),
    );
    create_runtime_backed_env(&cwd, &env);

    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ],
    );
    assert!(watch.status.success(), "{}", stderr(&watch));
    assert!(stdout(&watch).contains("service restored for demo"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultRuntime"], "stable");
    assert!(show_json["devRepoRoot"].is_null());
    assert!(show_json["devWorktreeRoot"].is_null());
    assert_eq!(show_json["gatewayPort"], 21901);
    assert_eq!(show_json["serviceEnabled"], true);
    assert_eq!(show_json["serviceRunning"], true);
    assert!(!source_watch_override_path(&root, "demo").exists());

    let node_log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(node_log.contains(&path_string(&repo)));
    assert!(node_log.contains(show_json["configPath"].as_str().unwrap()));
    assert!(node_log.contains("21901|--input-type=module"));
    assert!(node_log.contains("gateway run --port 21901"));
    assert!(node_log.contains(&format!(
        "|bundled={}",
        path_string(&repo.join("extensions"))
    )));
    assert!(node_log.contains(&format!("|devroot={}", path_string(&repo))));
    assert!(stdout(&watch).contains("source watch stdout"));
    assert!(stderr(&watch).contains("source watch stderr"));

    let state_dir = PathBuf::from(show_json["stateDir"].as_str().unwrap());
    let gateway_log = fs::read_to_string(state_dir.join("logs/gateway.log")).unwrap();
    let gateway_err_log = fs::read_to_string(state_dir.join("logs/gateway.err.log")).unwrap();
    assert!(gateway_log.contains("source watch stdout"));
    assert!(gateway_err_log.contains("source watch stderr"));

    let logs = run_ocm(&cwd, &env, &["logs", "demo", "--tail", "5", "--raw"]);
    assert!(logs.status.success(), "{}", stderr(&logs));
    assert!(stdout(&logs).contains("source watch stdout"));

    let error_logs = run_ocm(
        &cwd,
        &env,
        &["logs", "demo", "--stream", "error", "--tail", "5", "--raw"],
    );
    assert!(error_logs.status.success(), "{}", stderr(&error_logs));
    assert!(stdout(&error_logs).contains("source watch stderr"));
}

#[cfg(unix)]
#[test]
fn dev_watch_force_rejects_dependencies_from_another_checkout_before_takeover() {
    let root = TestDir::new("dev-command-runtime-watch-external-dependencies");
    let repo = init_openclaw_repo(&root);
    let shared_dependencies = root.child("other-checkout/node_modules");
    fs::create_dir_all(&shared_dependencies).unwrap();
    std::os::unix::fs::symlink(&shared_dependencies, repo.join("node_modules")).unwrap();
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);

    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ],
    );

    assert!(!watch.status.success());
    let error = stderr(&watch);
    assert!(
        error.contains("dependencies resolve outside the selected checkout"),
        "{error}"
    );
    assert!(
        error.contains(&path_string(&shared_dependencies)),
        "{error}"
    );
    assert!(error.contains("standalone checkout"), "{error}");
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["serviceRunning"], true);
    assert!(!root.child("node.log").exists());
}

#[test]
fn dev_watch_force_warns_for_installed_plugins_missing_from_source() {
    let root = TestDir::new("dev-command-runtime-watch-external-plugin");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);

    let installs_path = root.child("ocm-home/envs/demo/.openclaw/plugins/installs.json");
    fs::create_dir_all(installs_path.parent().unwrap()).unwrap();
    fs::write(
        &installs_path,
        r#"{"installRecords":{"external-chat":{"source":"npm","spec":"external-chat"},"codex":{"source":"npm","spec":"@openclaw/codex"}}}"#,
    )
    .unwrap();

    let watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ],
    );
    assert!(watch.status.success(), "{}", stderr(&watch));

    let watch_stderr = stderr(&watch);
    assert!(watch_stderr.contains("Installed plugin \"external-chat\" is not present"));
    assert!(!watch_stderr.contains("Installed plugin \"codex\" is not present"));
}

#[test]
fn dev_watch_force_restores_runtime_service_when_source_watch_cannot_spawn() {
    let root = TestDir::new("dev-command-runtime-watch-spawn-fails");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let env = service_env(&root);
    create_runtime_backed_env(&cwd, &env);

    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let empty_bin = root.child("empty-bin");
    fs::create_dir_all(&empty_bin).unwrap();
    let mut watch_env = env.clone();
    watch_env.insert("PATH".to_string(), path_string(&empty_bin));
    let watch = run_ocm(
        &cwd,
        &watch_env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ],
    );
    assert!(!watch.status.success());
    assert!(
        stderr(&watch).contains("failed to run \"node\""),
        "{}",
        stderr(&watch)
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultRuntime"], "stable");
    assert_eq!(show_json["serviceEnabled"], true);
    assert_eq!(show_json["serviceRunning"], true);
}

#[cfg(unix)]
#[test]
fn dev_watch_rejects_overlap_and_reclaims_the_released_lock() {
    let root = TestDir::new("dev-command-watch-overlap");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, release, watch_log) = install_blocking_fake_dev_runners(&root, &mut env);

    let mut first = Command::new(env!("CARGO_BIN_EXE_ocm"));
    first
        .current_dir(&cwd)
        .args(["dev", "demo", "--repo", &path_string(&repo), "--watch"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = first.spawn().unwrap();
    let did_start = wait_for_path(&started, Duration::from_secs(30));
    let override_path = source_watch_override_path(&root, "demo");
    let did_write_override = wait_for_path(&override_path, Duration::from_secs(30));
    let override_before_overlap = fs::read_to_string(&override_path).unwrap_or_default();

    let overlap = run_ocm(&cwd, &env, &["dev", "demo", "--watch"]);
    let override_after_overlap = fs::read_to_string(&override_path).unwrap_or_default();
    fs::write(&release, "release\n").unwrap();
    let first_output = first.wait_with_output().unwrap();

    assert!(
        did_start,
        "first source watch did not start: {}",
        stderr(&first_output)
    );
    assert!(
        did_write_override,
        "first source watch did not publish its override"
    );
    assert!(
        !overlap.status.success(),
        "overlapping source watch unexpectedly succeeded"
    );
    assert!(
        stderr(&overlap).contains("source watch for env \"demo\" is already active or starting"),
        "{}",
        stderr(&overlap)
    );
    assert_eq!(override_after_overlap, override_before_overlap);
    assert!(first_output.status.success(), "{}", stderr(&first_output));
    assert!(source_watch_lock_path(&root, "demo").exists());
    assert!(!source_watch_override_path(&root, "demo").exists());

    let after_release = run_ocm(&cwd, &env, &["dev", "demo", "--watch"]);
    assert!(after_release.status.success(), "{}", stderr(&after_release));
    let starts = fs::read_to_string(watch_log).unwrap();
    assert_eq!(starts.lines().count(), 2);
}

#[cfg(unix)]
#[test]
fn dev_watch_lease_survives_parent_crash_until_the_watcher_exits() {
    let root = TestDir::new("dev-command-watch-parent-crash");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, release, watch_log) = install_blocking_fake_dev_runners(&root, &mut env);

    let mut first = Command::new(env!("CARGO_BIN_EXE_ocm"));
    first
        .current_dir(&cwd)
        .args(["dev", "demo", "--repo", &path_string(&repo), "--watch"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut first = first.spawn().unwrap();
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "source watch did not start"
    );
    let override_path = source_watch_override_path(&root, "demo");
    assert!(
        wait_for_path(&override_path, Duration::from_secs(30)),
        "source watch did not publish its override"
    );
    let source_watch: Value =
        serde_json::from_str(&fs::read_to_string(&override_path).unwrap()).unwrap();
    let watcher_pid = source_watch["watchPid"].as_u64().unwrap() as u32;

    let killed = Command::new("kill")
        .args(["-KILL", &first.id().to_string()])
        .output()
        .unwrap();
    assert!(killed.status.success(), "{}", stderr(&killed));
    assert!(!first.wait().unwrap().success());
    assert!(process_is_alive(watcher_pid));

    let mut overlap = Command::new(env!("CARGO_BIN_EXE_ocm"));
    overlap
        .current_dir(&cwd)
        .args(["dev", "demo", "--watch"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut overlap = overlap.spawn().unwrap();
    let overlap_deadline = Instant::now() + Duration::from_secs(5);
    let overlap_exited = loop {
        if overlap.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= overlap_deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(25));
    };
    if !overlap_exited {
        fs::write(&release, "release\n").unwrap();
        let _ = overlap.kill();
    }
    let overlap = overlap.wait_with_output().unwrap();
    assert!(
        overlap_exited,
        "overlapping source watch blocked instead of rejecting the live lease"
    );
    assert!(!overlap.status.success());
    assert!(
        stderr(&overlap).contains("source watch for env \"demo\" is already active or starting"),
        "{}",
        stderr(&overlap)
    );

    fs::write(&release, "release\n").unwrap();
    assert!(wait_for_process_exit(watcher_pid, Duration::from_secs(10)));

    let after_release = run_ocm(&cwd, &env, &["dev", "demo", "--watch"]);
    assert!(after_release.status.success(), "{}", stderr(&after_release));
    let starts = fs::read_to_string(watch_log).unwrap();
    assert_eq!(starts.lines().count(), 2);
}

#[cfg(unix)]
#[test]
fn dev_watch_force_stops_the_entire_stubborn_process_tree() {
    let root = TestDir::new("dev-command-watch-force-tree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, descendant_pid_path) = install_stubborn_fake_dev_runners(&root, &mut env);

    let mut watch = Command::new(env!("CARGO_BIN_EXE_ocm"));
    watch
        .current_dir(&cwd)
        .args(["dev", "demo", "--repo", &path_string(&repo), "--watch"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let watch = watch.spawn().unwrap();
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "source watch did not start"
    );
    assert!(
        wait_for_path(&descendant_pid_path, Duration::from_secs(30)),
        "source watch descendant did not start"
    );
    let descendant_pid = fs::read_to_string(&descendant_pid_path)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();

    let signal = Command::new("kill")
        .args(["-INT", &watch.id().to_string()])
        .output()
        .unwrap();
    assert!(signal.status.success(), "{}", stderr(&signal));
    let watch = watch.wait_with_output().unwrap();

    assert_eq!(watch.status.code(), Some(130), "{}", stderr(&watch));
    assert!(
        wait_for_process_exit(descendant_pid, Duration::from_secs(3)),
        "source watch descendant {descendant_pid} survived forced shutdown"
    );
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[cfg(unix)]
#[test]
fn dev_watch_stops_descendants_when_the_wrapper_exits_first() {
    let root = TestDir::new("dev-command-watch-wrapper-exit");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, descendant_pid_path, stdin_kind, node_args) =
        install_orphaning_fake_dev_runners(&root, &mut env);

    let watch = run_ocm(
        &cwd,
        &env,
        &["dev", "demo", "--repo", &path_string(&repo), "--watch"],
    );
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "source watch did not start"
    );
    assert!(
        wait_for_path(&descendant_pid_path, Duration::from_secs(30)),
        "source watch descendant did not start"
    );
    let descendant_pid = fs::read_to_string(&descendant_pid_path)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();

    assert_eq!(watch.status.code(), Some(23), "{}", stderr(&watch));
    assert_eq!(fs::read_to_string(stdin_kind).unwrap().trim(), "pipe");
    let node_args = fs::read_to_string(node_args).unwrap();
    assert!(node_args.contains("--input-type=module"), "{node_args}");
    assert!(
        node_args.contains("Object.defineProperty(process.stdin"),
        "{node_args}"
    );
    assert!(
        wait_for_process_exit(descendant_pid, Duration::from_secs(3)),
        "source watch descendant {descendant_pid} survived wrapper exit"
    );
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[cfg(unix)]
#[test]
fn dev_watch_gives_interactive_child_terminal_foreground_ownership() {
    let root = TestDir::new("dev-command-watch-interactive-terminal");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, received) = install_interactive_fake_dev_runners(&root, &mut env);

    let repo_path = path_string(&repo);
    let (mut watch, mut terminal) = spawn_ocm_with_controlling_pty(
        &cwd,
        &env,
        &["dev", "demo", "--repo", &repo_path, "--watch"],
    );
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "source watch did not reach its interactive stdin read"
    );
    terminal.write_all(&[0x1a]).unwrap();
    assert!(
        wait_for_process_stop(watch.id(), Duration::from_secs(10)),
        "OCM did not suspend with its foreground source watch"
    );
    let resumed = unsafe { libc::kill(watch.id() as libc::pid_t, libc::SIGCONT) };
    assert_eq!(
        resumed,
        0,
        "failed resuming OCM: {}",
        std::io::Error::last_os_error()
    );
    terminal.write_all(b"terminal-input\n").unwrap();
    assert!(
        wait_for_path(&received, Duration::from_secs(10)),
        "source watch was suspended while reading inherited terminal stdin"
    );

    drop(terminal);
    let status = watch.wait().unwrap();
    assert!(status.success(), "source watch exited with {status}");
    assert_eq!(
        fs::read_to_string(received).unwrap().trim(),
        "terminal-input"
    );
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[cfg(unix)]
#[test]
fn dev_watch_background_resume_waits_for_foreground_ownership() {
    let root = TestDir::new("dev-command-watch-background-resume");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, received) = install_interactive_fake_dev_runners(&root, &mut env);
    let override_path = source_watch_override_path(&root, "demo");
    let script = r#"
import fcntl
import os
import pty
import signal
import subprocess
import termios
import time

def wait_path(path, label):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if os.path.exists(path):
            return
        time.sleep(0.025)
    raise RuntimeError(f"timed out waiting for {label}")

def wait_stopped(pid, label):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        waited, status = os.waitpid(pid, os.WNOHANG | os.WUNTRACED)
        if waited == pid:
            if os.WIFSTOPPED(status):
                return
            raise RuntimeError(f"OCM exited while waiting for {label}: status={status}")
        time.sleep(0.025)
    raise RuntimeError(f"timed out waiting for {label}")

master, slave = pty.openpty()
process = None
succeeded = False
try:
    os.setsid()
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    shell_group = os.getpgrp()
    os.tcsetpgrp(slave, shell_group)
    process = subprocess.Popen(
        [
            os.environ["OCM_TEST_BINARY"],
            "dev",
            "demo",
            "--repo",
            os.environ["OCM_TEST_REPO"],
            "--watch",
        ],
        cwd=os.environ["OCM_TEST_CWD"],
        stdin=slave,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        preexec_fn=os.setpgrp,
    )
    wait_path(os.environ["OCM_TEST_STARTED"], "watcher startup")
    wait_stopped(process.pid, "initial background stop")

    os.killpg(process.pid, signal.SIGCONT)
    wait_stopped(process.pid, "background resume stop")
    if not os.path.exists(os.environ["OCM_TEST_OVERRIDE"]):
        raise RuntimeError("background resume dropped the active watch lease")

    os.tcsetpgrp(slave, process.pid)
    os.killpg(process.pid, signal.SIGCONT)
    os.write(master, b"terminal-input\n")
    wait_path(os.environ["OCM_TEST_RECEIVED"], "foreground input")
    time.sleep(0.2)
    os.close(master)
    master = -1
    os.close(slave)
    slave = -1
    exit_code = process.wait(timeout=10)
    if exit_code != 0:
        error = process.stderr.read().decode(errors="replace")
        raise RuntimeError(f"OCM exited with {exit_code}: {error}")
    succeeded = True
finally:
    if not succeeded and process is not None:
        for process_group in (process.pid,):
            try:
                os.killpg(process_group, signal.SIGKILL)
            except ProcessLookupError:
                pass
    if master >= 0:
        os.close(master)
    if slave >= 0:
        os.close(slave)
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .env_clear()
        .envs(&env)
        .env("OCM_TEST_BINARY", env!("CARGO_BIN_EXE_ocm"))
        .env("OCM_TEST_REPO", path_string(&repo))
        .env("OCM_TEST_CWD", path_string(&cwd))
        .env("OCM_TEST_STARTED", path_string(&started))
        .env("OCM_TEST_RECEIVED", path_string(&received))
        .env("OCM_TEST_OVERRIDE", path_string(&override_path))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "python PTY scenario failed with {}:\nstdout={}\nstderr={}",
        output.status,
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(received).unwrap().trim(),
        "terminal-input"
    );
    assert!(!override_path.exists());
}

#[test]
fn dev_watch_aborts_and_restores_policy_when_service_stop_times_out() {
    let root = TestDir::new("dev-command-watch-stop-timeout");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);

    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let stdout_path = path_string(&root.child("demo.stdout.log"));
    let stderr_path = path_string(&root.child("demo.stderr.log"));
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: path_string(&root.child("ocm-home")),
        updated_at: now_utc(),
        services: vec![SupervisorRuntimeService {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: "stable".to_string(),
            gateway_state: "running".to_string(),
            restart_handoff: Some("protocol-v1".to_string()),
            restart_count: 0,
            child_port: 21901,
            pid: Some(std::process::id()),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
        }],
        children: vec![SupervisorRuntimeChild {
            env_name: "demo".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: "stable".to_string(),
            pid: std::process::id(),
            restart_count: 0,
            child_port: 21901,
            stdout_path,
            stderr_path,
        }],
    };
    fs::write(&runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();

    let watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ],
    );
    assert!(!watch.status.success());
    assert!(
        stderr(&watch)
            .contains("background service for demo is still running after the stop request"),
        "{}",
        stderr(&watch)
    );
    assert!(
        stderr(&watch)
            .contains("restored the background service policy and did not start source watch"),
        "{}",
        stderr(&watch)
    );

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["serviceRunning"], true);
    assert!(!root.child("node.log").exists());
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[cfg(unix)]
fn assert_dev_watch_signal_restores_service(test_name: &str, signal_name: &str) {
    let root = TestDir::new(test_name);
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let (started, release, _) = install_blocking_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);

    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let mut watch = Command::new(env!("CARGO_BIN_EXE_ocm"));
    watch
        .current_dir(&cwd)
        .args([
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut watch = watch.spawn().unwrap();
    let did_start = wait_for_path(&started, Duration::from_secs(30));
    let signal = Command::new("kill")
        .args([signal_name, &watch.id().to_string()])
        .output()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stopped_from_signal = false;
    while Instant::now() < deadline {
        if watch.try_wait().unwrap().is_some() {
            stopped_from_signal = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    if !stopped_from_signal {
        fs::write(&release, "release\n").unwrap();
    }
    let watch = watch.wait_with_output().unwrap();

    assert!(did_start, "source watch did not start: {}", stderr(&watch));
    assert!(signal.status.success(), "{}", stderr(&signal));
    assert!(stopped_from_signal, "source watch ignored SIGINT");
    assert_eq!(watch.status.code(), Some(130), "{}", stderr(&watch));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["serviceRunning"], true);
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[cfg(unix)]
#[test]
fn dev_watch_interrupt_stops_the_child_and_restores_the_service() {
    assert_dev_watch_signal_restores_service("dev-command-watch-interrupt", "-INT");
}

#[cfg(unix)]
#[test]
fn dev_watch_termination_stops_the_child_and_restores_the_service() {
    assert_dev_watch_signal_restores_service("dev-command-watch-termination", "-TERM");
}

#[cfg(unix)]
#[test]
fn dev_watch_nonzero_child_exit_restores_the_service() {
    let root = TestDir::new("dev-command-watch-child-failure");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let started = install_failing_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);

    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));

    let watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
        ],
    );
    assert!(started.exists());
    assert_eq!(watch.status.code(), Some(23), "{}", stderr(&watch));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["serviceRunning"], true);
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[test]
fn dev_command_still_rejects_plain_runtime_env_reuse() {
    let root = TestDir::new("dev-command-runtime-plain-reuse");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);

    let run = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
    assert!(!run.status.success());
    assert!(stderr(&run).contains("environment \"demo\" is not a dev env"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["defaultRuntime"], "stable");
    assert!(show_json["devRepoRoot"].is_null());
}

#[test]
fn dev_watch_force_temporarily_takes_over_and_restores_the_background_service() {
    let root = TestDir::new("dev-command-watch-force");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let service = run_ocm(
        &cwd,
        &env,
        &["dev", "demo", "--repo", &path_string(&repo), "--service"],
    );
    assert!(service.status.success(), "{}", stderr(&service));

    let watch = run_ocm(&cwd, &env, &["dev", "demo", "--watch", "--force"]);
    assert!(watch.status.success(), "{}", stderr(&watch));
    assert!(stdout(&watch).contains("service restored for demo"));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(show_json["serviceEnabled"], true);
    assert_eq!(show_json["serviceRunning"], true);
    assert!(!source_watch_override_path(&root, "demo").exists());

    let node_log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(node_log.contains("scripts/watch-node.mjs"));
    assert!(node_log.contains("gateway run --port"));
}
