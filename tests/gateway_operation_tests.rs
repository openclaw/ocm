#![cfg(target_os = "linux")]

mod support;

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use fs2::FileExt;
use ocm::env::EnvironmentService;
use ocm::store::{now_utc, supervisor_runtime_path};
use ocm::supervisor::{SupervisorRuntimeChild, SupervisorRuntimeState, SupervisorService};

use support::{
    TestDir, install_fake_launchctl, ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr,
    write_executable_script, write_json_replacing_path,
};

struct OwnedProcess(Child);

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // SAFETY: this unreaped fixture child either owns this process group
            // or has not detached yet. No unrelated process group is selected.
            unsafe { libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL) };
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn admission_has_waiter(admission: &File, pid: u32) -> bool {
    let metadata = admission.metadata().unwrap();
    let identity = format!(
        "{:02x}:{:02x}:{}",
        libc::major(metadata.dev()),
        libc::minor(metadata.dev()),
        metadata.ino()
    );
    let pid = pid.to_string();
    fs::read_to_string("/proc/locks")
        .unwrap()
        .lines()
        .any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.get(1) == Some(&"->")
                && fields.get(2) == Some(&"FLOCK")
                && fields.get(5) == Some(&pid.as_str())
                && fields.get(6) == Some(&identity.as_str())
        })
}

#[test]
fn gateway_owned_refresh_waits_for_pid_publication() {
    gateway_owned_operation_waits_for_pid_publication(false);
}

#[test]
fn gateway_owned_snapshot_waits_for_pid_publication() {
    gateway_owned_operation_waits_for_pid_publication(true);
}

fn gateway_owned_operation_waits_for_pid_publication(snapshot: bool) {
    let root = TestDir::new("gateway-operation-publication");
    let cwd = root.child("workspace");
    fs::create_dir_all(&cwd).unwrap();
    let mut env = ocm_env(&root);
    env.insert("OCM_INTERNAL_SERVICE_MANAGER".into(), "launchd".into());
    install_fake_launchctl(&root, &mut env);
    for args in [
        vec!["launcher", "add", "stable", "--command", "openclaw"],
        vec!["env", "create", "demo", "--launcher", "stable"],
        vec!["service", "start", "demo"],
    ] {
        let output = run_ocm(&cwd, &env, &args);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let spec = SupervisorService::new(&env, &cwd)
        .sync()
        .unwrap()
        .children
        .remove(0);
    let runtime_path = supervisor_runtime_path(&env, &cwd).unwrap();
    let publish = |pid: Option<u32>| {
        let runtime = SupervisorRuntimeState {
            kind: "ocm-supervisor-runtime".into(),
            ocm_home: env["OCM_HOME"].clone(),
            daemon_version: Some(env!("CARGO_PKG_VERSION").into()),
            gateway_admission: None,
            updated_at: now_utc(),
            services: Vec::new(),
            children: pid
                .into_iter()
                .map(|pid| SupervisorRuntimeChild {
                    env_name: spec.env_name.clone(),
                    binding_kind: spec.binding_kind.clone(),
                    binding_name: spec.binding_name.clone(),
                    pid,
                    restart_count: 0,
                    child_port: spec.child_port,
                    stdout_path: spec.stdout_path.clone(),
                    stderr_path: spec.stderr_path.clone(),
                })
                .collect(),
        };
        write_json_replacing_path(&runtime_path, &runtime);
    };
    publish(None);

    // Stand in for a Gateway with an actual, fixture-owned process group. Both
    // CLI commands use their normal entry points and process-preservation code.
    let spawn_gateway = || {
        OwnedProcess(
            Command::new("/bin/sleep")
                .arg("30")
                .env_clear()
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap(),
        )
    };
    let mut gateway = spawn_gateway();
    let gateway_pid = gateway.0.id();

    // Block the fake manager at shutdown so even a broken refresh cannot finish
    // before the fixture delivers the same group signal as the real supervisor.
    let stop_requested = root.child("stop-requested");
    let stop_released = root.child("stop-released");
    let manager = root.child("controlled-launchctl");
    write_executable_script(
        &manager,
        &format!(
            r#"#!/bin/sh
if [ "$1" = bootout ] && [ ! -f '{released}' ]; then
  : > '{requested}'
  attempt=0
  while [ ! -f '{released}' ] && [ "$attempt" -lt 500 ]; do
    /bin/sleep 0.01
    attempt=$((attempt + 1))
  done
  [ -f '{released}' ] || exit 1
fi
exec '{manager}' "$@"
"#,
            released = path_string(&stop_released),
            requested = path_string(&stop_requested),
            manager = env["OCM_INTERNAL_LAUNCHCTL_BIN"],
        ),
    );
    let mut operation_env = env.clone();
    operation_env.insert("OCM_INTERNAL_LAUNCHCTL_BIN".into(), path_string(&manager));
    operation_env.insert("OCM_ACTIVE_ENV".into(), "demo".into());
    operation_env.insert("OPENCLAW_SERVICE_KIND".into(), "gateway".into());

    let admission_path = root.child("ocm-home/source-watch/demo.admission");
    fs::create_dir_all(admission_path.parent().unwrap()).unwrap();
    let admission = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(admission_path)
        .unwrap();
    admission.lock_exclusive().unwrap();
    let output_path = root.child("operation-output");
    let output = File::create(&output_path).unwrap();
    let args = if snapshot {
        vec!["env", "snapshot", "create", "demo", "--json"]
    } else {
        vec![
            "service",
            "refresh-daemon",
            "--acknowledge-gateway-restarts",
            "--json",
        ]
    };
    let mut operation = OwnedProcess(
        Command::new(ocm_test_binary_path())
            .args(args)
            .env_clear()
            .envs(&operation_env)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .process_group(gateway_pid as i32)
            .spawn()
            .unwrap(),
    );

    // Observe the kernel's waiter for this exact process and lock, not a sleep
    // that merely hopes the command has reached the unpublished-PID interval.
    let deadline = Instant::now() + Duration::from_secs(5);
    let waited_for_publication = loop {
        if admission_has_waiter(&admission, operation.0.id()) {
            break true;
        }
        if stop_requested.exists()
            || operation.0.try_wait().unwrap().is_some()
            || Instant::now() >= deadline
        {
            break false;
        }
        sleep(Duration::from_millis(5));
    };
    // SAFETY: the fixture owns this live command process.
    let initial_group = unsafe { libc::getpgid(operation.0.id() as libc::pid_t) };
    publish(Some(gateway_pid));
    drop(admission);

    let env_service = EnvironmentService::new(&env, &cwd);
    let mut stopped = false;
    let mut stop_group = None;
    let mut replacement = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        let desired_running = env_service.get("demo").unwrap().service_running;
        if !stopped && (stop_requested.exists() || !desired_running) {
            // SAFETY: both the process and the group are owned by this fixture.
            stop_group = Some(unsafe { libc::getpgid(operation.0.id() as libc::pid_t) });
            assert_eq!(
                unsafe { libc::kill(-(gateway_pid as libc::pid_t), libc::SIGTERM) },
                0
            );
            gateway.0.wait().unwrap();
            publish(None);
            fs::write(&stop_released, "stopped\n").unwrap();
            stopped = true;
        }
        if snapshot && stopped && desired_running && replacement.is_none() {
            let child = spawn_gateway();
            publish(Some(child.0.id()));
            replacement = Some(child);
        }
        if let Some(status) = operation.0.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        sleep(Duration::from_millis(5));
    };
    let output = fs::read_to_string(output_path).unwrap();
    assert!(
        waited_for_publication,
        "command did not wait for Gateway publication: {output}"
    );
    assert_eq!(initial_group, gateway_pid as libc::pid_t);
    assert!(stopped, "command never stopped its Gateway: {output}");
    assert_eq!(
        stop_group,
        Some(operation.0.id() as libc::pid_t),
        "OCM was not preserved before stop: {output}"
    );
    assert!(
        result.is_some_and(|status| status.success()),
        "command did not survive Gateway stop: {result:?}\n{output}"
    );
    assert!(env_service.get("demo").unwrap().service_running);
    if snapshot {
        assert!(
            replacement.is_some(),
            "snapshot did not restore the service"
        );
    } else {
        assert!(output.contains("\"action\": \"refresh\""), "{output}");
    }
}
