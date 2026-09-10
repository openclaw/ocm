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

use ocm::store::{get_environment, now_utc, save_environment, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeService, SupervisorRuntimeState};
use serde_json::Value;

use crate::support::{
    TestDir, dev_plain, dev_watch, enable_fake_daemon_gateway_admission,
    install_fake_service_manager, ocm_env, path_string, run_ocm, stderr, stdout,
    write_executable_script,
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
            session: source_watch_override_path(root, args[1]).with_extension("session"),
        }
    }

    fn crash_controller(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }

    fn wait_without_release(&mut self) -> std::process::Output {
        let deadline = Instant::now() + Duration::from_secs(20);
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
        let _ = fs::write(&self.release, "release\n");
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

#[test]
fn dev_command_provisions_worktree_bootstraps_config_and_runs_gateway() {
    let root = TestDir::new("dev-command-run");
    let repo = init_openclaw_repo(&root);
    let canonical_repo = fs::canonicalize(&repo).unwrap();
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

    assert_eq!(show_json["devRepoRoot"], path_string(&canonical_repo));
    assert!(worktree_root.starts_with(canonical_repo.join(".worktrees")));
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
    assert!(!pnpm_log.contains("|install"));
    assert!(pnpm_log.contains("openclaw gateway run --port"));
    assert!(pnpm_log.contains(&path_string(&worktree_root)));
    assert!(pnpm_log.contains(&path_string(&config_path)));
    assert!(pnpm_log.contains(&format!(
        "|bundled={}",
        path_string(&worktree_root.join("extensions"))
    )));
    assert!(pnpm_log.contains(&format!("|devroot={}", path_string(&worktree_root))));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let auth = serde_json::json!({"mode": "token", "token": {"source": "env", "provider": "default", "id": "AUTHORED_TOKEN"}});
        let mut authored = config.clone();
        authored["gateway"]["auth"] = auth.clone();
        fs::write(&config_path, serde_json::to_vec(&authored).unwrap()).unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        let resumed = run_ocm(&cwd, &env, &["dev", "demo"]);
        assert!(resumed.status.success(), "{}", stderr(&resumed));
        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "private config lost owner-only access on dev restart"
        );
        let updated: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        assert_eq!(updated["gateway"]["auth"], auth);
    }
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
    let worktree = PathBuf::from(meta.dev.unwrap().worktree_root);
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
    fs::remove_file(root.child("pnpm.log")).unwrap();

    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    let log = fs::read_to_string(root.child("pnpm.log")).unwrap();
    assert!(log.contains("openclaw gateway run"));
    assert!(!log.contains("|install"));

    #[cfg(unix)]
    {
        let modules = worktree.join("node_modules");
        let linked_modules = worktree.join("installed-modules");
        fs::rename(&modules, &linked_modules).unwrap();
        std::os::unix::fs::symlink(&linked_modules, &modules).unwrap();
        fs::remove_file(root.child("pnpm.log")).unwrap();
        let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
        assert!(resumed.status.success(), "{}", stderr(&resumed));
        let log = fs::read_to_string(root.child("pnpm.log")).unwrap();
        assert!(!log.contains("|install"));
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
    install_frozen_source_dependency_runner(&root);
    env.insert("OCM_TEST_INSTALL_EXIT_CODE".to_string(), "42".to_string());

    let failed = run_ocm(
        &cwd,
        &env,
        &dev_watch(&["demo", "--repo", &path_string(&repo), "--watch"]),
    );
    assert_eq!(failed.status.code(), Some(42), "{}", stderr(&failed));
    let meta = get_environment("demo", &env, &cwd).unwrap();
    let worktree = PathBuf::from(meta.dev.unwrap().worktree_root);
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
            .filter(|line| line.ends_with("|install --frozen-lockfile"))
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
    for (relative, configured) in [
        ("node_modules", false),
        ("node_modules/.pnpm", false),
        ("installed-modules", true),
        ("installed-modules/.pnpm", true),
    ] {
        let root = TestDir::new("dev-dependencies-linked-install");
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
        let worktree = PathBuf::from(
            get_environment("demo", &env, &cwd)
                .unwrap()
                .dev
                .unwrap()
                .worktree_root,
        );
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
        fs::remove_file(root.child("pnpm.log")).unwrap();

        let failed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
        assert!(!failed.status.success());
        assert!(stderr(&failed).contains("refusing to install"));
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert!(!root.child("pnpm.log").exists());
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
    fs::remove_file(root.child("pnpm.log")).unwrap();

    let second = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(!second.status.success());
    assert!(stderr(&second).contains("saved dev worktree is missing"));
    assert!(!worktree_root.exists());
    assert_eq!(git_worktree_paths(&repo), registered);
    assert!(!root.child("pnpm.log").exists());
    let meta = get_environment("demo", &env, &cwd).unwrap();
    assert_eq!(meta.dev.unwrap().worktree_root, path_string(&worktree_root));

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

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    let dev = meta.dev.as_mut().unwrap();
    let original_worktree = PathBuf::from(&dev.worktree_root);
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
    dev.worktree_root = path_string(&recorded_worktree);
    save_environment(meta, &env, &cwd).unwrap();
    fs::write(recorded_worktree.join("SENTINEL"), "keep my edits\n").unwrap();
    fs::write(
        recorded_worktree.join("scripts/run-node.mjs"),
        "console.log('edited');\n",
    )
    .unwrap();
    fs::remove_file(root.child("pnpm.log")).unwrap();

    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert!(!original_worktree.exists());
    assert_eq!(
        fs::read_to_string(recorded_worktree.join("SENTINEL")).unwrap(),
        "keep my edits\n"
    );
    assert_eq!(
        fs::read_to_string(recorded_worktree.join("scripts/run-node.mjs")).unwrap(),
        "console.log('edited');\n"
    );
    let pnpm_log = fs::read_to_string(root.child("pnpm.log")).unwrap();
    assert!(pnpm_log.contains("openclaw gateway run --port"));
    let source_prefix = format!(
        "{}|",
        path_string(&fs::canonicalize(&recorded_worktree).unwrap())
    );
    assert!(
        pnpm_log
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

    let first = run_ocm(
        &cwd,
        &env,
        &dev_plain(&["demo", "--repo", &path_string(&repo)]),
    );
    assert!(first.status.success(), "{}", stderr(&first));
    let unrelated_worktree = root.child("unrelated-source");
    init_nested_openclaw_repo(&unrelated_worktree);
    let mut meta = get_environment("demo", &env, &cwd).unwrap();
    meta.dev.as_mut().unwrap().worktree_root = path_string(&unrelated_worktree);
    save_environment(meta, &env, &cwd).unwrap();
    fs::remove_file(root.child("pnpm.log")).unwrap();

    let resumed = run_ocm(&cwd, &env, &dev_plain(&["demo"]));
    assert!(!resumed.status.success());
    assert!(stderr(&resumed).contains("saved dev worktree is not registered"));
    assert!(!root.child("pnpm.log").exists());
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
    assert_eq!(after.repo_root, before.repo_root);
    assert_eq!(after.worktree_root, before.worktree_root);

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
    let after = get_environment("demo", &env, &cwd).unwrap().dev.unwrap();
    assert_eq!(after.repo_root, before.repo_root);
    assert_eq!(after.worktree_root, before.worktree_root);
}

#[test]
fn dev_command_rejects_a_stale_registration_replaced_by_an_unrelated_clone() {
    let root = TestDir::new("dev-command-stale-replacement");
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
        ],
    );
    assert!(run.status.success(), "{}", stderr(&run));

    let pnpm_log = fs::read_to_string(root.child("pnpm.log")).unwrap();
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
    assert_eq!(reachable["serviceRunning"], false);
    assert_eq!(reachable["sourceWatch"]["state"], "inactive");
    drop(listener);
    let closed = run_ocm(&cwd, &env, &["dev", "status", "demo", "--json"]);
    assert!(closed.status.success(), "{}", stderr(&closed));
    let closed: Value = serde_json::from_str(&stdout(&closed)).unwrap();
    assert_eq!(closed["gatewayPortReachable"], false);
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

    let mut other_env = env.clone();
    let other_home = root.child("other-ocm-home");
    other_env.insert("OCM_HOME".to_string(), path_string(&other_home));
    save_environment(
        get_environment("demo", &env, &cwd).unwrap(),
        &other_env,
        &cwd,
    )
    .unwrap();
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

#[cfg(unix)]
#[test]
fn dev_command_records_the_canonical_explicit_source() {
    let root = TestDir::new("dev-command-canonical-source");
    let repo = init_openclaw_repo(&root);
    let alias = root.child("source-alias");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();
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

    let status = run_ocm(&cwd, &env, &["service", "status", "demo", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let status_json: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(status_json["bindingKind"], "dev");
    assert_eq!(status_json["bindingName"], "dev");
    assert_eq!(status_json["desiredRunning"], true);

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
            assert!(!Path::new(&dev.worktree_root).exists());
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
fn dev_watch_retains_unverified_worker_ownership_across_retries() {
    struct OwnedProcess(std::process::Child);
    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for failure in ["eof", "signal", "controller"] {
        let root = TestDir::new(&format!("dev-watch-unverified-{failure}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = service_env_with_gateway_admission(&root);
        env.insert("OCM_TEST_FOREGROUND_ROOT".into(), path_string(root.path()));
        env.insert("OCM_TEST_WATCH_FAILURE".into(), failure.into());
        create_runtime_backed_env(&cwd, &env);
        let started_service = run_ocm(&cwd, &env, &["service", "start", "demo"]);
        assert!(
            started_service.status.success(),
            "{}",
            stderr(&started_service)
        );
        // Each worker has a separate group and no inherited lease descriptor.
        // EOF holds the outer stderr pipe; the other rows use private pipes.
        fs::write(repo.join("scripts/watch-node.mjs"), r#"
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
        let args = ["dev", "demo", "--repo", &repo_arg, "--watch", "--force"];
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
        assert_eq!(session["kind"], "ocm-source-watch-session-v2");
        assert!(process_is_alive(descendant));
        // The existing module and worker are loaded. Any broken future admission
        // now runs a finite entry instead of spawning another detached worker.
        fs::write(
            repo.join("scripts/watch-node.mjs"),
            "console.log('unexpected restart');\n",
        )
        .unwrap();
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
fn dev_watch_interactive_setup_requires_completion_evidence() {
    for success in [true, false] {
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
        let created = run_ocm(&cwd, &env, &["dev", "demo", "--repo", &path_string(&repo)]);
        assert!(created.status.success(), "{}", stderr(&created));
        env.insert("OCM_TEST_FOREGROUND_ROOT".into(), path_string(root.path()));
        env.insert(
            "OCM_TEST_SETUP_SUCCESS".into(),
            if success { "1" } else { "0" }.into(),
        );
        let script = root.child("setup.mjs");
        fs::write(&script, r#"
import fs from 'node:fs'; import path from 'node:path'; import { isatty } from 'node:tty';
import { spawn } from 'node:child_process';
const root = process.env.OCM_TEST_FOREGROUND_ROOT;
fs.writeFileSync(path.join(root,'setup-tty'), JSON.stringify([0,1,2].map(isatty)));
if (process.env.OCM_TEST_SETUP_SUCCESS === '1') process.exit(0);
process.on('SIGTERM', () => process.exit(0));
const worker = `const fs=require('node:fs'),path=require('node:path'),root=process.env.OCM_TEST_FOREGROUND_ROOT,file=path.join(root,'source-watch-descendant.pid');fs.writeFileSync(file+'.tmp',String(process.pid));fs.renameSync(file+'.tmp',file);setInterval(()=>{if(fs.existsSync(path.join(root,'source-watch.release')))process.exit(0)},25);`;
spawn(process.execPath,['-e',worker],{detached:true,stdio:'ignore',env:process.env}).unref();
setInterval(() => { if (fs.existsSync(path.join(root,'source-watch.release'))) process.exit(0); },25);
"#).unwrap();
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        write_executable_script(
            &root.child("fake-dev-bin/pnpm"),
            &format!(
                "#!/bin/sh\nexec {} {}\n",
                quote(&node),
                quote(&path_string(&script))
            ),
        );
        let (child, terminal) = spawn_ocm_with_controlling_pty(
            &cwd,
            &env,
            &["dev", "demo", "--watch", "--onboard"],
            true,
        );
        // Keep draining until after the watch guard cleans up, including on panic.
        let terminal_drain = PtyOutputDrain::start(terminal);
        let mut watch = DevWatchFixture {
            child: Some(child),
            release: root.child("source-watch.release"),
            session: source_watch_override_path(&root, "demo").with_extension("session"),
        };
        if success {
            assert!(watch.wait_without_release().status.success());
            assert_eq!(read_source_watch_session(&root)["closed"], true);
            drop(watch);
        } else {
            let pid_path = root.child("source-watch-descendant.pid");
            assert!(wait_for_path(&pid_path, Duration::from_secs(20)));
            let worker = fs::read_to_string(pid_path)
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap();
            let stopped = run_dev_stop(&cwd, &env);
            let output = watch.wait_without_release();
            if stopped.status.success() {
                drop(watch);
                assert!(wait_for_process_exit(worker, Duration::from_secs(3)));
                panic!(
                    "cancelled inherited-output setup certified cleanup after exit0; fixture cleanup verified"
                );
            }
            assert!(!output.status.success() && process_is_alive(worker));
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
            drop(watch);
            assert!(wait_for_process_exit(worker, Duration::from_secs(3)));
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
fn dev_stop_cancels_owned_preparation_before_gateway_start() {
    for phase in ["dependencies", "probe", "onboard"] {
        let root = TestDir::new(&format!("dev-stop-{phase}"));
        let repo = init_openclaw_repo(&root);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        install_probe_aware_fake_dev_runners(&root, &mut env);
        let prepare = run_ocm(
            &cwd,
            &env,
            &dev_plain(&["demo", "--repo", &path_string(&repo)]),
        );
        assert!(prepare.status.success(), "{}", stderr(&prepare));
        let meta = get_environment("demo", &env, &cwd).unwrap();
        let worktree = Path::new(&meta.dev.as_ref().unwrap().worktree_root);
        if phase != "onboard" {
            declare_source_tooling(worktree);
        }
        let started = root.child("preparation.started");
        let child_pid = root.child("preparation.pid");
        let release = root.child("source-watch.release");
        let acknowledgment = if phase == "probe" {
            ""
        } else {
            "trap 'exit 143' TERM\n"
        };
        let script = format!(
            "#!/bin/sh\n{acknowledgment}printf '%s\\n' \"$$\" > '{}'\nprintf 'ready\\n' > '{}'\nwhile [ ! -f '{}' ]; do /bin/sleep 0.05; done\nprintf '[]\\n'\n",
            path_string(&child_pid),
            path_string(&started),
            path_string(&release),
        );
        if phase == "probe" {
            write_fake_dev_node(&root, &script);
        } else {
            write_executable_script(&root.child("fake-dev-bin/pnpm"), &script);
        }
        let mut args = dev_watch(&["demo", "--watch"]);
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
    let worktree = Path::new(&meta.dev.as_ref().unwrap().worktree_root);
    let manifest_before = fs::read(worktree.join("package.json")).unwrap();
    declare_source_tooling(worktree);
    let pnpm_before = fs::read(root.child("pnpm.log")).ok();

    let overlap = run_ocm(&cwd, &env, &dev_watch(&["demo", "--watch"]));
    let conflicting = [
        vec!["dev", "demo", "--watch", "--repo", cwd.to_str().unwrap()],
        vec!["dev", "demo", "--watch", "--root", cwd.to_str().unwrap()],
        vec!["dev", "demo", "--watch", "--port", "1"],
        vec!["dev", "demo", "--watch", "--onboard"],
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
    fs::write(worktree.join("package.json"), manifest_before).unwrap();
    fs::remove_file(worktree.join("scripts/tsx.mjs")).unwrap();
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
