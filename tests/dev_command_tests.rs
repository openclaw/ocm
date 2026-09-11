mod support;

use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use ocm::env::EnvDevMeta;
use ocm::store::{get_environment, now_utc, save_environment, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::Value;

use crate::support::{
    TestDir, create_owned_dev_env, dev_plain, dev_watch, enable_fake_daemon_gateway_admission,
    hold_environment_operation, install_fake_service_manager, ocm_env, path_string,
    register_owned_dev_env, run_ocm, stderr, stdout, write_executable_script,
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
        "#!/bin/sh\nlogged_args=$(printf '%s' \"$*\" | tr '\\n' ' ')\nprintf '%s|%s|%s|%s|bundled=%s|devroot=%s\\n' \"$PWD\" \"$OPENCLAW_CONFIG_PATH\" \"$OPENCLAW_GATEWAY_PORT\" \"$logged_args\" \"$OPENCLAW_BUNDLED_PLUGINS_DIR\" \"$OPENCLAW_DEV_SOURCE_ROOT\" >> \"{}\"\nif [ -n \"$OCM_TEST_NODE_STDOUT\" ]; then printf '%s\\n' \"$OCM_TEST_NODE_STDOUT\"; fi\nif [ -n \"$OCM_TEST_NODE_STDERR\" ]; then printf '%s\\n' \"$OCM_TEST_NODE_STDERR\" >&2; fi\n",
        path_string(&node_log)
    );
    write_executable_script(&bin_dir.join("pnpm"), &pnpm);
    write_fake_dev_node(root, &node);
    prepend_fake_bin(env, &bin_dir);
}

fn write_fake_dev_node(root: &TestDir, script: &str) {
    // Match the real Node shim's startup gate so short-lived stand-ins cannot
    // exit before the controller records their identity.
    let gate = r#"#!/bin/sh
if [ -n "$OCM_SOURCE_WATCH_START_FD" ]; then
  IFS= read -r ocm_start < "/dev/fd/$OCM_SOURCE_WATCH_START_FD" || exit 1
  unset OCM_SOURCE_WATCH_START_FD
fi
"#;
    write_executable_script(
        &root.child("fake-dev-bin/node"),
        &script.replacen("#!/bin/sh\n", gate, 1),
    );
}

fn declare_source_tooling(repo: &Path) {
    let manifest_path = repo.join("package.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["devDependencies"] = serde_json::json!({
        "tsx": "0.0.0",
        "tsdown": "0.0.0"
    });
    manifest["dependencies"] = serde_json::json!({"chokidar": "0.0.0"});
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    fs::write(repo.join("scripts/tsx.mjs"), "// Fixture source loader.\n").unwrap();
}

fn write_resolvable_source_tool(repo: &Path, name: &str) {
    let package_root = repo.join("node_modules").join(name);
    fs::create_dir_all(&package_root).unwrap();
    fs::write(
        package_root.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": name,
            "type": "module",
            "bin": "./cli.mjs",
            "exports": {
                ".": {"import": "./entry.mjs", "require": "./missing.cjs"},
                "./esm": "./entry.mjs"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        package_root.join("entry.mjs"),
        "throw new Error('Prerequisite checks must not execute dependency code');\n",
    )
    .unwrap();
    if name == "tsdown" {
        fs::write(
            package_root.join("cli.mjs"),
            "throw new Error('Prerequisite checks must not execute dependency code');\n",
        )
        .unwrap();
        let bin_dir = repo.join("node_modules/.bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable_script(
            &bin_dir.join(if cfg!(windows) {
                "tsdown.cmd"
            } else {
                "tsdown"
            }),
            "#!/bin/sh\nexit 0\n",
        );
    }
}

fn install_probe_aware_fake_dev_runners(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) {
    let real_node = Command::new("node")
        .args(["-p", "process.execPath"])
        .env_clear()
        .envs(&*env)
        .output()
        .expect("source prerequisite tests require Node");
    assert!(real_node.status.success(), "{}", stderr(&real_node));
    let real_node = stdout(&real_node).trim().replace('\'', "'\\''");
    install_fake_dev_runners(root, env);
    let node_path = root.child("fake-dev-bin/node");
    let script = fs::read_to_string(&node_path).unwrap();
    let prefix = format!(
        "#!/bin/sh\nif [ \"$6\" = ocm-source-dependencies ]; then exec '{real_node}' \"$@\"; fi\n"
    );
    write_executable_script(&node_path, &script.replacen("#!/bin/sh\n", &prefix, 1));
}

fn install_frozen_source_dependency_runner(root: &TestDir) {
    let script = format!(
        r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> '{}'
if [ "$1" != install ]; then exit 0; fi
if [ "$2" != --frozen-lockfile ]; then exit 41; fi
if [ -n "$OCM_TEST_INSTALL_EXIT_CODE" ]; then exit "$OCM_TEST_INSTALL_EXIT_CODE"; fi
modules_dir="${{PNPM_CONFIG_MODULES_DIR:-node_modules}}"
for name in tsx tsdown chokidar; do
  mkdir -p "$modules_dir/$name"
  printf '{{"name":"%s","type":"module","bin":"./entry.mjs","exports":{{".":"./entry.mjs","./esm":"./entry.mjs"}}}}\n' "$name" > "$modules_dir/$name/package.json"
  printf "throw new Error('Prerequisite checks must not execute dependency code');\n" > "$modules_dir/$name/entry.mjs"
done
mkdir -p "$modules_dir/.bin"
printf '#!/bin/sh\nexit 0\n' > "$modules_dir/.bin/tsdown"
chmod +x "$modules_dir/.bin/tsdown"
"#,
        path_string(&root.child("pnpm.log"))
    );
    write_executable_script(&root.child("fake-dev-bin/pnpm"), &script);
}

fn source_watch_override_path(root: &TestDir, name: &str) -> PathBuf {
    root.child(format!("ocm-home/source-watch/{name}.json"))
}

fn source_watch_lock_path(root: &TestDir, name: &str) -> PathBuf {
    root.child(format!("ocm-home/source-watch/{name}.lock"))
}

#[cfg(unix)]
struct DevWatchFixture {
    child: Option<std::process::Child>,
    release: PathBuf,
    session: PathBuf,
    owns_session: bool,
}

#[cfg(unix)]
impl DevWatchFixture {
    fn spawn(
        root: &TestDir,
        cwd: &Path,
        env: &std::collections::BTreeMap<String, String>,
        args: &[&str],
    ) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_ocm"))
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            child: Some(child),
            release: root.child("source-watch.release"),
            session: PathBuf::from(&env["OCM_HOME"])
                .join("source-watch")
                .join(format!("{}.session", args[1])),
            owns_session: true,
        }
    }

    fn spawn_caller(
        root: &TestDir,
        cwd: &Path,
        env: &std::collections::BTreeMap<String, String>,
        args: &[&str],
    ) -> Self {
        let mut caller = Self::spawn(root, cwd, env, args);
        caller.owns_session = false;
        caller
    }

    fn crash_controller(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }

    fn wait_without_release(&mut self) -> std::process::Output {
        self.wait_without_release_for(Duration::from_secs(20))
    }

    fn wait_without_release_for(&mut self, timeout: Duration) -> std::process::Output {
        let deadline = Instant::now() + timeout;
        while self.child.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "watch did not finish within its cleanup deadline"
            );
            thread::sleep(Duration::from_millis(25));
        }
        self.child.take().unwrap().wait_with_output().unwrap()
    }

    fn finish(mut self) -> std::process::Output {
        fs::write(&self.release, "release\n").unwrap();
        self.child.take().unwrap().wait_with_output().unwrap()
    }
}

#[cfg(unix)]
impl Drop for DevWatchFixture {
    fn drop(&mut self) {
        if self.owns_session {
            let _ = fs::write(&self.release, "release\n");
        }
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if child.try_wait().is_ok_and(|status| status.is_some()) {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if !self.owns_session {
            return;
        }
        if let Ok(pid) =
            fs::read_to_string(self.release.with_file_name("source-watch-descendant.pid"))
            && let Ok(pid) = pid.trim().parse::<u32>()
        {
            let _ = wait_for_process_exit(pid, Duration::from_secs(3));
        }
        if let Some(pid) = fs::read(&self.session)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|session| session["child"]["pid"].as_u64())
        {
            let _ = wait_for_process_exit(pid as u32, Duration::from_secs(2));
        }
        if let Some(children) = fs::read(&self.session)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|session| session["ui"]["children"].as_object().cloned())
        {
            for child in children.values() {
                if let Some(pid) = child["pid"].as_u64() {
                    let _ = wait_for_process_exit(pid as u32, Duration::from_secs(3));
                }
            }
        }
    }
}

#[cfg(unix)]
fn run_dev_stop(
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
) -> std::process::Output {
    run_named_dev_stop(cwd, env, "demo")
}

#[cfg(unix)]
fn run_named_dev_stop(
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .args(["dev", "stop", name, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
fn encode_legacy_watch_session(root: &TestDir) {
    let path = source_watch_override_path(root, "demo").with_extension("session");
    let mut session = read_source_watch_session(root);
    session["kind"] = "ocm-source-watch-session".into();
    fs::write(path, serde_json::to_vec(&session).unwrap()).unwrap();
}

#[cfg(unix)]
fn read_source_watch_session(root: &TestDir) -> Value {
    let path = source_watch_override_path(root, "demo").with_extension("session");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
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
        "#!/bin/sh\ntrap 'exit 143' TERM\nprintf 'started\\n' >> \"{}\"\nprintf 'ready\\n' > \"{}\"\nwhile [ ! -f \"{}\" ]; do /bin/sleep 0.05; done\n",
        path_string(&log),
        path_string(&started),
        path_string(&release),
    );
    write_fake_dev_node(root, &node);
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
    write_fake_dev_node(root, &node);
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
    write_fake_dev_node(root, &node);
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
    write_fake_dev_node(root, &node);
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
    write_fake_dev_node(root, &node);
    (started, received)
}

#[cfg(unix)]
fn spawn_ocm_with_controlling_pty(
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    args: &[&str],
    terminal_output: bool,
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
    let stdout = if terminal_output {
        Stdio::from(slave.try_clone().unwrap())
    } else {
        Stdio::null()
    };
    let stderr = if terminal_output {
        Stdio::from(slave.try_clone().unwrap())
    } else {
        Stdio::null()
    };

    let mut command = Command::new(env!("CARGO_BIN_EXE_ocm"));
    command
        .current_dir(cwd)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::from(slave))
        .stdout(stdout)
        .stderr(stderr);
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

#[cfg(unix)]
struct PtyOutputDrain {
    stop: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<std::io::Result<()>>>,
}

#[cfg(unix)]
impl PtyOutputDrain {
    fn start(mut terminal: File) -> Self {
        let flags = unsafe { libc::fcntl(terminal.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe {
                libc::fcntl(
                    terminal.as_raw_fd(),
                    libc::F_SETFL,
                    flags | libc::O_NONBLOCK,
                )
            },
            0
        );
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let reader = thread::spawn(move || {
            let mut buffer = [0; 4096];
            while !reader_stop.load(Ordering::Acquire) {
                match terminal.read(&mut buffer) {
                    Ok(0) => return Ok(()),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    // Linux PTYs report EIO after the last slave closes.
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        });
        Self {
            stop,
            reader: Some(reader),
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        self.reader.take().unwrap().join().unwrap().unwrap();
    }
}

#[cfg(unix)]
impl Drop for PtyOutputDrain {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
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

fn service_env_with_gateway_admission(
    root: &TestDir,
) -> std::collections::BTreeMap<String, String> {
    let mut env = service_env(root);
    enable_fake_daemon_gateway_admission(root, &mut env);
    env
}

#[cfg(unix)]
static INITIAL_UI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
fn prepare_initial_ui_repo(
    root: &TestDir,
    env: &mut std::collections::BTreeMap<String, String>,
) -> PathBuf {
    let repo = init_openclaw_repo(root);
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .env_clear()
        .envs(&*env)
        .output()
        .unwrap();
    assert!(node.status.success(), "{}", stderr(&node));
    let node = stdout(&node).trim().replace('\'', "'\\''");
    let bin = root.child("initial-ui-bin");
    fs::create_dir_all(&bin).unwrap();
    // Root dependency inspection is covered by its own owner. The UI probe
    // executes real resolution against these deliberately nonexecutable stubs.
    write_executable_script(
        &bin.join("node"),
        &format!(
            r#"#!/bin/sh
case "${{3:-}}" in
  *uiRequire*)
    : > "$OCM_TEST_DEV_UI_DIR/ui-probe-ready"
    while [ -f "$OCM_TEST_DEV_UI_DIR/ui-probe-hold" ] &&
          [ ! -f "$OCM_TEST_DEV_UI_DIR/source-watch.release" ]; do
      sleep 0.02
    done
    exec '{node}' "$@";;
  *createRequire*|*ocm-source-dependencies*)
    if [ -n "$OCM_SOURCE_WATCH_START_FD" ]; then
      IFS= read -r start < "/dev/fd/$OCM_SOURCE_WATCH_START_FD" || exit 1
      unset OCM_SOURCE_WATCH_START_FD
    fi
    exit 0;;
esac
exec '{node}' "$@"
"#
        ),
    );
    prepend_fake_bin(env, &bin);
    env.insert("OCM_TEST_DEV_UI_DIR".to_string(), path_string(root.path()));
    fs::create_dir_all(repo.join("ui")).unwrap();
    fs::write(
        repo.join("ui/index.html"),
        "<!doctype html><title>UI fixture</title>",
    )
    .unwrap();
    fs::write(repo.join("ui/package.json"), r#"{"name":"ui-fixture"}"#).unwrap();
    fs::write(
        repo.join("scripts/dev-ui-fixture.cjs"),
        include_str!("support/dev_ui.cjs"),
    )
    .unwrap();
    fs::write(
        repo.join("scripts/ui.js"),
        "require('./dev-ui-fixture.cjs');\n",
    )
    .unwrap();
    for entry in ["watch-node.mjs", "run-node.mjs"] {
        fs::write(
            repo.join("scripts").join(entry),
            "import './dev-ui-fixture.cjs';\n",
        )
        .unwrap();
    }
    fs::write(
        repo.join("openclaw.mjs"),
        "import './scripts/dev-ui-fixture.cjs';\n",
    )
    .unwrap();
    for name in ["vite", "dompurify"] {
        let directory = repo.join("node_modules").join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("package.json"),
            format!(r#"{{"name":"{name}","main":"index.js"}}"#),
        )
        .unwrap();
        fs::write(
            directory.join("index.js"),
            "throw new Error('UI inspection must not execute package code');\n",
        )
        .unwrap();
    }
    // These are tiny synthetic resolution fixtures, not copied dependencies.
    let added = Command::new("git")
        .args(["-C", &path_string(&repo), "add", "-f", "node_modules"])
        .output()
        .unwrap();
    assert!(added.status.success(), "{}", stderr(&added));
    commit_nested_openclaw_repo(&repo);
    repo
}

#[cfg(unix)]
fn initial_ui_process(root: &TestDir, role: &str, owner: &mut DevWatchFixture) -> Value {
    let file = root.child(format!("{role}.json"));
    if !wait_for_path(&file, Duration::from_secs(10)) {
        // Read only bytes already in this fixture's stderr pipe; diagnosing a
        // failed startup must not block on EOF or release/stop its processes.
        let mut diagnostic = String::new();
        let mut controller = None;
        let mut status = None;
        if let Some(child) = owner.child.as_mut() {
            controller = Some(child.id());
            status = Some(child.try_wait());
            if let Some(stderr) = child.stderr.as_mut() {
                let mut available: libc::c_int = 0;
                if unsafe { libc::ioctl(stderr.as_raw_fd(), libc::FIONREAD, &mut available) } == 0
                    && available > 0
                {
                    let mut bytes = vec![0; (available as usize).min(64 * 1024)];
                    if stderr.read_exact(&mut bytes).is_ok() {
                        diagnostic = String::from_utf8_lossy(&bytes).into_owned();
                    }
                }
            }
        }
        let recorded = fs::read_to_string(&owner.session).unwrap_or_default();
        let source = owner
            .session
            .parent()
            .and_then(Path::parent)
            .and_then(|store| fs::read(store.join("envs.json")).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|registry| {
                registry["envs"].as_array().and_then(|envs| {
                    envs.iter()
                        .find(|meta| {
                            meta["name"].as_str()
                                == owner.session.file_stem().and_then(|name| name.to_str())
                        })
                        .map(|meta| meta["dev"].clone())
                })
            });
        panic!(
            "{role} did not start at {}; controller={controller:?}, status={status:?}, source={source:?}, session={recorded}, stderr={diagnostic}",
            file.display()
        );
    }
    serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
}

#[cfg(unix)]
#[test]
fn dev_defaults_start_live_components_and_opt_outs_select_the_owned_mode() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (label, args, watching, ui) in [
        ("default", vec!["dev", "demo"], true, true),
        ("no-ui", vec!["dev", "demo", "--no-ui"], true, false),
        ("no-watch", vec!["dev", "demo", "--no-watch"], false, true),
        (
            "plain",
            vec!["dev", "demo", "--no-watch", "--no-ui"],
            false,
            false,
        ),
        ("watch-alias", vec!["dev", "demo", "--watch"], true, true),
        ("ui-alias", vec!["dev", "demo", "--ui"], true, true),
        (
            "both-aliases",
            vec!["dev", "demo", "--watch", "--ui"],
            true,
            true,
        ),
    ] {
        let root = TestDir::new(&format!("dev-default-mode-{label}"));
        let mut env = ocm_env(&root);
        let repo = prepare_initial_ui_repo(&root, &mut env);
        fs::write(root.child("gateway-document-ready"), "ready").unwrap();
        let mut controller = DevWatchFixture::spawn(&root, &repo, &env, &args);
        let controller_pid = controller.child.as_ref().unwrap().id();
        let gateway = initial_ui_process(&root, "gateway", &mut controller);
        assert_eq!(
            gateway["entrypoint"],
            if watching {
                "watch-node.mjs"
            } else {
                "run-node.mjs"
            },
            "{label}"
        );
        let gateway_pid = gateway["pid"].as_u64().unwrap() as u32;
        let ui_process = ui.then(|| initial_ui_process(&root, "ui", &mut controller));
        if let Some(ui_process) = &ui_process {
            assert_eq!(gateway["cwd"], ui_process["cwd"]);
        } else {
            assert!(!root.child("ui.json").exists(), "{label}");
        }
        let status = run_ocm(&repo, &env, &["dev", "status", "demo", "--json"]);
        assert!(status.status.success(), "{label}: {}", stderr(&status));
        let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
        assert_eq!(status["sourceWatch"]["watching"], watching, "{label}");

        // Reuse compares effective components, including implicit and explicit defaults.
        let mut repeat = vec!["dev", "demo"];
        if !watching {
            repeat.push("--no-watch");
        }
        if !ui {
            repeat.push("--no-ui");
        }
        if label == "default" {
            repeat.extend(["--watch", "--ui"]);
        }
        let reused = run_ocm(&repo, &env, &repeat);
        assert!(reused.status.success(), "{label}: {}", stderr(&reused));
        let session = read_source_watch_session(&root);
        assert_eq!(session["controller"]["pid"], controller_pid, "{label}");
        assert_eq!(session["watching"], watching, "{label}");
        if let Some(ui_process) = &ui_process {
            assert_eq!(session["ui"]["children"]["gateway"]["pid"], gateway_pid);
            assert_eq!(session["ui"]["children"]["ui"]["pid"], ui_process["pid"]);
            assert_eq!(session["ui"]["target"]["port"], ui_process["port"]);
        } else {
            assert_eq!(session["child"]["pid"], gateway_pid);
            assert!(session["ui"].is_null());
        }

        let stopped = run_dev_stop(&repo, &env);
        assert!(stopped.status.success(), "{label}: {}", stderr(&stopped));
        let finished = controller.wait_without_release();
        assert_eq!(
            finished.status.code(),
            Some(130),
            "{label}: {}",
            stderr(&finished)
        );
        assert!(wait_for_process_exit(gateway_pid, Duration::from_secs(3)));
        if let Some(ui_process) = ui_process {
            assert!(wait_for_process_exit(
                ui_process["pid"].as_u64().unwrap() as u32,
                Duration::from_secs(3)
            ));
        } else {
            assert!(!root.child("ui.json").exists(), "{label}");
        }
        assert_eq!(read_source_watch_session(&root)["closed"], true);
    }
}

#[cfg(unix)]
#[test]
fn dev_defaults_preserve_incompatible_existing_envs_and_allow_ui_opt_out() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (variant, expected) in [
        ("tls", "requires a local HTTP Gateway"),
        ("disabled-ui", "disables the Control UI"),
        ("missing-ui", "Control UI dependencies are not ready"),
    ] {
        let root = TestDir::new(&format!("dev-default-existing-{variant}"));
        let mut env = ocm_env(&root);
        let repo = prepare_initial_ui_repo(&root, &mut env);
        let mut initial = DevWatchFixture::spawn(&root, &repo, &env, &["dev", "demo", "--no-ui"]);
        let gateway = initial_ui_process(&root, "gateway", &mut initial);
        let selected = PathBuf::from(gateway["cwd"].as_str().unwrap());
        let stopped = run_dev_stop(&repo, &env);
        assert!(stopped.status.success(), "{variant}: {}", stderr(&stopped));
        assert_eq!(initial.wait_without_release().status.code(), Some(130));
        drop(initial);
        fs::remove_file(root.child("source-watch.release")).unwrap();
        fs::remove_file(root.child("gateway.json")).unwrap();

        let original = get_environment("demo", &env, &repo).unwrap();
        let config_path = Path::new(&original.root).join(".openclaw/openclaw.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        match variant {
            "tls" => config["gateway"]["tls"] = serde_json::json!({"enabled": true}),
            "disabled-ui" => config["gateway"]["controlUi"] = serde_json::json!({"enabled": false}),
            "missing-ui" => {
                assert!(selected.join("node_modules/vite").is_dir());
                // An owned worktree can also resolve a package from its parent repo.
                for source in [&selected, &repo] {
                    let dependency = source.join("node_modules/vite");
                    if dependency.is_dir() {
                        fs::remove_dir_all(dependency).unwrap();
                    }
                }
            }
            _ => unreachable!(),
        }
        let config_bytes = serde_json::to_vec(&config).unwrap();
        fs::write(&config_path, &config_bytes).unwrap();
        let source_bytes = [
            "package.json",
            "scripts/watch-node.mjs",
            "scripts/run-node.mjs",
        ]
        .map(|path| (selected.join(path), fs::read(selected.join(path)).unwrap()));
        let assert_preserved = || {
            assert!(
                fs::read(&config_path).unwrap() == config_bytes,
                "{variant}: config changed"
            );
            let current = get_environment("demo", &env, &repo).unwrap();
            assert_eq!(current.root, original.root);
            assert_eq!(current.created_at, original.created_at);
            assert_eq!(
                serde_json::to_value(&current.dev).unwrap(),
                serde_json::to_value(&original.dev).unwrap()
            );
            for (path, bytes) in &source_bytes {
                assert!(
                    fs::read(path).unwrap() == *bytes,
                    "{variant}: source changed"
                );
            }
            if variant == "missing-ui" {
                assert!(!selected.join("node_modules/vite").exists());
                assert!(!repo.join("node_modules/vite").exists());
            }
        };

        let rejected = run_ocm(&repo, &env, &["dev", "demo"]);
        assert!(!rejected.status.success(), "{variant}");
        assert!(
            stderr(&rejected).contains(expected),
            "{variant}: {}",
            stderr(&rejected)
        );
        assert!(stderr(&rejected).contains("--no-ui"));
        for path in ["gateway.json", "ui.json", "dashboard-attempts"] {
            assert!(!root.child(path).exists(), "{variant}: {path} started");
        }
        assert_eq!(read_source_watch_session(&root)["closed"], true);
        assert_preserved();

        let mut recovered = DevWatchFixture::spawn(&root, &repo, &env, &["dev", "demo", "--no-ui"]);
        let running = initial_ui_process(&root, "gateway", &mut recovered);
        assert_eq!(running["entrypoint"], "watch-node.mjs");
        assert_eq!(running["cwd"], gateway["cwd"]);
        assert!(read_source_watch_session(&root)["ui"].is_null());
        assert_preserved();
        let stopped = run_dev_stop(&repo, &env);
        assert!(stopped.status.success(), "{variant}: {}", stderr(&stopped));
        assert_eq!(recovered.wait_without_release().status.code(), Some(130));
        assert!(wait_for_process_exit(
            running["pid"].as_u64().unwrap() as u32,
            Duration::from_secs(3)
        ));
        assert!(!root.child("ui.json").exists());
        assert!(!root.child("dashboard-attempts").exists());
        assert_preserved();
    }
}

#[cfg(unix)]
#[test]
fn dev_ui_initial_handoff_and_owned_lifecycle() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (watch, outcome) in [
        (false, "stop"),
        (true, "sibling"),
        (false, "deadline"),
        (true, "raw"),
        (true, "crash"),
    ] {
        let root = TestDir::new("initial-ui-owner");
        let mut env = ocm_env(&root);
        let repo = prepare_initial_ui_repo(&root, &mut env);
        let repo_arg = path_string(&repo);
        let args = if watch {
            dev_watch(&["demo", "--repo", &repo_arg, "--watch", "--ui"])
        } else {
            dev_plain(&["demo", "--repo", &repo_arg, "--ui"])
        };
        if outcome == "deadline" {
            fs::write(root.child("dashboard-hold"), "hold").unwrap();
        }
        if outcome == "stop" {
            fs::write(root.child("ui-start-hold"), "hold").unwrap();
        }
        let mut controller = DevWatchFixture::spawn(&root, &repo, &env, &args);
        let gateway = initial_ui_process(&root, "gateway", &mut controller);
        if outcome == "stop" {
            let status = run_ocm(&repo, &env, &["dev", "status", "demo", "--json"]);
            assert!(status.status.success(), "{}", stderr(&status));
            let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
            assert_eq!(status["gatewayHealthReady"], true);
            assert_eq!(status["ui"]["processRunning"], true);
            assert_eq!(status["ui"]["httpReady"], false);
            fs::remove_file(root.child("ui-start-hold")).unwrap();
        }
        let ui = initial_ui_process(&root, "ui", &mut controller);
        let gateway_pid = gateway["pid"].as_u64().unwrap() as u32;
        let ui_pid = ui["pid"].as_u64().unwrap() as u32;
        let session = read_source_watch_session(&root);
        assert_eq!(session["watching"], watch);
        assert_eq!(session["ui"]["children"]["gateway"]["pid"], gateway_pid);
        assert_eq!(session["ui"]["children"]["ui"]["pid"], ui_pid);
        assert_eq!(session["ui"]["target"]["port"], ui["port"]);
        assert_eq!(gateway["cwd"], ui["cwd"]);
        assert_eq!(ui["cwd"], path_string(&fs::canonicalize(&repo).unwrap()));
        assert!(
            ui["args"]
                .as_array()
                .unwrap()
                .contains(&Value::String("--strictPort".to_string()))
        );
        let until = Instant::now() + Duration::from_secs(3);
        while !root.child("gateway-requests").exists() && Instant::now() < until {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            !root.child("dashboard-attempts").exists(),
            "Gateway health alone started a handoff"
        );
        if outcome == "stop" {
            let meta = get_environment("demo", &env, &repo).unwrap();
            let config_path = Path::new(&meta.root).join(".openclaw/openclaw.json");
            let config_before = fs::read(&config_path).unwrap();
            let mut config: Value = serde_json::from_slice(&config_before).unwrap();
            config["gateway"]["port"] = (meta.gateway_port.unwrap() + 300).into();
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let status = run_ocm(&repo, &env, &["dev", "status", "demo", "--json"]);
            fs::write(&config_path, config_before).unwrap();
            assert!(status.status.success(), "{}", stderr(&status));
            let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
            assert_eq!(status["gatewayPort"], gateway["port"]);
            assert_eq!(status["gatewayHealthReady"], true);
            assert_eq!(status["ui"]["processRunning"], true);
            assert_eq!(status["ui"]["httpReady"], true);
            assert_eq!(status["uiUrl"], status["ui"]["url"]);
            let raw = run_ocm(&repo, &env, &["dev", "status", "demo", "--raw"]);
            assert!(raw.status.success(), "{}", stderr(&raw));
            assert!(stdout(&raw).contains("ui_process_running=true"));
            assert!(stdout(&raw).contains("ui_http_ready=true"));
            assert!(stdout(&raw).contains("ui_url="));

            let session_path = source_watch_override_path(&root, "demo").with_extension("session");
            let saved_session = fs::read(&session_path).unwrap();
            let ui_requests = fs::read(root.child("ui-requests")).unwrap();
            for foreign_scope in [false, true] {
                let mut changed: Value = serde_json::from_slice(&saved_session).unwrap();
                if foreign_scope {
                    changed["processScope"] = "another-process-scope".into();
                } else {
                    changed["ui"]["children"]["ui"]["startedAt"] = "another-process-start".into();
                }
                let changed = serde_json::to_vec(&changed).unwrap();
                fs::write(&session_path, &changed).unwrap();
                let status = run_ocm(&repo, &env, &["dev", "status", "demo", "--json"]);
                let after = fs::read(&session_path).unwrap();
                fs::write(&session_path, &saved_session).unwrap();
                assert!(status.status.success(), "{}", stderr(&status));
                assert_eq!(after, changed, "status modified session metadata");
                let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
                let expected = if foreign_scope {
                    Value::Null
                } else {
                    Value::Bool(false)
                };
                assert_eq!(status["ui"]["processRunning"], expected);
                assert_eq!(status["ui"]["httpReady"], expected);
                if foreign_scope {
                    assert_eq!(status["sourceWatch"]["state"], "unknown");
                    assert!(status["ui"]["issue"].as_str().is_some());
                }
                assert_eq!(fs::read(root.child("ui-requests")).unwrap(), ui_requests);
            }
        }
        fs::write(root.child("gateway-document-ready"), "ready").unwrap();
        assert!(wait_for_path(
            &root.child("dashboard-attempt-1"),
            Duration::from_secs(5)
        ));
        if outcome == "deadline" {
            let helper = fs::read_to_string(root.child("dashboard-attempt-1"))
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap();
            thread::sleep(Duration::from_secs(31));
            assert!(
                process_is_alive(helper)
                    && process_is_alive(gateway_pid)
                    && process_is_alive(ui_pid)
            );
            let reused = run_ocm(&repo, &env, &args);
            assert!(reused.status.success(), "{}", stderr(&reused));
            assert!(stdout(&reused).contains("ui_url="));
            assert!(!stdout(&reused).contains("bootstrapToken"));
            assert_eq!(
                fs::read_to_string(root.child("dashboard-attempts"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            fs::remove_file(root.child("dashboard-hold")).unwrap();
            assert!(wait_for_process_exit(helper, Duration::from_secs(3)));
        }
        let until = Instant::now() + Duration::from_secs(3);
        while read_source_watch_session(&root)["ui"]["children"]
            .get("command")
            .is_some()
            && Instant::now() < until
        {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            read_source_watch_session(&root)["ui"]["children"]
                .get("command")
                .is_none()
        );
        let mismatch = run_ocm(
            &repo,
            &env,
            &if watch {
                dev_watch(&["demo", "--watch"])
            } else {
                dev_plain(&["demo"])
            },
        );
        assert!(!mismatch.status.success());
        assert!(stderr(&mismatch).contains("different UI mode"));
        if outcome == "raw" || outcome == "crash" {
            if outcome == "crash" {
                controller.crash_controller();
            } else {
                assert_eq!(
                    unsafe { libc::kill(ui_pid as libc::pid_t, libc::SIGKILL) },
                    0
                );
                let output = controller.wait_without_release();
                assert!(!output.status.success());
                assert!(stderr(&output).contains("terminated by signal"));
                let retained = read_source_watch_session(&root);
                assert_eq!(retained["ui"]["children"]["ui"]["pid"], ui_pid);
                assert!(retained["ui"]["children"].get("gateway").is_none());
            }
            let stopped = run_dev_stop(&repo, &env);
            assert!(!stopped.status.success());
            assert!(stderr(&stopped).contains(if outcome == "crash" {
                "source controller exited"
            } else {
                "terminated by signal"
            }));
            assert_eq!(read_source_watch_session(&root)["closed"], false);
            let session_path = source_watch_override_path(&root, "demo").with_extension("session");
            let retained = fs::read(&session_path).unwrap();
            let removed = run_ocm(&repo, &env, &["env", "destroy", "demo", "--yes"]);
            assert!(!removed.status.success());
            assert!(stderr(&removed).contains("unverified"));
            assert!(fs::read(session_path).unwrap() == retained);
        } else if outcome == "sibling" {
            fs::write(root.child("ui.exit"), "exit").unwrap();
            let output = controller.wait_without_release();
            assert_eq!(output.status.code(), Some(17));
            assert!(stdout(&output).contains("UI: http://127.0.0.1:"));
        } else {
            let stopped = run_dev_stop(&repo, &env);
            assert!(stopped.status.success(), "{}", stderr(&stopped));
            let output = controller.wait_without_release();
            assert_eq!(output.status.code(), Some(130));
            assert_eq!(
                stdout(&output).contains("UI: http://127.0.0.1:"),
                outcome != "deadline"
            );
            assert!(!stdout(&output).contains("synthetic-legacy"));
        }
        assert!(wait_for_process_exit(gateway_pid, Duration::from_secs(3)));
        assert!(wait_for_process_exit(ui_pid, Duration::from_secs(3)));
    }
}

#[cfg(unix)]
fn wait_for_ui_command_cleanup(root: &TestDir) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let session = read_source_watch_session(root);
        if session["ui"]["children"].get("command").is_none() && session["ui"]["pending"].is_null()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native helper ownership was not cleared"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn ui_dashboard_pid(root: &TestDir, attempt: usize) -> u32 {
    let path = root.child(format!("dashboard-attempt-{attempt}"));
    assert!(wait_for_path(&path, Duration::from_secs(5)));
    fs::read_to_string(path).unwrap().parse().unwrap()
}

#[cfg(unix)]
fn ui_handoff_directory(session: &Value) -> PathBuf {
    use sha2::{Digest, Sha256};
    let identity = serde_json::to_vec(&(
        &session["envName"],
        &session["envRoot"],
        &session["leaseId"],
    ))
    .unwrap();
    let digest = Sha256::digest(identity)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    PathBuf::from("/tmp").join(format!("ocm-dev-ui-{digest}"))
}

#[cfg(unix)]
#[test]
fn dev_ui_reuse_keeps_a_healthy_session_when_its_handoff_endpoint_is_absent() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("ui-reuse-without-handoff-endpoint");
    let mut env = ocm_env(&root);
    let repo = prepare_initial_ui_repo(&root, &mut env);
    let repo_arg = path_string(&repo);
    let args = dev_watch(&["demo", "--repo", &repo_arg, "--watch", "--ui"]);
    fs::write(root.child("gateway-document-ready"), "ready").unwrap();
    let mut controller = DevWatchFixture::spawn(&root, &repo, &env, &args);
    let gateway = initial_ui_process(&root, "gateway", &mut controller);
    let ui = initial_ui_process(&root, "ui", &mut controller);
    ui_dashboard_pid(&root, 1);
    wait_for_ui_command_cleanup(&root);
    let session = read_source_watch_session(&root);
    let socket_dir = ui_handoff_directory(&session);
    // Model the transport absent from pre-handoff controllers without changing
    // their shared session schema or ownership. The fixture still runs current
    // OCM for both processes.
    fs::remove_file(socket_dir.join("socket")).unwrap();
    fs::remove_dir(&socket_dir).unwrap();
    let mut caller = DevWatchFixture::spawn_caller(&root, &repo, &env, &args);
    let reused = caller.wait_without_release_for(Duration::from_secs(5));
    assert!(reused.status.success(), "{}", stderr(&reused));
    assert!(stdout(&reused).contains(&format!(
        "ui_url=http://127.0.0.1:{}/",
        ui["port"].as_u64().unwrap()
    )));
    assert!(stderr(&reused).contains("UI link unavailable"));
    assert!(!stdout(&reused).contains("bootstrapToken"));
    assert!(!stdout(&reused).contains("UI: "));
    assert_eq!(read_source_watch_session(&root), session);
    assert_eq!(
        fs::read_to_string(root.child("dashboard-attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    for process in [&gateway, &ui] {
        assert!(process_is_alive(process["pid"].as_u64().unwrap() as u32));
        let address = format!("127.0.0.1:{}", process["port"].as_u64().unwrap())
            .parse()
            .unwrap();
        assert!(std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_ok());
    }
    let stopped = run_dev_stop(&repo, &env);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(controller.wait_without_release().status.code(), Some(130));
    for process in [&gateway, &ui] {
        assert!(wait_for_process_exit(
            process["pid"].as_u64().unwrap() as u32,
            Duration::from_secs(3)
        ));
    }
    assert_eq!(read_source_watch_session(&root)["closed"], true);
    assert!(!socket_dir.exists());
}

#[cfg(unix)]
#[test]
fn dev_ui_reuse_gets_fresh_grants_without_restarting_components() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for watching in [false, true] {
        let root = TestDir::new("ui-fresh-reuse");
        let mut env = ocm_env(&root);
        let repo = prepare_initial_ui_repo(&root, &mut env);
        let repo_arg = path_string(&repo);
        let args = if watching {
            dev_watch(&["demo", "--repo", &repo_arg, "--watch", "--ui"])
        } else {
            dev_plain(&["demo", "--repo", &repo_arg, "--ui"])
        };
        fs::write(root.child("gateway-document-ready"), "ready").unwrap();
        let mut controller = DevWatchFixture::spawn(&root, &repo, &env, &args);
        let gateway = initial_ui_process(&root, "gateway", &mut controller);
        let ui = initial_ui_process(&root, "ui", &mut controller);
        ui_dashboard_pid(&root, 1);
        wait_for_ui_command_cleanup(&root);
        let initial = read_source_watch_session(&root);
        assert_eq!(initial["kind"], "ocm-source-ui-session-v1");
        assert_eq!(initial["watching"], watching);
        for attempt in [2, 3] {
            let mut caller = DevWatchFixture::spawn_caller(&root, &repo, &env, &args);
            let reused = caller.wait_without_release_for(Duration::from_secs(5));
            assert!(reused.status.success(), "{}", stderr(&reused));
            let output = stdout(&reused);
            let link = output
                .lines()
                .find_map(|line| line.strip_prefix("UI: "))
                .unwrap();
            assert!(link.contains(&format!("bootstrapToken=synthetic-owner-grant-{attempt}")));
            assert_eq!(
                url::Url::parse(link).unwrap().port(),
                Some(ui["port"].as_u64().unwrap() as u16)
            );
            assert!(!output.contains("synthetic-legacy"));
            let current = read_source_watch_session(&root);
            assert_eq!(current["controller"], initial["controller"]);
            assert_eq!(current["leaseId"], initial["leaseId"]);
            assert_eq!(current["ui"], initial["ui"]);
            for process in [&gateway, &ui] {
                assert!(process_is_alive(process["pid"].as_u64().unwrap() as u32));
            }
        }
        assert_eq!(
            fs::read_to_string(root.child("dashboard-attempts"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        let stopped = run_dev_stop(&repo, &env);
        assert!(stopped.status.success(), "{}", stderr(&stopped));
        let output = controller.wait_without_release();
        assert_eq!(output.status.code(), Some(130));
        assert!(stdout(&output).contains("synthetic-owner-grant-1"));
        assert!(!stdout(&output).contains("synthetic-owner-grant-2"));
        assert!(!stdout(&output).contains("synthetic-owner-grant-3"));
        assert!(!ui_handoff_directory(&initial).exists());
    }
}

#[cfg(unix)]
#[test]
fn dev_ui_reuse_discards_disconnected_callers_and_closes_pending_requests_on_stop() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("ui-reuse-owned-helper");
    let mut env = ocm_env(&root);
    let repo = prepare_initial_ui_repo(&root, &mut env);
    let repo_arg = path_string(&repo);
    let args = dev_plain(&["demo", "--repo", &repo_arg, "--ui"]);
    fs::write(root.child("gateway-document-ready"), "ready").unwrap();
    let mut controller = DevWatchFixture::spawn(&root, &repo, &env, &args);
    let gateway = initial_ui_process(&root, "gateway", &mut controller);
    let ui = initial_ui_process(&root, "ui", &mut controller);
    ui_dashboard_pid(&root, 1);
    wait_for_ui_command_cleanup(&root);
    fs::write(root.child("dashboard-hold"), "hold").unwrap();
    let mut requester = DevWatchFixture::spawn_caller(&root, &repo, &env, &args);
    let helper = ui_dashboard_pid(&root, 2);
    let session = read_source_watch_session(&root);
    assert_eq!(
        session["controller"]["pid"],
        controller.child.as_ref().unwrap().id()
    );
    assert_eq!(session["ui"]["children"]["command"]["pid"], helper);
    assert_eq!(session["ui"]["children"].as_object().unwrap().len(), 3);
    for crash in [false, true] {
        if crash {
            requester.crash_controller();
        }
        let mut busy = DevWatchFixture::spawn_caller(&root, &repo, &env, &args);
        let output = busy.wait_without_release_for(Duration::from_secs(5));
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("ui_url="));
        assert!(!stdout(&output).contains("bootstrapToken"));
        assert_eq!(read_source_watch_session(&root)["ui"], session["ui"]);
        assert!(process_is_alive(helper));
        assert_eq!(
            fs::read_to_string(root.child("dashboard-attempts"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
    fs::remove_file(root.child("dashboard-hold")).unwrap();
    assert!(wait_for_path(
        &root.child("dashboard-emitted-2"),
        Duration::from_secs(5)
    ));
    wait_for_ui_command_cleanup(&root);
    assert!(wait_for_process_exit(helper, Duration::from_secs(3)));
    let mut fresh = DevWatchFixture::spawn_caller(&root, &repo, &env, &args);
    let output = fresh.wait_without_release_for(Duration::from_secs(5));
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("synthetic-owner-grant-3"));
    assert!(!stdout(&output).contains("synthetic-owner-grant-2"));

    thread::scope(|scope| {
        // On a failed assertion, close the owner before scoped connectors join.
        let mut controller = controller;
        fs::write(root.child("dashboard-hold"), "hold").unwrap();
        let mut pending = DevWatchFixture::spawn_caller(&root, &repo, &env, &args);
        let helper = ui_dashboard_pid(&root, 4);
        let socket_dir = ui_handoff_directory(&session);
        assert!(socket_dir.join("socket").exists());
        let controller_pid = controller.child.as_ref().unwrap().id();
        assert_eq!(
            unsafe { libc::kill(controller_pid as i32, libc::SIGSTOP) },
            0
        );
        assert!(wait_for_process_stop(
            controller_pid,
            Duration::from_secs(3)
        ));
        const QUEUED_CALLERS: usize = 8;
        let ready = Arc::new(std::sync::Barrier::new(QUEUED_CALLERS + 1));
        let (completed, results) = std::sync::mpsc::channel();
        let queued = (0..QUEUED_CALLERS)
            .map(|_| {
                let endpoint = socket_dir.join("socket");
                let ready = Arc::clone(&ready);
                let completed = completed.clone();
                scope.spawn(move || {
                    ready.wait();
                    let connection =
                        std::os::unix::net::UnixStream::connect(endpoint).and_then(|stream| {
                            stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                            Ok(stream)
                        });
                    let _ = completed.send(connection.as_ref().map(|_| ()).map_err(|e| e.kind()));
                    connection
                })
            })
            .collect::<Vec<_>>();
        drop(completed);
        ready.wait();
        let mut admitted = 0;
        while let Ok(result) = results.recv_timeout(Duration::from_millis(500)) {
            match result {
                Ok(()) => admitted += 1,
                // Darwin refuses a full backlog; Linux waits for admission.
                Err(error) => assert_eq!(error, std::io::ErrorKind::ConnectionRefused),
            }
        }
        assert!(
            admitted > 0 && admitted < QUEUED_CALLERS,
            "request queue did not fill"
        );
        let mut stopper =
            DevWatchFixture::spawn_caller(&root, &repo, &env, &["dev", "stop", "demo", "--json"]);
        let output = pending.wait_without_release_for(Duration::from_secs(5));
        assert!(!stdout(&output).contains("bootstrapToken"));
        assert!(!socket_dir.exists());
        let deadline = Instant::now() + Duration::from_secs(5);
        while queued.iter().any(|client| !client.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        for client in queued {
            assert!(
                client.is_finished(),
                "queued connection survived listener closure"
            );
            if let Ok(mut stream) = client.join().unwrap() {
                assert!(match stream.read(&mut [0; 1]) {
                    Ok(0) => true,
                    Err(error) => error.kind() == std::io::ErrorKind::ConnectionReset,
                    _ => false,
                });
            }
        }
        assert!(std::os::unix::net::UnixStream::connect(socket_dir.join("socket")).is_err());
        for process in [&gateway, &ui] {
            assert!(wait_for_process_exit(
                process["pid"].as_u64().unwrap() as u32,
                Duration::from_secs(5)
            ));
        }
        assert!(
            process_is_alive(helper),
            "stop shortened the original native helper budget"
        );
        assert!(
            stopper
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none()
        );
        let stopping = read_source_watch_session(&root);
        assert_eq!(stopping["closed"], false);
        assert_eq!(stopping["ui"]["children"]["command"]["pid"], helper);
        fs::remove_file(root.child("dashboard-hold")).unwrap();
        assert!(wait_for_path(
            &root.child("dashboard-emitted-4"),
            Duration::from_secs(5)
        ));
        let stopped = stopper.wait_without_release();
        assert!(stopped.status.success(), "{}", stderr(&stopped));
        let output = controller.wait_without_release();
        assert_eq!(output.status.code(), Some(130));
        assert!(stdout(&output).contains("synthetic-owner-grant-1"));
        for attempt in 2..=4 {
            assert!(!stdout(&output).contains(&format!("synthetic-owner-grant-{attempt}")));
        }
        assert!(wait_for_process_exit(helper, Duration::from_secs(3)));
        let closed = read_source_watch_session(&root);
        assert_eq!(closed["closed"], true);
        assert!(closed["ui"]["children"].as_object().unwrap().is_empty());
        assert!(closed["ui"]["pending"].is_null());
        assert!(!source_watch_override_path(&root, "demo").exists());
        assert!(!socket_dir.exists());
    });
}

#[cfg(unix)]
#[test]
fn dev_ui_claims_an_address_before_the_listener_starts() {
    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("initial-ui-first-claim");
    let mut env = ocm_env(&root);
    let repo = prepare_initial_ui_repo(&root, &mut env);
    fs::write(root.child("ui-start-hold"), "hold").unwrap();
    fs::write(root.child("gateway-start-hold"), "hold").unwrap();
    let mut first = DevWatchFixture::spawn(
        &root,
        &repo,
        &env,
        &[
            "dev",
            "first",
            "--repo",
            &path_string(&repo),
            "--ui",
            "--no-watch",
        ],
    );
    assert!(wait_for_path(
        &source_watch_override_path(&root, "first"),
        Duration::from_secs(10)
    ));
    let session_file = source_watch_override_path(&root, "first").with_extension("session");
    let session: Value = serde_json::from_slice(&fs::read(&session_file).unwrap()).unwrap();
    let gateway_port = url::Url::parse(session["ui"]["target"]["gatewayUrl"].as_str().unwrap())
        .unwrap()
        .port()
        .unwrap();
    // Other tests' isolated stores probe this same OS port after selection.
    // Hold one such transient conflict until the fake actually observes it.
    let until = Instant::now() + Duration::from_secs(5);
    let probe = loop {
        match std::net::TcpListener::bind(("127.0.0.1", gateway_port)) {
            Ok(probe) => break probe,
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < until =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("failed to hold the selected Gateway port: {error}"),
        }
    };
    fs::remove_file(root.child("gateway-start-hold")).unwrap();
    assert!(wait_for_path(
        &root.child("gateway-listen-error"),
        Duration::from_secs(5)
    ));
    assert_eq!(
        fs::read_to_string(root.child("gateway-listen-error")).unwrap(),
        "EADDRINUSE"
    );
    assert!(!root.child("gateway.json").exists());
    drop(probe);
    let first_gateway = initial_ui_process(&root, "gateway", &mut first);
    let port = session["ui"]["target"]["port"].as_u64().unwrap() as u16;
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "held UI already listened"
    );
    let second_root = TestDir::new("initial-ui-second-claim");
    let mut second_env = ocm_env(&second_root);
    let second_repo = prepare_initial_ui_repo(&second_root, &mut second_env);
    second_env.insert("OCM_HOME".to_string(), env["OCM_HOME"].clone());
    let mut second = DevWatchFixture::spawn(
        &second_root,
        &second_repo,
        &second_env,
        &[
            "dev",
            "second",
            "--repo",
            &path_string(&second_repo),
            "--ui",
            "--no-watch",
        ],
    );
    let second_gateway = initial_ui_process(&second_root, "gateway", &mut second);
    let second_ui = initial_ui_process(&second_root, "ui", &mut second);
    let second_port = second_ui["port"].as_u64().unwrap() as u16;
    assert_ne!(second_port, port);
    fs::remove_file(root.child("ui-start-hold")).unwrap();
    let first_ui = initial_ui_process(&root, "ui", &mut first);
    for name in ["first", "second"] {
        let stopped = run_named_dev_stop(&repo, &env, name);
        assert!(stopped.status.success(), "{}", stderr(&stopped));
    }
    assert_eq!(first.wait_without_release().status.code(), Some(130));
    assert_eq!(second.wait_without_release().status.code(), Some(130));
    for process in [first_gateway, first_ui, second_gateway, second_ui] {
        assert!(wait_for_process_exit(
            process["pid"].as_u64().unwrap() as u32,
            Duration::from_secs(3)
        ));
    }
    for (name, expected_port) in [("first", port), ("second", second_port)] {
        assert_eq!(
            get_environment(name, &env, &repo).unwrap().dev_ui_port,
            Some(u32::from(expected_port))
        );
    }

    let saved = get_environment("second", &env, &repo).unwrap();
    let config_path = Path::new(&saved.root).join(".openclaw/openclaw.json");
    let config_before = fs::read(&config_path).unwrap();
    for name in ["gateway.json", "ui.json"] {
        fs::remove_file(second_root.child(name)).unwrap();
    }
    let occupied = std::net::TcpListener::bind(("127.0.0.1", second_port)).unwrap();
    let mut busy = DevWatchFixture::spawn(
        &second_root,
        &second_repo,
        &second_env,
        &["dev", "second", "--ui", "--no-watch"],
    );
    let rejected = busy.wait_without_release();
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("occupied or reserved"));
    assert_eq!(fs::read(&config_path).unwrap(), config_before);
    assert_eq!(
        get_environment("second", &env, &repo).unwrap().dev_ui_port,
        saved.dev_ui_port
    );
    assert!(!second_root.child("gateway.json").exists());
    assert!(!second_root.child("ui.json").exists());
    assert!(!second_root.child("ui-listen-error").exists());
    assert!(!second_root.child("dashboard-attempts").exists());
    let rejected_session: Value =
        serde_json::from_slice(&fs::read(&busy.session).unwrap()).unwrap();
    assert_eq!(rejected_session["closed"], true);
    assert!(
        rejected_session["ui"]["children"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(rejected_session["ui"]["pending"].is_null());
    drop(occupied);

    // Both original sessions are closed. A new generation must use the same
    // environment's reservation even when its backend watch mode changes.
    let mut restarted = DevWatchFixture::spawn(
        &second_root,
        &second_repo,
        &second_env,
        &["dev", "second", "--watch", "--ui"],
    );
    let gateway = initial_ui_process(&second_root, "gateway", &mut restarted);
    let ui = initial_ui_process(&second_root, "ui", &mut restarted);
    assert_eq!(ui["port"].as_u64().unwrap(), u64::from(second_port));
    let current = get_environment("second", &env, &repo).unwrap();
    assert_eq!(current.created_at, saved.created_at);
    assert_eq!(current.root, saved.root);
    assert_eq!(current.dev_ui_port, saved.dev_ui_port);
    assert_eq!(fs::read(&config_path).unwrap(), config_before);
    let stopped = run_named_dev_stop(&second_repo, &second_env, "second");
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(restarted.wait_without_release().status.code(), Some(130));
    for process in [gateway, ui] {
        assert!(wait_for_process_exit(
            process["pid"].as_u64().unwrap() as u32,
            Duration::from_secs(3)
        ));
    }
}

#[cfg(unix)]
#[test]
fn dev_ui_port_reservation_waits_for_metadata_mutations() {
    use fs2::FileExt;

    let _serial = INITIAL_UI_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = TestDir::new("dev-ui-port-operation-lock");
    let mut env = ocm_env(&root);
    let repo = prepare_initial_ui_repo(&root, &mut env);
    fs::write(root.child("ui-probe-hold"), "hold").unwrap();
    let mut controller = DevWatchFixture::spawn(
        &root,
        &repo,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--ui",
            "--no-watch",
        ],
    );
    assert!(wait_for_path(
        &root.child("ui-probe-ready"),
        Duration::from_secs(10)
    ));
    assert!(read_source_watch_session(&root)["ui"]["children"]["command"]["pid"].is_number());
    let operation = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.child("ocm-home/locks/environments/demo.lock"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match operation.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("UI prerequisite probe retained the operation lock: {error}"),
        }
    }
    let mut stale = get_environment("demo", &env, &repo).unwrap();
    let identity = (stale.root.clone(), stale.created_at);
    fs::remove_file(root.child("ui-probe-hold")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_source_watch_session(&root)["ui"]["children"]
        .get("command")
        .is_some()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        read_source_watch_session(&root)["ui"]["children"]
            .get("command")
            .is_none()
    );
    thread::sleep(Duration::from_millis(250));
    assert!(
        !root.child("gateway.json").exists() && !root.child("ui.json").exists(),
        "UI reservation bypassed the held operation boundary"
    );
    assert!(read_source_watch_session(&root)["ui"]["target"].is_null());
    assert_eq!(
        get_environment("demo", &env, &repo).unwrap().dev_ui_port,
        None
    );
    stale.last_used_at = Some(now_utc());
    let touched_at = stale.last_used_at;
    save_environment(stale, &env, &repo).unwrap();
    FileExt::unlock(&operation).unwrap();

    let gateway = initial_ui_process(&root, "gateway", &mut controller);
    let ui = initial_ui_process(&root, "ui", &mut controller);
    let saved = get_environment("demo", &env, &repo).unwrap();
    assert_eq!(saved.last_used_at, touched_at);
    assert_eq!((saved.root, saved.created_at), identity);
    assert_eq!(
        saved.dev_ui_port,
        ui["port"].as_u64().map(|port| port as u32)
    );
    assert_eq!(
        read_source_watch_session(&root)["ui"]["target"]["port"],
        ui["port"]
    );
    let stopped = run_dev_stop(&repo, &env);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(controller.wait_without_release().status.code(), Some(130));
    for process in [gateway, ui] {
        assert!(wait_for_process_exit(
            process["pid"].as_u64().unwrap() as u32,
            Duration::from_secs(3)
        ));
    }
}

#[test]
fn dev_rejects_conflicting_modes_before_creating_environment_state() {
    let root = TestDir::new("dev-conflicting-modes");
    let env = ocm_env(&root);
    let repo = init_openclaw_repo(&root);
    let repo_arg = path_string(&repo);
    for (flags, expected) in [
        (vec!["--watch", "--no-watch"], "--watch with --no-watch"),
        (vec!["--ui", "--no-ui"], "--ui with --no-ui"),
        (vec!["--watch", "--service"], "--watch with --service"),
        (vec!["--ui", "--service"], "--ui with --service"),
        (vec!["--force", "--no-watch"], "backend watching enabled"),
        (vec!["--force", "--service"], "backend watching enabled"),
    ] {
        let mut args = vec!["dev", "demo", "--repo", &repo_arg];
        args.extend(flags);
        let result = run_ocm(root.path(), &env, &args);
        assert!(!result.status.success());
        assert!(stderr(&result).contains(expected), "{}", stderr(&result));
        assert!(!source_watch_override_path(&root, "demo").exists());
        assert!(
            !source_watch_override_path(&root, "demo")
                .with_extension("session")
                .exists()
        );
        assert!(!repo.join(".worktrees/demo").exists());
    }
    let listed = run_ocm(root.path(), &env, &["env", "list", "--json"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&listed)).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn dev_command_borrows_checkout_bootstraps_config_and_runs_gateway() {
    let root = TestDir::new("dev-command-run");
    let repo = init_openclaw_repo(&root);
    let canonical_repo = fs::canonicalize(&repo).unwrap();
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let registered = git_worktree_paths(&repo);

    let run = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--no-watch",
            "--no-ui",
        ],
    );
    assert!(run.status.success(), "{}", stderr(&run));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    let config_path = PathBuf::from(show_json["configPath"].as_str().unwrap());
    let workspace_dir = PathBuf::from(show_json["workspaceDir"].as_str().unwrap());

    assert_eq!(show_json["devRepoRoot"], path_string(&canonical_repo));
    assert_eq!(worktree_root, canonical_repo);
    assert_eq!(git_worktree_paths(&repo), registered);
    assert!(!repo.join(".worktrees").exists());
    assert!(matches!(
        get_environment("demo", &env, &cwd).unwrap().dev,
        Some(EnvDevMeta::Borrowed { .. })
    ));
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

    let node_log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(!root.child("pnpm.log").exists());
    assert!(node_log.contains("scripts/run-node.mjs gateway run --port"));
    assert!(node_log.contains(&path_string(&worktree_root)));
    assert!(node_log.contains(&path_string(&config_path)));
    assert!(node_log.contains(&format!(
        "|bundled={}",
        path_string(&worktree_root.join("extensions"))
    )));
    assert!(node_log.contains(&format!("|devroot={}", path_string(&worktree_root))));
    let token = config["gateway"]["auth"]["token"].as_str().unwrap();
    assert_eq!(config["gateway"]["auth"]["mode"], "token");
    assert_eq!(token.len(), 64);
    assert!(!stdout(&run).contains(token) && !stderr(&run).contains(token));
    assert!(!stdout(&show).contains(token));
    let original = fs::read(&config_path).unwrap();
    let resumed = run_ocm(&cwd, &env, &["dev", "demo", "--no-watch", "--no-ui"]);
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert!(fs::read(&config_path).unwrap() == original);
    assert!(!stdout(&resumed).contains(token) && !stderr(&resumed).contains(token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let auth = serde_json::json!({"mode": "token", "token": {"source": "env", "provider": "default", "id": "AUTHORED_TOKEN"}});
        let mut authored = config.clone();
        authored["gateway"]["auth"] = auth.clone();
        let raw = format!(
            "// authored after creation\n{}\n",
            serde_json::to_string_pretty(&authored).unwrap()
        );
        fs::write(&config_path, &raw).unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        let resumed = run_ocm(&cwd, &env, &["dev", "demo", "--no-watch", "--no-ui"]);
        assert!(resumed.status.success(), "{}", stderr(&resumed));
        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "private config lost owner-only access on dev restart"
        );
        assert!(fs::read(&config_path).unwrap() == raw.as_bytes());
        let updated: Value = json5::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(updated["gateway"]["auth"], auth);
    }
}

#[cfg(unix)]
#[test]
fn dev_registration_rejects_a_busy_containing_environment_before_preparing_source() {
    let root = TestDir::new("dev-registration-operation-lock");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let parent = run_ocm(&cwd, &env, &["env", "create", "parent"]);
    assert!(parent.status.success(), "{}", stderr(&parent));
    let parent = get_environment("parent", &env, &cwd).unwrap();
    let source = Path::new(&parent.root).join(".openclaw/workspace/openclaw");
    fs::rename(init_openclaw_repo(&root), &source).unwrap();

    let lock = hold_environment_operation(&root, "parent");
    let registry = ocm::store::env_registry_path(&env, &cwd).unwrap();
    let before = fs::read(&registry).unwrap();
    let mut child = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &dev_plain(&["child", "--repo", &path_string(&source)]),
    );
    let rejected = child.wait_without_release();
    assert!(
        !rejected.status.success(),
        "dev published a child while its containing environment operation was locked"
    );
    assert!(
        stderr(&rejected).contains("parent"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
    assert!(!source.join(".worktrees/child").exists());
    assert!(!source.join(".git/worktrees/child").exists());
    assert!(!root.child("pnpm.log").exists());

    let independent = init_openclaw_repo(&root);
    let allowed = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["independent", "--repo", &path_string(&independent)]),
    );
    assert!(allowed.status.success(), "{}", stderr(&allowed));
    drop(lock);
    create_owned_dev_env(&source, "child", &env, &cwd);
    let created = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["child", "--repo", &path_string(&source)]),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let dev = get_environment("child", &env, &cwd).unwrap().dev.unwrap();
    assert_eq!(
        dev.repo_root(),
        path_string(&fs::canonicalize(&source).unwrap())
    );
    assert!(Path::new(dev.source_root()).join(".git").exists());
    let _child_operation = hold_environment_operation(&root, "child");
    let before_nested = fs::read(&registry).unwrap();
    let mut nested = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &dev_plain(&["grandchild", "--repo", dev.source_root()]),
    );
    let rejected = nested.wait_without_release();
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("environment child has an operation in progress"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(fs::read(&registry).unwrap(), before_nested);
    assert!(
        !Path::new(dev.source_root())
            .join(".worktrees/grandchild")
            .exists()
    );
    // A sibling uses the common repository without depending on child's assets.
    let sibling = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["sibling", "--repo", &path_string(&source)]),
    );
    assert!(sibling.status.success(), "{}", stderr(&sibling));
}

#[test]
fn dev_registration_failures_preserve_source_and_existing_worktrees() {
    let root = TestDir::new("dev-registration-cleanup");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let occupied = root.child("occupied");
    fs::create_dir_all(&occupied).unwrap();
    fs::write(occupied.join("sentinel"), "keep").unwrap();
    for name in ["fresh", "reused"] {
        let worktree = repo.join(".worktrees").join(name);
        if name == "reused" {
            let added = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "add", "--detach"])
                .arg(&worktree)
                .output()
                .unwrap();
            assert!(added.status.success(), "{}", stderr(&added));
        }
        let rejected = run_ocm(
            &cwd,
            &env,
            &dev_plain(&[
                name,
                "--repo",
                &path_string(&repo),
                "--root",
                &path_string(&occupied),
            ]),
        );
        assert!(!rejected.status.success());
        assert!(
            stderr(&rejected).contains("root already exists and is not empty"),
            "{}",
            stderr(&rejected)
        );
        assert_eq!(worktree.exists(), name == "reused");
        assert_eq!(
            repo.join(".git/worktrees").join(name).exists(),
            name == "reused"
        );
        assert!(get_environment(name, &env, &cwd).is_err());
    }
    // Let daemon preflight pass so the corrupt state fails after publication.
    env.insert(
        "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
        "unsupported".to_string(),
    );
    let state = ocm::store::supervisor_state_path(&env, &cwd).unwrap();
    fs::create_dir_all(&state).unwrap();
    let sync_failed = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["published", "--repo", &path_string(&repo)]),
    );
    assert!(!sync_failed.status.success());
    let published = get_environment("published", &env, &cwd)
        .unwrap()
        .dev
        .unwrap();
    assert_eq!(
        published.source_root(),
        path_string(&fs::canonicalize(&repo).unwrap())
    );
    assert!(!repo.join(".worktrees/published").exists());
    assert!(state.is_dir());
    #[cfg(unix)]
    assert!(
        stderr(&sync_failed).contains("directory"),
        "{}",
        stderr(&sync_failed)
    );
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
    register_owned_dev_env(&repo, &worktree_root, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
fn dev_borrowed_linked_checkout_uses_explicit_and_enclosing_source() {
    let root = TestDir::new("dev-borrowed-linked");
    let repo = init_openclaw_repo(&root);
    for destination in [".worktrees/input", ".worktrees/other"] {
        let linked = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "add", "--detach", destination])
            .output()
            .unwrap();
        assert!(linked.status.success(), "{}", stderr(&linked));
    }
    let source = fs::canonicalize(repo.join(".worktrees/input")).unwrap();
    let source_arg = path_string(&source);
    fs::write(source.join("scripts/run-node.mjs"), "// current edits\n").unwrap();
    let legacy_path = source.join(".worktrees/demo");
    fs::create_dir_all(&legacy_path).unwrap();
    fs::write(legacy_path.join("SENTINEL"), "untracked source\n").unwrap();
    fs::create_dir_all(source.join("node_modules/local")).unwrap();
    fs::write(
        source.join("node_modules/local/entry.js"),
        "// retained dependency\n",
    )
    .unwrap();
    let registered = git_worktree_paths(&repo);
    let nested = source.join("scripts");
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    for (name, cwd, args) in [
        ("demo", &repo, dev_plain(&["demo", "--repo", &source_arg])),
        ("enclosing", &nested, dev_plain(&["enclosing"])),
    ] {
        let created = run_ocm(cwd, &env, &args);
        assert!(created.status.success(), "{}", stderr(&created));
        let meta = get_environment(name, &env, cwd).unwrap();
        assert_eq!(
            meta.dev.unwrap().borrowed_source_root(),
            Some(source_arg.as_str())
        );
    }
    let resumed = run_ocm(&nested, &env, &dev_plain(&["demo"]));
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let node_before = fs::read(root.child("node.log")).unwrap();
    let repo_arg = path_string(&repo);
    for args in [
        dev_plain(&["demo"]),
        dev_plain(&["demo", "--repo", &repo_arg]),
    ] {
        let changed = run_ocm(&repo, &env, &args);
        assert!(!changed.status.success());
        assert!(stderr(&changed).contains("cannot change the repo"));
    }
    let git_path = source.join(".git");
    let original_git = fs::read(&git_path).unwrap();
    fs::copy(repo.join(".worktrees/other/.git"), &git_path).unwrap();
    let wrong_backlink = run_ocm(&nested, &env, &dev_plain(&["demo"]));
    fs::write(&git_path, &original_git).unwrap();
    assert!(!wrong_backlink.status.success());
    assert!(
        stderr(&wrong_backlink).contains("not the registered root"),
        "{}",
        stderr(&wrong_backlink)
    );
    let common = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .unwrap();
    assert!(common.status.success(), "{}", stderr(&common));
    fs::write(&git_path, format!("gitdir: {}\n", stdout(&common).trim())).unwrap();
    let stale_slot = run_ocm(&nested, &env, &dev_plain(&["demo"]));
    fs::write(&git_path, original_git).unwrap();
    assert!(!stale_slot.status.success());
    assert!(
        stderr(&stale_slot).contains("not the registered root"),
        "{}",
        stderr(&stale_slot)
    );
    assert_eq!(fs::read(root.child("node.log")).unwrap(), node_before);
    let prefix = format!("{source_arg}|");
    assert!(
        String::from_utf8(node_before)
            .unwrap()
            .lines()
            .all(|line| line.starts_with(&prefix))
    );
    for name in ["demo", "enclosing"] {
        let removed = run_ocm(&repo, &env, &["env", "remove", name]);
        assert!(removed.status.success(), "{}", stderr(&removed));
    }
    assert_eq!(git_worktree_paths(&repo), registered);
    assert_eq!(
        fs::read_to_string(source.join("scripts/run-node.mjs")).unwrap(),
        "// current edits\n"
    );
    assert_eq!(
        fs::read_to_string(legacy_path.join("SENTINEL")).unwrap(),
        "untracked source\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("node_modules/local/entry.js")).unwrap(),
        "// retained dependency\n"
    );
}

#[test]
fn dev_borrowed_missing_or_redirected_source_is_not_recreated() {
    let root = TestDir::new("dev-borrowed-missing");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let created = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let meta = get_environment("demo", &env, &cwd).unwrap();
    let binding = meta.dev;
    let retained = root.child("retained-source");
    fs::rename(&repo, &retained).unwrap();
    fs::remove_file(root.child("node.log")).unwrap();
    for args in [
        dev_plain(&["demo"]),
        vec!["env", "resolve", "demo", "--json"],
    ] {
        let missing = run_ocm(&cwd, &env, &args);
        assert!(!missing.status.success());
        assert!(
            stderr(&missing).contains("restore that checkout"),
            "{}",
            stderr(&missing)
        );
    }
    assert!(!repo.exists());
    let watch_path = source_watch_override_path(&root, "demo");
    let metadata = [
        ocm::store::env_registry_path(&env, &cwd).unwrap(),
        Path::new(&meta.root).join(".openclaw/openclaw.json"),
        watch_path.with_extension("session"),
        watch_path,
        root.child("pnpm.log"),
    ];
    let metadata_before = metadata.each_ref().map(|path| fs::read(path).ok());
    let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let source = binding.as_ref().unwrap().source_root();
    assert_eq!(status["repoRoot"].as_str(), Some(source));
    assert_eq!(status["worktreeRoot"].as_str(), Some(source));
    assert_eq!(
        metadata.each_ref().map(|path| fs::read(path).ok()),
        metadata_before
    );
    assert!(
        !repo.exists(),
        "passive status recreated the borrowed source"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&retained, &repo).unwrap();
        let redirected = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
        assert!(!redirected.status.success());
        assert!(stderr(&redirected).contains("now resolves to a different checkout"));
        fs::remove_file(&repo).unwrap();
    }
    assert!(!root.child("node.log").exists());
    assert_eq!(get_environment("demo", &env, &cwd).unwrap().dev, binding);
    let removed = run_ocm(&cwd, &env, &["env", "remove", "demo"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(retained.join("scripts/run-node.mjs").exists());
    assert!(!repo.exists());
}

#[test]
fn dev_borrowed_dependencies_are_prepared_only_on_first_creation() {
    for mode in [None, Some("--watch"), Some("--service")] {
        let root = TestDir::new("dev-borrowed-dependencies");
        let repo = init_openclaw_repo(&root);
        declare_source_tooling(&repo);
        fs::write(repo.join("pnpm-lock.yaml"), "frozen fixture\n").unwrap();
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env_with_gateway_admission(&root);
        install_probe_aware_fake_dev_runners(&root, &mut env);
        install_frozen_source_dependency_runner(&root);
        let repo_arg = path_string(&repo);
        let mut args = vec!["dev", "demo", "--repo", &repo_arg];
        match mode {
            None => args.extend(["--no-watch", "--no-ui"]),
            Some("--watch") => args.extend(["--watch", "--no-ui"]),
            Some(mode) => args.push(mode),
        }
        let created = run_ocm(&cwd, &env, &args);
        assert!(created.status.success(), "{mode:?}: {}", stderr(&created));
        let install_log = fs::read(root.child("pnpm.log")).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&install_log)
                .lines()
                .filter(|line| line.contains("|install --frozen-lockfile"))
                .count(),
            1
        );
        assert!(repo.join("node_modules/tsx/entry.mjs").is_file());
        fs::remove_file(repo.join("node_modules/tsx/entry.mjs")).unwrap();
        let before = serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap();
        let node_before = fs::read(root.child("node.log")).ok();
        let mut attempts = vec![args.clone()];
        if mode.is_none() {
            attempts.extend([
                vec!["dev", "demo", "--ui", "--no-watch"],
                vec!["dev", "demo", "--watch", "--ui"],
            ]);
        }
        for args in attempts {
            let refused = run_ocm(&cwd, &env, &args);
            assert!(!refused.status.success());
            assert!(
                stderr(&refused).contains("pnpm install --frozen-lockfile"),
                "{}",
                stderr(&refused)
            );
            assert_eq!(fs::read(root.child("pnpm.log")).unwrap(), install_log);
            assert_eq!(fs::read(root.child("node.log")).ok(), node_before);
            assert_eq!(
                serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap(),
                before
            );
        }
        assert!(!repo.join("node_modules/tsx/entry.mjs").exists());
        assert_eq!(
            fs::read_to_string(repo.join("pnpm-lock.yaml")).unwrap(),
            "frozen fixture\n"
        );
    }
}

#[test]
fn dev_dependencies_reuse_flat_source_tooling_without_running_it() {
    let root = TestDir::new("dev-dependencies-reuse");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_probe_aware_fake_dev_runners(&root, &mut env);
    let created = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let meta = get_environment("demo", &env, &cwd).unwrap();
    let worktree = PathBuf::from(meta.dev.unwrap().source_root());
    declare_source_tooling(&worktree);
    // The normal source runner does not require the watch-only package.
    write_resolvable_source_tool(&worktree, "tsx");
    write_resolvable_source_tool(&worktree, "tsdown");
    assert!(!worktree.join("node_modules/.pnpm").exists());
    assert!(!worktree.join("node_modules/.bin/tsx").exists());
    env.insert(
        "PNPM_CONFIG_MODULES_DIR".to_string(),
        path_string(&worktree.join("node_modules")),
    );
    fs::remove_file(root.child("node.log")).unwrap();

    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(log.contains("scripts/run-node.mjs gateway run"));
    assert!(!root.child("pnpm.log").exists());

    #[cfg(unix)]
    {
        let modules = worktree.join("node_modules");
        let linked_modules = worktree.join("installed-modules");
        fs::rename(&modules, &linked_modules).unwrap();
        std::os::unix::fs::symlink(&linked_modules, &modules).unwrap();
        fs::remove_file(root.child("node.log")).unwrap();
        let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
        assert!(resumed.status.success(), "{}", stderr(&resumed));
        assert!(!root.child("pnpm.log").exists());
        assert_eq!(fs::read_link(&modules).unwrap(), linked_modules);
    }
}

#[test]
fn dev_dependencies_reuse_configured_modules_before_native_linking() {
    let root = TestDir::new("dev-dependencies-configured-modules");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env_with_gateway_admission(&root);
    install_probe_aware_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    declare_source_tooling(&repo);
    for name in ["tsx", "tsdown", "chokidar"] {
        write_resolvable_source_tool(&repo, name);
    }
    // Older watcher packages use Node's legacy main entry instead of exports.
    let watcher_manifest = repo.join("node_modules/chokidar/package.json");
    fs::write(
        &watcher_manifest,
        br#"{"name":"chokidar","type":"module","main":"./entry.mjs"}"#,
    )
    .unwrap();
    let modules = repo.join("installed-modules");
    fs::rename(repo.join("node_modules"), &modules).unwrap();

    for key in [
        "PNPM_CONFIG_MODULES_DIR",
        "pnpm_config_modules_dir",
        "npm_config_modules_dir",
    ] {
        env.insert(key.to_string(), "installed-modules".to_string());
        let reused = run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        assert!(reused.status.success(), "{key}: {}", stderr(&reused));
        assert!(
            fs::read_to_string(root.child("node.log"))
                .unwrap()
                .contains("scripts/watch-node.mjs")
        );
        assert!(!repo.join("node_modules").exists());
        assert!(!root.child("pnpm.log").exists());
        assert!(get_environment("demo", &env, &cwd).unwrap().service_running);
        fs::remove_file(root.child("node.log")).unwrap();
        env.remove(key);
    }

    env.insert(
        "PNPM_CONFIG_MODULES_DIR".to_string(),
        "installed-modules".to_string(),
    );
    fs::remove_file(modules.join("chokidar/entry.mjs")).unwrap();
    let before = serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap();
    let broken = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
            "--no-ui",
        ],
    );
    assert!(!broken.status.success());
    assert!(stderr(&broken).contains("chokidar"));
    assert_eq!(
        serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap(),
        before
    );
    assert!(!repo.join("node_modules").exists());
    assert!(!root.child("node.log").exists());
    assert!(!root.child("pnpm.log").exists());

    fs::write(
        modules.join("chokidar/entry.mjs"),
        "throw new Error('Do not execute dependency code');\n",
    )
    .unwrap();
    fs::rename(&modules, repo.join("node_modules")).unwrap();
    // Match the source loader's local fallback when the configured tree is absent.
    let fallback = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
            "--no-ui",
        ],
    );
    assert!(fallback.status.success(), "{}", stderr(&fallback));
    assert!(!modules.exists());
    assert!(!root.child("pnpm.log").exists());

    fs::create_dir(&modules).unwrap();
    fs::rename(repo.join("node_modules/tsx"), modules.join("tsx")).unwrap();
    // An existing root still owns watcher/build tools while TSX comes from the override.
    let selected = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
            "--no-ui",
        ],
    );
    assert!(selected.status.success(), "{}", stderr(&selected));
    assert!(!repo.join("node_modules/tsx").exists());
    assert!(!root.child("pnpm.log").exists());
}

#[test]
fn dev_dependencies_bootstrap_with_a_frozen_lockfile_and_retry_after_failure() {
    let root = TestDir::new("dev-dependencies-frozen-install");
    let repo = init_openclaw_repo(&root);
    declare_source_tooling(&repo);
    let lockfile = "fixture lockfile must remain unchanged\n";
    fs::write(repo.join("pnpm-lock.yaml"), lockfile).unwrap();
    let staged = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["add", "package.json", "pnpm-lock.yaml", "scripts/tsx.mjs"])
        .output()
        .unwrap();
    assert!(staged.status.success(), "{}", stderr(&staged));
    let committed = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["commit", "-m", "declare source tooling"])
        .output()
        .unwrap();
    assert!(committed.status.success(), "{}", stderr(&committed));
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_probe_aware_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);
    install_frozen_source_dependency_runner(&root);
    env.insert("OCM_TEST_INSTALL_EXIT_CODE".to_string(), "42".to_string());

    let failed = run_ocm(
        &cwd,
        &env,
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
    );
    assert_eq!(failed.status.code(), Some(42), "{}", stderr(&failed));
    let meta = get_environment("demo", &env, &cwd).unwrap();
    let worktree = PathBuf::from(meta.dev.unwrap().source_root());
    assert!(!root.child("node.log").exists());
    assert!(!source_watch_override_path(&root, "demo").exists());
    assert_eq!(
        fs::read_to_string(worktree.join("pnpm-lock.yaml")).unwrap(),
        lockfile
    );

    env.remove("OCM_TEST_INSTALL_EXIT_CODE");
    let retried = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    assert!(retried.status.success(), "{}", stderr(&retried));
    let log = fs::read_to_string(root.child("pnpm.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line
                .ends_with("|install --frozen-lockfile --reporter=ndjson --loglevel=debug"))
            .count(),
        2
    );
    assert_eq!(
        fs::read_to_string(worktree.join("pnpm-lock.yaml")).unwrap(),
        lockfile
    );
    assert_eq!(
        fs::read_to_string(repo.join("pnpm-lock.yaml")).unwrap(),
        lockfile
    );
    assert!(
        fs::read_to_string(root.child("node.log"))
            .unwrap()
            .contains("gateway run")
    );

    fs::remove_dir_all(worktree.join("node_modules")).unwrap();
    fs::remove_file(root.child("pnpm.log")).unwrap();
    env.insert(
        "PNPM_CONFIG_MODULES_DIR".to_string(),
        "installed-modules".to_string(),
    );
    let configured = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    assert!(configured.status.success(), "{}", stderr(&configured));
    assert!(
        worktree
            .join("installed-modules/tsx/package.json")
            .is_file()
    );
    assert!(!worktree.join("node_modules").exists());
    assert!(
        fs::read_to_string(root.child("pnpm.log"))
            .unwrap()
            .contains("|install --frozen-lockfile")
    );
    assert_eq!(
        fs::read_to_string(worktree.join("pnpm-lock.yaml")).unwrap(),
        lockfile
    );
}

#[cfg(unix)]
#[test]
fn dev_dependencies_preserve_reported_lifecycle_uncertainty() {
    for (label, lifecycle_exit, installer_exit, optional, retained) in [
        ("ordinary", Some(23), 1, false, false),
        ("signal", Some(-1), 1, false, true),
        ("optional-signal", Some(-1), 0, true, true),
        ("incomplete", None, 1, false, true),
        ("wrapper-signal", Some(0), 137, false, true),
    ] {
        let root = TestDir::new(&format!("dev-install-reporter-{label}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        install_probe_aware_fake_dev_runners(&root, &mut env);
        create_owned_dev_env(&repo, "demo", &env, &cwd);
        let created = run_ocm(
            &cwd,
            &env,
            &dev_plain(&["demo", "--repo", &path_string(&repo)]),
        );
        assert!(created.status.success(), "{label}: {}", stderr(&created));
        fs::remove_file(root.child("node.log")).unwrap();
        let meta = get_environment("demo", &env, &cwd).unwrap();
        let source = Path::new(meta.dev.as_ref().unwrap().source_root());
        declare_source_tooling(source);
        fs::write(source.join("pnpm-lock.yaml"), "retained frozen lock\n").unwrap();

        // These are the pinned reporter's start/exit shapes. In particular, a
        // signaled script reports -1 while the installer itself can return 1 or,
        // for an optional build, 0. Ordinary acknowledged failures stay usable.
        let record = |payload: Value| {
            let mut value = serde_json::json!({
                "time": 1, "hostname": "private-fixture-host", "pid": 42,
                "name": "pnpm:lifecycle", "depPath": "private-fixture-dependency",
                "stage": "install", "wd": "/private-fixture-cwd",
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(payload.as_object().unwrap().clone());
            value.to_string()
        };
        let mut records = vec![
            record(serde_json::json!({"script": "fixture install", "optional": optional})),
            record(serde_json::json!({"line": "fixture install diagnostic", "stdio": "stderr"})),
        ];
        if let Some(code) = lifecycle_exit {
            records.push(record(
                serde_json::json!({"exitCode": code, "optional": optional}),
            ));
        }
        let output = records
            .into_iter()
            .map(|line| format!("printf '%s\\n' '{line}' >&2\n"))
            .collect::<String>();
        write_executable_script(
            &root.child("fake-dev-bin/pnpm"),
            &format!("#!/bin/sh\n{output}exit {installer_exit}\n"),
        );
        let failed = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
        assert!(!failed.status.success(), "{label}: {}", stderr(&failed));
        assert!(
            stderr(&failed).contains("fixture install diagnostic"),
            "{label}"
        );
        let rendered = format!("{}{}", stdout(&failed), stderr(&failed));
        assert!(!rendered.contains("private-fixture-host"), "{label}");
        assert!(!rendered.contains("private-fixture-dependency"), "{label}");
        assert!(!rendered.contains("private-fixture-cwd"), "{label}");
        assert!(!root.child("node.log").exists(), "{label}");
        assert_eq!(
            fs::read_to_string(source.join("pnpm-lock.yaml")).unwrap(),
            "retained frozen lock\n"
        );
        let session = read_source_watch_session(&root);
        assert_eq!(session["closed"], !retained, "{label}");
        if retained {
            assert!(session["child"]["pid"].is_number(), "{label}");
            assert!(
                session["completion"]["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("cleanup is unverified")),
                "{label}"
            );
            let session_path = source_watch_override_path(&root, "demo").with_extension("session");
            let before = fs::read(&session_path).unwrap();
            // All future fixture commands are finite even if admission regresses.
            for args in [
                vec!["dev", "stop", "demo"],
                vec!["env", "destroy", "demo", "--yes"],
                dev_watch(&["demo", "--watch"]),
            ] {
                let refused = run_ocm(&cwd, &env, &args);
                assert!(!refused.status.success(), "{label}: {args:?}");
                assert_eq!(
                    fs::read(&session_path).unwrap(),
                    before,
                    "{label}: {args:?}"
                );
                assert!(source.is_dir() && Path::new(&meta.root).is_dir(), "{label}");
            }
        } else {
            assert!(!source_watch_override_path(&root, "demo").exists());
            install_frozen_source_dependency_runner(&root);
            let retried = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
            assert!(retried.status.success(), "{label}: {}", stderr(&retried));
        }
    }
}

#[test]
fn dev_dependencies_reject_unready_borrowed_source_before_service_changes() {
    let root = TestDir::new("dev-dependencies-borrowed");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env_with_gateway_admission(&root);
    install_probe_aware_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);
    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let before = serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap();

    for case in [
        "watch-package",
        "loader-entry",
        "build-bin",
        "build-shim",
        "watch-entry",
        "metadata",
    ] {
        declare_source_tooling(&repo);
        fs::write(
            repo.join("scripts/watch-node.mjs"),
            "console.log('watch');\n",
        )
        .unwrap();
        for name in ["tsx", "tsdown", "chokidar"] {
            write_resolvable_source_tool(&repo, name);
        }
        match case {
            "watch-package" => fs::remove_dir_all(repo.join("node_modules/chokidar")).unwrap(),
            "loader-entry" => fs::remove_file(repo.join("node_modules/tsx/entry.mjs")).unwrap(),
            "build-bin" => fs::remove_file(repo.join("node_modules/tsdown/cli.mjs")).unwrap(),
            "build-shim" => {
                fs::remove_file(repo.join("node_modules/.bin").join(if cfg!(windows) {
                    "tsdown.cmd"
                } else {
                    "tsdown"
                }))
                .unwrap()
            }
            "watch-entry" => fs::remove_file(repo.join("scripts/watch-node.mjs")).unwrap(),
            "metadata" => {
                let manifest = serde_json::json!({"name": "openclaw", "devDependencies": []});
                fs::write(
                    repo.join("package.json"),
                    serde_json::to_vec(&manifest).unwrap(),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let failed = run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        assert!(!failed.status.success(), "{case}: {}", stdout(&failed));
        let expected = match case {
            "watch-package" => "chokidar: not installed",
            "loader-entry" => "tsx",
            "build-bin" | "build-shim" => "tsdown",
            "watch-entry" => "source entry is missing",
            "metadata" => "devDependencies must be an object",
            _ => unreachable!(),
        };
        assert!(
            stderr(&failed).contains(expected),
            "{case}: {}",
            stderr(&failed)
        );
        if !matches!(case, "watch-entry" | "metadata") {
            assert!(stderr(&failed).contains("pnpm install --frozen-lockfile"));
        }
        assert_eq!(
            serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap(),
            before,
            "{case}"
        );
        assert!(!root.child("pnpm.log").exists(), "{case}");
        assert!(!root.child("node.log").exists(), "{case}");
        assert!(
            !source_watch_override_path(&root, "demo").exists(),
            "{case}"
        );
    }

    declare_source_tooling(&repo);
    fs::write(repo.join("scripts/watch-node.mjs"), "").unwrap();
    for name in ["tsx", "tsdown", "chokidar"] {
        write_resolvable_source_tool(&repo, name);
    }
    for key in [
        "PNPM_CONFIG_MODULES_DIR",
        "pnpm_config_modules_dir",
        "npm_config_modules_dir",
    ] {
        env.insert(key.to_string(), path_string(&root.child("other-modules")));
        let failed = run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        assert!(!failed.status.success(), "{key}: {}", stdout(&failed));
        assert!(stderr(&failed).contains("outside the selected checkout"));
        assert_eq!(
            serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap(),
            before
        );
        assert!(!root.child("pnpm.log").exists());
        assert!(!root.child("node.log").exists());
        env.remove(key);
    }

    #[cfg(unix)]
    {
        let foreign = root.child("foreign-checkout");
        write_resolvable_source_tool(&foreign, "tsx");
        let link = repo.join("configured-modules");
        std::os::unix::fs::symlink(foreign.join("node_modules"), &link).unwrap();
        env.insert(
            "PNPM_CONFIG_MODULES_DIR".to_string(),
            "configured-modules".to_string(),
        );
        let rejected = run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        assert!(!rejected.status.success());
        assert!(stderr(&rejected).contains("outside the selected checkout"));
        assert_eq!(
            serde_json::to_value(get_environment("demo", &env, &cwd).unwrap()).unwrap(),
            before
        );
        assert_eq!(fs::read_link(&link).unwrap(), foreign.join("node_modules"));
        assert!(!root.child("pnpm.log").exists());
        assert!(!root.child("node.log").exists());
    }
}

#[cfg(unix)]
#[test]
fn dev_dependencies_do_not_repair_through_linked_install_targets() {
    for (relative, configured, borrowed) in [
        ("node_modules", false, false),
        ("node_modules/.pnpm", false, false),
        ("installed-modules", true, false),
        ("installed-modules/.pnpm", true, false),
        ("node_modules/.pnpm", false, true),
    ] {
        let root = TestDir::new("dev-dependencies-linked-install");
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        install_probe_aware_fake_dev_runners(&root, &mut env);
        let worktree = if borrowed {
            repo.clone()
        } else {
            create_owned_dev_env(&repo, "demo", &env, &cwd)
        };
        declare_source_tooling(&worktree);
        if configured {
            env.insert(
                "PNPM_CONFIG_MODULES_DIR".to_string(),
                "installed-modules".to_string(),
            );
        }
        let link = worktree.join(relative);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let target = worktree.join("borrowed-modules");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let failed = run_ocm(
            &cwd,
            &env,
            &dev_plain(&["demo", "--repo", &path_string(&repo)]),
        );
        assert!(!failed.status.success());
        assert!(stderr(&failed).contains("refusing to install"));
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert!(!root.child("pnpm.log").exists());
        assert!(!root.child("node.log").exists());
    }
}

#[test]
fn dev_command_does_not_recreate_a_missing_saved_worktree() {
    let root = TestDir::new("dev-command-missing-worktree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    let registered = git_worktree_paths(&repo);
    fs::remove_dir_all(&worktree_root).unwrap();
    fs::remove_file(root.child("node.log")).unwrap();

    let second = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(!second.status.success());
    assert!(stderr(&second).contains("saved dev worktree is missing"));
    assert!(!worktree_root.exists());
    assert_eq!(git_worktree_paths(&repo), registered);
    assert!(!root.child("node.log").exists());
    let meta = get_environment("demo", &env, &cwd).unwrap();
    assert_eq!(meta.dev.unwrap().source_root(), path_string(&worktree_root));

    let resolve = run_ocm(&cwd, &env, &["env", "resolve", "demo", "--json"]);
    assert!(!resolve.status.success());
    assert!(stderr(&resolve).contains("saved dev worktree is missing"));
    let gateway_error = ocm::env::EnvironmentService::new(&env, &cwd)
        .resolve_gateway_process("demo", false)
        .unwrap_err();
    assert!(gateway_error.contains("saved dev worktree is missing"));
    for command in ["env", "dev"] {
        let status = run_ocm(&cwd, &env, &[command, "status", "demo", "--json"]);
        assert!(status.status.success(), "{}", stderr(&status));
    }
    for kind in ["runtime", "launcher"] {
        let option = if kind == "runtime" {
            "--path"
        } else {
            "--command"
        };
        let runner = path_string(&root.child("fake-dev-bin/pnpm"));
        let add = run_ocm(&cwd, &env, &[kind, "add", "fallback", option, &runner]);
        assert!(add.status.success(), "{}", stderr(&add));
        for action in ["resolve", "run"] {
            let overridden = run_ocm(
                &cwd,
                &env,
                &[
                    "env",
                    action,
                    "demo",
                    &format!("--{kind}"),
                    "fallback",
                    "--",
                    "--version",
                ],
            );
            assert!(overridden.status.success(), "{}", stderr(&overridden));
        }
    }

    let replacement = run_ocm(
        &cwd,
        &env,
        &[
            "env",
            "create",
            "replacement",
            "--root",
            &path_string(&worktree_root),
        ],
    );
    assert!(replacement.status.success(), "{}", stderr(&replacement));
    let notes = worktree_root.join(".openclaw/workspace/notes");
    fs::write(&notes, "preserve replacement state\n").unwrap();
    let registry = ocm::store::env_registry_path(&env, &cwd).unwrap();
    let registry_before = fs::read(&registry).unwrap();
    let remove = run_ocm(&cwd, &env, &["env", "remove", "demo", "--force"]);
    assert!(!remove.status.success());
    assert!(
        stderr(&remove).contains("checkout identity does not match"),
        "{}",
        stderr(&remove)
    );
    assert_eq!(
        fs::read_to_string(notes).unwrap(),
        "preserve replacement state\n"
    );
    assert_eq!(fs::read(&registry).unwrap(), registry_before);
}

#[test]
fn dev_command_resumes_the_recorded_worktree_without_recreating_the_default() {
    let root = TestDir::new("dev-command-recorded-worktree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    let dev = meta.dev.as_mut().unwrap();
    let original_worktree = PathBuf::from(dev.source_root());
    let recorded_worktree = repo.join(".worktrees/recorded-source");
    let moved = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "move"])
        .arg(&original_worktree)
        .arg(&recorded_worktree)
        .output()
        .unwrap();
    assert!(moved.status.success(), "{}", stderr(&moved));
    *dev = EnvDevMeta::Owned {
        repo_root: dev.repo_root().to_string(),
        worktree_root: path_string(&recorded_worktree),
    };
    save_environment(meta, &env, &cwd).unwrap();
    fs::write(recorded_worktree.join("SENTINEL"), "keep my edits\n").unwrap();
    fs::write(
        recorded_worktree.join("scripts/run-node.mjs"),
        "console.log('edited');\n",
    )
    .unwrap();
    fs::remove_file(root.child("node.log")).unwrap();

    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let service = ocm::env::EnvironmentService::new(&env, &cwd);
    let resolved = service
        .resolve("demo", None, None, &[])
        .unwrap()
        .into_summary();
    assert_eq!(resolved.run_dir, path_string(&recorded_worktree));
    assert_eq!(
        service
            .resolve_gateway_process("demo", false)
            .unwrap()
            .run_dir,
        recorded_worktree
    );
    assert!(!original_worktree.exists());
    assert_eq!(
        fs::read_to_string(recorded_worktree.join("SENTINEL")).unwrap(),
        "keep my edits\n"
    );
    assert_eq!(
        fs::read_to_string(recorded_worktree.join("scripts/run-node.mjs")).unwrap(),
        "console.log('edited');\n"
    );
    let node_log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(node_log.contains("scripts/run-node.mjs gateway run --port"));
    let source_prefix = format!(
        "{}|",
        path_string(&fs::canonicalize(&recorded_worktree).unwrap())
    );
    assert!(
        node_log
            .lines()
            .all(|line| line.starts_with(&source_prefix))
    );
}

#[test]
fn dev_command_rejects_an_unrelated_recorded_worktree_before_running_source() {
    let root = TestDir::new("dev-command-unrelated-recorded-worktree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let unrelated_worktree = root.child("unrelated-source");
    init_nested_openclaw_repo(&unrelated_worktree);
    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    meta.dev = Some(EnvDevMeta::Owned {
        repo_root: meta.dev.as_ref().unwrap().repo_root().to_string(),
        worktree_root: path_string(&unrelated_worktree),
    });
    save_environment(meta, &env, &cwd).unwrap();
    fs::remove_file(root.child("node.log")).unwrap();

    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(!resumed.status.success());
    assert!(stderr(&resumed).contains("saved dev worktree is not registered"));
    assert!(!root.child("node.log").exists());
    assert_eq!(
        fs::read_to_string(unrelated_worktree.join("SENTINEL")).unwrap(),
        "preserve me\n"
    );
}

#[cfg(unix)]
#[test]
fn dev_command_accepts_a_repo_alias_without_rebinding_the_env() {
    let root = TestDir::new("dev-command-repo-alias");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let before = get_environment("demo", &env, &cwd).unwrap().dev.unwrap();
    let alias = root.child("source-alias");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();

    let resumed = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&alias)]),
    );
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let after = get_environment("demo", &env, &cwd).unwrap().dev.unwrap();
    assert_eq!(after, before);

    let unrelated_repo = root.child("unrelated-repo");
    init_nested_openclaw_repo(&unrelated_repo);
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&unrelated_repo, &alias).unwrap();
    let changed = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&alias)]),
    );
    assert!(!changed.status.success());
    assert!(stderr(&changed).contains("dev cannot change the repo for existing env"));
    let worktree_cwd = Path::new(before.source_root()).join("scripts");
    for cwd in [&unrelated_repo, &worktree_cwd] {
        let resumed = run_ocm(cwd, &env, &dev_plain(&["demo"]));
        assert!(resumed.status.success(), "{}", stderr(&resumed));
    }
    let after = get_environment("demo", &env, &cwd).unwrap().dev.unwrap();
    assert_eq!(after, before);
}

#[test]
fn dev_command_rejects_a_stale_registration_replaced_by_an_unrelated_clone() {
    let root = TestDir::new("dev-command-stale-replacement");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    let worktree_root = PathBuf::from(show_json["devWorktreeRoot"].as_str().unwrap());
    fs::remove_dir_all(&worktree_root).unwrap();
    init_nested_openclaw_repo(&worktree_root);

    let second = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
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
    fs::remove_file(root.child("node.log")).unwrap();
    let config_path = PathBuf::from(show_json["configPath"].as_str().unwrap());
    let config_before = b"{\"agents\":{\"defaults\":{\"skipBootstrap\":true}}}\n";
    fs::write(&config_path, config_before).unwrap();
    let mut admitted = Vec::new();
    for args in [
        vec!["env", "resolve", "demo", "--json", "--", "--version"],
        vec!["env", "run", "demo", "--", "--version"],
        vec!["@demo", "--", "--version"],
        vec!["@demo", "--", "onboard"],
    ] {
        let output = run_ocm(&cwd, &env, &args);
        if output.status.success() {
            admitted.push(args.join(" "));
        } else {
            assert!(
                stderr(&output).contains("registered worktree is not a valid OpenClaw checkout"),
                "{}",
                stderr(&output)
            );
        }
    }
    let gateway = ocm::env::EnvironmentService::new(&env, &cwd)
        .resolve_gateway_process("demo", false)
        .err();
    if gateway.is_none() {
        admitted.push("gateway process resolution".to_string());
    }
    assert!(
        admitted.is_empty(),
        "accepted a replaced worktree: {admitted:?}"
    );
    assert!(
        gateway
            .unwrap()
            .contains("registered worktree is not a valid OpenClaw checkout")
    );
    assert!(!root.child("pnpm.log").exists());
    assert!(!root.child("node.log").exists());
    assert_eq!(fs::read(config_path).unwrap(), config_before);
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
    register_owned_dev_env(&repo, &repo.join(".worktrees/demo"), "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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

    let second = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);
    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
            "--no-ui",
        ],
    );
    assert!(run.status.success(), "{}", stderr(&run));

    let node_log = fs::read_to_string(root.child("node.log")).unwrap();
    assert!(node_log.contains("scripts/run-node.mjs onboard --mode local --no-install-daemon"));
    assert!(node_log.contains("scripts/watch-node.mjs"));
    assert!(node_log.contains("gateway run --port"));
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[test]
fn dev_status_reports_dev_envs() {
    let root = TestDir::new("dev-status");
    let repo = init_openclaw_repo(&root);
    let canonical_repo = fs::canonicalize(&repo).unwrap();
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(run.status.success(), "{}", stderr(&run));

    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    meta.service_enabled = true;
    meta.service_running = true;
    save_environment(meta, &env, &cwd).unwrap();

    let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let summary: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(summary["envName"], "demo");
    assert_eq!(summary["serviceDesiredRunning"], true);
    assert_eq!(summary["serviceRunning"], false);
    assert_eq!(summary["sourceWatch"]["state"], "inactive");
    assert_eq!(summary["repoRoot"], path_string(&canonical_repo));
    assert_eq!(summary["worktreeRoot"], path_string(&canonical_repo));
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
            .contains("dev status demo")
    );
    assert!(
        summary["logsCommand"]
            .as_str()
            .unwrap()
            .contains("logs demo --follow")
    );

    let config_path = PathBuf::from(summary["configPath"].as_str().unwrap());
    let config_before = fs::read(&config_path).unwrap();
    let mut config: Value = serde_json::from_slice(&config_before).unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    config["gateway"]["port"] = serde_json::json!(listener.local_addr().unwrap().port());
    let listening_config = serde_json::to_vec(&config).unwrap();
    fs::write(&config_path, &listening_config).unwrap();
    let reachable = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(reachable.status.success(), "{}", stderr(&reachable));
    let reachable: Value = serde_json::from_str(&stdout(&reachable)).unwrap();
    assert_eq!(reachable["gatewayPortReachable"], true);
    assert_eq!(reachable["gatewayHealthReady"], false);
    assert_eq!(reachable["serviceRunning"], false);
    assert_eq!(reachable["sourceWatch"]["state"], "inactive");
    drop(listener);
    let closed = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(closed.status.success(), "{}", stderr(&closed));
    let closed: Value = serde_json::from_str(&stdout(&closed)).unwrap();
    assert_eq!(closed["gatewayPortReachable"], false);
    assert_eq!(closed["gatewayHealthReady"], false);
    assert_eq!(fs::read(&config_path).unwrap(), listening_config);
    fs::write(&config_path, config_before).unwrap();

    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = SupervisorRuntimeState {
        kind: "ocm-supervisor-runtime".to_string(),
        ocm_home: path_string(&root.child("ocm-home")),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        gateway_admission: None,
        updated_at: now_utc(),
        services: vec![],
        children: vec![SupervisorRuntimeChild {
            env_name: "demo".to_string(),
            binding_kind: "dev".to_string(),
            binding_name: summary["worktreeRoot"].as_str().unwrap().to_string(),
            pid: std::process::id(),
            restart_count: 0,
            child_port: summary["gatewayPort"].as_u64().unwrap() as u32,
            stdout_path: path_string(&root.child("demo.stdout.log")),
            stderr_path: path_string(&root.child("demo.stderr.log")),
        }],
    };
    let runtime_bytes = serde_json::to_vec(&runtime).unwrap();
    fs::write(&runtime_path, &runtime_bytes).unwrap();
    let stale = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(stale.status.success(), "{}", stderr(&stale));
    let stale: Value = serde_json::from_str(&stdout(&stale)).unwrap();
    assert_eq!(stale["serviceRunning"], false);
    assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);

    // Seed the foreign store before the status-only fake daemon is loaded.
    let mut other_env = env.clone();
    let other_home = root.child("other-ocm-home");
    other_env.insert("OCM_HOME".to_string(), path_string(&other_home));
    save_environment(
        get_environment("demo", &env, &cwd).unwrap(),
        &other_env,
        &cwd,
    )
    .unwrap();

    let started = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(started.status.success(), "{}", stderr(&started));
    let live = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(live.status.success(), "{}", stderr(&live));
    let live: Value = serde_json::from_str(&stdout(&live)).unwrap();
    assert_eq!(live["serviceRunning"], true);
    assert_eq!(live["servicePid"], std::process::id());
    assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = runtime_path.parent().unwrap();
        let permissions = fs::metadata(directory).unwrap().permissions();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
        fs::set_permissions(directory, permissions).unwrap();
        assert!(!unreadable.status.success());
        assert!(stderr(&unreadable).contains("failed inspecting supervisor runtime state"));
        assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);
    }

    let mut other_runtime = runtime.clone();
    other_runtime.ocm_home = path_string(&other_home);
    let other_runtime_path = supervisor_runtime_path(&other_env, &cwd).unwrap();
    fs::create_dir_all(other_runtime_path.parent().unwrap()).unwrap();
    fs::write(
        &other_runtime_path,
        serde_json::to_vec(&other_runtime).unwrap(),
    )
    .unwrap();
    let foreign = run_ocm(&cwd, &other_env, &["dev", "status", "demo", "--json"]);
    assert!(foreign.status.success(), "{}", stderr(&foreign));
    let foreign: Value = serde_json::from_str(&stdout(&foreign)).unwrap();
    assert_eq!(foreign["serviceRunning"], false);
    assert!(foreign["servicePid"].is_null());

    for invalid_kind in [true, false] {
        let mut invalid = runtime.clone();
        if invalid_kind {
            invalid.kind = "another-runtime-kind".to_string();
        } else {
            invalid.ocm_home = path_string(&other_home);
        }
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&runtime_path, &bytes).unwrap();
        let rejected = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
        assert!(!rejected.status.success());
        assert!(stderr(&rejected).contains("runtime state does not belong"));
        assert_eq!(fs::read(&runtime_path).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn dev_status_preserves_absent_and_read_only_stores() {
    use std::os::unix::fs::PermissionsExt;

    let root = TestDir::new("dev-status-read-only-store");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let store = root.child("ocm-home");
    fs::remove_dir(&store).unwrap();
    let absent = run_ocm(&cwd, &env, &["dev", "status", "--json"]);
    assert!(absent.status.success(), "{}", stderr(&absent));
    assert_eq!(stdout(&absent).trim(), "[]");
    assert!(!store.exists());

    let repo = init_openclaw_repo(&root);
    install_fake_dev_runners(&root, &mut env);
    let prepared = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(prepared.status.success(), "{}", stderr(&prepared));
    let unused = store.join("runtimes");
    fs::remove_dir(&unused).unwrap();
    let permissions = fs::metadata(&store).unwrap().permissions();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o500)).unwrap();
    let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    fs::set_permissions(&store, permissions).unwrap();
    assert!(status.status.success(), "{}", stderr(&status));
    assert!(!unused.exists());
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
            "--no-watch",
            "--no-ui",
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
            "--no-watch",
            "--no-ui",
        ],
    );
    assert!(first.status.success(), "{}", stderr(&first));

    let second = run_ocm(&cwd, &env, &dev_plain(&["demo", "--port", "21901"]));
    assert!(second.status.success(), "{}", stderr(&second));

    let changed = run_ocm(&cwd, &env, &dev_plain(&["demo", "--port", "21902"]));
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
fn dev_command_does_not_use_a_saved_repo_for_new_envs() {
    let root = TestDir::new("dev-command-saved-repo");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    fs::write(
        root.child("ocm-home/dev.json"),
        serde_json::to_vec(&serde_json::json!({
            "kind": "ocm-dev-preferences",
            "preferredRepoRoot": path_string(&repo),
        }))
        .unwrap(),
    )
    .unwrap();

    let second = run_ocm(&cwd, &env, &dev_plain(&["preview"]));
    assert!(!second.status.success());
    assert!(stderr(&second).contains("pass --repo /path/to/openclaw"));
    assert!(!repo.join(".worktrees/preview").exists());

    let show = run_ocm(&cwd, &env, &["env", "show", "preview", "--json"]);
    assert!(!show.status.success());
    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(resumed.status.success(), "{}", stderr(&resumed));
}

#[test]
fn dev_command_discovers_the_enclosing_checkout_from_a_deep_directory() {
    let root = TestDir::new("dev-command-deep-checkout");
    let repo = init_openclaw_repo(&root);
    let cwd = repo.join("src/a/b/c/d/e/f/g/h/i");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(run.status.success(), "{}", stderr(&run));

    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(
        show_json["devRepoRoot"],
        path_string(&fs::canonicalize(&repo).unwrap())
    );
    assert_eq!(show_json["devWorktreeRoot"], show_json["devRepoRoot"]);
}

#[test]
fn dev_command_does_not_select_a_neighboring_checkout() {
    let root = TestDir::new("dev-command-neighbor-checkout");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("repo/other-project");
    fs::create_dir_all(&cwd).unwrap();
    let env = ocm_env(&root);

    let run = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(!run.status.success());
    assert!(stderr(&run).contains("pass --repo /path/to/openclaw"));
    assert!(!repo.join(".worktrees/demo").exists());
}

#[test]
fn dev_command_records_the_canonical_explicit_source() {
    let root = TestDir::new("dev-command-canonical-source");
    let repo = init_openclaw_repo(&root);
    let separate = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "--quiet", "--separate-git-dir"])
        .arg(root.child("external-git-dir"))
        .output()
        .unwrap();
    assert!(separate.status.success(), "{}", stderr(&separate));
    assert!(repo.join(".git").is_file());
    #[cfg(unix)]
    let alias = {
        let alias = root.child("source-alias");
        std::os::unix::fs::symlink(&repo, &alias).unwrap();
        alias
    };
    #[cfg(not(unix))]
    let alias = repo.clone();
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&alias)]),
    );
    assert!(run.status.success(), "{}", stderr(&run));
    let resumed = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&alias)]),
    );
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let show = run_ocm(&cwd, &env, &["env", "show", "demo", "--json"]);
    assert!(show.status.success(), "{}", stderr(&show));
    let show_json: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(
        show_json["devRepoRoot"],
        path_string(&fs::canonicalize(&repo).unwrap())
    );
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
    #[cfg(unix)]
    {
        let preparation = read_source_watch_session(&root);
        assert_eq!(preparation["servicePreparation"], true);
        assert_eq!(preparation["watching"], false);
        assert!(preparation["ui"].is_null());
        assert_eq!(preparation["closed"], true);
        assert_eq!(preparation["restoreService"], false);
    }

    let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let status_json: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(status_json["bindingKind"], "dev");
    assert_eq!(status_json["bindingName"], "dev");
    assert_eq!(status_json["desiredRunning"], true);

    assert!(stdout(&run).contains("http://127.0.0.1:"));
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
    let mut env = service_env_with_gateway_admission(&root);
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

    // Force uses the default watcher without requiring its positive alias.
    let watch = run_ocm(
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--force",
            "--no-ui",
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
    let mut env = service_env_with_gateway_admission(&root);
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
            "--no-ui",
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

#[cfg(unix)]
#[test]
fn dev_watch_force_rejects_dangling_dependency_link_before_takeover() {
    let root = TestDir::new("dev-command-runtime-watch-dangling-dependencies");
    let repo = init_openclaw_repo(&root);
    let missing_dependencies = root.child("missing-checkout/node_modules");
    std::os::unix::fs::symlink(&missing_dependencies, repo.join("node_modules")).unwrap();
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env_with_gateway_admission(&root);
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
            "--no-ui",
        ],
    );

    assert!(!watch.status.success());
    assert!(
        stderr(&watch).contains("failed to resolve OpenClaw dependencies"),
        "{}",
        stderr(&watch)
    );
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
    let mut env = service_env_with_gateway_admission(&root);
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
            "--no-ui",
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
    let env = service_env_with_gateway_admission(&root);
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
            "--no-ui",
        ],
    );
    assert!(!watch.status.success());
    #[cfg(unix)]
    {
        assert_eq!(watch.status.code(), Some(127));
        assert!(
            stderr(&watch).contains("node") && stderr(&watch).contains("not found"),
            "{}",
            stderr(&watch)
        );
    }
    #[cfg(not(unix))]
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
fn dev_watch_rejects_service_activation_until_the_watch_exits() {
    for runtime_backed in [false, true] {
        let root = TestDir::new("dev-command-watch-service-exclusion");
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env(&root);
        let (started, release, _) = install_blocking_fake_dev_runners(&root, &mut env);
        if runtime_backed {
            create_runtime_backed_env(&cwd, &env);
        }
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
                "--no-ui",
            ])
            .env_clear()
            .envs(&env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let watch = watch.spawn().unwrap();
        let did_start = wait_for_path(&started, Duration::from_secs(30));
        let did_publish = wait_for_path(
            &source_watch_override_path(&root, "demo"),
            Duration::from_secs(30),
        );
        let override_before = fs::read(source_watch_override_path(&root, "demo")).unwrap();
        let reused = run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        let override_after = fs::read(source_watch_override_path(&root, "demo")).unwrap();
        let attempts = [
            vec!["service", "install", "demo"],
            vec!["service", "start", "demo"],
            vec!["service", "restart", "demo"],
            vec!["service", "restart", "demo", "--force"],
        ]
        .map(|args| run_ocm(&cwd, &env, &args));
        let during = ocm::env::EnvironmentService::new(&env, &cwd)
            .get("demo")
            .unwrap();
        fs::write(&release, "release\n").unwrap();
        let watch = watch.wait_with_output().unwrap();

        assert!(did_start && did_publish, "{}", stderr(&watch));
        assert!(watch.status.success(), "{}", stderr(&watch));
        assert!(reused.status.success(), "{}", stderr(&reused));
        assert_eq!(override_after, override_before);
        for attempt in attempts {
            assert!(!attempt.status.success(), "{}", stdout(&attempt));
            assert!(
                stderr(&attempt).contains("source watch is active"),
                "{}",
                stderr(&attempt)
            );
        }
        assert!(!during.service_enabled);
        assert!(!during.service_running);
        let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
        assert!(start.status.success(), "{}", stderr(&start));
    }
}

#[cfg(unix)]
#[test]
fn dev_destroy_stops_the_recorded_watch_before_removing_state() {
    for runtime_backed in [false, true] {
        let root = TestDir::new("dev-destroy-watch");
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env_with_gateway_admission(&root);
        let (started, _, _) = install_blocking_fake_dev_runners(&root, &mut env);
        if runtime_backed {
            create_runtime_backed_env(&cwd, &env);
            let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
            assert!(start.status.success(), "{}", stderr(&start));
        }
        let watch = DevWatchFixture::spawn(
            &root,
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        assert!(wait_for_path(&started, Duration::from_secs(10)));
        assert!(wait_for_path(
            &source_watch_override_path(&root, "demo"),
            Duration::from_secs(10)
        ));
        let mut before = get_environment("demo", &env, &cwd).unwrap();
        before.last_used_at = Some(time::OffsetDateTime::UNIX_EPOCH);
        save_environment(before.clone(), &env, &cwd).unwrap();
        let session_path = source_watch_override_path(&root, "demo").with_extension("session");
        let session_before = fs::read(&session_path).unwrap();
        let preview = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--json"]);
        assert!(preview.status.success(), "{}", stderr(&preview));
        let preview: Value = serde_json::from_str(&stdout(&preview)).unwrap();
        assert!(preview["sourceWatchPid"].is_number());
        assert_eq!(preview["processInspectionDeferred"], true);
        assert!(
            preview["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step["kind"] == "source-watch")
        );
        let guarded = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "destroy",
                "demo",
                "--yes",
                "--if-state-token",
                preview["stateToken"].as_str().unwrap(),
                "--json",
            ],
        );
        assert!(!guarded.status.success());
        let guarded: Value = serde_json::from_str(&stdout(&guarded)).unwrap();
        assert_eq!(guarded["code"], "source_watch_active");
        assert_eq!(fs::read(&session_path).unwrap(), session_before);
        for args in [
            vec!["env", "remove", "demo", "--force"],
            vec!["env", "prune", "--older-than", "1", "--yes"],
        ] {
            let refused = run_ocm(&cwd, &env, &args);
            assert!(!refused.status.success());
            assert!(
                stderr(&refused).contains("unfinished ownership"),
                "{}",
                stderr(&refused)
            );
        }
        let direct = ocm::store::remove_environment("demo", true, &env, &cwd);
        assert!(direct.unwrap_err().contains("unfinished ownership"));
        let destroyed = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes", "--json"]);
        let watch = watch.finish();
        assert!(destroyed.status.success(), "{}", stderr(&destroyed));
        let destroyed: Value = serde_json::from_str(&stdout(&destroyed)).unwrap();
        assert_eq!(destroyed["sourceWatchStopped"], true);
        assert_eq!(destroyed["processInspectionDeferred"], false);
        assert_eq!(destroyed["removed"], true);
        assert_eq!(watch.status.code(), Some(130), "{}", stderr(&watch));
        assert!(!Path::new(&before.root).exists());
        assert!(!session_path.exists());
        assert!(source_watch_lock_path(&root, "demo").exists());
        assert!(repo.exists());
        if let Some(dev) = before.dev {
            assert!(Path::new(dev.source_root()).exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn dev_destroy_preserves_state_when_binding_changes_during_stop() {
    let root = TestDir::new("dev-destroy-watch-binding-race");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let (started, release, _) = install_blocking_fake_dev_runners(&root, &mut env);
    let stopping = root.child("source-watch.stopping");
    let node = format!(
        "#!/bin/sh\ntrap 'printf stopping > \"{}\"; while [ ! -f \"{}\" ]; do /bin/sleep 0.05; done; exit 143' TERM\nprintf 'ready\\n' > \"{}\"\nwhile [ ! -f \"{}\" ]; do /bin/sleep 0.05; done\n",
        path_string(&stopping),
        path_string(&release),
        path_string(&started),
        path_string(&release),
    );
    write_fake_dev_node(&root, &node);
    let watch = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
    );
    assert!(wait_for_path(&started, Duration::from_secs(10)));
    let before = get_environment("demo", &env, &cwd).unwrap();
    let destroy = Command::new(env!("CARGO_BIN_EXE_ocm"))
        .current_dir(&cwd)
        .args(["env", "destroy", "demo", "--yes", "--json"])
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(wait_for_path(
        &source_watch_override_path(&root, "demo").with_extension("stop"),
        Duration::from_secs(5)
    ));
    assert!(wait_for_path(&stopping, Duration::from_secs(5)));
    ocm::env::EnvironmentService::new(&env, &cwd)
        .set_protected("demo", true)
        .unwrap();
    fs::write(&release, "release\n").unwrap();
    let destroyed = destroy.wait_with_output().unwrap();
    let watch = watch.finish();
    assert!(!destroyed.status.success());
    assert!(
        stderr(&destroyed).contains("changed while stopping source watch"),
        "{}",
        stderr(&destroyed)
    );
    assert_eq!(watch.status.code(), Some(130));
    assert!(Path::new(&before.root).exists());
    assert!(get_environment("demo", &env, &cwd).unwrap().protected);
}

#[cfg(unix)]
#[test]
fn dev_stop_restores_service_and_preserves_the_env_and_borrowed_source() {
    let root = TestDir::new("dev-stop-runtime");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env_with_gateway_admission(&root);
    let (started, _, _) = install_blocking_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);
    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));
    let before = get_environment("demo", &env, &cwd).unwrap();
    let sentinel = Path::new(&before.root).join("keep.txt");
    fs::write(&sentinel, "persistent env state").unwrap();
    fs::write(repo.join("keep.txt"), "borrowed source").unwrap();

    let watch = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
            "--no-ui",
        ],
    );
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "watch did not start"
    );
    let mut unrelated = Command::new("/bin/sleep")
        .arg("300")
        .current_dir(&repo)
        .spawn()
        .unwrap();
    let stopped = run_dev_stop(&cwd, &env);
    let unrelated_survived = unrelated.try_wait().unwrap().is_none();
    let _ = unrelated.kill();
    unrelated.wait().unwrap();
    let watched = watch.finish();

    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&stopped)).unwrap(),
        serde_json::json!({
            "envName": "demo", "stopped": true, "serviceRestored": true,
        })
    );
    assert_eq!(watched.status.code(), Some(130), "{}", stderr(&watched));
    assert!(
        unrelated_survived,
        "stop treated a shared source cwd as process ownership"
    );
    let after = get_environment("demo", &env, &cwd).unwrap();
    assert_eq!(after.root, before.root);
    assert_eq!(after.default_runtime, before.default_runtime);
    assert!(after.dev.is_none());
    assert!(after.service_running);
    assert_eq!(
        fs::read_to_string(sentinel).unwrap(),
        "persistent env state"
    );
    assert_eq!(
        fs::read_to_string(repo.join("keep.txt")).unwrap(),
        "borrowed source"
    );
    assert!(!source_watch_override_path(&root, "demo").exists());
    assert_eq!(read_source_watch_session(&root)["closed"], true);

    let again = run_dev_stop(&cwd, &env);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&again)).unwrap()["stopped"],
        false
    );
    assert!(get_environment("demo", &env, &cwd).unwrap().service_running);
    let help = run_ocm(&cwd, &env, &["help", "dev", "stop"]);
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("dev stop <env>"));
    assert!(stdout(&help).contains("--acknowledge-stopped-processes"));
}

#[cfg(unix)]
#[test]
fn dev_stop_keeps_dotted_environment_watch_ownership_separate() {
    for suffix in ["session", "stop", "admission"] {
        let root = TestDir::new(&format!("dev-stop-dotted-{suffix}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        let _ = install_blocking_fake_dev_runners(&root, &mut env);
        create_runtime_backed_env(&cwd, &env);
        let other_name = format!("demo.{suffix}");
        let created = run_ocm(
            &cwd,
            &env,
            &[
                "env",
                "create",
                &other_name,
                "--runtime",
                "stable",
                "--port",
                "21902",
            ],
        );
        assert!(created.status.success(), "{}", stderr(&created));
        let mut other = DevWatchFixture::spawn(
            &root,
            &cwd,
            &env,
            &[
                "dev",
                &other_name,
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        let other_override = source_watch_override_path(&root, &other_name);
        assert!(
            wait_for_path(&other_override, Duration::from_secs(30)),
            "{other_name} did not start"
        );
        let before = fs::read(&other_override).unwrap();
        let demo = DevWatchFixture::spawn(
            &root,
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--watch",
                "--force",
                "--no-ui",
            ],
        );
        assert!(
            wait_for_path(
                &source_watch_override_path(&root, "demo"),
                Duration::from_secs(30)
            ),
            "demo could not start alongside {other_name}",
        );
        let stopped = run_dev_stop(&cwd, &env);
        let other_survived = other.child.as_mut().unwrap().try_wait().unwrap().is_none();
        let metadata_preserved = fs::read(&other_override).ok() == Some(before);
        let status = run_ocm(&cwd, &env, &["dev", "status", &other_name, "--json"]);
        let other_stopped = run_named_dev_stop(&cwd, &env, &other_name);
        let demo = demo.finish();
        let other = other.finish();

        assert!(stopped.status.success(), "{suffix}: {}", stderr(&stopped));
        assert!(other_survived, "stop demo also stopped {other_name}");
        assert!(
            metadata_preserved,
            "stop demo replaced {other_name}'s watch metadata"
        );
        assert!(status.status.success(), "{}", stderr(&status));
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&status)).unwrap()["sourceWatch"]["state"],
            "active"
        );
        assert!(other_stopped.status.success(), "{}", stderr(&other_stopped));
        assert_eq!(demo.status.code(), Some(130), "{}", stderr(&demo));
        assert_eq!(other.status.code(), Some(130), "{}", stderr(&other));
    }
}

#[cfg(unix)]
#[test]
fn dev_foreground_reuses_plain_mode_and_named_stop_preserves_source() {
    let root = TestDir::new("dev-plain-owned-reuse");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let (started, _, starts) = install_blocking_fake_dev_runners(&root, &mut env);
    let mut foreground = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(wait_for_path(&started, Duration::from_secs(30)));
    let session_path = source_watch_override_path(&root, "demo").with_extension("session");
    let owner_before = fs::read(&session_path).unwrap();
    let session: Value = serde_json::from_slice(&owner_before).unwrap();
    assert_eq!(session["kind"], "ocm-source-foreground-session-v1");
    assert_eq!(session["watching"], false);
    let meta = get_environment("demo", &env, &cwd).unwrap();
    let source = Path::new(meta.dev.as_ref().unwrap().source_root());
    let config_path = Path::new(&meta.root).join(".openclaw/openclaw.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["gateway"].as_object_mut().unwrap().remove("mode");
    config["gateway"].as_object_mut().unwrap().remove("bind");
    let config_before = serde_json::to_vec(&config).unwrap();
    fs::write(&config_path, &config_before).unwrap();
    fs::write(source.join("SENTINEL"), "managed source edits\n").unwrap();

    let reused = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(reused.status.success(), "{}", stderr(&reused));
    assert!(stdout(&reused).contains("watching=false"));
    assert!(stdout(&reused).contains(&format!("http://127.0.0.1:{}", meta.gateway_port.unwrap())));
    let changed_mode = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    assert!(!changed_mode.status.success());
    assert!(stderr(&changed_mode).contains("different foreground mode"));
    let service = run_ocm(&cwd, &env, &["dev", "demo", "--service"]);
    assert!(!service.status.success());
    let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&status)).unwrap()["sourceWatch"]["watching"],
        false
    );
    assert_eq!(fs::read(&session_path).unwrap(), owner_before);
    assert_eq!(fs::read(&config_path).unwrap(), config_before);
    assert_eq!(fs::read_to_string(&starts).unwrap().lines().count(), 1);

    let stopped = run_dev_stop(&cwd, &env);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let output = foreground.wait_without_release();
    assert_eq!(output.status.code(), Some(130), "{}", stderr(&output));
    assert_eq!(read_source_watch_session(&root)["closed"], true);
    assert!(Path::new(&meta.root).is_dir());
    assert_eq!(
        fs::read_to_string(source.join("SENTINEL")).unwrap(),
        "managed source edits\n"
    );
    assert_eq!(
        serde_json::to_value(get_environment("demo", &env, &cwd).unwrap().dev).unwrap(),
        serde_json::to_value(&meta.dev).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn dev_watch_retains_unverified_worker_ownership_across_retries() {
    struct OwnedProcess(std::process::Child);
    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for (failure, watching) in [
        ("eof", true),
        ("signal", true),
        ("controller", true),
        ("eof", false),
        ("signal", false),
        ("controller", false),
    ] {
        let root = TestDir::new(&format!("dev-unverified-{failure}-{watching}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env_with_gateway_admission(&root);
        env.insert("OCM_TEST_FOREGROUND_ROOT".into(), path_string(root.path()));
        env.insert("OCM_TEST_WATCH_FAILURE".into(), failure.into());
        let source = if watching {
            create_runtime_backed_env(&cwd, &env);
            let started_service = run_ocm(&cwd, &env, &["service", "start", "demo"]);
            assert!(
                started_service.status.success(),
                "{}",
                stderr(&started_service)
            );
            repo.clone()
        } else {
            let mut setup_env = env.clone();
            install_fake_dev_runners(&root, &mut setup_env);
            let created = run_ocm(
                &cwd,
                &setup_env,
                &dev_plain(&["demo", "--repo", &path_string(&repo)]),
            );
            assert!(created.status.success(), "{}", stderr(&created));
            PathBuf::from(
                get_environment("demo", &env, &cwd)
                    .unwrap()
                    .dev
                    .unwrap()
                    .source_root(),
            )
        };
        let entry = source.join(if watching {
            "scripts/watch-node.mjs"
        } else {
            "scripts/run-node.mjs"
        });
        // Each worker has a separate group and no inherited lease descriptor.
        // EOF holds the outer stderr pipe; the other rows use private pipes.
        fs::write(&entry, r#"
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
const root = process.env.OCM_TEST_FOREGROUND_ROOT;
const failure = process.env.OCM_TEST_WATCH_FAILURE;
const pidFile = path.join(root, 'source-watch-descendant.pid');
const worker = `
import fs from 'node:fs';
import path from 'node:path';
const root = process.env.OCM_TEST_FOREGROUND_ROOT;
const file = path.join(root, 'source-watch-descendant.pid');
fs.writeFileSync(file + '.tmp', String(process.pid)); fs.renameSync(file + '.tmp', file);
setInterval(() => { if (fs.existsSync(path.join(root, 'source-watch.release'))) process.exit(0); }, 25);
`;
const child = spawn(process.execPath, ['--input-type=module', '--eval', worker], {
  detached: true, stdio: failure === 'eof' ? ['ignore','ignore','inherit'] : ['ignore','pipe','pipe'], env: process.env,
});
child.stdout?.resume(); child.stderr?.resume(); child.unref();
const deadline = Date.now() + 5000;
while (!fs.existsSync(pidFile)) {
  if (Date.now() >= deadline || fs.existsSync(path.join(root, 'source-watch.release'))) process.exit(1);
  await new Promise(resolve => setTimeout(resolve, 10));
}
fs.writeFileSync(path.join(root, 'source-watch.started'), 'ready');
if (failure === 'eof') process.exit(23);
setInterval(() => { if (fs.existsSync(path.join(root, 'source-watch.release'))) process.exit(0); }, 25);
"#).unwrap();
        let repo_arg = path_string(&repo);
        let mut args = vec!["dev", "demo", "--repo", &repo_arg, "--no-ui"];
        if watching {
            args.extend(["--watch", "--force"]);
        } else {
            args.push("--no-watch");
        }
        let mut watch = DevWatchFixture::spawn(&root, &cwd, &env, &args);
        assert!(wait_for_path(
            &root.child("source-watch.started"),
            Duration::from_secs(20)
        ));
        let descendant = fs::read_to_string(root.child("source-watch-descendant.pid"))
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let session = read_source_watch_session(&root);
        let leader = session["child"]["pid"].as_u64().unwrap() as u32;
        assert_eq!(session["kind"], "ocm-source-foreground-session-v1");
        assert_eq!(session["watching"], watching);
        assert!(process_is_alive(descendant));
        // The existing module and worker are loaded. Any broken future admission
        // now runs a finite entry instead of spawning another detached worker.
        fs::write(&entry, "console.log('unexpected restart');\n").unwrap();
        let mut unrelated = OwnedProcess(
            Command::new("/bin/sleep")
                .arg("300")
                .current_dir(&repo)
                .spawn()
                .unwrap(),
        );
        let registry = root.child("ocm-home/envs.json");
        let registry_before = fs::read(&registry).unwrap();
        let meta = get_environment("demo", &env, &cwd).unwrap();
        if failure == "controller" {
            watch.crash_controller();
            assert!(
                process_is_alive(leader),
                "the recorded leader must be live at recovery"
            );
            let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
            assert_eq!(
                serde_json::from_str::<Value>(&stdout(&status)).unwrap()["sourceWatch"]["state"],
                "unknown"
            );
            assert!(!run_ocm(&cwd, &env, &args).status.success());
        } else {
            if failure == "signal" {
                assert_eq!(
                    unsafe { libc::kill(leader as libc::pid_t, libc::SIGKILL) },
                    0
                );
            }
            let output = watch.wait_without_release();
            assert!(!output.status.success());
        }
        let stopped = run_dev_stop(&cwd, &env);
        if stopped.status.success() {
            let escaped = process_is_alive(descendant);
            drop(watch);
            assert!(wait_for_process_exit(leader, Duration::from_secs(3)));
            assert!(wait_for_process_exit(descendant, Duration::from_secs(3)));
            assert!(unrelated.0.try_wait().unwrap().is_none());
            drop(unrelated);
            panic!(
                "{failure} certified completion with escaped worker alive={escaped}; fixture cleanup verified"
            );
        }
        assert!(
            process_is_alive(descendant),
            "the owned group cannot certify this worker"
        );
        let retained = read_source_watch_session(&root);
        assert_eq!(retained["closed"], false);
        assert!(retained["child"]["pid"].is_number());
        let error = retained["completion"]["error"].as_str().unwrap();
        assert!(
            error.contains(if failure == "eof" {
                "EOF"
            } else if failure == "signal" {
                "signal"
            } else {
                "unverified"
            }),
            "{error}"
        );
        assert!(!get_environment("demo", &env, &cwd).unwrap().service_running);
        let session_path = source_watch_override_path(&root, "demo").with_extension("session");
        let session_before = fs::read(&session_path).unwrap();
        let override_before = fs::read(source_watch_override_path(&root, "demo")).unwrap();
        for retry in [
            vec!["dev", "stop", "demo"],
            vec!["service", "start", "demo"],
            vec!["env", "destroy", "demo", "--yes"],
            args.to_vec(),
        ] {
            let refused = run_ocm(&cwd, &env, &retry);
            assert!(!refused.status.success(), "{failure}: {retry:?}");
            assert_eq!(fs::read(&session_path).unwrap(), session_before);
            assert_eq!(
                fs::read(source_watch_override_path(&root, "demo")).unwrap(),
                override_before
            );
            assert_eq!(fs::read(&registry).unwrap(), registry_before);
            assert!(Path::new(&meta.root).is_dir() && repo.is_dir());
            assert!(process_is_alive(descendant));
        }
        assert!(unrelated.0.try_wait().unwrap().is_none());
        drop(watch);
        assert!(wait_for_process_exit(leader, Duration::from_secs(3)));
        assert!(wait_for_process_exit(descendant, Duration::from_secs(3)));
    }
}

#[cfg(unix)]
#[test]
fn dev_stop_acknowledgement_refuses_live_recorded_ownership() {
    let root = TestDir::new("dev-stop-acknowledge-live");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    install_fake_dev_runners(&root, &mut env);
    let started = root.child("source-watch.started");
    let release = root.child("source-watch.release");
    let worker_path = root.child("source-watch-descendant.pid");
    let script = format!(
        "#!/bin/sh\n(while [ ! -f \"{release}\" ]; do /bin/sleep 0.05; done) &\nprintf '%s\\n' \"$!\" > \"{worker}\"\nprintf ready > \"{started}\"\nwhile [ ! -f \"{release}\" ]; do /bin/sleep 0.05; done\n",
        release = path_string(&release),
        worker = path_string(&worker_path),
        started = path_string(&started),
    );
    write_fake_dev_node(&root, &script);
    let mut watch = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
    );
    assert!(wait_for_path(&started, Duration::from_secs(20)));
    let worker = fs::read_to_string(worker_path)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    let session_path = watch.session.clone();
    let mut session = read_source_watch_session(&root);
    let leader = session["child"]["pid"].as_u64().unwrap() as u32;
    session["completion"] = serde_json::json!({
        "serviceRestored":false, "error":"fixture retained cleanup failure",
    });
    let retained = serde_json::to_vec(&session).unwrap();
    fs::write(&session_path, &retained).unwrap();
    let recover = || {
        run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "stop",
                "demo",
                "--acknowledge-stopped-processes",
                "--json",
            ],
        )
    };
    let controller_live = recover();
    assert!(!controller_live.status.success());
    assert!(stderr(&controller_live).contains("controller is still running"));
    assert_eq!(fs::read(&session_path).unwrap(), retained);
    assert!(process_is_alive(leader) && process_is_alive(worker));

    watch.crash_controller();
    let child_live = recover();
    assert!(!child_live.status.success());
    assert!(stderr(&child_live).contains(&format!("process {leader} is still running")));
    assert_eq!(fs::read(&session_path).unwrap(), retained);
    assert!(process_is_alive(leader) && process_is_alive(worker));

    assert_eq!(
        unsafe { libc::kill(leader as libc::pid_t, libc::SIGKILL) },
        0
    );
    assert!(wait_for_process_exit(leader, Duration::from_secs(3)));
    let group_live = recover();
    assert!(!group_live.status.success());
    assert!(stderr(&group_live).contains(&format!("process group {leader} is still active")));
    assert_eq!(fs::read(&session_path).unwrap(), retained);
    assert!(
        process_is_alive(worker),
        "acknowledgement must not signal live processes"
    );

    drop(watch);
    assert!(wait_for_process_exit(worker, Duration::from_secs(3)));
    let recovered = recover();
    assert!(recovered.status.success(), "{}", stderr(&recovered));
    assert_eq!(read_source_watch_session(&root)["closed"], true);
}

#[cfg(unix)]
#[test]
fn dev_watch_interactive_setup_requires_completion_evidence() {
    for mode in ["success", "cancel", "detached"] {
        let root = TestDir::new("dev-watch-interactive-completion");
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let node = Command::new("node")
            .args(["--print", "process.execPath"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .output()
            .unwrap();
        assert!(node.status.success());
        let node = stdout(&node).trim().to_string();
        let mut env = ocm_env(&root);
        install_fake_dev_runners(&root, &mut env);
        let created = run_ocm(
            &cwd,
            &env,
            &[
                "dev",
                "demo",
                "--repo",
                &path_string(&repo),
                "--no-watch",
                "--no-ui",
            ],
        );
        assert!(created.status.success(), "{}", stderr(&created));
        fs::remove_file(root.child("node.log")).unwrap();
        env.insert("OCM_TEST_FOREGROUND_ROOT".into(), path_string(root.path()));
        env.insert("OCM_TEST_SETUP_MODE".into(), mode.into());
        let script = root.child("setup.mjs");
        fs::write(&script, r#"
import fs from 'node:fs'; import path from 'node:path'; import { isatty } from 'node:tty';
import { spawn } from 'node:child_process';
const root = process.env.OCM_TEST_FOREGROUND_ROOT;
fs.writeFileSync(path.join(root,'setup-tty'), JSON.stringify([0,1,2].map(isatty)));
if (process.env.OCM_TEST_SETUP_MODE === 'success') process.exit(0);
process.on('SIGTERM', () => process.exit(0));
const worker = `const fs=require('node:fs'),path=require('node:path'),root=process.env.OCM_TEST_FOREGROUND_ROOT,file=path.join(root,'source-watch-descendant.pid');fs.writeFileSync(file+'.tmp',String(process.pid));fs.renameSync(file+'.tmp',file);setInterval(()=>{if(fs.existsSync(path.join(root,'source-watch.release')))process.exit(0)},25);`;
if (process.env.OCM_TEST_SETUP_MODE === 'detached') {
  spawn(process.execPath,['-e',worker],{detached:true,stdio:'ignore',env:process.env}).unref();
}
fs.writeFileSync(path.join(root,'setup-ready'), 'ready');
setInterval(() => { if (fs.existsSync(path.join(root,'source-watch.release'))) process.exit(0); },25);
"#).unwrap();
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let node_path = root.child("fake-dev-bin/node");
        let gateway_runner = fs::read_to_string(&node_path).unwrap();
        let onboard = format!(
            "#!/bin/sh\ncase \" $* \" in\n  *' onboard '*) exec {} {} ;;\nesac\n",
            quote(&node),
            quote(&path_string(&script))
        );
        write_executable_script(
            &node_path,
            &gateway_runner.replacen("#!/bin/sh\n", &onboard, 1),
        );
        let (child, terminal) = spawn_ocm_with_controlling_pty(
            &cwd,
            &env,
            &["dev", "demo", "--watch", "--onboard", "--no-ui"],
            true,
        );
        // Keep draining until after the watch guard cleans up, including on panic.
        let terminal_drain = PtyOutputDrain::start(terminal);
        let mut watch = DevWatchFixture {
            child: Some(child),
            owns_session: true,
            release: root.child("source-watch.release"),
            session: source_watch_override_path(&root, "demo").with_extension("session"),
        };
        if mode == "success" {
            assert!(watch.wait_without_release().status.success());
            assert_eq!(read_source_watch_session(&root)["closed"], true);
            drop(watch);
        } else {
            assert!(wait_for_path(
                &root.child("setup-ready"),
                Duration::from_secs(20)
            ));
            let worker = if mode == "detached" {
                let pid_path = root.child("source-watch-descendant.pid");
                assert!(wait_for_path(&pid_path, Duration::from_secs(20)));
                Some(
                    fs::read_to_string(pid_path)
                        .unwrap()
                        .trim()
                        .parse::<u32>()
                        .unwrap(),
                )
            } else {
                None
            };
            let recover = || {
                run_ocm(
                    &cwd,
                    &env,
                    &[
                        "dev",
                        "stop",
                        "demo",
                        "--acknowledge-stopped-processes",
                        "--json",
                    ],
                )
            };
            assert!(
                !recover().status.success(),
                "cannot recover an active controller"
            );
            let stopped = run_dev_stop(&cwd, &env);
            let output = watch.wait_without_release();
            if stopped.status.success() {
                drop(watch);
                if let Some(worker) = worker {
                    assert!(wait_for_process_exit(worker, Duration::from_secs(3)));
                }
                panic!(
                    "cancelled inherited-output setup certified cleanup after exit0; fixture cleanup verified"
                );
            }
            assert!(!output.status.success());
            if let Some(worker) = worker {
                assert!(process_is_alive(worker));
            }
            assert!(
                !root.child("node.log").exists(),
                "unverified setup must not start the watcher"
            );
            let session = read_source_watch_session(&root);
            assert_eq!(session["closed"], false);
            assert!(
                session["completion"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("output EOF witness")
            );
            let session_path = watch.session.clone();
            let retained = fs::read(&session_path).unwrap();
            assert!(!run_dev_stop(&cwd, &env).status.success());
            assert_eq!(fs::read(&session_path).unwrap(), retained);
            // The operator must stop detached workers before acknowledging.
            drop(watch);
            if let Some(worker) = worker {
                assert!(wait_for_process_exit(worker, Duration::from_secs(3)));
            }
            if mode == "cancel" {
                for invalid in ["binding", "pending", "scope"] {
                    let mut changed = session.clone();
                    match invalid {
                        "binding" => {
                            changed["envRoot"] = path_string(&root.child("replacement")).into()
                        }
                        "pending" => {
                            changed["child"] = Value::Null;
                            changed["childSpawnPending"] = true.into();
                        }
                        "scope" => changed["processScope"] = "other-process-scope".into(),
                        _ => unreachable!(),
                    }
                    let bytes = serde_json::to_vec(&changed).unwrap();
                    fs::write(&session_path, &bytes).unwrap();
                    assert!(!recover().status.success(), "{invalid}");
                    assert_eq!(fs::read(&session_path).unwrap(), bytes, "{invalid}");
                }
                fs::write(&session_path, &retained).unwrap();
                let lock_path = source_watch_lock_path(&root, "demo");
                let generation = fs::read(&lock_path).unwrap();
                fs::write(&lock_path, "another-generation\n").unwrap();
                assert!(!recover().status.success());
                assert_eq!(fs::read(&session_path).unwrap(), retained);
                fs::write(&lock_path, generation).unwrap();
                let held_lease = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .unwrap();
                fs2::FileExt::try_lock_exclusive(&held_lease).unwrap();
                assert!(!recover().status.success());
                assert_eq!(fs::read(&session_path).unwrap(), retained);
                drop(held_lease);
                let request = session_path.with_extension("stop");
                fs::write(&request, "broken request").unwrap();
                assert!(!recover().status.success());
                assert_eq!(fs::read(&session_path).unwrap(), retained);
                fs::remove_file(request).unwrap();
            }
            let meta = get_environment("demo", &env, &cwd).unwrap();
            let config = Path::new(&meta.root).join(".openclaw/openclaw.json");
            let config_before = fs::read(&config).unwrap();
            let recovered = recover();
            assert!(recovered.status.success(), "{}", stderr(&recovered));
            assert_eq!(
                serde_json::from_str::<Value>(&stdout(&recovered)).unwrap(),
                serde_json::json!({"envName":"demo", "stopped":true, "serviceRestored":false}),
            );
            assert_eq!(read_source_watch_session(&root)["closed"], true);
            assert_eq!(fs::read(config).unwrap(), config_before);
            let after = get_environment("demo", &env, &cwd).unwrap();
            assert_eq!(after.service_enabled, meta.service_enabled);
            assert_eq!(after.service_running, meta.service_running);
            assert!(run_dev_stop(&cwd, &env).status.success());
            let retry = run_ocm(&cwd, &env, &["dev", "demo", "--no-watch", "--no-ui"]);
            assert!(retry.status.success(), "{}", stderr(&retry));
            let removed = run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"]);
            assert!(removed.status.success(), "{}", stderr(&removed));
            assert!(
                repo.is_dir(),
                "recovery/removal must preserve borrowed source"
            );
        }
        terminal_drain.finish();
        assert_eq!(
            fs::read_to_string(root.child("setup-tty")).unwrap(),
            "[true,true,true]"
        );
    }
}

#[cfg(unix)]
#[test]
fn dev_service_preparation_cancellation_preserves_its_running_service() {
    let root = TestDir::new("dev-service-owned-preparation");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env_with_gateway_admission(&root);
    install_fake_dev_runners(&root, &mut env);
    let initial = run_ocm(
        &cwd,
        &env,
        &["dev", "demo", "--repo", &path_string(&repo), "--service"],
    );
    assert!(initial.status.success(), "{}", stderr(&initial));
    let meta = get_environment("demo", &env, &cwd).unwrap();
    assert!(meta.service_running && meta.service_enabled);
    let state_path = root.child("ocm-home/supervisor/state.json");
    let initial_state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(initial_state["children"].as_array().unwrap().len(), 1);
    let config_path = Path::new(&meta.root).join(".openclaw/openclaw.json");
    let original_config = fs::read(&config_path).unwrap();
    let (started, _, _) = install_blocking_fake_dev_runners(&root, &mut env);
    let mut preparation = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &["dev", "demo", "--service", "--onboard"],
    );
    assert!(wait_for_path(&started, Duration::from_secs(10)));
    let session = read_source_watch_session(&root);
    assert_eq!(session["servicePreparation"], true);
    assert_eq!(session["watching"], false);
    assert_eq!(session["restoreService"], false);
    assert!(session["child"]["pid"].is_number());
    assert!(get_environment("demo", &env, &cwd).unwrap().service_running);

    let mut edited_config: Value = serde_json::from_slice(&original_config).unwrap();
    edited_config["gateway"]["port"] = (meta.gateway_port.unwrap() + 1000).into();
    let edited_config = serde_json::to_vec(&edited_config).unwrap();
    fs::write(&config_path, &edited_config).unwrap();
    let supervisor = ocm::supervisor::SupervisorService::new(&env, &cwd);
    supervisor.sync().unwrap();
    let during: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(
        during["children"], initial_state["children"],
        "preparation changed the running service plan"
    );
    let conflicting_start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(!conflicting_start.status.success());
    let conflicting_foreground = run_ocm(&cwd, &env, &["dev", "demo"]);
    assert!(!conflicting_foreground.status.success());
    assert!(stderr(&conflicting_foreground).contains("service preparation"));

    let stopped = run_dev_stop(&cwd, &env);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    let output = preparation.wait_without_release();
    assert_eq!(output.status.code(), Some(130), "{}", stderr(&output));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&stopped)).unwrap()["serviceRestored"],
        false
    );
    let completed = read_source_watch_session(&root);
    assert_eq!(completed["closed"], true);
    assert_eq!(completed["restoreService"], false);
    let after = get_environment("demo", &env, &cwd).unwrap();
    assert!(after.service_running && after.service_enabled);
    assert_eq!(after.dev, meta.dev);
    assert_eq!(fs::read(&config_path).unwrap(), edited_config);
    let after_state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(after_state["children"], initial_state["children"]);

    // A completed preparation does not make an unchanged --service rerun an
    // install/stop/start cycle. The native start owner retains the same plan.
    fs::write(&config_path, original_config).unwrap();
    let repeated = run_ocm(&cwd, &env, &["dev", "demo", "--service"]);
    assert!(repeated.status.success(), "{}", stderr(&repeated));
    let repeated_state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(repeated_state["children"], initial_state["children"]);
    assert_eq!(
        repeated_state["restartRequests"],
        initial_state["restartRequests"]
    );
    assert!(get_environment("demo", &env, &cwd).unwrap().service_running);

    // A newer stop intent wins even when the service was already stopped.
    drop(preparation);
    fs::remove_file(root.child("source-watch.release")).unwrap();
    fs::remove_file(&started).unwrap();
    let stop = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(stop.status.success(), "{}", stderr(&stop));
    let preparation = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &["dev", "demo", "--service", "--onboard"],
    );
    assert!(wait_for_path(&started, Duration::from_secs(10)));
    let stop = run_ocm(&cwd, &env, &["service", "stop", "demo"]);
    assert!(stop.status.success(), "{}", stderr(&stop));
    let output = preparation.finish();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("service policy changed during service preparation"));
    assert!(!get_environment("demo", &env, &cwd).unwrap().service_running);
    assert_eq!(read_source_watch_session(&root)["closed"], true);
    let final_state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert!(final_state["children"].as_array().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn dev_stop_cancels_owned_preparation_before_gateway_start() {
    for (phase, mode) in [
        ("dependencies", "watch"),
        ("probe", "watch"),
        ("onboard", "watch"),
        ("dependencies", "plain"),
        ("probe", "plain"),
        ("onboard", "plain"),
        ("dependencies", "service"),
        ("probe", "service"),
        ("onboard", "service"),
    ] {
        let root = TestDir::new(&format!("dev-stop-{phase}-{mode}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env_with_gateway_admission(&root);
        install_probe_aware_fake_dev_runners(&root, &mut env);
        create_owned_dev_env(&repo, "demo", &env, &cwd);
        let prepare = run_ocm(
            &cwd,
            &env,
            &dev_plain(&["demo", "--repo", &path_string(&repo)]),
        );
        assert!(prepare.status.success(), "{}", stderr(&prepare));
        let meta = get_environment("demo", &env, &cwd).unwrap();
        let worktree = Path::new(meta.dev.as_ref().unwrap().source_root());
        if phase != "onboard" {
            declare_source_tooling(worktree);
        }
        let started = root.child("preparation.started");
        let child_pid = root.child("preparation.pid");
        let release = root.child("source-watch.release");
        let acknowledgment = match phase {
            "probe" => "",
            "dependencies" => "trap 'exit 0' TERM\n",
            _ => "trap 'exit 143' TERM\n",
        };
        let script = format!(
            "#!/bin/sh\n{acknowledgment}printf '%s\\n' \"$$\" > '{}'\nprintf 'ready\\n' > '{}'\nwhile [ ! -f '{}' ]; do /bin/sleep 0.05; done\nprintf '[]\\n'\n",
            path_string(&child_pid),
            path_string(&started),
            path_string(&release),
        );
        if phase != "dependencies" {
            write_fake_dev_node(&root, &script);
        } else {
            write_executable_script(&root.child("fake-dev-bin/pnpm"), &script);
        }
        let mut args = match mode {
            "watch" => dev_watch(&["demo", "--watch"]),
            "service" => vec!["dev", "demo", "--service"],
            _ => dev_plain(&["demo"]),
        };
        if phase == "onboard" {
            args.push("--onboard");
        }
        let watch = DevWatchFixture::spawn(&root, &cwd, &env, &args);
        assert!(
            wait_for_path(&started, Duration::from_secs(30)),
            "{phase} did not start"
        );
        let pid = fs::read_to_string(&child_pid)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let session = read_source_watch_session(&root);
        assert_eq!(session["child"]["pid"], pid);
        assert_eq!(session["watching"], mode == "watch");
        assert_eq!(session["servicePreparation"], mode == "service");
        let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
        let stopped = run_dev_stop(&cwd, &env);
        let watched = watch.finish();

        assert!(stopped.status.success(), "{phase}: {}", stderr(&stopped));
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&stopped)).unwrap()["serviceRestored"],
            false
        );
        assert_eq!(
            watched.status.code(),
            Some(130),
            "{phase}: {}",
            stderr(&watched)
        );
        assert!(
            wait_for_process_exit(pid, Duration::from_secs(2)),
            "{phase} outlived stop completion"
        );
        assert!(status.status.success(), "{}", stderr(&status));
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&status)).unwrap()["sourceWatch"]["state"],
            "starting"
        );
        assert!(!source_watch_override_path(&root, "demo").exists());
        assert!(worktree.join("package.json").is_file());
        assert!(Path::new(&meta.root).is_dir());
        assert_eq!(read_source_watch_session(&root)["closed"], true);
        assert!(!get_environment("demo", &env, &cwd).unwrap().service_running);
    }
}

#[cfg(unix)]
#[test]
fn dev_stop_recovers_a_crashed_controller_and_its_stubborn_tree_before_restoration() {
    let root = TestDir::new("dev-stop-crashed-tree");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env_with_gateway_admission(&root);
    install_fake_dev_runners(&root, &mut env);
    create_runtime_backed_env(&cwd, &env);
    let start = run_ocm(&cwd, &env, &["service", "start", "demo"]);
    assert!(start.status.success(), "{}", stderr(&start));
    let started = root.child("source-watch.started");
    let descendant_pid = root.child("source-watch-descendant.pid");
    let release = root.child("source-watch.release");
    let node = format!(
        "#!/bin/sh\ntrap '' TERM\n/bin/sleep 300 &\nowned_child=$!\nprintf '%s\\n' \"$owned_child\" > '{}'\nprintf 'ready\\n' > '{}'\nwhile [ ! -f '{}' ]; do /bin/sleep 0.05; done\nkill -KILL \"$owned_child\"\nwait \"$owned_child\"\n",
        path_string(&descendant_pid),
        path_string(&started),
        path_string(&release),
    );
    write_fake_dev_node(&root, &node);
    let mut watch = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &[
            "dev",
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
            "--force",
            "--no-ui",
        ],
    );
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "watch did not start"
    );
    let pid = read_source_watch_session(&root)["child"]["pid"]
        .as_u64()
        .unwrap() as u32;
    let descendant = fs::read_to_string(&descendant_pid)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    encode_legacy_watch_session(&root);
    watch.crash_controller();
    assert!(process_is_alive(pid));
    assert!(!get_environment("demo", &env, &cwd).unwrap().service_running);
    let stopped = run_dev_stop(&cwd, &env);
    drop(watch);

    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&stopped)).unwrap()["serviceRestored"],
        true
    );
    assert!(
        wait_for_process_exit(pid, Duration::from_secs(3)),
        "watch survived stop"
    );
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(3)),
        "descendant survived stop"
    );
    assert!(get_environment("demo", &env, &cwd).unwrap().service_running);
    assert_eq!(read_source_watch_session(&root)["closed"], true);
    assert!(!source_watch_override_path(&root, "demo").exists());
}

#[cfg(unix)]
#[test]
fn dev_stop_refuses_unverified_orphan_identity_without_signaling_the_tree() {
    for invalid in ["start", "range", "scope"] {
        let root = TestDir::new(&format!("dev-stop-stale-{invalid}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        let (started, _, _) = install_blocking_fake_dev_runners(&root, &mut env);
        let mut watch = DevWatchFixture::spawn(
            &root,
            &cwd,
            &env,
            &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
        );
        assert!(
            wait_for_path(&started, Duration::from_secs(30)),
            "watch did not start"
        );
        let session_path = source_watch_override_path(&root, "demo").with_extension("session");
        // This control preserves the released legacy recovery contract.
        encode_legacy_watch_session(&root);
        let before = fs::read(&session_path).unwrap();
        let mut session: Value = serde_json::from_slice(&before).unwrap();
        let pid = session["child"]["pid"].as_u64().unwrap() as u32;
        watch.crash_controller();
        match invalid {
            "start" => session["child"]["startedAt"] = "different-process-start".into(),
            "range" => session["child"]["pid"] = u32::MAX.into(),
            "scope" => session["processScope"] = "another-process-scope".into(),
            _ => unreachable!(),
        }
        let changed = serde_json::to_vec(&session).unwrap();
        fs::write(&session_path, &changed).unwrap();
        let stopped = run_dev_stop(&cwd, &env);
        let survived = process_is_alive(pid);
        let preserved = fs::read(&session_path).unwrap() == changed;
        fs::write(&session_path, before).unwrap();
        let recovered = run_dev_stop(&cwd, &env);
        drop(watch);

        assert!(
            !stopped.status.success(),
            "{invalid} unexpectedly permitted stop"
        );
        assert!(survived, "{invalid} signaled an unverified process");
        assert!(preserved, "{invalid} discarded unfinished ownership");
        assert!(
            recovered.status.success(),
            "{invalid}: {}",
            stderr(&recovered)
        );
        assert!(!get_environment("demo", &env, &cwd).unwrap().service_running);
        assert!(wait_for_process_exit(pid, Duration::from_secs(3)));
    }
}

#[cfg(unix)]
#[test]
fn dev_stop_rejects_a_different_lease_and_can_resume_a_suspended_controller() {
    let root = TestDir::new("dev-stop-generation");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    let (started, _, _) = install_blocking_fake_dev_runners(&root, &mut env);
    let watch = DevWatchFixture::spawn(
        &root,
        &cwd,
        &env,
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
    );
    assert!(
        wait_for_path(&started, Duration::from_secs(30)),
        "watch did not start"
    );
    let controller = watch.child.as_ref().unwrap().id();
    assert_eq!(unsafe { libc::kill(controller as i32, libc::SIGSTOP) }, 0);
    assert!(wait_for_process_stop(controller, Duration::from_secs(3)));
    let lease_path = source_watch_lock_path(&root, "demo");
    let lease = fs::read(&lease_path).unwrap();
    fs::write(&lease_path, "different-session-generation").unwrap();
    let refused = run_dev_stop(&cwd, &env);
    fs::write(&lease_path, lease).unwrap();
    let stopped = run_dev_stop(&cwd, &env);
    let watched = watch.finish();

    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("generation changed"),
        "{}",
        stderr(&refused)
    );
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert_eq!(watched.status.code(), Some(130), "{}", stderr(&watched));
    assert_eq!(read_source_watch_session(&root)["closed"], true);
}

#[cfg(unix)]
#[test]
fn dev_watch_reuses_active_session_and_reclaims_the_released_lock() {
    let root = TestDir::new("dev-command-watch-overlap");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let (started, release, watch_log) = install_blocking_fake_dev_runners(&root, &mut env);
    create_owned_dev_env(&repo, "demo", &env, &cwd);

    let mut first = Command::new(env!("CARGO_BIN_EXE_ocm"));
    first
        .current_dir(&cwd)
        .args(dev_watch(&[
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
        ]))
        .env_clear()
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = first.spawn().unwrap();
    let did_start = wait_for_path(&started, Duration::from_secs(30));
    let override_path = source_watch_override_path(&root, "demo");
    let did_write_override = wait_for_path(&override_path, Duration::from_secs(30));
    let override_before_overlap = fs::read_to_string(&override_path).unwrap_or_default();

    let meta = get_environment("demo", &env, &cwd).unwrap();
    let config_path = Path::new(&meta.root).join(".openclaw/openclaw.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["gateway"].as_object_mut().unwrap().remove("mode");
    config["gateway"].as_object_mut().unwrap().remove("bind");
    let config_before = serde_json::to_vec(&config).unwrap();
    fs::write(&config_path, &config_before).unwrap();
    let worktree = Path::new(meta.dev.as_ref().unwrap().source_root());
    let manifest_before = fs::read(worktree.join("package.json")).unwrap();
    declare_source_tooling(worktree);
    let pnpm_before = fs::read(root.child("pnpm.log")).ok();

    let overlap = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    let conflicting = [
        vec!["dev", "demo", "--no-watch", "--no-ui"],
        vec!["dev", "demo", "--service"],
        vec![
            "dev",
            "demo",
            "--watch",
            "--repo",
            cwd.to_str().unwrap(),
            "--no-ui",
        ],
        vec![
            "dev",
            "demo",
            "--watch",
            "--root",
            cwd.to_str().unwrap(),
            "--no-ui",
        ],
        vec!["dev", "demo", "--watch", "--port", "1", "--no-ui"],
        vec!["dev", "demo", "--watch", "--onboard", "--no-ui"],
    ]
    .map(|args| run_ocm(&cwd, &env, &args));
    let first_port = meta.gateway_port.unwrap();
    let next_port = first_port + 1;
    config["gateway"]["port"] = serde_json::json!(next_port);
    let changed_config = serde_json::to_vec(&config).unwrap();
    fs::write(&config_path, &changed_config).unwrap();
    let changed = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    let same_port = run_ocm(
        &cwd,
        &env,
        &dev_watch(&["demo", "--watch", "--port", &first_port.to_string()]),
    );
    let other_port = run_ocm(
        &cwd,
        &env,
        &dev_watch(&["demo", "--watch", "--port", &next_port.to_string()]),
    );
    let status = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    let resolved = ocm::env::EnvironmentService::new(&env, &cwd)
        .resolve_gateway_process("demo", false)
        .unwrap();
    let routed = ocm::env::EnvironmentService::new(&env, &cwd)
        .resolve("demo", None, None, &["status".to_string()])
        .unwrap();
    let routed_port = match routed {
        ocm::env::ResolvedExecution::SourceWatch { env, .. } => env.gateway_port,
        _ => None,
    };
    let config_after_port_change = fs::read(&config_path).unwrap();
    fs::write(&config_path, &config_before).unwrap();
    let config_after = fs::read(&config_path).unwrap();
    let pnpm_after = fs::read(root.child("pnpm.log")).ok();
    let meta_after = get_environment("demo", &env, &cwd).unwrap();
    let mut route_env = env.clone();
    let route_bin = root.child("route-bin");
    write_executable_script(&route_bin.join("node"), "#!/bin/sh\nexit 0\n");
    prepend_fake_bin(&mut route_env, &route_bin);
    let exec_args = ["env", "exec", "demo", "--", "openclaw", "--version"];
    let valid_exec = run_ocm(&cwd, &route_env, &exec_args);
    let git_path = worktree.join(".git");
    let backlink = fs::read(&git_path).unwrap();
    fs::remove_file(&git_path).unwrap();
    init_nested_openclaw_repo(worktree);
    let mut invalid_routes = Vec::new();
    for with_endpoint in [true, false] {
        if !with_endpoint {
            let mut source: Value = serde_json::from_str(&override_before_overlap).unwrap();
            source.as_object_mut().unwrap().remove("endpoint");
            fs::write(&override_path, serde_json::to_vec(&source).unwrap()).unwrap();
        }
        let service = ocm::env::EnvironmentService::new(&env, &cwd);
        invalid_routes.push(service.resolve("demo", None, None, &[]).err());
        invalid_routes.push(service.resolve_gateway_process("demo", false).err());
        let exec = run_ocm(&cwd, &route_env, &exec_args);
        invalid_routes.push((!exec.status.success()).then(|| stderr(&exec)));
    }
    fs::remove_dir_all(&git_path).unwrap();
    fs::write(&git_path, backlink).unwrap();
    fs::remove_file(worktree.join("SENTINEL")).unwrap();
    fs::write(&override_path, &override_before_overlap).unwrap();
    fs::write(worktree.join("package.json"), manifest_before).unwrap();
    fs::remove_file(worktree.join("scripts/tsx.mjs")).unwrap();
    let override_after_overlap = fs::read_to_string(&override_path).unwrap_or_default();
    fs::write(&release, "release\n").unwrap();
    let first_output = first.wait_with_output().unwrap();

    assert!(valid_exec.status.success(), "{}", stderr(&valid_exec));
    for error in invalid_routes {
        assert!(
            error.as_deref().is_some_and(
                |error| error.contains("registered worktree is not a valid OpenClaw checkout")
            ),
            "active source route accepted the replacement: {error:?}"
        );
    }
    assert!(
        did_start,
        "first source watch did not start: {}",
        stderr(&first_output)
    );
    assert!(
        did_write_override,
        "first source watch did not publish its override"
    );
    assert!(overlap.status.success(), "{}", stderr(&overlap));
    assert!(stderr(&overlap).contains("is active; keeping the existing session"));
    assert!(stdout(&overlap).contains(&meta.gateway_port.unwrap().to_string()));
    for attempt in conflicting {
        assert!(!attempt.status.success(), "{}", stdout(&attempt));
    }
    assert!(changed.status.success(), "{}", stderr(&changed));
    assert!(same_port.status.success(), "{}", stderr(&same_port));
    assert!(!other_port.status.success());
    assert!(stderr(&other_port).contains(&format!("is using port {first_port}")));
    assert!(stdout(&changed).contains(&format!("http://127.0.0.1:{first_port}")));
    assert!(!stdout(&changed).contains(&format!("http://127.0.0.1:{next_port}")));
    assert!(status.status.success(), "{}", stderr(&status));
    let status: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(status["gatewayPort"], first_port);
    assert_eq!(status["sourceWatch"]["watching"], true);
    assert_eq!(
        resolved.process_env.get("OPENCLAW_GATEWAY_PORT"),
        Some(&first_port.to_string())
    );
    assert!(
        resolved
            .args
            .windows(2)
            .any(|pair| pair == ["--port", &first_port.to_string()])
    );
    assert_eq!(routed_port, Some(first_port));
    assert_eq!(config_after_port_change, changed_config);
    assert_eq!(config_after, config_before);
    assert_eq!(pnpm_after, pnpm_before);
    assert_eq!(
        serde_json::to_value(meta_after).unwrap(),
        serde_json::to_value(&meta).unwrap()
    );
    assert_eq!(override_after_overlap, override_before_overlap);
    assert!(first_output.status.success(), "{}", stderr(&first_output));
    assert!(source_watch_lock_path(&root, "demo").exists());
    assert!(!source_watch_override_path(&root, "demo").exists());

    let after_release = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    assert!(after_release.status.success(), "{}", stderr(&after_release));
    let starts = fs::read_to_string(watch_log).unwrap();
    assert_eq!(starts.lines().count(), 2);
}

#[cfg(unix)]
#[test]
fn dev_watch_lost_claim_preserves_config_and_explicit_endpoint() {
    for explicit_port in [false, true] {
        let root = TestDir::new("dev-watch-lost-claim");
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env(&root);
        install_fake_dev_runners(&root, &mut env);
        let created = run_ocm(
            &cwd,
            &env,
            &dev_plain(&["demo", "--repo", &path_string(&repo)]),
        );
        assert!(created.status.success(), "{}", stderr(&created));
        let meta = get_environment("demo", &env, &cwd).unwrap();
        let old_port = meta.gateway_port.unwrap();
        let requested_port = old_port.to_string();
        let config_path = Path::new(&meta.root).join(".openclaw/openclaw.json");
        let (started, release, watch_log) = install_blocking_fake_dev_runners(&root, &mut env);
        let paused = root.child("validation.paused");
        let resume = root.child("validation.resume");
        let real_git = Command::new("/bin/sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(real_git.status.success());
        let gate_bin = root.child("gated-git");
        write_executable_script(
            &gate_bin.join("git"),
            r#"#!/bin/sh
if [ "$3" = worktree ] && [ "$4" = list ]; then
  printf 'paused\n' > "$OCM_TEST_GIT_PAUSED"
  while [ ! -f "$OCM_TEST_GIT_RESUME" ]; do /bin/sleep 0.02; done
fi
exec "$OCM_TEST_REAL_GIT" "$@"
"#,
        );
        let mut losing_env = env.clone();
        prepend_fake_bin(&mut losing_env, &gate_bin);
        losing_env.insert(
            "OCM_TEST_REAL_GIT".to_string(),
            stdout(&real_git).trim().to_string(),
        );
        losing_env.insert("OCM_TEST_GIT_PAUSED".to_string(), path_string(&paused));
        losing_env.insert("OCM_TEST_GIT_RESUME".to_string(), path_string(&resume));
        let mut args = dev_watch(&["demo", "--watch"]);
        if explicit_port {
            args.extend(["--port", &requested_port]);
        }
        let mut loser = Command::new(env!("CARGO_BIN_EXE_ocm"))
            .current_dir(&cwd)
            .args(args)
            .env_clear()
            .envs(&losing_env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let did_pause = wait_for_path(&paused, Duration::from_secs(5));
        if explicit_port {
            let mut config: Value =
                serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
            config["gateway"]["port"] = serde_json::json!(old_port + 1);
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        }
        let winner = Command::new(env!("CARGO_BIN_EXE_ocm"))
            .current_dir(&cwd)
            .args(dev_watch(&["demo", "--watch"]))
            .env_clear()
            .envs(&env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let did_start = wait_for_path(&started, Duration::from_secs(5));
        let override_path = source_watch_override_path(&root, "demo");
        let did_publish = wait_for_path(&override_path, Duration::from_secs(5));
        let override_before = fs::read(&override_path).unwrap_or_default();
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["gateway"].as_object_mut().unwrap().remove("mode");
        config["gateway"].as_object_mut().unwrap().remove("bind");
        let config_before = serde_json::to_vec(&config).unwrap();
        fs::write(&config_path, &config_before).unwrap();
        fs::write(&resume, "resume\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let did_exit = loop {
            if loser.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };
        if !did_exit {
            let _ = loser.kill();
        }
        let loser = loser.wait_with_output().unwrap();
        let config_after = fs::read(&config_path).unwrap();
        let override_after = fs::read(&override_path).unwrap_or_default();
        fs::write(&release, "release\n").unwrap();
        let winner = winner.wait_with_output().unwrap();
        assert!(
            did_pause && did_start && did_publish && did_exit,
            "loser: {}; winner: {}",
            stderr(&loser),
            stderr(&winner)
        );
        assert!(winner.status.success(), "{}", stderr(&winner));
        if explicit_port {
            assert!(!loser.status.success());
            assert!(
                stderr(&loser).contains(&format!("is using port {}", old_port + 1)),
                "{}",
                stderr(&loser)
            );
        } else {
            assert!(loser.status.success(), "{}", stderr(&loser));
            assert!(stderr(&loser).contains("keeping the existing session"));
        }
        assert_eq!(config_after, config_before);
        assert_eq!(override_after, override_before);
        assert_eq!(fs::read_to_string(watch_log).unwrap().lines().count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn dev_watch_lease_survives_parent_crash_until_the_watcher_exits() {
    let root = TestDir::new("dev-command-watch-parent-crash");
    let repo = init_openclaw_repo(&root);
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = service_env(&root);
    let (started, release, watch_log) = install_blocking_fake_dev_runners(&root, &mut env);

    let mut first = Command::new(env!("CARGO_BIN_EXE_ocm"));
    first
        .current_dir(&cwd)
        .args(dev_watch(&[
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
        ]))
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

    encode_legacy_watch_session(&root);
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
        .args(dev_watch(&["demo", "--watch"]))
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
    assert!(overlap_exited, "reusing a surviving source watch blocked");
    assert!(overlap.status.success(), "{}", stderr(&overlap));
    assert!(
        stderr(&overlap).contains("is active; keeping the existing session"),
        "{}",
        stderr(&overlap)
    );

    fs::write(&release, "release\n").unwrap();
    assert!(wait_for_process_exit(watcher_pid, Duration::from_secs(10)));

    let after_release = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
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
        .args(dev_watch(&[
            "demo",
            "--repo",
            &path_string(&repo),
            "--watch",
        ]))
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

    assert!(!watch.status.success(), "{}", stderr(&watch));
    assert!(
        wait_for_process_exit(descendant_pid, Duration::from_secs(3)),
        "source watch descendant {descendant_pid} survived forced shutdown"
    );
    assert!(source_watch_override_path(&root, "demo").exists());
    let session = read_source_watch_session(&root);
    assert_eq!(session["closed"], false);
    assert!(
        session["completion"]["error"]
            .as_str()
            .unwrap()
            .contains("terminated by signal")
    );
    assert!(!run_dev_stop(&cwd, &env).status.success());
    assert!(
        !run_ocm(&cwd, &env, &["env", "destroy", "demo", "--yes"])
            .status
            .success()
    );
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
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
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
        &dev_watch(&["demo", "--repo", &repo_path, "--watch"]),
        false,
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
            "--no-ui",
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
    let capability = enable_fake_daemon_gateway_admission(&root, &mut env);
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
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        gateway_admission: Some(capability),
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
            "--no-ui",
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
    let mut env = service_env_with_gateway_admission(&root);
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
            "--no-ui",
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
    let mut env = service_env_with_gateway_admission(&root);
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
            "--no-ui",
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

    let run = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
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
    let mut env = service_env_with_gateway_admission(&root);
    install_fake_dev_runners(&root, &mut env);

    let service = run_ocm(
        &cwd,
        &env,
        &["dev", "demo", "--repo", &path_string(&repo), "--service"],
    );
    assert!(service.status.success(), "{}", stderr(&service));

    let watch = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch", "--force"]));
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
