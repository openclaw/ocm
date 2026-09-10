#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ocm::env::{CreateEnvironmentOptions, EnvDevMeta, EnvironmentService};
use ocm::store::{env_registry_path, supervisor_runtime_path, supervisor_state_path};
use serde_json::{Value, json};

use support::{
    TestDir, dev_watch, install_fake_launchctl, install_fake_systemd_tools,
    managed_service_definition_path, ocm_env, ocm_test_binary_path, path_string, run_ocm, stderr,
    write_executable_script, write_json_replacing_path,
};

struct AdmissionFixture {
    root: TestDir,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    repo: PathBuf,
    definition: PathBuf,
    runtime: PathBuf,
    query: PathBuf,
    daemon: Option<Child>,
    launchd: bool,
}

impl AdmissionFixture {
    fn new(label: &str) -> Self {
        Self::with_manager(label, cfg!(target_os = "macos"))
    }

    fn with_manager(label: &str, launchd: bool) -> Self {
        let root = TestDir::new(label);
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let mut env = ocm_env(&root);
        if launchd {
            env.insert(
                "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
                "launchd".to_string(),
            );
            install_fake_launchctl(&root, &mut env);
        } else {
            install_fake_systemd_tools(&root, &mut env);
        }
        let runtime_binary = root.child("bin/openclaw");
        write_executable_script(&runtime_binary, "#!/bin/sh\nexit 0\n");
        let added = run_ocm(
            &cwd,
            &env,
            &[
                "runtime",
                "add",
                "stable",
                "--path",
                &path_string(&runtime_binary),
            ],
        );
        assert!(added.status.success(), "{}", stderr(&added));
        let created = run_ocm(
            &cwd,
            &env,
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
        let installed = run_ocm(&cwd, &env, &["service", "install", "demo"]);
        assert!(installed.status.success(), "{}", stderr(&installed));
        assert!(
            !EnvironmentService::new(&env, &cwd)
                .get("demo")
                .unwrap()
                .service_running
        );

        let repo = root.child("source/openclaw");
        fs::create_dir_all(repo.join("scripts")).unwrap();
        fs::write(repo.join("package.json"), r#"{"name":"openclaw"}"#).unwrap();
        fs::write(repo.join("openclaw.mjs"), "// fixture\n").unwrap();
        fs::write(repo.join("scripts/run-node.mjs"), "// fixture\n").unwrap();
        fs::write(repo.join("scripts/watch-node.mjs"), "// fixture\n").unwrap();
        fs::write(repo.join("SENTINEL"), "preserve source\n").unwrap();
        let node = root.child("fake-bin/node");
        write_executable_script(
            &node,
            &format!(
                "#!/bin/bash\nif [ -n \"$OCM_SOURCE_WATCH_START_FD\" ]; then\n  IFS= read -r ocm_start < \"/dev/fd/$OCM_SOURCE_WATCH_START_FD\" || exit 1\nfi\nprintf '%s\\n' \"$*\" >> '{}'\n",
                path_string(&root.child("node.log")),
            ),
        );
        let definition = managed_service_definition_path(&env, &cwd, "demo");
        let runtime = supervisor_runtime_path(&env, &cwd).unwrap();
        let query = root.child("manager-query");
        let query_output = root.child("manager-output");
        write_executable_script(
            &query,
            &format!("#!/bin/sh\n/bin/cat '{}'\n", path_string(&query_output),),
        );
        env.insert(
            if launchd {
                "OCM_INTERNAL_LAUNCHCTL_BIN"
            } else {
                "OCM_INTERNAL_SYSTEMCTL_BIN"
            }
            .to_string(),
            path_string(&query),
        );
        Self {
            root,
            cwd,
            env,
            repo,
            definition,
            runtime,
            query,
            daemon: None,
            launchd,
        }
    }

    fn start_daemon(&mut self) -> Value {
        // No services are desired running, so the owned daemon has no child
        // processes. Its real native identity supplies the capability fixture.
        let daemon = Command::new(ocm_test_binary_path())
            .args(["__daemon", "run"])
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = daemon.id();
        self.daemon = Some(daemon);
        self.set_manager_state("running", pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(raw) = fs::read(&self.runtime)
                && let Ok(runtime) = serde_json::from_slice::<Value>(&raw)
                && runtime["gatewayAdmission"]["process"]["pid"] == pid
            {
                assert_eq!(runtime["children"], json!([]));
                assert_eq!(runtime["gatewayAdmission"]["version"], 7);
                assert!(
                    runtime["gatewayAdmission"]["process"]["startedAt"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                // Freeze this empty daemon while invalid metadata is supplied.
                // Otherwise its last startup publication could race a fixture
                // write. The direct Child remains unreaped and owns this PID.
                assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
                let mut status = 0;
                let waited = unsafe { libc::waitpid(pid as i32, &mut status, libc::WUNTRACED) };
                if waited == pid as i32 && !libc::WIFSTOPPED(status) {
                    self.daemon.take();
                    panic!("empty daemon exited before fixture suspension");
                }
                assert_eq!(waited, pid as i32);
                assert!(libc::WIFSTOPPED(status));
                return runtime;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon did not publish its process-bound admission capability");
    }

    fn stop_daemon(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    fn set_manager_state(&self, state: &str, pid: u32) {
        let definition = path_string(&self.definition);
        let output = if self.launchd {
            format!("path = {definition}\nstate = {state}\npid = {pid}\n")
        } else {
            let (active, sub) = match state {
                "running" => ("active", "running"),
                "stopped" => ("inactive", "dead"),
                _ => ("activating", "auto-restart"),
            };
            format!(
                "LoadState=loaded\nUnitFileState=enabled\nActiveState={active}\nSubState={sub}\nMainPID={pid}\nFragmentPath={definition}\n"
            )
        };
        fs::write(self.root.child("manager-output"), output).unwrap();
    }

    fn watch(&self) -> std::process::Output {
        run_ocm(
            &self.cwd,
            &self.env,
            &dev_watch(&[
                "demo",
                "--repo",
                &path_string(&self.repo),
                "--watch",
                "--force",
            ]),
        )
    }

    fn assert_refused_without_mutation(&self, case: &str) {
        let registry = env_registry_path(&self.env, &self.cwd).unwrap();
        let before = fs::read(&registry).unwrap();
        let runtime_before = fs::read(&self.runtime).ok();
        let rejected = self.watch();
        assert!(!rejected.status.success(), "{case}");
        assert!(
            stderr(&rejected).contains("service refresh-daemon --acknowledge-gateway-restarts"),
            "{case}: {}",
            stderr(&rejected)
        );
        assert_eq!(fs::read(&registry).unwrap(), before, "{case}");
        assert_eq!(fs::read(&self.runtime).ok(), runtime_before, "{case}");
        assert!(
            !self
                .root
                .child("ocm-home/source-watch/demo.session")
                .exists(),
            "{case}"
        );
        assert!(!self.root.child("node.log").exists(), "{case}");
        assert_eq!(
            fs::read_to_string(self.repo.join("SENTINEL")).unwrap(),
            "preserve source\n"
        );
    }
}

impl Drop for AdmissionFixture {
    fn drop(&mut self) {
        self.stop_daemon();
    }
}

#[test]
fn dev_watch_requires_the_current_managed_daemon_capability() {
    let mut fixture = AdmissionFixture::new("dev-daemon-capability");
    let current = fixture.start_daemon();
    let cases: [(&str, Box<dyn Fn(&mut Value)>); 6] = [
        (
            "missing capability",
            Box::new(|value| {
                value.as_object_mut().unwrap().remove("gatewayAdmission");
            }),
        ),
        (
            "unknown version",
            Box::new(|value| value["gatewayAdmission"]["version"] = json!(2)),
        ),
        (
            "stale process",
            Box::new(|value| value["gatewayAdmission"]["process"]["startedAt"] = json!("stale")),
        ),
        (
            "different process",
            Box::new(|value| {
                value["gatewayAdmission"]["process"]["pid"] = json!(std::process::id())
            }),
        ),
        (
            "different scope",
            Box::new(|value| {
                value["gatewayAdmission"]["processScope"] = json!("another-process-scope")
            }),
        ),
        (
            "different store",
            Box::new(|value| value["ocmHome"] = json!("/different-store")),
        ),
    ];
    for (case, mutate) in cases {
        let mut runtime = current.clone();
        mutate(&mut runtime);
        write_json_replacing_path(&fixture.runtime, &runtime);
        fixture.assert_refused_without_mutation(case);
    }
    fs::write(&fixture.runtime, "{").unwrap();
    fixture.assert_refused_without_mutation("unreadable runtime record");
    fs::remove_file(&fixture.runtime).unwrap();
    fixture.assert_refused_without_mutation("absent runtime record");
    let desired_path = supervisor_state_path(&fixture.env, &fixture.cwd).unwrap();
    let desired = fs::read(&desired_path).unwrap();
    fs::remove_file(&desired_path).unwrap();
    fixture.assert_refused_without_mutation("owned definition without desired or runtime records");
    fs::write(desired_path, desired).unwrap();

    write_json_replacing_path(&fixture.runtime, &current);
    let definition = fs::read(&fixture.definition).unwrap();
    fs::remove_file(&fixture.definition).unwrap();
    fixture.assert_refused_without_mutation("deleted loaded definition");
    fs::write(
        &fixture.definition,
        String::from_utf8(definition.clone())
            .unwrap()
            .replace(fixture.env.get("OCM_HOME").unwrap(), "/different-store"),
    )
    .unwrap();
    fixture.assert_refused_without_mutation("replaced loaded definition owner");
    fs::write(&fixture.definition, definition).unwrap();

    assert_eq!(
        unsafe { libc::kill(fixture.daemon.as_ref().unwrap().id() as i32, libc::SIGCONT) },
        0,
    );
    let admitted = fixture.watch();
    assert!(admitted.status.success(), "{}", stderr(&admitted));
    assert!(
        fs::read_to_string(fixture.root.child("node.log"))
            .unwrap()
            .contains("scripts/watch-node.mjs")
    );
    let session: Value = serde_json::from_slice(
        &fs::read(fixture.root.child("ocm-home/source-watch/demo.session")).unwrap(),
    )
    .unwrap();
    assert_eq!(session["closed"], true);
}

#[test]
fn dev_watch_rejects_unknown_daemon_before_creating_an_environment() {
    let fixture = AdmissionFixture::new("dev-daemon-preflight");
    fixture.set_manager_state("starting", 0);
    let registry = env_registry_path(&fixture.env, &fixture.cwd).unwrap();
    let before = fs::read(&registry).unwrap();
    let root = fixture.root.child("fresh-env");
    let rejected = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &dev_watch(&[
            "fresh",
            "--repo",
            &path_string(&fixture.repo),
            "--root",
            &path_string(&root),
            "--watch",
        ]),
    );
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("starting"),
        "{}",
        stderr(&rejected)
    );
    assert!(!root.exists());
    assert!(!fixture.repo.join(".worktrees").exists());
    assert_eq!(fs::read(registry).unwrap(), before);

    write_executable_script(&fixture.query, "#!/bin/sh\nexit 69\n");
    fixture.assert_refused_without_mutation("manager unavailable");
}

#[test]
fn dev_watch_checks_launchd_ownership_without_searching_path_for_id() {
    let mut fixture = AdmissionFixture::with_manager("dev-daemon-launchd-path", true);
    fixture.start_daemon();
    let empty_bin = fixture.root.child("empty-bin");
    fs::create_dir_all(&empty_bin).unwrap();
    fixture
        .env
        .insert("PATH".to_string(), path_string(&empty_bin));
    let failed_spawn = fixture.watch();
    assert!(!failed_spawn.status.success());
    assert_eq!(failed_spawn.status.code(), Some(127));
    assert!(
        stderr(&failed_spawn).contains("node") && stderr(&failed_spawn).contains("not found"),
        "{}",
        stderr(&failed_spawn)
    );
}

#[test]
fn dev_watch_allows_confirmed_stopped_and_unloaded_daemons() {
    let fixture = AdmissionFixture::new("dev-daemon-stopped");
    fixture.set_manager_state("stopped", 0);
    fs::write(&fixture.runtime, "{old runtime record").unwrap();
    let stopped = fixture.watch();
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    assert!(fixture.definition.exists());

    let absent = if cfg!(target_os = "macos") {
        "#!/bin/sh\nprintf 'Could not find service \"ai.openclaw.ocm\" in domain for user gui\\n' >&2\nexit 113\n"
    } else {
        "#!/bin/sh\nprintf 'LoadState=not-found\\nActiveState=inactive\\nSubState=dead\\nMainPID=0\\n'\nexit 4\n"
    };
    write_executable_script(&fixture.query, absent);
    let unloaded = fixture.watch();
    assert!(unloaded.status.success(), "{}", stderr(&unloaded));
    assert!(fixture.definition.exists());
}

#[test]
fn saved_dev_plan_revalidates_source_and_binding_before_launch() {
    let mut fixture = AdmissionFixture::new("dev-daemon-saved-plan");
    fixture.set_manager_state("stopped", 0);
    fixture.repo = fs::canonicalize(&fixture.repo).unwrap();
    let worktree = fixture.repo.join(".worktrees/source-env");
    let worktree_path = path_string(&worktree);
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=OCM Tests",
            "-c",
            "user.email=tests@example.com",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
        vec!["worktree", "add", "--detach", "--quiet", &worktree_path],
    ] {
        let git = Command::new("git")
            .args(args)
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(git.status.success(), "{}", stderr(&git));
    }
    let command_log = fixture.root.child("pnpm.log");
    write_executable_script(
        &fixture.root.child("fake-bin/pnpm"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$*\" >> '{}'\n",
            path_string(&command_log)
        ),
    );
    let launcher = run_ocm(
        &fixture.cwd,
        &fixture.env,
        &["launcher", "add", "custom", "--command", "openclaw"],
    );
    assert!(launcher.status.success(), "{}", stderr(&launcher));
    let source = EnvironmentService::new(&fixture.env, &fixture.cwd)
        .create(CreateEnvironmentOptions {
            name: "source-env".to_string(),
            root: Some(path_string(&fixture.root.child("source-env"))),
            gateway_port: None,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: Some(EnvDevMeta {
                repo_root: path_string(&fixture.repo),
                worktree_root: worktree_path.clone(),
            }),
            protected: false,
        })
        .unwrap();
    let state_path = supervisor_state_path(&fixture.env, &fixture.cwd).unwrap();
    let saved = fs::read(&state_path).unwrap();
    let plan: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(plan["children"].as_array().unwrap().len(), 1);
    assert_eq!(plan["children"][0]["bindingKind"], "dev");
    assert_eq!(plan["children"][0]["bindingName"], "dev");
    assert_eq!(plan["children"][0]["runDir"], worktree_path);
    let control = run_ocm(&fixture.cwd, &fixture.env, &["__daemon", "run", "--once"]);
    assert!(control.status.success(), "{}", stderr(&control));
    let launched = fs::read_to_string(&command_log).unwrap();
    assert!(launched.starts_with(&format!("{worktree_path}\nopenclaw gateway run")));
    fs::remove_file(&command_log).unwrap();
    let worktree_git = worktree.join(".git");
    let git_link = fs::read(&worktree_git).unwrap();

    for case in [
        "replaced-worktree",
        "saved-source",
        "binding-name",
        "runtime",
        "launcher",
    ] {
        let mut stale = plan.clone();
        let mut changed = source.clone();
        let expected = if case == "replaced-worktree" {
            "registered worktree is not a valid OpenClaw checkout"
        } else if case == "saved-source" {
            "saved dev source no longer matches"
        } else {
            "saved dev plan no longer matches the registered binding"
        };
        match case {
            "replaced-worktree" => {
                fs::remove_file(&worktree_git).unwrap();
                let git = Command::new("git")
                    .args(["init", "--quiet"])
                    .current_dir(&worktree)
                    .env_clear()
                    .envs(&fixture.env)
                    .output()
                    .unwrap();
                assert!(git.status.success(), "{}", stderr(&git));
            }
            "saved-source" => stale["children"][0]["runDir"] = json!(path_string(&fixture.repo)),
            "binding-name" => stale["children"][0]["bindingName"] = json!("old-dev"),
            "runtime" => changed.default_runtime = Some("stable".to_string()),
            "launcher" => changed.default_launcher = Some("custom".to_string()),
            _ => unreachable!(),
        }
        ocm::store::save_environment(changed, &fixture.env, &fixture.cwd).unwrap();
        if case == "replaced-worktree" {
            let error = EnvironmentService::new(&fixture.env, &fixture.cwd)
                .resolve("source-env", None, None, &[])
                .err()
                .expect("fresh resolution accepted the replacement worktree");
            assert!(error.contains(expected), "{error}");
        }
        write_json_replacing_path(&state_path, &stale);
        let before = fs::read(&state_path).unwrap();
        let daemon = Command::new(ocm_test_binary_path())
            .args(["__daemon", "run"])
            .current_dir(&fixture.cwd)
            .env_clear()
            .envs(&fixture.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = daemon.id();
        fixture.daemon = Some(daemon);
        fixture.set_manager_state("running", pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(!command_log.exists(), "{case} launched the stale command");
            if let Ok(raw) = fs::read(&fixture.runtime)
                && let Ok(runtime) = serde_json::from_slice::<Value>(&raw)
                && runtime["gatewayAdmission"]["process"]["pid"] == pid
                && runtime["services"].as_array().is_some_and(|services| {
                    services.iter().any(|service| {
                        service["envName"] == source.name
                            && service["lastError"]
                                .as_str()
                                .is_some_and(|error| error.contains(expected))
                    })
                })
            {
                assert_eq!(runtime["children"], json!([]), "{case}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{case} did not record its admission refusal"
            );
            let daemon = fixture.daemon.as_mut().unwrap();
            assert!(daemon.try_wait().unwrap().is_none());
            thread::sleep(Duration::from_millis(20));
        }
        fixture.stop_daemon();
        fixture.set_manager_state("stopped", 0);
        if case == "replaced-worktree" {
            fs::remove_dir_all(&worktree_git).unwrap();
            fs::write(&worktree_git, &git_link).unwrap();
        }
        assert_eq!(
            fs::read(&state_path).unwrap(),
            before,
            "{case} rewrote its saved plan"
        );
        assert!(!command_log.exists(), "{case} launched the stale command");
        assert!(!fixture.root.child("node.log").exists(), "{case}");
    }
}
