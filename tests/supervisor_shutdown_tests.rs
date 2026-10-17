#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use ocm::store::{supervisor_runtime_path, supervisor_state_path};
use serde_json::Value;
use support::{
    TestDir, install_fake_launchctl, ocm_env, ocm_test_binary_path, run_ocm, stderr,
    write_executable_script,
};

struct ShutdownFixture {
    root: TestDir,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    runtime: PathBuf,
    daemon: Option<Child>,
    groups: Vec<(i32, i32)>,
}

impl ShutdownFixture {
    fn new() -> Self {
        let root = TestDir::new("supervisor-owned-shutdown");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
        install_fake_launchctl(&root, &mut env);
        let fail = root.child("fail-group");
        write_executable_script(
            &root.child("fake-bin/kill"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ -f '{}' ] && [ \"$3\" = \"$(/bin/cat '{}')\" ]; then\n  printf '%s\\n' \"$*\" >> '{}'\n  exit 1\nfi\nexec /bin/kill \"$@\"\n",
                root.child("signals.log").display(),
                fail.display(),
                fail.display(),
                root.child("failed-kill.log").display()
            ),
        );
        let heartbeat = root.child("heartbeat.sh");
        write_executable_script(
            &heartbeat,
            "#!/bin/sh\ntrap '' HUP\nwhile :; do printf . >> \"$1\"; /bin/sleep 0.05; done\n",
        );
        for name in ["target", "sibling"] {
            let leader = root.child(format!("{name}.sh"));
            write_executable_script(
                &leader,
                &format!(
                    "#!/bin/sh\ntrap '' HUP\n/bin/sh '{}' '{}' &\ndescendant=$!\nprintf '%s %s' \"$$\" \"$descendant\" > '{}'\nwait \"$descendant\"\n",
                    heartbeat.display(),
                    root.child(format!("{name}.heartbeat")).display(),
                    root.child(format!("{name}.pids")).display()
                ),
            );
            let launcher = format!("exec /bin/sh '{}'", leader.display());
            for args in [
                vec!["launcher", "add", name, "--command", launcher.as_str()],
                vec!["env", "create", name, "--launcher", name],
                vec!["service", "start", name],
            ] {
                let output = run_ocm(&cwd, &env, &args);
                assert!(output.status.success(), "{}", stderr(&output));
            }
        }
        let runtime = supervisor_runtime_path(&env, &cwd).unwrap();
        Self {
            root,
            cwd,
            env,
            runtime,
            daemon: None,
            groups: Vec::new(),
        }
    }

    fn start(&mut self) -> Value {
        let log = fs::File::create(self.root.child("daemon.log")).unwrap();
        self.daemon = Some(
            Command::new(ocm_test_binary_path())
                .args(["__daemon", "run", "--json"])
                .env_clear()
                .envs(&self.env)
                .current_dir(&self.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(bytes) = fs::read(&self.runtime)
                && let Ok(runtime) = serde_json::from_slice::<Value>(&bytes)
                && runtime["children"]
                    .as_array()
                    .is_some_and(|children| children.len() == 2)
                && ["target", "sibling"]
                    .iter()
                    .all(|name| self.root.child(format!("{name}.pids")).exists())
            {
                for name in ["target", "sibling"] {
                    let pids = fs::read_to_string(self.root.child(format!("{name}.pids"))).unwrap();
                    let pids: Vec<i32> = pids
                        .split_whitespace()
                        .map(|pid| pid.parse().unwrap())
                        .collect();
                    assert_eq!(pids.len(), 2);
                    assert_eq!(unsafe { libc::getpgid(pids[1]) }, pids[0]);
                    self.groups.push((pids[0], pids[1]));
                }
                return runtime;
            }
            sleep(Duration::from_millis(20));
        }
        panic!(
            "daemon did not start: {}",
            fs::read_to_string(self.root.child("daemon.log")).unwrap()
        );
    }

    fn stop(&mut self) -> std::process::ExitStatus {
        let daemon = self.daemon.as_mut().unwrap();
        assert_eq!(unsafe { libc::kill(daemon.id() as i32, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = daemon.try_wait().unwrap() {
                return status;
            }
            sleep(Duration::from_millis(20));
        }
        panic!("daemon shutdown exceeded its bounded wait");
    }

    fn runtime(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.runtime).unwrap()).unwrap()
    }

    fn wait_for_cleanup_error(&self, target: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let runtime = self.runtime();
            if runtime["services"]
                .as_array()
                .unwrap()
                .iter()
                .any(|service| {
                    service["pid"] == target
                        && service["lastError"]
                            .as_str()
                            .is_some_and(|error| error.contains("cleanup is unverified"))
                })
            {
                return runtime;
            }
            sleep(Duration::from_millis(20));
        }
        panic!("cleanup failure was not retained: {}", self.runtime());
    }

    fn assert_heartbeat_advances(&self, name: &str) {
        let before = self.heartbeat(name);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.heartbeat(name) > before {
                return;
            }
            sleep(Duration::from_millis(20));
        }
        panic!("{name} heartbeat did not advance");
    }

    fn heartbeat(&self, name: &str) -> u64 {
        fs::metadata(self.root.child(format!("{name}.heartbeat")))
            .unwrap()
            .len()
    }
}

impl Drop for ShutdownFixture {
    fn drop(&mut self) {
        // Cover a setup assertion failing before start() records both groups.
        for name in ["target", "sibling"] {
            if let Ok(pids) = fs::read_to_string(self.root.child(format!("{name}.pids"))) {
                let pids: Vec<i32> = pids
                    .split_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                    .collect();
                if pids.len() == 2 && !self.groups.contains(&(pids[0], pids[1])) {
                    self.groups.push((pids[0], pids[1]));
                }
            }
        }
        if let Some(daemon) = &mut self.daemon {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        for (group, descendant) in &self.groups {
            // The fixture records and checks this still-live same-group child.
            if unsafe { libc::getpgid(*descendant) } == *group {
                unsafe {
                    libc::kill(-*group, libc::SIGKILL);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let unsettled = self.groups.iter().any(|(_, descendant)| {
                // Zombies cannot execute; Linux test runners may defer reaping them.
                let output = Command::new("/bin/ps")
                    .args(["-o", "stat=", "-p", &descendant.to_string()])
                    .output();
                match output {
                    Ok(output) if output.status.success() => {
                        !String::from_utf8_lossy(&output.stdout)
                            .trim()
                            .starts_with('Z')
                    }
                    Ok(output) if output.status.code() == Some(1) && output.stdout.is_empty() => {
                        false
                    }
                    _ => true,
                }
            });
            if !unsettled {
                break;
            }
            if Instant::now() >= deadline {
                if std::thread::panicking() {
                    eprintln!("fixture cleanup could not verify descendant settlement");
                } else {
                    panic!("fixture cleanup could not verify descendant settlement");
                }
                break;
            }
            sleep(Duration::from_millis(20));
        }
    }
}

fn child_pid(runtime: &Value, name: &str) -> u64 {
    find_child_pid(runtime, name).unwrap_or_else(|| panic!("missing {name} child: {runtime}"))
}

fn find_child_pid(runtime: &Value, name: &str) -> Option<u64> {
    runtime["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["envName"] == name)
        .map(|child| child["pid"].as_u64().unwrap())
}

#[test]
fn failed_owned_group_shutdown_retains_runtime_and_reports_failure() {
    let mut fixture = ShutdownFixture::new();
    let before = fixture.start();
    let target = child_pid(&before, "target");
    fs::write(fixture.root.child("fail-group"), format!("-{target}")).unwrap();
    let status = fixture.stop();
    let after: Value = serde_json::from_slice(&fs::read(&fixture.runtime).unwrap()).unwrap();
    let sibling_heartbeat = fixture.heartbeat("sibling");
    fixture.assert_heartbeat_advances("target");
    let sibling_settled = fixture.heartbeat("sibling") == sibling_heartbeat;
    eprintln!(
        "shutdown={status}; target heartbeat advanced; sibling_settled={sibling_settled}; runtime_children={}",
        after["children"]
    );
    assert!(sibling_settled, "successful sibling cleanup regressed");
    assert!(
        !status.success(),
        "shutdown falsely reported completed cleanup"
    );
    assert!(
        after["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["pid"] == target)
    );
    assert!(
        fs::read_to_string(fixture.root.child("daemon.log"))
            .unwrap()
            .contains("cleanup")
    );
}

#[test]
fn failed_target_stop_blocks_replacement_without_stalling_siblings() {
    for trigger in ["remove", "restart", "exit"] {
        let mut fixture = ShutdownFixture::new();
        let before = fixture.start();
        let target = child_pid(&before, "target");
        let sibling = child_pid(&before, "sibling");
        fs::write(fixture.root.child("fail-group"), format!("-{target}")).unwrap();
        if trigger == "exit" {
            assert_eq!(unsafe { libc::kill(target as i32, libc::SIGKILL) }, 0);
        } else {
            let args = if trigger == "remove" {
                vec!["service", "stop", "target"]
            } else {
                vec!["service", "restart", "target", "--force"]
            };
            let output = run_ocm(&fixture.cwd, &fixture.env, &args);
            if trigger == "restart" {
                assert!(
                    !output.status.success(),
                    "restart must not certify replacement"
                );
                assert!(
                    stderr(&output).contains("restart was not observed"),
                    "{}",
                    stderr(&output)
                );
            } else {
                assert!(output.status.success(), "{}", stderr(&output));
            }
        }
        let retained = fixture.wait_for_cleanup_error(target);
        assert_eq!(child_pid(&retained, "target"), target);
        assert_eq!(child_pid(&retained, "sibling"), sibling);
        fixture.assert_heartbeat_advances("target");
        fixture.assert_heartbeat_advances("sibling");
        if trigger == "restart" {
            let state: Value = serde_json::from_slice(
                &fs::read(supervisor_state_path(&fixture.env, &fixture.cwd).unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(state["restartRequests"][0]["envName"], "target");
        }
        let failures = fs::read(fixture.root.child("failed-kill.log")).unwrap();
        sleep(Duration::from_millis(450));
        assert_eq!(
            fs::read(fixture.root.child("failed-kill.log")).unwrap(),
            failures,
            "unchanged cleanup retried on the polling loop: {trigger}"
        );
        assert_eq!(child_pid(&fixture.runtime(), "target"), target);
        fs::remove_file(fixture.root.child("fail-group")).unwrap();
        assert!(
            fixture.stop().success(),
            "cleanup retry at shutdown failed: {trigger}"
        );
        assert!(fixture.runtime()["children"].as_array().unwrap().is_empty());
    }
}

#[test]
fn owned_group_shutdown_settles_term_and_kill_paths() {
    for ignore_term in [false, true] {
        let mut fixture = ShutdownFixture::new();
        if ignore_term {
            for file in ["target.sh", "sibling.sh", "heartbeat.sh"] {
                let path = fixture.root.child(file);
                let text = fs::read_to_string(&path)
                    .unwrap()
                    .replace("trap '' HUP", "trap '' HUP TERM");
                write_executable_script(&path, &text);
            }
        }
        fixture.start();
        assert!(fixture.stop().success());
        assert!(fixture.runtime()["children"].as_array().unwrap().is_empty());
        let signals = fs::read_to_string(fixture.root.child("signals.log")).unwrap();
        assert!(
            signals.contains("-STOP") && signals.contains("-KILL"),
            "{signals}"
        );
        let before = [fixture.heartbeat("target"), fixture.heartbeat("sibling")];
        sleep(Duration::from_millis(150));
        assert_eq!(
            before,
            [fixture.heartbeat("target"), fixture.heartbeat("sibling")]
        );
    }
}

#[test]
fn post_reap_uncertainty_can_recover_after_group_disappears() {
    use ocm::supervisor::SupervisorService;
    use std::os::unix::process::CommandExt;
    struct OwnedProbe(Child);
    impl Drop for OwnedProbe {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut fixture = ShutdownFixture::new();
    let before = fixture.start();
    let target = child_pid(&before, "target");
    let mut unreaped = OwnedProbe(
        Command::new("/bin/sleep")
            .arg("60")
            .process_group(target as i32)
            .spawn()
            .unwrap(),
    );
    SupervisorService::new(&fixture.env, &fixture.cwd)
        .request_child_restart("target")
        .unwrap();
    let retained = fixture.wait_for_cleanup_error(target);
    let initial_error = retained["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["envName"] == "target")
        .unwrap()["lastError"]
        .as_str()
        .unwrap();
    eprintln!("first_cleanup_error={initial_error}");
    assert!(
        initial_error.contains("still exists after child exit")
            || initial_error.contains("failed verifying process group"),
        "probe missed the post-reap boundary"
    );
    unreaped.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let absent = unsafe { libc::kill(-(target as i32), 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if absent {
            break;
        }
        assert!(Instant::now() < deadline, "probe group did not disappear");
        sleep(Duration::from_millis(20));
    }
    let signals_before_retry = fs::read(fixture.root.child("signals.log")).unwrap();
    SupervisorService::new(&fixture.env, &fixture.cwd)
        .request_child_restart("target")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut after = fixture.runtime();
    // Pending ownership is published before the replacement PID.
    while find_child_pid(&after, "target").is_none_or(|pid| pid == target)
        && Instant::now() < deadline
    {
        sleep(Duration::from_millis(20));
        after = fixture.runtime();
    }
    fixture.assert_heartbeat_advances("sibling");
    assert_eq!(
        fs::read(fixture.root.child("signals.log")).unwrap(),
        signals_before_retry,
        "a post-reap retry must not signal the released group identity"
    );
    let replacement = find_child_pid(&after, "target");
    eprintln!(
        "original_group_absent=true; old_target={target}; observed_target={replacement:?}; services={}",
        after["services"]
    );
    assert!(
        replacement.is_some_and(|pid| pid != target),
        "fresh restart cannot retire an already-reaped child after cleanup uncertainty resolves: {after}"
    );
}
